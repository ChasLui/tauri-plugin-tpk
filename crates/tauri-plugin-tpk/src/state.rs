//! Plugin state and the logic behind each command.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Runtime};
use tpk_client::{
    download::{download_pack, prune_parts, DownloadRequest},
    verify_channel, ChannelSource, FetchContext, HttpChannelSource, Plan, PlanContext,
};
use tpk_format::channel::ChannelManifest;
use tpk_format::error::ErrorCode;
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
    ///
    /// One slot, last writer wins. A launch auto-check and a frontend `check`
    /// can therefore overwrite each other, and a `download` may stage the plan
    /// from whichever check finished last rather than the one whose result the
    /// caller saw. Both plans come from a signed manifest for the same channel
    /// and every pack is verified again at stage, so the worst case is a
    /// different-but-valid revision, not an unchecked one. Known; not fixed
    /// here, because a per-caller plan token changes the command signatures.
    pending: Mutex<Option<(ChannelManifest, Plan)>>,
    failed_layers: Vec<String>,
    rolled_back: Option<String>,
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
    /// The revision rolled back during this launch's boot, if any.
    pub rolled_back: Option<String>,
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
            rolled_back: parts.rolled_back,
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
        let (floor, min_epoch, install_id, installed, version_floor, degraded, rollbacks) = {
            let store = self.lock_store()?;
            (
                store.state().watermark_floor(&self.config.channel),
                store.state().min_key_epoch,
                store.state().install_id.clone(),
                installed_versions(&store),
                store
                    .state()
                    .version_floor
                    .iter()
                    .map(|(id, code)| (id.clone(), *code))
                    .collect(),
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
        let signed = self
            .source
            .fetch(&ctx)
            .await
            .map_err(|e| self.fail(app, Error::Client(e)))?;
        let manifest = verify_channel(&signed, &self.trust, min_epoch, floor)
            .map_err(|e| self.fail(app, Error::Client(e)))?;

        // Advance both monotonic floors before planning: even a manifest that
        // turns out to offer nothing still proves this channel has reached that
        // watermark and that generation of key.
        let saved = {
            let mut store = self.lock_store()?;
            store
                .state_mut()
                .observe_watermark(&self.config.channel, manifest.watermark);
            let epoch = store.state().min_key_epoch.max(manifest.key_epoch);
            store.state_mut().min_key_epoch = epoch;
            // The channel answered and verified, so whatever failed before it is
            // no longer the state of play. Folded into the write below rather
            // than taking a second one.
            store.state_mut().last_error = None;
            store.save()
        };
        // Not a bare `?`: a failed state write is a failure like any other and
        // owes the frontend an event. The guard is dropped first, because
        // `fail` takes the same lock.
        saved.map_err(|e| self.fail(app, Error::Store(e)))?;

        let plan_ctx = PlanContext {
            shell_version: self.shell_version.clone(),
            install_id,
            installed,
            version_floor,
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

        let tmp_dir = self.lock_store()?.layout().tmp_dir();
        // Abandoned plans leave part files behind; they are keyed by hash, so
        // anything not in this plan is dead weight.
        prune_downloads(&tmp_dir, &packs);

        let client = reqwest::Client::new();
        let count = packs.len();
        let mut downloaded = Vec::with_capacity(count);
        let mut finished = RemoveOnDrop::default();
        let mut total_bytes = 0u64;

        for (index, pack) in packs.iter().enumerate() {
            let app_for_progress = app.clone();
            // Finalized inside the cache root, not renamed into the pool: the
            // two roots may be different filesystems (EXDEV), and an
            // unreferenced pool file is fair game for `gc`. Staging writes its
            // own fsynced pool copy from these bytes.
            let request = DownloadRequest {
                client: &client,
                tmp_dir: &tmp_dir,
                out_dir: &tmp_dir,
                headers: self.source.headers(),
                on_progress: &move |p| events::emit_progress(&app_for_progress, p),
                index,
                count,
            };
            match download_pack(&request, pack).await {
                Ok(done) => {
                    finished.0.push(done.path.clone());
                    total_bytes += pack.size;
                    downloaded.push((done, pack.clone()));
                }
                Err(e) => return Ok(self.failed_download(app, Error::Client(e))),
            }
        }

        // The cache root may be purged under us; that is retryable, so it is an
        // outcome like any other failure here, not an `Err`.
        let incoming = match read_downloads(&downloaded) {
            Ok(incoming) => incoming,
            Err(e) => return Ok(self.failed_download(app, Error::Io(e))),
        };

        // Staged outside the guard the failure path needs: recording the
        // failure takes the same lock, and it is not reentrant.
        let staged =
            self.lock_store()?
                .stage_for_shell(incoming, &self.trust, Some(&self.shell_version));
        let rev = match staged {
            Ok(rev) => rev,
            Err(e) => return Ok(self.failed_download(app, Error::Store(e))),
        };

        // A stage is the only thing that adds files to the layer pool, and
        // inheritance of the backup flag is not documented. One syscall.
        for dir in self.lock_store()?.layout().backup_exclusions() {
            crate::exclude_from_backup(&dir);
        }

        events::emit_state(app, "staged", Some(&rev));
        Ok(DownloadOutcome::Staged {
            rev,
            bytes: total_bytes,
        })
    }

    /// Run the update steps the configuration asks for at launch.
    ///
    /// `auto_check_on_launch` is the trigger; `auto_download` only says what to
    /// do with what that check found, so on its own it does nothing here — see
    /// [`TpkConfig::auto_download`](crate::config::TpkConfig::auto_download).
    ///
    /// Reuses the command paths, so the events a frontend sees are exactly the
    /// ones it would see had it called `check` and `download` itself. Nothing is
    /// returned and nothing fails the launch: a failure is already on the
    /// `tpk://error` event and in `status().last_error`. A device in the
    /// degraded state stops inside [`Self::check`], before any request is made.
    pub(crate) async fn auto_update<R: Runtime>(&self, app: &AppHandle<R>) {
        if !self.config.auto_check_on_launch {
            return;
        }
        match self.check(app).await {
            Ok(CheckOutcome::Available { .. }) if self.config.auto_download => {
                if let Err(e) = self.download(app).await {
                    log::warn!("[tpk] the automatic download could not run: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("[tpk] the automatic check could not run: {e}"),
        }
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
            rolled_back: self.rolled_back.clone(),
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

    /// Announce a failure that stopped an update from landing, and remember it.
    ///
    /// Returns the error it was given so call sites stay one expression. The
    /// store must not be locked by the caller: this takes the same lock.
    fn fail<R: Runtime>(&self, app: &AppHandle<R>, error: Error) -> Error {
        let code = error.code();
        events::emit_error(app, &code.to_string(), &error.to_string());
        // Shell range, blacklist and disabled are business outcomes, not
        // failures; recording them would leave a device that is merely a shell
        // version behind showing a permanent error.
        if !matches!(
            code,
            ErrorCode::Shell | ErrorCode::Blacklist | ErrorCode::Disabled
        ) {
            if let Ok(mut store) = self.lock_store() {
                if let Err(e) = store.record_last_error(code, error.to_string()) {
                    log::warn!("[tpk] could not record the last error: {e}");
                }
            }
        }
        error
    }

    /// [`Self::fail`], shaped as the outcome `download` reports.
    fn failed_download<R: Runtime>(&self, app: &AppHandle<R>, error: Error) -> DownloadOutcome {
        let error = self.fail(app, error);
        DownloadOutcome::Failed {
            code: error.code().to_string(),
            message: error.to_string(),
        }
    }
}

/// Read finished downloads back for staging.
fn read_downloads(
    downloaded: &[(
        tpk_client::download::DownloadedPack,
        tpk_format::channel::PackRef,
    )],
) -> std::io::Result<Vec<IncomingPack>> {
    downloaded
        .iter()
        .map(|(done, pack)| {
            Ok(IncomingPack {
                bytes: std::fs::read(&done.path)?,
                id: pack.id.clone(),
                kind: pack.kind,
                version_code: pack.version_code,
            })
        })
        .collect()
}

/// Delete scratch files in `tmp_dir` that the plan about to run does not want.
///
/// `prune_parts` covers `.part` files. A `<sha256>.tpk` is a finished download
/// that a crash kept from being staged, and nothing resumes it. One of this
/// plan's own is kept: a concurrent `download` of the same plan may be about
/// to read it. Only the download code writes here; the store's `.tmp.` files
/// live in the pool and the materialized directory.
fn prune_downloads(tmp_dir: &std::path::Path, wanted: &[tpk_format::channel::PackRef]) {
    prune_parts(tmp_dir, wanted);
    let Ok(entries) = std::fs::read_dir(tmp_dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".tpk")) else {
            continue;
        };
        if !wanted.iter().any(|p| p.sha256.to_hex() == stem) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Finished downloads, deleted however `download` returns.
///
/// Staging copies them into the pool, and a `.tpk` in the cache root is never
/// resumed, so once the call ends they are dead weight. A crash skips this;
/// `prune_downloads` catches what it leaves on the next download.
#[derive(Default)]
struct RemoveOnDrop(Vec<std::path::PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Highest installed `version_code` per pack id.
///
/// Derived from the layers the active revision lists, not from a counter.
///
/// Blacklisted layers do not count as installed. `Store::write_revision` drops
/// them from the stack it builds, so a planner that still believed a condemned
/// base was there would keep choosing patches on top of it, and every one of
/// them would be refused with `E_PARENT` — the same doomed download on every
/// check until the server published a new base. Telling the truth here makes
/// the planner ask for a full base instead.
///
/// This is only half the answer. It says what a patch may stack on, and it
/// deliberately forgets a condemned version — so the anti-downgrade floor comes
/// from `StoreState::version_floor` instead, which remembers.
fn installed_versions(
    store: &Store,
) -> std::collections::HashMap<tpk_format::manifest::PackId, u64> {
    let now = tpk_store::blacklist::now_secs();
    let mut installed = std::collections::HashMap::new();
    for rev in [&store.state().committed, &store.state().booting]
        .into_iter()
        .flatten()
    {
        for layer in rev.layers.iter().filter(|l| !store.blocks(l, now)) {
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
    fn a_download_reaches_the_pool_only_through_staging() {
        // Mirrors `download`: the finished file sits in the cache root, staging
        // writes the pool copy, and the download is gone afterwards.
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let key = tpk_format::secret::SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[tpk_format::sign::TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let layout = tpk_store::Layout::new(data.path().join("tpk"), cache.path().join("tpk"));
        let mut store = Store::open(layout.clone()).unwrap();

        let pack = crate::tests::base_pack(cache.path(), &key, 1, b"v1");
        let sha = tpk_format::sign::sha256_hex(&pack.bytes);
        std::fs::create_dir_all(layout.tmp_dir()).unwrap();
        let downloaded = layout.tmp_dir().join(format!("{sha}.tpk"));
        std::fs::write(&downloaded, &pack.bytes).unwrap();

        {
            let _finished = RemoveOnDrop(vec![downloaded.clone()]);
            assert!(
                !layout.layer_file(&sha).exists(),
                "not pooled before staging"
            );
            let incoming = IncomingPack {
                bytes: std::fs::read(&downloaded).unwrap(),
                ..pack
            };
            store.stage(vec![incoming], &trust).unwrap();
            assert!(layout.layer_file(&sha).exists());
        }
        assert!(!downloaded.exists(), "the download is removed afterwards");
    }

    #[test]
    fn a_purged_download_reads_back_as_an_io_failure() {
        let pack = tpk_format::channel::PackRef {
            id: tpk_format::manifest::PackId::parse("core").unwrap(),
            kind: tpk_format::manifest::PackKind::Base,
            version: "1.0.0".parse().unwrap(),
            version_code: 1,
            parent_version_code: None,
            url: "https://example.invalid/core.tpk".into(),
            size: 1,
            sha256: tpk_format::sign::sha256_hex(b"x"),
            optional: false,
            rollout: 100,
            min_shell: None,
            max_shell: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let done = tpk_client::download::DownloadedPack {
            pack: pack.clone(),
            path: dir.path().join("purged.tpk"),
        };
        let err = read_downloads(&[(done, pack)]).unwrap_err();
        // `download` turns this into `Failed { code: "E_IO" }`.
        assert_eq!(Error::Io(err).code().to_string(), "E_IO");
    }

    fn pack_ref(bytes: &[u8]) -> tpk_format::channel::PackRef {
        tpk_format::channel::PackRef {
            id: tpk_format::manifest::PackId::parse("core").unwrap(),
            kind: tpk_format::manifest::PackKind::Base,
            version: "1.0.0".parse().unwrap(),
            version_code: 1,
            parent_version_code: None,
            url: String::new(),
            size: bytes.len() as u64,
            sha256: tpk_format::sign::sha256_hex(bytes),
            optional: false,
            rollout: 100,
            min_shell: None,
            max_shell: None,
        }
    }

    #[test]
    fn pruning_removes_finished_downloads_outside_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path();
        let wanted = pack_ref(b"wanted");
        let stale = pack_ref(b"left by a crash before staging");
        let file =
            |p: &tpk_format::channel::PackRef, ext: &str| tmp.join(format!("{}.{ext}", p.sha256));
        for path in [
            file(&wanted, "tpk"),
            file(&wanted, "part"),
            file(&stale, "tpk"),
            file(&stale, "part"),
            tmp.join("other.txt"),
        ] {
            std::fs::write(path, b"x").unwrap();
        }

        prune_downloads(tmp, std::slice::from_ref(&wanted));
        assert!(file(&wanted, "tpk").exists());
        assert!(file(&wanted, "part").exists());
        assert!(!file(&stale, "tpk").exists());
        assert!(!file(&stale, "part").exists());
        assert!(
            tmp.join("other.txt").exists(),
            "only download scratch files"
        );
    }

    /// Serve `body` to every request on 127.0.0.1, calling `before` first.
    fn serve(body: Vec<u8>, before: impl Fn() + Send + 'static) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pack.tpk", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let _ = stream.read(&mut [0u8; 4096]);
                before();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        url
    }

    /// Run `download` against `packs` as if `check` had just planned them.
    ///
    /// The state and its app come back so a case can go on to call `status`.
    #[allow(clippy::type_complexity)]
    fn run_download(
        layout: tpk_store::Layout,
        key: &tpk_format::secret::SecretKey,
        packs: Vec<tpk_format::channel::PackRef>,
    ) -> (
        DownloadOutcome,
        Vec<String>,
        TpkState,
        tauri::App<tauri::test::MockRuntime>,
    ) {
        use tauri::Listener as _;
        let trust = Arc::new(
            TrustStore::new(&[tpk_format::sign::TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&errors);
        app.listen(events::ERROR, move |e| {
            sink.lock().unwrap().push(e.payload().to_string())
        });

        let state = TpkState::new(TpkStateParts {
            config: TpkConfig::new("http://127.0.0.1:9/latest.json", vec![]),
            trust,
            store: Store::open(layout).unwrap(),
            shared: Arc::default(),
            shell_version: "1.0.0".parse().unwrap(),
            failed_layers: vec![],
            rolled_back: None,
            unsafe_capabilities: vec![],
            has_embedded_fallback: true,
        });
        let manifest = ChannelManifest {
            spec: "tpk-channel/1".into(),
            channel: "stable".into(),
            published_at: "2026-09-11T15:00:00Z".into(),
            watermark: 1,
            key_epoch: 1,
            min_shell: None,
            force_shell: None,
            notes: None,
            packs: packs.clone(),
        };
        let plan = Plan::Apply {
            bytes: packs.iter().map(|p| p.size).sum(),
            packs,
            skipped: vec![],
        };
        *state.pending.lock().unwrap() = Some((manifest, plan));
        let outcome = tauri::async_runtime::block_on(state.download(app.handle())).unwrap();
        let errors = errors.lock().unwrap().clone();
        (outcome, errors, state, app)
    }

    #[test]
    fn a_download_that_cannot_be_read_back_reports_failed() {
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let layout = tpk_store::Layout::new(data.path().join("tpk"), cache.path().join("tpk"));

        // The first pack is already fully fetched; the second pack's request
        // stands in for the OS purging the cache root between finalize and
        // read-back.
        let first = pack_ref(b"first pack");
        let finished = layout.tmp_dir().join(format!("{}.tpk", first.sha256));
        std::fs::create_dir_all(layout.tmp_dir()).unwrap();
        std::fs::write(
            tpk_client::download::part_path(&layout.tmp_dir(), &first),
            b"first pack",
        )
        .unwrap();
        let mut second = pack_ref(b"second pack");
        second.url = serve(b"second pack".to_vec(), move || {
            let _ = std::fs::remove_file(&finished);
        });

        let (outcome, errors, state, app) = run_download(
            layout,
            &tpk_format::secret::SecretKey::generate(),
            vec![first, second],
        );
        match outcome {
            DownloadOutcome::Failed { code, .. } => assert_eq!(code, "E_IO"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("E_IO"), "{errors:?}");

        // The frontend can ask later what went wrong, without having kept the
        // event.
        let last = state
            .status(app.handle())
            .unwrap()
            .last_error
            .expect("a failed download is recorded");
        assert_eq!(last.code, "E_IO");
    }

    #[test]
    fn a_download_is_staged_and_its_scratch_file_removed() {
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let layout = tpk_store::Layout::new(data.path().join("tpk"), cache.path().join("tpk"));
        let key = tpk_format::secret::SecretKey::generate();
        let pack = crate::tests::base_pack(cache.path(), &key, 1, b"v1");
        let mut pack_ref = pack_ref(&pack.bytes);
        pack_ref.url = serve(pack.bytes.clone(), || {});

        let (outcome, errors, state, app) =
            run_download(layout.clone(), &key, vec![pack_ref.clone()]);
        assert!(
            matches!(outcome, DownloadOutcome::Staged { .. }),
            "{outcome:?} {errors:?}"
        );
        assert!(layout.layer_file(&pack_ref.sha256).exists());
        assert!(!layout
            .tmp_dir()
            .join(format!("{}.tpk", pack_ref.sha256))
            .exists());
        assert!(state.status(app.handle()).unwrap().last_error.is_none());
    }

    #[test]
    fn a_rollback_during_boot_is_reported_by_status() {
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let key = tpk_format::secret::SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[tpk_format::sign::TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let layout = tpk_store::Layout::new(data.path().join("tpk"), cache.path().join("tpk"));
        let pack = crate::tests::base_pack(cache.path(), &key, 1, b"v1");
        let rev = Store::open(layout.clone())
            .unwrap()
            .stage(vec![pack], &trust)
            .unwrap();

        // Launches that never acknowledge; the last one rolls back, which is
        // what setup sees in `BootOutcome`.
        let mut outcome = None;
        for _ in 0..tpk_store::MAX_BOOT_ATTEMPTS {
            let mut store = Store::open(layout.clone()).unwrap();
            outcome = Some((store.boot().unwrap(), store));
        }
        let (outcome, store) = outcome.unwrap();
        assert_eq!(outcome.rolled_back.as_deref(), Some(rev.as_str()));

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = TpkState::new(TpkStateParts {
            config: TpkConfig::new("https://example.invalid/latest.json", vec![]),
            trust,
            store,
            shared: Arc::default(),
            shell_version: "1.0.0".parse().unwrap(),
            failed_layers: vec![],
            rolled_back: outcome.rolled_back,
            unsafe_capabilities: vec![],
            has_embedded_fallback: true,
        });
        let status = state.status(app.handle()).unwrap();
        assert_eq!(status.rolled_back, Some(rev));
        assert_eq!(status.pointer, "committed");
    }

    /// A state whose channel URL is https (the only scheme `expand_url`
    /// allows) and whose port refuses connections, so `check` fails at the
    /// network without needing a TLS server.
    fn auto_world(
        layout: tpk_store::Layout,
        auto_check: bool,
        auto_download: bool,
    ) -> (TpkState, tauri::App<tauri::test::MockRuntime>) {
        let key = tpk_format::secret::SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[tpk_format::sign::TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let mut config = TpkConfig::new("https://127.0.0.1:1/latest.json", vec![]);
        config.auto_check_on_launch = auto_check;
        config.auto_download = auto_download;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let state = TpkState::new(TpkStateParts {
            config,
            trust,
            store: Store::open(layout).unwrap(),
            shared: Arc::default(),
            shell_version: "1.0.0".parse().unwrap(),
            failed_layers: vec![],
            rolled_back: None,
            unsafe_capabilities: vec![],
            has_embedded_fallback: true,
        });
        (state, app)
    }

    fn layout_in(data: &tempfile::TempDir, cache: &tempfile::TempDir) -> tpk_store::Layout {
        tpk_store::Layout::new(data.path().join("tpk"), cache.path().join("tpk"))
    }

    #[test]
    fn a_launch_check_runs_when_it_is_configured() {
        let (data, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (state, app) = auto_world(layout_in(&data, &cache), true, false);

        tauri::async_runtime::block_on(state.auto_update(app.handle()));

        // It reached `check`, which went down the shared failure path.
        let last = state
            .status(app.handle())
            .unwrap()
            .last_error
            .expect("the launch check ran and failed");
        assert_eq!(last.code, "E_NETWORK");
    }

    #[test]
    fn auto_download_on_its_own_polls_nothing() {
        // Documented on the field: it qualifies the launch check rather than
        // being a trigger of its own.
        let (data, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (state, app) = auto_world(layout_in(&data, &cache), false, true);

        tauri::async_runtime::block_on(state.auto_update(app.handle()));

        assert!(state.status(app.handle()).unwrap().last_error.is_none());
    }

    #[test]
    fn a_degraded_device_polls_nothing_at_launch() {
        let (data, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let layout = layout_in(&data, &cache);
        {
            // Repeated rollbacks on this device: pulling more updates is exactly
            // what the degraded state exists to stop.
            let mut store = Store::open(layout.clone()).unwrap();
            store.state_mut().consecutive_rollbacks = tpk_store::MAX_CONSECUTIVE_ROLLBACKS;
            store.save().unwrap();
        }
        let (state, app) = auto_world(layout, true, true);

        tauri::async_runtime::block_on(state.auto_update(app.handle()));

        // `check` short-circuits before the request, so there is no failure to
        // record and nothing was staged.
        let status = state.status(app.handle()).unwrap();
        assert!(status.degraded);
        assert!(status.last_error.is_none(), "no request was made");
        assert!(!status.pending, "nothing was downloaded");
    }

    #[test]
    fn a_blacklisted_layer_does_not_count_as_installed() {
        // Otherwise the planner keeps choosing a patch on a base the store has
        // already dropped from the stack, and every stage fails with E_PARENT
        // until the server publishes a new base.
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let key = tpk_format::secret::SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[tpk_format::sign::TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let layout = tpk_store::Layout::new(data.path().join("tpk"), cache.path().join("tpk"));
        let pack = crate::tests::base_pack(cache.path(), &key, 7, b"v7");
        let sha = tpk_format::sign::sha256_hex(&pack.bytes);
        let core = tpk_format::manifest::PackId::parse("core").unwrap();

        let mut store = Store::open(layout).unwrap();
        store.stage(vec![pack], &trust).unwrap();
        store.boot().unwrap();
        store.commit_booting().unwrap();
        assert_eq!(installed_versions(&store).get(&core), Some(&7));

        assert!(store
            .record_failure(sha, tpk_store::blacklist::Reason::Signature)
            .unwrap());
        assert_eq!(
            installed_versions(&store).get(&core),
            None,
            "a condemned base is not installed as far as planning is concerned"
        );
        // But the device did run v7, so the downgrade floor still remembers it.
        // These are two different questions and two different maps.
        assert_eq!(
            store.state().version_floor.get(&core),
            Some(&7),
            "condemning a layer must not open the door to an older one"
        );
    }

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
