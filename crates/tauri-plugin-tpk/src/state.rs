//! Plugin state and the logic behind each command.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Runtime};
use tpk_client::{
    download::{download_pack, prune_parts, DownloadRequest},
    verify_channel, ChannelSource, FetchContext, HttpChannelSource, Plan, PlanContext,
};
use tpk_format::channel::ChannelManifest;
use tpk_format::sign::TrustStore;
use tpk_store::{IncomingPack, Store};

use crate::config::TpkConfig;
use crate::error::{Error, Result};
use crate::events;
use crate::outcome::{
    CheckOutcome, DownloadOutcome, LastError, LayerSummary, PackSummary, ReadyOutcome,
    ResetOptions, Status,
};
use crate::pack_assets::Shared;

/// Capabilities that let overlay JavaScript reach past the WebView.
///
/// Pack content runs on the `tauri://` origin and inherits whatever the main
/// window was granted. None of these are wrong in themselves — they are wrong
/// in combination with remotely delivered content, which is why they are
/// surfaced rather than blocked.
pub const RISKY_PERMISSIONS: &[&str] = &[
    "shell:allow-execute",
    "shell:allow-spawn",
    "shell:allow-open",
    "shell:default",
    "process:allow-restart",
    "fs:allow-write-file",
    "fs:allow-write-text-file",
    "fs:default",
];

/// Everything the commands need.
pub struct TpkState {
    config: TpkConfig,
    trust: Arc<TrustStore>,
    store: Mutex<Store>,
    shared: Arc<Shared>,
    source: HttpChannelSource,
    shell_version: semver::Version,
    /// The most recent plan, so `download` does not have to poll again.
    pending: Mutex<Option<(ChannelManifest, Plan)>>,
    failed_layers: Vec<String>,
    unsafe_capabilities: Vec<String>,
    has_embedded_fallback: bool,
}

impl std::fmt::Debug for TpkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TpkState")
            .field("channel", &self.config.channel)
            .field("shell", &self.shell_version)
            .field("failed_layers", &self.failed_layers.len())
            .finish()
    }
}

/// Everything `setup` gathered, handed over in one piece.
pub(crate) struct TpkStateParts {
    /// Configuration in force.
    pub config: TpkConfig,
    /// Trusted signing keys.
    pub trust: Arc<TrustStore>,
    /// The booted store.
    pub store: Store,
    /// The slot the assets provider reads from.
    pub shared: Arc<Shared>,
    /// The running shell version.
    pub shell_version: semver::Version,
    /// Layers that failed to load this launch, by hash.
    pub failed_layers: Vec<String>,
    /// Risky capabilities granted to the main window.
    pub unsafe_capabilities: Vec<String>,
    /// Whether the binary has a fallback for a rollback to land on.
    pub has_embedded_fallback: bool,
}

impl TpkState {
    /// Assemble the state after the store has booted.
    pub(crate) fn new(parts: TpkStateParts) -> Self {
        let source = HttpChannelSource::new(parts.config.manifest_url.clone())
            .with_headers(parts.config.headers.clone());
        Self {
            config: parts.config,
            trust: parts.trust,
            store: Mutex::new(parts.store),
            shared: parts.shared,
            source,
            shell_version: parts.shell_version,
            pending: Mutex::new(None),
            failed_layers: parts.failed_layers,
            unsafe_capabilities: parts.unsafe_capabilities,
            has_embedded_fallback: parts.has_embedded_fallback,
        }
    }

    /// The configuration in force.
    pub fn config(&self) -> &TpkConfig {
        &self.config
    }

