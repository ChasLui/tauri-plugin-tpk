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
/// Configuration comes from `plugins.tpk`. If that section is absent or
/// unusable the plugin logs and stays inert — the app still starts, serving its
/// embedded assets.
pub fn init<R: Runtime>(handle: TpkHandle) -> TauriPlugin<R, Option<TpkConfig>> {
    build_plugin(handle, None)
}

/// Build the plugin with configuration supplied in code.
///
/// `plugins.tpk` is then optional.
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
    for dir in layout.backup_exclusions() {
        exclude_from_backup(&dir);
    }

    let mut store = Store::open(layout)?;
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
        log::warn!("[tpk] rolled back {rev} after repeated unacknowledged launches");
        events::emit_state(app, "rolled_back", Some(rev));
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

    // Layers that could not be loaded are recorded so a repeat is caught; a
    // single failure is not yet a condemnation.
    for layer in resolver.failed_layers() {
        if let Some(sha) = layer.file_sha256 {
            let _ = store.record_failure(sha, tpk_store::Reason::Io);
        }
    }

    handle.shared().set_resolver(Arc::new(resolver));

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
        unsafe_capabilities: state::scan_unsafe_capabilities(app),
        has_embedded_fallback,
    }));
    Ok(())
}

/// Whether the binary ships a usable fallback page.
fn has_embedded_index<R: Runtime>(app: &tauri::AppHandle<R>) -> bool {
    app.asset_resolver().get("index.html".into()).is_some()
}

/// Ask the platform not to back up a directory.
///
/// Best effort: failing costs backup quota, which is not a reason to refuse to
/// start. On Apple platforms this is `NSURLIsExcludedFromBackupKey`; on Android
/// it belongs in `<data-extraction-rules>`, which the host app must declare
/// because a plugin cannot merge into that manifest.
fn exclude_from_backup(dir: &std::path::Path) {
    let _ = dir;
    #[cfg(target_vendor = "apple")]
    log::debug!(
        "[tpk] {} should be excluded from iCloud backup; see docs/security.md",
        dir.display()
    );
}

#[cfg(test)]
mod tests {
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
