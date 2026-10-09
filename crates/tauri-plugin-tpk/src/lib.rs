//! OTA frontend updates for Tauri v2.
//!
//! Content ships as signed `.tpk` packs which stack as layers over the binary's
//! embedded assets. The WebView keeps loading from `tauri://localhost`; the
//! swap is invisible to it. A revision is on trial until the frontend
//! acknowledges it, and an unacknowledged one is rolled back.
//!
//! ```ignore
//! fn main() {
//!     let mut context = tauri::generate_context!();
//!     let tpk = tauri_plugin_tpk::attach(&mut context);
//!
//!     tauri::Builder::default()
//!         .plugin(tauri_plugin_tpk::init(tpk))
//!         .run(context)
//!         .expect("error running app");
//! }
//! ```
//!
//! ```json
//! // tauri.conf.json
//! { "plugins": { "tpk": {
//!     "manifest_url": "https://cdn.example.com/tpk/{{channel}}/latest.json",
//!     "pubkeys": [{ "key": "RWT...", "epoch": 1 }]
//! } } }
//! ```
//!
//! The update URL and the trusted keys are native configuration with no runtime
//! setter. That is deliberate: a scripting bug in the frontend must not be able
//! to repoint the updater.
//!
//! **This is not a way around app store review.** Anything that changes what
//! the app *does* has to ship through the store. See `docs/security.md`.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod commands;
pub mod config;
pub mod error;
pub mod events;
pub mod outcome;
pub mod pack_assets;
pub mod state;

use std::sync::Arc;

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};
use tpk_format::sign::TrustStore;
use tpk_store::{Layout, Store};

pub use config::{PubKey, TpkConfig};
pub use error::{Error, Result};
pub use outcome::{CheckOutcome, DownloadOutcome, ReadyOutcome, Status};
pub use pack_assets::{attach, PackAssets, TpkHandle};
pub use state::TpkState;

/// Build the plugin from the handle [`attach`] returned.
///
/// Configuration comes from `plugins.tpk`. If that section is absent — which
/// includes a misspelled section name such as `plugins.tkp` — the plugin logs
/// and stays inert, and the app still starts on its embedded assets.
///
/// A `plugins.tpk` section that is present but does not deserialize (an unknown
/// or misspelled key, a missing `manifest_url` or `pubkeys`, a wrong type) is
/// different: Tauri parses it before the plugin's setup runs, so
/// `Builder::build` / `run` returns an error and the app does not start.
pub fn init<R: Runtime>(handle: TpkHandle) -> TauriPlugin<R, Option<TpkConfig>> {
    build_plugin(handle, None)
}

/// Build the plugin with configuration supplied in code.
///
/// `plugins.tpk` is then optional and ignored — but if the section exists it
/// must still deserialize: Tauri parses it before the plugin's setup runs, and
/// an unknown key or a missing required field makes `Builder::build` / `run`
/// return an error, so the app does not start.
pub fn init_with_config<R: Runtime>(
    handle: TpkHandle,
    config: TpkConfig,
) -> TauriPlugin<R, Option<TpkConfig>> {
    build_plugin(handle, Some(config))
}

fn build_plugin<R: Runtime>(
    handle: TpkHandle,
    programmatic: Option<TpkConfig>,
) -> TauriPlugin<R, Option<TpkConfig>> {
    Builder::<R, Option<TpkConfig>>::new("tpk")
        .invoke_handler(tauri::generate_handler![
            commands::check,
            commands::download,
            commands::notify_ready,
            commands::status,
            commands::reset,
            commands::set_mod_enabled,
        ])
        .setup(move |app, api| {
            // This hook runs at the end of `Builder::build`, before any window
            // exists. Returning `Err` here aborts the build and the app never
            // starts — so a disk problem must degrade to the embedded assets,
            // never to a launch failure.
            let config = programmatic.clone().or_else(|| api.config().clone());
            match setup(app, &handle, config) {
                Ok(()) => {}
                Err(e) => {
                    log::error!("[tpk] disabled for this launch: {e}");
                }
            }
            Ok(())
        })
        .build()
}