    /// Poll the channel and work out what applies.
    pub(crate) async fn check<R: Runtime>(&self, app: &AppHandle<R>) -> Result<CheckOutcome> {
        if !self.config.enabled {
            return Ok(CheckOutcome::Disabled);
        }
        let (floor, min_epoch, install_id, installed, degraded, rollbacks) = {
            let store = self.lock_store()?;
            (
                store.state().watermark_floor(&self.config.channel),
                store.state().min_key_epoch,
                store.state().install_id.clone(),
                installed_versions(&store),
                store.is_degraded(),
                store.state().consecutive_rollbacks,
            )
        };
        if degraded {
            // Repeated rollbacks mean something on this device is not working.
            // Continuing to download would just repeat the cycle.
            return Ok(CheckOutcome::Degraded {
                consecutive_rollbacks: rollbacks,
            });
        }

        let ctx = FetchContext::for_current(&self.config.channel, self.shell_version.to_string());
        let signed = self.source.fetch(&ctx).await.map_err(|e| {
            events::emit_error(app, &e.code().to_string(), &e.to_string());
            Error::Client(e)
        })?;
        let manifest = verify_channel(&signed, &self.trust, min_epoch, floor).map_err(|e| {
            events::emit_error(app, &e.code().to_string(), &e.to_string());
            Error::Client(e)
        })?;

        // Advance both monotonic floors before planning: even a manifest that
        // turns out to offer nothing still proves this channel has reached that
        // watermark and that generation of key.
        {
            let mut store = self.lock_store()?;
            store
                .state_mut()
                .observe_watermark(&self.config.channel, manifest.watermark);
            let epoch = store.state().min_key_epoch.max(manifest.key_epoch);
            store.state_mut().min_key_epoch = epoch;
            store.save()?;
        }

        let plan_ctx = PlanContext {
            shell_version: self.shell_version.clone(),
            install_id,
            installed,
        };
        let plan = {
            let store = self.lock_store()?;
            tpk_client::plan(&manifest, &plan_ctx, &|sha, id, code| {
                store
                    .blacklist()
                    .blocks(sha, id, code, tpk_store::blacklist::now_secs())
            })
        };

        let outcome = match &plan {
            Plan::ShellRequired { min_shell } => CheckOutcome::ShellRequired {
                min_shell: min_shell.to_string(),
            },
            Plan::UpToDate { .. } => CheckOutcome::UpToDate {
                watermark: manifest.watermark,
            },
            Plan::Apply { packs, bytes, .. } => CheckOutcome::Available {
                packs: packs.iter().map(PackSummary::from_ref).collect(),
                bytes: *bytes,
                notes: manifest.notes.clone(),
            },
        };

        *self.pending.lock().map_err(|_| Error::NotInitialized)? = Some((manifest, plan));
        Ok(outcome)
    }

    /// Fetch and stage whatever the last check found.
    pub(crate) async fn download<R: Runtime>(&self, app: &AppHandle<R>) -> Result<DownloadOutcome> {
        if !self.config.enabled {
            return Ok(DownloadOutcome::Disabled);
        }

        let pending = self
            .pending
            .lock()
            .map_err(|_| Error::NotInitialized)?
            .clone();
        let (_, plan) = match pending {
            Some(pair) => pair,
            None => {
                // Nothing planned yet; poll first so a caller can just invoke
                // download() on its own.
                self.check(app).await?;
                self.pending
                    .lock()
                    .map_err(|_| Error::NotInitialized)?
                    .clone()
                    .ok_or(Error::NotInitialized)?
            }
        };

        let packs = match plan {
            Plan::Apply { packs, .. } => packs,
            Plan::UpToDate { .. } => return Ok(DownloadOutcome::UpToDate),
            Plan::ShellRequired { min_shell } => {
                return Ok(DownloadOutcome::ShellRequired {
                    min_shell: min_shell.to_string(),
                })
            }
        };

        let (tmp_dir, layers_dir) = {
            let store = self.lock_store()?;
            (store.layout().tmp_dir(), store.layout().layers_dir())
        };
        // Abandoned plans leave part files behind; they are keyed by hash, so
        // anything not in this plan is dead weight.
        prune_parts(&tmp_dir, &packs);

        let client = reqwest::Client::new();
        let count = packs.len();
        let mut downloaded = Vec::with_capacity(count);
        let mut total_bytes = 0u64;

        for (index, pack) in packs.iter().enumerate() {
            let app_for_progress = app.clone();
            let request = DownloadRequest {
                client: &client,
                tmp_dir: &tmp_dir,
                out_dir: &layers_dir,
                headers: self.source.headers(),
                on_progress: &move |p| events::emit_progress(&app_for_progress, p),
                index,
                count,
            };
            match download_pack(&request, pack).await {
                Ok(done) => {
                    total_bytes += pack.size;
                    downloaded.push((done, pack.clone()));
                }
                Err(e) => {
                    events::emit_error(app, &e.code().to_string(), &e.to_string());
                    return Ok(DownloadOutcome::Failed {
                        code: e.code().to_string(),
                        message: e.to_string(),
                    });
                }
            }
        }

        let incoming: Vec<IncomingPack> = downloaded
            .iter()
            .map(|(done, pack)| {
                Ok(IncomingPack {
                    bytes: std::fs::read(&done.path)?,
                    id: pack.id.clone(),
                    kind: pack.kind,
                    version_code: pack.version_code,
                })
            })
            .collect::<std::io::Result<_>>()?;

        let rev = {
            let mut store = self.lock_store()?;
            match store.stage_for_shell(incoming, &self.trust, Some(&self.shell_version)) {
                Ok(rev) => rev,
                Err(e) => {
                    let code = e.code().to_string();
                    events::emit_error(app, &code, &e.to_string());
                    return Ok(DownloadOutcome::Failed {
                        code,
                        message: e.to_string(),
                    });
                }
            }
        };

        events::emit_state(app, "staged", Some(&rev));
        Ok(DownloadOutcome::Staged {
            rev,
            bytes: total_bytes,
        })
    }

