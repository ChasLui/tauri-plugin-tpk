//! The store: layer pool, three-state machine, garbage collection.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tpk_format::container::UnverifiedPack;
use tpk_format::error::ErrorCode;
use tpk_format::manifest::{Op, PackId, PackKind, ParentRef, Sha256Hex};
use tpk_format::sign::{sha256_hex, TrustStore};
use tpk_resolve::{IndexBuilder, LayerSpec, NoMaterialized};

use crate::blacklist::{now_secs, Blacklist, Reason};
use crate::error::{Result, StoreError};
use crate::layout::Layout;
use crate::materialize::{materialize_layer, MaterializedDir};
use crate::state::{
    atomic_write, atomic_write_json, LastError, LayerRecord, Pointer, Revision, StoreState,
    TEMP_MARKER,
};

/// How many unacknowledged launches a revision gets before it is rolled back.
///
/// Three, not one. A process killed by the OS, a power loss, or the user
/// quitting during startup are indistinguishable from a bad pack at this level,
/// and on mobile they are routine. One strike would blacklist working releases.
pub const MAX_BOOT_ATTEMPTS: u32 = 3;

/// After this many consecutive rollbacks, automatic updating stops.
///
/// The blacklist breaks the loop for one pack. This breaks it for a device whose
/// frontend never acknowledges anything — otherwise every new release repeats
/// the same cycle.
pub const MAX_CONSECUTIVE_ROLLBACKS: u32 = 3;

/// A pack ready to be staged.
#[derive(Debug, Clone)]
pub struct IncomingPack {
    /// The downloaded `.tpk` bytes.
    pub bytes: Vec<u8>,
    /// Pack identity, as advertised by the channel and confirmed against the pack.
    pub id: PackId,
    /// What it contributes.
    pub kind: PackKind,
    /// Monotonic ordering key.
    pub version_code: u64,
}

/// What `boot` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootOutcome {
    /// Layers to load, lowest first.
    pub layers: Vec<LayerSpec>,
    /// Which state is active.
    pub pointer: Pointer,
    /// The revision that was rolled back, if any.
    pub rolled_back: Option<String>,
    /// Layers blacklisted during this boot.
    pub blacklisted: Vec<Sha256Hex>,
    /// Whether automatic updating is now disabled.
    pub degraded: bool,
}

/// What `commit_booting` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    /// A booting revision was promoted.
    Committed,
    /// Nothing was on trial; calling again is harmless.
    Noop,
}

/// The on-disk store.
#[derive(Debug)]
pub struct Store {
    layout: Layout,
    state: StoreState,
    blacklist: Blacklist,
    /// Set for the duration of a `stage_for_shell` call.
    shell_version: Option<semver::Version>,
}

impl Store {
    /// Open, or initialise, a store.
    ///
    /// A missing or unparseable `state.json` is treated as "no state": the app
    /// starts on its embedded assets rather than refusing to start.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] only if the directories cannot be created.
    pub fn open(layout: Layout) -> Result<Self> {
        layout.create_dirs()?;
        let state = std::fs::read(layout.state_file())
            .ok()
            .and_then(|bytes| StoreState::parse(&bytes).ok())
            .unwrap_or_default();
        let blacklist = Blacklist::load(&layout.blacklist_file());
        let mut store = Self {
            layout,
            state,
            blacklist,
            shell_version: None,
        };
        if store.backfill_version_floor() {
            // A state written before the floor existed. Persisted so the next
            // open is read-only; a failure costs nothing this launch, because
            // the floor is already correct in memory and the next open retries.
            if let Err(e) = atomic_write_json(&store.layout.state_file(), &store.state) {
                log::warn!("[tpk] could not persist the backfilled version floor: {e}");
            }
        }
        Ok(store)
    }