fn setup<R: Runtime>(
    app: &tauri::AppHandle<R>,
    handle: &TpkHandle,
    config: Option<TpkConfig>,
) -> Result<()> {
    let Some(config) = config else {
        return Err(Error::Config(
            "no `plugins.tpk` section and no programmatic config".into(),
        ));
    };
    if !config.enabled {
        log::info!("[tpk] disabled by configuration");
        return Ok(());
    }

    let trust = Arc::new(TrustStore::new(&config.trusted_keys())?);
    let shell_version: semver::Version = app
        .package_info()
        .version
        .to_string()
        .parse()
        .map_err(|e| Error::Config(format!("app version is not SemVer: {e}")))?;

    // Paths are resolved here rather than in `attach`: on Android this goes
    // through JNI and is not available before the core plugins are up.
    let layout = Layout::new(
        app.path()
            .app_local_data_dir()
            .map_err(|e| Error::Config(format!("no app data directory: {e}")))?
            .join("tpk"),
        app.path()
            .app_cache_dir()
            .map_err(|e| Error::Config(format!("no app cache directory: {e}")))?
            .join("tpk"),
    );
    let mut store = Store::open(layout)?;
    // After `Store::open`, never before it: `create_dirs` runs in there, and the
    // flag cannot be set on a directory that does not exist yet. Setting it
    // first meant a fresh install kept its layer pool in iCloud until the
    // second launch — the opposite of the intent.
    for dir in store.layout().backup_exclusions() {
        exclude_from_backup(&dir);
    }
    if let Some(seed) = &config.seed_dir {
        if let Ok(resource_dir) = app.path().resource_dir() {
            let seed_path = resource_dir.join(seed);
            match store.seed_if_absent(&seed_path, &trust) {
                Ok(true) => log::info!("[tpk] seeded from {}", seed_path.display()),
                Ok(false) => {}
                Err(e) => log::warn!("[tpk] could not seed: {e}"),
            }
        }
    }

    let outcome = store.boot()?;
    if let Some(rev) = &outcome.rolled_back {
        // Not an event: setup runs before any webview exists, so an emit here
        // reaches nobody. `status().rolled_back` reports it instead.
        log::warn!("[tpk] rolled back {rev} after repeated unacknowledged launches");
    }

    let resolver = tpk_store::resolver_for(
        &outcome,
        Arc::clone(&trust),
        store.state().min_key_epoch,
        config.cache_budget_bytes,
        store.materialized(),
    );
    let failed: Vec<String> = resolver
        .failed_layers()
        .iter()
        .map(|f| {
            log::error!("[tpk] layer {} failed: {}", f.path.display(), f.reason);
            f.file_sha256
                .map(|s| s.to_hex())
                .unwrap_or_else(|| "unknown".to_string())
        })
        .collect();

    record_layer_failures(&mut store, resolver.failed_layers());

    let resolver = Arc::new(resolver);

    // The OS may have purged the delta results. Check and rebuild off the
    // launch path, so setup does no extra IO; until then those paths fall back
    // to embedded assets, and the resolver serves them as soon as the files
    // appear. Never a strike.
    if !resolver.index().layers().is_empty() {
        let resolver = Arc::clone(&resolver);
        let trust = Arc::clone(&trust);
        let min_key_epoch = store.state().min_key_epoch;
        let materialized_dir = store.layout().materialized_dir();
        std::thread::spawn(move || {
            if !tpk_store::missing_materialized(&resolver, &materialized_dir) {
                return;
            }
            let specs: Vec<tpk_resolve::LayerSpec> = resolver
                .index()
                .layers()
                .iter()
                .map(|l| tpk_resolve::LayerSpec {
                    path: l.path.clone(),
                    file_sha256: l.file_sha256,
                })
                .collect();
            match tpk_store::materialize_stack(&specs, &trust, min_key_epoch, &materialized_dir) {
                Ok(n) => log::info!("[tpk] rebuilt {n} purged delta result(s)"),
                Err(e) => log::error!("[tpk] could not rebuild delta results: {e}"),
            }
        });
    }

    handle.shared().set_resolver(resolver);

    let has_embedded_fallback = has_embedded_index(app);
    if !has_embedded_fallback {
        // Without one, a rollback has nowhere to land.
        log::error!(
            "[tpk] the binary has no embedded index.html; a rollback would leave a blank window"
        );
    }

    app.manage(TpkState::new(state::TpkStateParts {
        config,
        trust,
        store,
        shared: Arc::clone(handle.shared()),
        shell_version,
        failed_layers: failed,
        rolled_back: outcome.rolled_back,
        unsafe_capabilities: state::scan_unsafe_capabilities(app),
        has_embedded_fallback,
    }));

    // Startup polling, if the configuration asks for it. Spawned rather than
    // awaited: `setup` runs inside `Builder::build`, where a network round trip
    // would hold up the first window. It can only reach here once the state is
    // managed, so a disabled or degraded-at-setup plugin polls nothing.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(state) = handle.try_state::<TpkState>() {
            state.auto_update(&handle).await;
        }
    });
    Ok(())
}