    /// Acknowledge the running revision.
    pub(crate) fn notify_ready<R: Runtime>(&self, app: &AppHandle<R>) -> Result<ReadyOutcome> {
        let mut store = self.lock_store()?;
        // Flushed before returning, deliberately: a process killed between "the
        // UI rendered" and "the state was written" would otherwise be counted
        // as another failed boot.
        match store.commit_booting()? {
            tpk_store::CommitOutcome::Committed => {
                let rev = store
                    .state()
                    .committed
                    .as_ref()
                    .map(|r| r.rev.clone())
                    .unwrap_or_default();
                events::emit_state(app, "committed", Some(&rev));
                Ok(ReadyOutcome::Committed { rev })
            }
            tpk_store::CommitOutcome::Noop => Ok(ReadyOutcome::Noop),
        }
    }

    /// Report what is loaded.
    pub(crate) fn status<R: Runtime>(&self, _app: &AppHandle<R>) -> Result<Status> {
        let store = self.lock_store()?;
        let state = store.state();
        let active = state.active();

        Ok(Status {
            pointer: match state.pointer {
                tpk_store::Pointer::Committed => "committed".to_string(),
                tpk_store::Pointer::Booting => "booting".to_string(),
            },
            rev: active.map(|r| r.rev.clone()),
            layers: active
                .map(|r| r.layers.iter().map(LayerSummary::from_record).collect())
                .unwrap_or_default(),
            shell: self.shell_version.to_string(),
            watermark: state.watermark_floor(&self.config.channel),
            pending: state.staged.is_some(),
            degraded: store.is_degraded(),
            failed_layers: self.failed_layers.clone(),
            unsafe_capabilities: self.unsafe_capabilities.clone(),
            has_embedded_fallback: self.has_embedded_fallback,
            last_error: state.last_error.as_ref().map(|e| LastError {
                code: e.code.clone(),
                message: e.message.clone(),
            }),
        })
    }

    /// Forget downloaded content.
    pub(crate) fn reset<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        options: ResetOptions,
    ) -> Result<Status> {
        {
            let mut store = self.lock_store()?;
            store.reset(options.clear_blacklist)?;
        }
        *self.pending.lock().map_err(|_| Error::NotInitialized)? = None;
        events::emit_state(app, "reset", None);
        self.status(app)
    }

    /// Whether the overlay is serving anything this launch.
    pub fn has_overlay(&self) -> bool {
        self.shared.resolver().is_some()
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, Store>> {
        self.store.lock().map_err(|_| Error::NotInitialized)
    }
}

/// Highest installed `version_code` per pack id.
///
/// Derived from the layers actually present rather than from a counter, so
/// deleting `state.json` cannot lower the downgrade floor.
fn installed_versions(
    store: &Store,
) -> std::collections::HashMap<tpk_format::manifest::PackId, u64> {
    let mut installed = std::collections::HashMap::new();
    for rev in [&store.state().committed, &store.state().booting]
        .into_iter()
        .flatten()
    {
        for layer in &rev.layers {
            let slot = installed.entry(layer.id.clone()).or_insert(0);
            *slot = (*slot).max(layer.version_code);
        }
    }
    installed
}

/// Capabilities granted to the main window that overlay JavaScript could reach.
pub(crate) fn scan_unsafe_capabilities<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    let mut found = Vec::new();
    for capability in &app.config().app.security.capabilities {
        let serialized = serde_json::to_string(capability).unwrap_or_default();
        for risky in RISKY_PERMISSIONS {
            if serialized.contains(risky) && !found.iter().any(|f| f == risky) {
                found.push((*risky).to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_risky_permission_list_covers_the_escape_hatches() {
        // Each of these turns "the overlay can render a page" into "the overlay
        // can run native code", which is the line App Review actually cares
        // about. Losing one silently would be bad, so pin the list.
        for expected in [
            "shell:allow-execute",
            "shell:allow-spawn",
            "process:allow-restart",
            "fs:allow-write-file",
        ] {
            assert!(
                RISKY_PERMISSIONS.contains(&expected),
                "{expected} should be flagged"
            );
        }
    }
}