    /// Raise the per-id version floor to everything this device is known to
    /// have actually run, for a state written before the floor existed.
    ///
    /// Two sources, both of which record `(id, version_code)`:
    ///
    /// * the committed revision's layers — what this device is running now;
    /// * blacklist entries other than [`Reason::NotAcknowledged`] — versions it
    ///   ran and later condemned. This source is the one that matters: a
    ///   condemned layer is dropped from the stack by `write_revision`, so at
    ///   the next commit its `version_code` leaves `committed.layers` for good
    ///   and the id would sit at floor 0 forever, which is exactly the hole an
    ///   older signed pack walks back through.
    ///
    /// `staged`, `booting` and `NotAcknowledged` entries are deliberately not
    /// sources: all three describe a revision that was put on trial and never
    /// acknowledged, and a version that never ran must not bar the one that
    /// did. Same rule as [`Self::raise_version_floor`].
    ///
    /// One gap, bounded to the migration window: the blacklist is a capped FIFO
    /// that also prunes transient entries on expiry, so a condemned version
    /// evicted before the first open on a build that has this backfill is not
    /// recoverable. From then on the floor only moves forward, so it cannot
    /// widen.
    ///
    /// Returns whether anything moved, so a normal open writes nothing.
    fn backfill_version_floor(&mut self) -> bool {
        let before = self.state.version_floor.clone();
        let committed: Vec<(PackId, u64)> = self
            .state
            .committed
            .iter()
            .flat_map(|rev| rev.layers.iter().map(|l| (l.id.clone(), l.version_code)))
            .collect();
        let condemned: Vec<(PackId, u64)> = self
            .blacklist
            .entries()
            .iter()
            .filter(|e| e.reason != Reason::NotAcknowledged)
            .map(|e| (e.id.clone(), e.version_code))
            .collect();
        for (id, version_code) in committed.into_iter().chain(condemned) {
            // `record_failure` files a layer it cannot name under a placeholder
            // id at version 0, and a floor of 0 is no floor at all.
            if version_code > 0 {
                self.state.observe_version(&id, version_code);
            }
        }
        self.state.version_floor != before
    }

    /// The directory layout.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Current state.
    pub fn state(&self) -> &StoreState {
        &self.state
    }

    /// Current blacklist.
    pub fn blacklist(&self) -> &Blacklist {
        &self.blacklist
    }

    /// Whether the blacklist currently refuses this layer.
    ///
    /// The one place that decides it. Anything reasoning about what is really
    /// installed — the resolver's stack, the inherit filter, the planner's view
    /// of installed versions — has to agree, or the planner keeps asking for
    /// patches on a base the stack has already dropped.
    pub fn blocks(&self, layer: &LayerRecord, now: u64) -> bool {
        self.blacklist
            .blocks(&layer.file_sha256, &layer.id, layer.version_code, now)
    }

    /// Whether automatic updating has been switched off after repeated rollbacks.
    pub fn is_degraded(&self) -> bool {
        self.state.consecutive_rollbacks >= MAX_CONSECUTIVE_ROLLBACKS
    }

    /// A source of materialized delta results for the resolver.
    pub fn materialized(&self) -> MaterializedDir {
        MaterializedDir::new(self.layout.materialized_dir())
    }