/// Strike the layers that failed to load for a fault in their own content.
///
/// A single failure is not yet a condemnation; recording it lets a repeat be
/// caught.
pub(crate) fn record_layer_failures(store: &mut Store, failed: &[tpk_resolve::FailedLayer]) {
    use tpk_format::error::ErrorCode;
    for layer in failed {
        let Some(sha) = layer.file_sha256 else {
            continue;
        };
        match layer.code {
            // Every pooled layer was verified when staged or seeded, so failing
            // now means the trust set or key floor moved, not a forged pack.
            ErrorCode::Signature => {}
            // Mods switched off or a full stack: nothing wrong with the pack.
            ErrorCode::Disabled | ErrorCode::State => {}
            // A shell outside the pack's range: a different shell could run it,
            // so a strike would wrongly condemn a good pack.
            ErrorCode::Shell => {}
            code => {
                let _ = store.record_failure(sha, tpk_store::Reason::from_code(code));
            }
        }
    }
}

/// Whether the binary ships a usable fallback page.
fn has_embedded_index<R: Runtime>(app: &tauri::AppHandle<R>) -> bool {
    app.asset_resolver().get("index.html".into()).is_some()
}

/// Ask the platform not to back up a directory.
///
/// Best effort: failing costs backup quota, which is not a reason to refuse to
/// start. The call itself lives in `tpk-backup`, because on Apple platforms it
/// is `unsafe` and this crate is `#![forbid(unsafe_code)]`. On Android it
/// belongs in `<data-extraction-rules>`, which the host app must declare
/// because a plugin cannot merge into that manifest.
pub(crate) fn exclude_from_backup(dir: &std::path::Path) {
    if let Err(e) = tpk_backup::exclude_from_backup(dir) {
        log::warn!(
            "[tpk] could not exclude {} from platform backups: {e}",
            dir.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::error::ErrorCode;
    use tpk_format::manifest::{PackId, PackKind};
    use tpk_format::pack::PackBuilder;
    use tpk_format::secret::SecretKey;
    use tpk_format::sign::{sha256_hex, TrustedKey};
    use tpk_store::IncomingPack;

    /// A one-file base pack for `core`.
    pub(crate) fn base_pack(
        dir: &std::path::Path,
        key: &SecretKey,
        version_code: u64,
        body: &[u8],
    ) -> IncomingPack {
        let path = dir.join(format!("core-{}.tpk", sha256_hex(body)));
        let mut b = PackBuilder::new(
            PackKind::Base,
            PackId::parse("core").unwrap(),
            "1.0.0".parse().unwrap(),
            version_code,
            "2026-09-11T15:00:00Z",
        );
        b.add_full("/index.html", body).unwrap();
        b.build(key, &path).unwrap();
        IncomingPack {
            bytes: std::fs::read(&path).unwrap(),
            id: PackId::parse("core").unwrap(),
            kind: PackKind::Base,
            version_code,
        }
    }

    #[test]
    fn commands_answer_without_state_when_disabled_or_degraded() {
        // Disabled by configuration, and a setup that degraded for lack of
        // config: neither manages state, and both must still answer in the
        // documented shapes rather than with "state not managed".
        for config in [
            Some(TpkConfig::new("https://example.invalid/latest.json", vec![]).disabled()),
            None,
        ] {
            let mut context = tauri::test::mock_context(tauri::test::noop_assets());
            let handle = attach(&mut context);
            let plugin = match config {
                Some(config) => init_with_config(handle, config),
                None => init(handle),
            };
            let app = tauri::test::mock_builder()
                .plugin(plugin)
                .build(context)
                .unwrap();
            let app = app.handle().clone();
            assert!(app.try_state::<TpkState>().is_none());

            tauri::async_runtime::block_on(async {
                assert_eq!(
                    serde_json::to_value(commands::check(app.clone()).await.unwrap()).unwrap(),
                    serde_json::json!({"status": "disabled"})
                );
                assert_eq!(
                    serde_json::to_value(commands::download(app.clone()).await.unwrap()).unwrap(),
                    serde_json::json!({"status": "disabled"})
                );
                assert_eq!(
                    commands::notify_ready(app.clone()).await.unwrap(),
                    ReadyOutcome::Noop
                );
                let errors = [
                    commands::status(app.clone()).await.unwrap_err(),
                    commands::reset(app.clone(), None).await.unwrap_err(),
                    commands::set_mod_enabled(app.clone(), "skin".into(), true)
                        .await
                        .unwrap_err(),
                ];
                for e in errors {
                    assert_eq!(serde_json::to_value(e).unwrap()["code"], "E_DISABLED");
                }
            });
        }
    }

    #[test]
    fn a_plugins_tpk_section_that_does_not_parse_stops_the_app() {
        // Documented on `init`: only an absent section degrades. A present one
        // is parsed by Tauri before setup, so a typo'd key is a launch failure,
        // even when the config is supplied in code.
        for programmatic in [false, true] {
            let mut context = tauri::test::mock_context(tauri::test::noop_assets());
            context.config_mut().plugins.0.insert(
                "tpk".into(),
                serde_json::json!({
                    "manifest_url": "https://example.invalid/latest.json",
                    "pubkeys": [],
                    "auto_chek_on_launch": false
                }),
            );
            let handle = attach(&mut context);
            let plugin = if programmatic {
                init_with_config(
                    handle,
                    TpkConfig::new("https://example.invalid/latest.json", vec![]).disabled(),
                )
            } else {
                init(handle)
            };
            let built = tauri::test::mock_builder().plugin(plugin).build(context);
            assert!(built.is_err(), "programmatic = {programmatic}");
        }
    }

    #[test]
    fn a_key_floor_raise_is_not_a_strike_against_the_layer() {
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let build = tempfile::tempdir().unwrap();
        let old_key = SecretKey::generate();
        let new_key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[
                TrustedKey {
                    key: old_key.public_key_base64(),
                    epoch: 1,
                },
                TrustedKey {
                    key: new_key.public_key_base64(),
                    epoch: 2,
                },
            ])
            .unwrap(),
        );
        let layout = Layout::new(data.path().join("tpk"), cache.path().join("tpk"));

        let mut store = Store::open(layout.clone()).unwrap();
        store
            .stage(vec![base_pack(build.path(), &old_key, 1, b"v1")], &trust)
            .unwrap();
        store.boot().unwrap();
        store.commit_booting().unwrap();
        store.state_mut().min_key_epoch = 2;
        store.save().unwrap();

        // Every launch after the rotation fails the epoch-1 layer at resolve.
        for _ in 0..tpk_store::blacklist::TRANSIENT_STRIKES {
            let mut store = Store::open(layout.clone()).unwrap();
            let outcome = store.boot().unwrap();
            let resolver =
                tpk_store::resolver_for(&outcome, Arc::clone(&trust), 2, 0, store.materialized());
            assert_eq!(resolver.failed_layers().len(), 1);
            assert_eq!(resolver.failed_layers()[0].code, ErrorCode::Signature);
            record_layer_failures(&mut store, resolver.failed_layers());
        }

        let mut store = Store::open(layout).unwrap();
        assert!(store.blacklist().is_empty(), "no strike for a floor raise");
        // The publisher's re-signed build of the same release still goes in.
        let resigned = base_pack(build.path(), &new_key, 1, b"v1 re-signed");
        store.stage(vec![resigned], &trust).unwrap();

        // A shell mismatch is never a strike: a later shell could run the pack.
        let shell = tpk_resolve::FailedLayer {
            path: "x.tpk".into(),
            file_sha256: Some(sha256_hex(b"needs another shell")),
            reason: "shell out of range".into(),
            code: ErrorCode::Shell,
        };
        record_layer_failures(&mut store, &[shell]);
        assert!(
            store.blacklist().is_empty(),
            "no strike for a shell mismatch"
        );

        // A genuine content fault is still recorded.
        let failed = tpk_resolve::FailedLayer {
            path: "x.tpk".into(),
            file_sha256: Some(sha256_hex(b"rotted")),
            reason: "file hash moved".into(),
            code: ErrorCode::Hash,
        };
        record_layer_failures(&mut store, &[failed]);
        assert_eq!(store.blacklist().len(), 1);
    }

    #[test]
    fn the_plugin_name_matches_the_permission_namespace() {
        // `tauri-plugin`'s build step derives the ACL namespace from the crate
        // name by stripping `tauri-plugin-`; the runtime uses the name passed to
        // `Builder::new`. They have to agree or every capability silently fails
        // to resolve.
        let crate_name = env!("CARGO_PKG_NAME");
        assert_eq!(crate_name.strip_prefix("tauri-plugin-"), Some("tpk"));
    }
}