    /// Copy a seed pack shipped inside the app bundle, if the pool is empty.
    ///
    /// Returns whether anything was copied.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the copy fails.
    pub fn seed_if_absent(&mut self, seed_dir: &Path, trust: &Arc<TrustStore>) -> Result<bool> {
        if self.state.committed.is_some() || !seed_dir.is_dir() {
            return Ok(false);
        }

        let mut packs = Vec::new();
        for entry in std::fs::read_dir(seed_dir)? {
            let path = entry?.path();
            // Only plain `.tpk` files; the seed directory ships inside the app
            // bundle, but there is no reason to follow anything unusual there.
            if path.extension().is_some_and(|e| e == "tpk") && path.is_file() {
                let bytes = std::fs::read(&path)?;
                let pack =
                    UnverifiedPack::open(&path)?.verify(trust, None, self.state.min_key_epoch)?;
                let m = pack.manifest();
                packs.push(IncomingPack {
                    bytes,
                    id: m.id.clone(),
                    kind: m.kind,
                    version_code: m.version_code,
                });
            }
        }
        if packs.is_empty() {
            return Ok(false);
        }

        // A seed is trusted content shipped with the binary: commit it directly
        // rather than putting it on trial. There is nothing to roll back to.
        //
        // Its `version_code` is not checked against the version floor, and that
        // is deliberate. This runs only when nothing is committed — after a
        // reset, or on a first launch — so refusing a seed below the floor
        // would leave the device on the embedded assets, which are that same
        // bundled content. The floor still governs everything the channel
        // offers, so the next successful check moves the device forward.
        let rev = self.write_revision(packs, trust)?;
        self.state.committed = Some(rev);
        self.state.pointer = Pointer::Committed;
        self.raise_version_floor();
        self.persist()?;
        Ok(true)
    }

    /// Run the cold-start state machine.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the new state cannot be persisted.
    pub fn boot(&mut self) -> Result<BootOutcome> {
        let now = now_secs();
        let mut blacklisted = Vec::new();
        let mut rolled_back = None;

        // An unacknowledged revision is dealt with before anything new is
        // promoted: whatever is on trial has first claim on the verdict.
        if self.state.pointer == Pointer::Booting && self.state.booting.is_some() {
            self.state.boot_attempts = self.state.boot_attempts.saturating_add(1);
            if self.state.boot_attempts >= MAX_BOOT_ATTEMPTS {
                let failed = self.state.booting.take().expect("checked above");
                let committed = self
                    .state
                    .committed
                    .as_ref()
                    .map(|rev| rev.layers.as_slice())
                    .unwrap_or_default();
                // Layers inherited from the committed revision were already
                // acknowledged; condemning them would take the fallback down
                // with the update that failed.
                for layer in failed
                    .layers
                    .iter()
                    .filter(|l| !committed.iter().any(|c| c.file_sha256 == l.file_sha256))
                {
                    // `condemn`, not `record`: the attempt counter above already
                    // distinguished a bad pack from an interrupted launch, and
                    // making the blacklist repeat that work would mean nine
                    // launches before a broken release stopped being retried.
                    self.blacklist.condemn(
                        layer.file_sha256,
                        layer.id.clone(),
                        layer.version_code,
                        Reason::NotAcknowledged,
                        now,
                    );
                    blacklisted.push(layer.file_sha256);
                }
                self.state.pointer = Pointer::Committed;
                self.state.boot_attempts = 0;
                self.state.consecutive_rollbacks =
                    self.state.consecutive_rollbacks.saturating_add(1);
                rolled_back = Some(failed.rev);
            }
        }

        // Promote a staged revision only once nothing is on trial.
        if self.state.pointer == Pointer::Committed && self.state.staged.is_some() {
            let staged = self.state.staged.take().expect("checked above");
            let blocked = staged.layers.iter().any(|l| self.blocks(l, now));
            if blocked {
                // It was blacklisted between download and launch; drop it
                // rather than putting a known-bad revision on trial.
                for layer in &staged.layers {
                    blacklisted.push(layer.file_sha256);
                }
            } else {
                self.state.booting = Some(staged);
                self.state.pointer = Pointer::Booting;
                self.state.boot_attempts = 1;
            }
        }

        let layers = self.active_layer_specs(now);
        self.persist()?;

        Ok(BootOutcome {
            layers,
            pointer: self.state.pointer,
            rolled_back,
            blacklisted,
            degraded: self.is_degraded(),
        })
    }

    /// Acknowledge the running revision. Idempotent.
    ///
    /// The state is flushed before this returns rather than on a background
    /// task: a process killed between "the UI rendered" and "the state was
    /// written" would otherwise count as a failed boot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the state cannot be persisted.
    pub fn commit_booting(&mut self) -> Result<CommitOutcome> {
        if self.state.pointer != Pointer::Booting || self.state.booting.is_none() {
            return Ok(CommitOutcome::Noop);
        }
        self.state.committed = self.state.booting.take();
        self.state.pointer = Pointer::Committed;
        self.state.boot_attempts = 0;
        self.state.consecutive_rollbacks = 0;
        self.state.last_error = None;
        self.raise_version_floor();
        self.persist()?;
        // The superseded revision is now unreferenced, as is anything an earlier
        // rollback or dropped stage left behind. Collected here rather than in
        // `boot`, which runs before any window exists. Inline under the caller's
        // lock, so no stage can add references mid-scan; the cost is a directory
        // scan plus each live layer's manifest, no hashing. The commit is already
        // on disk, so a failure costs only space and is not the caller's error.
        if let Err(e) = self.gc() {
            log::warn!("[tpk] could not collect superseded layers: {e}");
        }
        Ok(CommitOutcome::Committed)
    }

    /// Write a downloaded revision to the pool and stage it.
    ///
    /// Delta entries are reconstructed here, while there is still a progress
    /// indicator on screen — never during `boot`, where the work would block
    /// window creation.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Blacklisted`] if any pack is blocked, and
    /// propagates verification, materialization and IO failures.
    pub fn stage(&mut self, packs: Vec<IncomingPack>, trust: &Arc<TrustStore>) -> Result<String> {
        self.stage_for_shell(packs, trust, None)
    }

    /// Stage, additionally enforcing each pack's own shell range.
    ///
    /// The planner already skips packs whose channel entry excludes the shell;
    /// this re-checks `min_shell` / `max_shell` against the signed manifest, for
    /// channels whose entries omit the range, before anything is written into a
    /// revision.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Shell`] when a pack does not support the running
    /// shell, plus everything [`Self::stage`] can return.
    pub fn stage_for_shell(
        &mut self,
        packs: Vec<IncomingPack>,
        trust: &Arc<TrustStore>,
        shell_version: Option<&semver::Version>,
    ) -> Result<String> {
        self.shell_version = shell_version.cloned();
        let now = now_secs();
        for pack in &packs {
            let sha = sha256_hex(&pack.bytes);
            if self
                .blacklist
                .blocks(&sha, &pack.id, pack.version_code, now)
            {
                return Err(StoreError::Blacklisted(format!(
                    "{} version_code {}",
                    pack.id, pack.version_code
                )));
            }
        }

        let rev = self.write_revision(packs, trust)?;
        let id = rev.rev.clone();
        self.state.staged = Some(rev);
        // An update landed, so whatever stopped the previous one is history.
        // Free: the state is written just below either way.
        self.state.last_error = None;
        // Writing state.json is the commit point: layers and materialized
        // results already exist, so a crash before this leaves orphans that the
        // collector cleans up, not a half-visible revision.
        self.persist()?;
        Ok(id)
    }

    /// Forget downloaded content, keeping the blacklist unless asked otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the state cannot be persisted.
    pub fn reset(&mut self, clear_blacklist: bool) -> Result<()> {
        let install_id = self.state.install_id.clone();
        let watermarks = self.state.last_watermark.clone();
        let version_floor = self.state.version_floor.clone();
        let min_key_epoch = self.state.min_key_epoch;

        self.state = StoreState::new();
        // Four things deliberately survive a reset: the install id (re-rolling
        // it would move the device to another rollout bucket), the watermark
        // floor, the per-id version floor and the key epoch floor (all three are
        // anti-downgrade defences, and a support action must not weaken them).
        // The version floor refuses only what is strictly older, so a reset can
        // still re-fetch the version the device was already on.
        self.state.install_id = install_id;
        self.state.last_watermark = watermarks;
        self.state.version_floor = version_floor;
        self.state.min_key_epoch = min_key_epoch;

        if clear_blacklist {
            self.blacklist.clear();
        }
        self.persist()?;
        self.gc()?;
        Ok(())
    }

    /// Record a failure against a layer, blacklisting it if warranted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the blacklist cannot be persisted.
    pub fn record_failure(&mut self, file_sha256: Sha256Hex, reason: Reason) -> Result<bool> {
        let now = now_secs();
        let record = self
            .state
            .referenced_layer_records()
            .into_iter()
            .find(|l| l.file_sha256 == file_sha256);
        let (id, version_code) = match record {
            Some(l) => (l.id, l.version_code),
            // Unknown layer: still record it by hash so a repeat is caught.
            None => (PackId::parse("unknown").expect("valid literal"), 0),
        };
        let blocked = self
            .blacklist
            .record(file_sha256, id, version_code, reason, now);
        self.blacklist.save(&self.layout.blacklist_file())?;
        Ok(blocked)
    }

    /// Record the failure that stopped an update from landing.
    ///
    /// This is what `status().last_error` reports, and it is deliberately
    /// narrow: a failed channel poll, a failed download, a refused stage —
    /// the last thing that kept a revision from reaching `staged`. Business
    /// outcomes (up-to-date, shell out of range, blacklisted, disabled) are
    /// not failures and must not be recorded, or a device that is merely a
    /// shell version behind would show a permanent error. A successful stage
    /// clears it, as does an acknowledgement.
    ///
    /// Only `state.json` is written, not the blacklist: nothing else changed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the state cannot be persisted.
    pub fn record_last_error(&mut self, code: ErrorCode, message: impl Into<String>) -> Result<()> {
        let error = LastError {
            code: code.as_str().to_string(),
            message: message.into(),
        };
        // An offline device polls on a loop and fails the same way every time.
        // Rewriting the same bytes would fsync `state.json` on every attempt for
        // no change at all.
        if self.state.last_error.as_ref() == Some(&error) {
            return Ok(());
        }
        self.state.last_error = Some(error);
        atomic_write_json(&self.layout.state_file(), &self.state)
    }

    /// Delete pool entries and materialized results nothing refers to.
    ///
    /// Reference counting rather than moving files, so it is idempotent and safe
    /// to interrupt.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the pool cannot be read.
    pub fn gc(&mut self) -> Result<usize> {
        let referenced = self.state.referenced_layers();
        let mut removed = 0usize;
        // A young temp file may belong to a write still in flight (a background
        // rebuild, say); deleting it would fail that write's rename. An old one
        // was left by a killed process and would otherwise leak for good.
        let in_flight = |entry: &std::fs::DirEntry| {
            entry.file_name().to_string_lossy().contains(TEMP_MARKER)
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map_or(true, |t| {
                        t.elapsed().map_or(true, |age| age < TEMP_FILE_GRACE)
                    })
        };

        if let Ok(entries) = std::fs::read_dir(self.layout.layers_dir()) {
            for entry in entries.filter_map(std::result::Result::ok) {
                if in_flight(&entry) {
                    continue;
                }
                let path = entry.path();
                let keep = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Sha256Hex::parse(s).ok())
                    .is_some_and(|sha| referenced.contains(&sha));
                if !keep && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }

        // Materialized results are keyed by content hash and cost nothing to
        // rebuild; anything not referenced by a live revision can go.
        let wanted = self.materialized_hashes();
        if let Ok(entries) = std::fs::read_dir(self.layout.materialized_dir()) {
            for entry in entries.filter_map(std::result::Result::ok) {
                if in_flight(&entry) {
                    continue;
                }
                let path = entry.path();
                let keep = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Sha256Hex::parse(s).ok())
                    .is_some_and(|sha| wanted.contains(&sha));
                if !keep && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Hashes of every delta result the live revisions still need.
    fn materialized_hashes(&self) -> std::collections::HashSet<Sha256Hex> {
        let mut wanted = std::collections::HashSet::new();
        for record in self.state.referenced_layer_records() {
            let path = self.layout.layer_file(&record.file_sha256);
            let Ok(pack) = UnverifiedPack::open(&path) else {
                continue;
            };
            // Reading our own pool, so the manifest is parsed without a
            // signature check; the gc only needs the list of delta hashes, and
            // deleting one that is still wanted merely costs a rebuild.
            let Ok(manifest) = tpk_format::manifest::PackManifest::parse(pack.raw_manifest_bytes())
            else {
                continue;
            };
            for entry in &manifest.entries {
                if entry.op == Op::Delta {
                    if let Some(sha) = entry.sha256 {
                        wanted.insert(sha);
                    }
                }
            }
        }
        wanted
    }

    /// Verify, store and materialize a set of packs, producing a revision.
    ///
    /// The revision is the active stack updated by the incoming packs: a base,
    /// dlc or mod replaces every layer of its id (patches included), a patch
    /// stacks on top of its id, and other ids are inherited unless blacklisted.
    ///
    /// Active, not committed: a download during a trial must not silently drop
    /// the layers on trial, which planning already counts as installed. If the
    /// trial is later rolled back, those layers are condemned and `boot` drops
    /// this revision with them.
    fn write_revision(
        &mut self,
        packs: Vec<IncomingPack>,
        trust: &Arc<TrustStore>,
    ) -> Result<Revision> {
        // Stack order: base, then patches by version_code, then dlc, then mods.
        let mut packs = packs;
        packs.sort_by_key(|p| (layer_rank(p.kind), p.version_code));

        let mut records = Vec::with_capacity(packs.len());
        let mut parents: Vec<(Sha256Hex, ParentRef)> = Vec::new();

        for pack in &packs {
            let sha = sha256_hex(&pack.bytes);
            let path = self.layout.layer_file(&sha);
            if !path.exists() {
                atomic_write(&path, &pack.bytes)?;
            }

            // Verify from the pool copy, not from the buffer we were handed:
            // what the resolver will open later is this file.
            let verified =
                UnverifiedPack::open(&path)?.verify(trust, None, self.state.min_key_epoch)?;
            let manifest = verified.manifest();
            if let Some(shell) = &self.shell_version {
                if manifest.min_shell.as_ref().is_some_and(|min| shell < min) {
                    return Err(StoreError::Shell(format!(
                        "{} requires shell >= {}, running {shell}",
                        pack.id,
                        manifest.min_shell.as_ref().expect("checked")
                    )));
                }
                if manifest.max_shell.as_ref().is_some_and(|max| shell > max) {
                    return Err(StoreError::Shell(format!(
                        "{} supports shell <= {}, running {shell}",
                        pack.id,
                        manifest.max_shell.as_ref().expect("checked")
                    )));
                }
            }
            if manifest.id != pack.id || manifest.version_code != pack.version_code {
                return Err(StoreError::Integrity(format!(
                    "{} declares {} version_code {}, channel said {} version_code {}",
                    path.display(),
                    manifest.id,
                    manifest.version_code,
                    pack.id,
                    pack.version_code
                )));
            }
            if let Some(parent) = manifest.parent.clone() {
                parents.push((sha, parent));
            }
            drop(verified);

            let meta = std::fs::metadata(&path)?;
            records.push(LayerRecord {
                id: pack.id.clone(),
                kind: pack.kind,
                version_code: pack.version_code,
                file_sha256: sha,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta),
            });
        }

        let now = now_secs();
        let new: Vec<Sha256Hex> = records.iter().map(|r| r.file_sha256).collect();
        let mut layers: Vec<LayerRecord> = self
            .state
            .active()
            .into_iter()
            .flat_map(|rev| rev.layers.iter())
            .filter(|l| {
                !records.iter().any(|r| {
                    r.file_sha256 == l.file_sha256 || (r.id == l.id && r.kind != PackKind::Patch)
                }) && !self.blocks(l, now)
            })
            .cloned()
            .collect();
        layers.extend(records);
        layers.sort_by_key(|l| (layer_rank(l.kind), l.version_code));

        // Before materialization: a patch on the wrong parent is refused rather
        // than diagnosed later as a delta failure, or — when the deltas happen
        // to apply — never diagnosed at all.
        self.check_parents(&layers, &parents)?;
        self.materialize_all(&layers, &new, trust)?;

        let counter = self.state.highest_counter() + 1;
        Ok(Revision {
            rev: Revision::id(counter),
            layers,
        })
    }

    /// Raise the per-id version floor to whatever is now committed.
    ///
    /// Commit, not stage: a staged revision can still be dropped and a booting
    /// one rolled back, and a version that never ran must not raise the bar
    /// against the version that did. By the time a revision is committed the
    /// frontend has acknowledged it, and `boot` never rolls a committed
    /// revision back, so this is the first point that cannot be undone.
    fn raise_version_floor(&mut self) {
        let Some(committed) = self.state.committed.clone() else {
            return;
        };
        for layer in &committed.layers {
            self.state.observe_version(&layer.id, layer.version_code);
        }
    }

    /// Why the layer a patch names as its parent is not in the stack.
    ///
    /// "Not installed" and "installed but condemned" are the same thing to the
    /// merge, and very different things to whoever reads the log.
    fn missing_parent(&self, parent: &ParentRef) -> String {
        let now = now_secs();
        let condemned = self
            .state
            .active()
            .into_iter()
            .flat_map(|rev| rev.layers.iter())
            .any(|l| {
                l.id == parent.id && l.version_code == parent.version_code && self.blocks(l, now)
            });
        if condemned {
            format!(
                "{} expects to apply on version_code {}, which is installed but blacklisted",
                parent.id, parent.version_code
            )
        } else {
            format!(
                "{} expects to apply on version_code {}, which is not installed",
                parent.id, parent.version_code
            )
        }
    }

    /// Check each incoming pack's parent link against the layer it lands on.
    ///
    /// `tpk pack` verifies the chain when a patch is built, but the device is
    /// where it matters: a patch whose `parent.manifest_sha256` names a pack
    /// this device never installed must not be staged.
    ///
    /// Incoming packs only. Inherited layers passed this same check when they
    /// were staged, and re-checking them would mean re-deriving a manifest hash
    /// for the whole stack on every update.
    ///
    /// The cost is one extra container open per incoming patch, to read the
    /// manifest hash of the layer beneath it. [`LayerRecord`] could cache that
    /// hash instead, but it is a frozen state shape, and the open is cheap next
    /// to the delta work that follows it.
    fn check_parents(
        &self,
        layers: &[LayerRecord],
        parents: &[(Sha256Hex, ParentRef)],
    ) -> Result<()> {
        for (sha, parent) in parents {
            let position = layers
                .iter()
                .position(|l| l.file_sha256 == *sha)
                .expect("an incoming layer is part of the revision it was merged into");
            let beneath = layers[..position]
                .iter()
                .rev()
                .find(|l| l.id == parent.id)
                .ok_or_else(|| StoreError::Parent(self.missing_parent(parent)))?;
            if beneath.version_code != parent.version_code {
                return Err(StoreError::Parent(format!(
                    "{} expects parent version_code {}, the layer beneath it is {}",
                    parent.id, parent.version_code, beneath.version_code
                )));
            }
            let found = UnverifiedPack::open(&self.layout.layer_file(&beneath.file_sha256))?
                .manifest_sha256();
            if found != parent.manifest_sha256 {
                return Err(StoreError::Parent(format!(
                    "{} expects parent manifest {}, the layer beneath it is {found}",
                    parent.id, parent.manifest_sha256
                )));
            }
        }
        Ok(())
    }

    /// Reconstruct every delta in the new layers, each against the stack beneath it.
    ///
    /// Inherited layers are repaired on the way up in case the OS purged their
    /// results. That is best-effort: a failure there is logged, and only
    /// surfaces if a new layer actually needs the missing base.
    fn materialize_all(
        &self,
        layers: &[LayerRecord],
        new: &[Sha256Hex],
        trust: &Arc<TrustStore>,
    ) -> Result<()> {
        let stack: Vec<LayerSpec> = layers
            .iter()
            .map(|l| LayerSpec {
                path: self.layout.layer_file(&l.file_sha256),
                file_sha256: l.file_sha256,
            })
            .collect();
        for (i, spec) in stack.iter().enumerate() {
            let result = materialize_layer(
                spec,
                &stack[..i],
                trust,
                self.state.min_key_epoch,
                &self.layout.materialized_dir(),
            );
            if new.contains(&spec.file_sha256) {
                result?;
            } else if let Err(e) = result {
                log::warn!(
                    "[tpk] could not repair inherited layer {}: {e}",
                    spec.file_sha256
                );
            }
        }
        Ok(())
    }

    /// Layer specs for the active revision, minus anything blacklisted.
    fn active_layer_specs(&self, now: u64) -> Vec<LayerSpec> {
        let Some(rev) = self.state.active() else {
            return Vec::new();
        };
        rev.layers
            .iter()
            .filter(|l| !self.blocks(l, now))
            .map(|l| LayerSpec {
                path: self.layout.layer_file(&l.file_sha256),
                file_sha256: l.file_sha256,
            })
            .collect()
    }

    fn persist(&self) -> Result<()> {
        atomic_write_json(&self.layout.state_file(), &self.state)?;
        self.blacklist.save(&self.layout.blacklist_file())
    }

    /// Mutable access for the plugin layer to record watermarks and epochs.
    pub fn state_mut(&mut self) -> &mut StoreState {
        &mut self.state
    }

    /// Flush state after mutating it through [`Self::state_mut`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the write fails.
    pub fn save(&self) -> Result<()> {
        self.persist()
    }
}

/// How long a temp file is presumed to belong to a live write.
///
/// Far above the slowest single write (a pack or a delta result of at most
/// [`crate::MAX_ASSET_BYTES`]), so `gc` never races a real writer.
const TEMP_FILE_GRACE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Stack position for a pack kind, lowest first.
fn layer_rank(kind: PackKind) -> u8 {
    match kind {
        PackKind::Base => 0,
        PackKind::Patch => 1,
        #[cfg(not(app_store))]
        PackKind::Dlc => 2,
        #[cfg(not(app_store))]
        PackKind::Mod => 3,
    }
}

fn mtime_ns(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Build a resolver over a boot outcome.
///
/// # Errors
///
/// Never fails: layers that cannot be loaded are reported by
/// [`tpk_resolve::Resolver::failed_layers`] rather than aborting.
pub fn resolver_for(
    outcome: &BootOutcome,
    trust: Arc<TrustStore>,
    min_key_epoch: u32,
    cache_budget_bytes: u64,
    materialized: MaterializedDir,
) -> tpk_resolve::Resolver {
    let mut builder = IndexBuilder::new(trust, min_key_epoch);
    for spec in &outcome.layers {
        builder.push_layer(spec);
    }
    builder.build(cache_budget_bytes, Box::new(materialized))
}

/// A resolver with no layers, for the embedded-only case.
pub fn empty_resolver(trust: Arc<TrustStore>) -> tpk_resolve::Resolver {
    IndexBuilder::new(trust, 1).build(0, Box::new(NoMaterialized))
}

impl StoreState {
    /// Every layer record any revision refers to.
    pub(crate) fn referenced_layer_records(&self) -> Vec<LayerRecord> {
        [&self.staged, &self.booting, &self.committed]
            .into_iter()
            .flatten()
            .flat_map(|rev| rev.layers.iter().cloned())
            .collect()
    }
}

/// Path helper used by tests and the plugin layer.
pub fn layer_path(layout: &Layout, sha: &Sha256Hex) -> PathBuf {
    layout.layer_file(sha)
}
