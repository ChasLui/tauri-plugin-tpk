//! The three-state machine from specification section 7, against real packs.
//!
//! "Killed" is simulated by dropping the `Store` and reopening it: everything
//! that matters is on disk, so a reopen is exactly what a cold start sees.
#![cfg(feature = "test-packs")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tpk_format::container::UnverifiedPack;
use tpk_format::error::ErrorCode;
use tpk_format::manifest::{PackId, PackKind, ParentRef, Sha256Hex};
use tpk_format::pack::PackBuilder;
use tpk_format::secret::SecretKey;
use tpk_format::sign::{sha256_hex, TrustStore, TrustedKey};
use tpk_resolve::{MaterializedSource, ResolveMiss};
use tpk_store::blacklist::Reason;
use tpk_store::{
    CommitOutcome, IncomingPack, Layout, Pointer, Store, MAX_BOOT_ATTEMPTS,
    MAX_CONSECUTIVE_ROLLBACKS,
};

/// A second layer with its own id, used wherever a case only needs "another
/// chain". A DLC is the natural one; an App Store build has none, so there it
/// is a second base — independent of `core` either way.
#[cfg(not(app_store))]
const ADDON_KIND: PackKind = PackKind::Dlc;
#[cfg(app_store)]
const ADDON_KIND: PackKind = PackKind::Base;

struct World {
    data: tempfile::TempDir,
    cache: tempfile::TempDir,
    build: tempfile::TempDir,
    key: SecretKey,
    /// Trusted at epoch 2, alongside `key` at epoch 1: a shell mid-rotation.
    newer_key: SecretKey,
    trust: Arc<TrustStore>,
    /// Manifest hash of every pack built here, so a patch can name its parent
    /// the way `tpk pack` would.
    built: RefCell<HashMap<(String, u64), Sha256Hex>>,
}

impl World {
    fn new() -> Self {
        let key = SecretKey::generate();
        let newer_key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[
                TrustedKey {
                    key: key.public_key_base64(),
                    epoch: 1,
                },
                TrustedKey {
                    key: newer_key.public_key_base64(),
                    epoch: 2,
                },
            ])
            .unwrap(),
        );
        Self {
            data: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
            build: tempfile::tempdir().unwrap(),
            key,
            newer_key,
            trust,
            built: RefCell::default(),
        }
    }

    fn layout(&self) -> Layout {
        Layout::new(self.data.path().join("tpk"), self.cache.path().join("tpk"))
    }

    /// Reopen the store, which is what a cold start does.
    fn store(&self) -> Store {
        Store::open(self.layout()).unwrap()
    }

    /// Build a base pack with one file.
    fn base(&self, version_code: u64, body: &[u8]) -> IncomingPack {
        self.pack(PackKind::Base, version_code, None, |b| {
            b.add_full("/index.html", body).unwrap();
        })
    }

    /// The link a patch must carry to land on a pack built earlier.
    fn parent_ref(&self, id: &str, version_code: u64) -> ParentRef {
        ParentRef {
            id: PackId::parse(id).unwrap(),
            version: "0.9.0".parse().unwrap(),
            version_code,
            manifest_sha256: self.built.borrow()[&(id.to_string(), version_code)],
        }
    }

    fn pack(
        &self,
        kind: PackKind,
        version_code: u64,
        parent: Option<ParentRef>,
        fill: impl FnOnce(&mut PackBuilder),
    ) -> IncomingPack {
        self.pack_signed_by(&self.key, "core", kind, version_code, parent, fill)
    }

    fn pack_signed_by(
        &self,
        key: &SecretKey,
        id: &str,
        kind: PackKind,
        version_code: u64,
        parent: Option<ParentRef>,
        fill: impl FnOnce(&mut PackBuilder),
    ) -> IncomingPack {
        let path = self
            .build
            .path()
            .join(format!("{id}-{kind:?}-{version_code}.tpk"));
        let mut builder = PackBuilder::new(
            kind,
            PackId::parse(id).unwrap(),
            "1.0.0".parse().unwrap(),
            version_code,
            "2026-09-11T15:00:00Z",
        );
        if let Some(parent) = parent {
            builder = builder.parent(parent);
        }
        fill(&mut builder);
        builder.build(key, &path).unwrap();
        self.built.borrow_mut().insert(
            (id.to_string(), version_code),
            UnverifiedPack::open(&path).unwrap().manifest_sha256(),
        );

        IncomingPack {
            bytes: std::fs::read(&path).unwrap(),
            id: PackId::parse(id).unwrap(),
            kind,
            version_code,
        }
    }

    fn layer_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(self.layout().layers_dir())
            .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        files
    }
}

#[test]
fn a_fresh_store_has_nothing_to_load() {
    let w = World::new();
    let mut store = w.store();
    let outcome = store.boot().unwrap();

    assert!(outcome.layers.is_empty());
    assert_eq!(outcome.pointer, Pointer::Committed);
    assert!(outcome.rolled_back.is_none());
    assert!(!outcome.degraded);
}

#[test]
fn staged_becomes_booting_on_the_next_cold_start() {
    let w = World::new();
    {
        let mut store = w.store();
        store.stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
        // Still committed for the running process: layers never swap mid-flight.
        assert_eq!(store.state().pointer, Pointer::Committed);
        assert!(store.state().staged.is_some());
    }

    let mut store = w.store();
    let outcome = store.boot().unwrap();
    assert_eq!(outcome.pointer, Pointer::Booting);
    assert_eq!(outcome.layers.len(), 1);
    assert!(store.state().staged.is_none(), "staged was consumed");
    assert_eq!(store.state().boot_attempts, 1);
}

#[test]
fn an_unacknowledged_revision_is_retried_before_being_condemned() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();

    // Launch 1: promoted to booting. Then killed before acknowledging.
    let first = w.store().boot().unwrap();
    assert_eq!(first.pointer, Pointer::Booting);

    // Launch 2: still on trial. An OS kill or a power loss looks exactly like a
    // bad pack here, so one strike must not be enough.
    let second = w.store().boot().unwrap();
    assert_eq!(second.pointer, Pointer::Booting);
    assert!(second.rolled_back.is_none());
    assert_eq!(second.layers.len(), 1);

    // Launch 3: out of patience.
    let third = w.store().boot().unwrap();
    assert_eq!(third.pointer, Pointer::Committed);
    assert!(third.rolled_back.is_some());
    assert_eq!(third.layers.len(), 0, "rolled back to embedded assets");
    assert_eq!(third.blacklisted.len(), 1);
    assert_eq!(MAX_BOOT_ATTEMPTS, 3);
}

#[test]
fn acknowledging_promotes_and_survives_a_kill() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();

    {
        let mut store = w.store();
        store.boot().unwrap();
        assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Committed);
        assert_eq!(store.state().pointer, Pointer::Committed);
    }

    // Killed right after acknowledging: the revision must stay committed.
    let mut store = w.store();
    let outcome = store.boot().unwrap();
    assert_eq!(outcome.pointer, Pointer::Committed);
    assert_eq!(outcome.layers.len(), 1);
    assert!(outcome.rolled_back.is_none());
    assert_eq!(store.state().boot_attempts, 0);
}

#[test]
fn acknowledging_twice_is_harmless() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    let mut store = w.store();
    store.boot().unwrap();

    assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Committed);
    assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Noop);
    assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Noop);
}

#[test]
fn acknowledging_without_a_trial_is_a_noop() {
    let w = World::new();
    let mut store = w.store();
    assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Noop);
}

#[test]
fn a_rollback_falls_back_to_the_previous_committed_revision() {
    let w = World::new();
    // v1 is acknowledged.
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
    }
    // v2 is staged but never acknowledged.
    w.store().stage(vec![w.base(2, b"v2")], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    let mut store = w.store();
    let outcome = store.boot().unwrap();
    assert_eq!(outcome.pointer, Pointer::Committed);
    assert_eq!(outcome.layers.len(), 1, "v1 is still there");
    assert_eq!(
        store.state().committed.as_ref().unwrap().layers[0].version_code,
        1
    );
}

#[test]
fn repeated_rollbacks_disable_automatic_updating() {
    let w = World::new();
    for version in 1..=MAX_CONSECUTIVE_ROLLBACKS as u64 {
        w.store()
            .stage(
                vec![w.base(version, format!("v{version}").as_bytes())],
                &w.trust,
            )
            .unwrap();
        for _ in 0..MAX_BOOT_ATTEMPTS {
            w.store().boot().unwrap();
        }
    }

    // The blacklist breaks the loop for one pack; this breaks it for a device
    // whose frontend never acknowledges anything.
    let store = w.store();
    assert!(store.is_degraded());
    assert_eq!(
        store.state().consecutive_rollbacks,
        MAX_CONSECUTIVE_ROLLBACKS
    );
}

#[test]
fn acknowledging_clears_the_rollback_streak() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }
    assert_eq!(w.store().state().consecutive_rollbacks, 1);

    w.store().stage(vec![w.base(2, b"v2")], &w.trust).unwrap();
    let mut store = w.store();
    store.boot().unwrap();
    store.commit_booting().unwrap();
    assert_eq!(store.state().consecutive_rollbacks, 0);
}

#[test]
fn a_blacklisted_pack_cannot_be_staged_again() {
    let w = World::new();
    let pack = w.base(1, b"bad");
    w.store().stage(vec![pack.clone()], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    let mut store = w.store();
    assert!(
        store.stage(vec![pack], &w.trust).is_err(),
        "the same release must not come back"
    );
}

#[test]
fn rebuilding_a_blacklisted_release_does_not_evade_it() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"bad")], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    // Same id and version_code, different bytes — a CI rerun.
    let rebuilt = w.base(1, b"bad, rebuilt");
    let mut store = w.store();
    assert!(store.stage(vec![rebuilt], &w.trust).is_err());

    // Bumping the version code is the way forward, and the signal that
    // something actually changed.
    assert!(store.stage(vec![w.base(2, b"fixed")], &w.trust).is_ok());
}

#[test]
fn reset_keeps_the_blacklist_and_the_install_id_by_default() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"bad")], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    let install_id = w.store().state().install_id.clone();
    let mut store = w.store();
    store.state_mut().observe_watermark("stable", 202609111500);
    store.save().unwrap();
    store.reset(false).unwrap();

    assert!(store.state().committed.is_none());
    assert!(!store.blacklist().is_empty(), "blacklist survives a reset");
    assert_eq!(
        store.state().install_id,
        install_id,
        "re-rolling it would move the device to another rollout bucket"
    );
    assert_eq!(
        store.state().watermark_floor("stable"),
        202609111500,
        "a support action must not weaken the downgrade defence"
    );
}

#[test]
fn reset_can_clear_the_blacklist_explicitly() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"bad")], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    let mut store = w.store();
    store.reset(true).unwrap();
    assert!(store.blacklist().is_empty());
    // And the release can be installed again.
    assert!(store.stage(vec![w.base(1, b"retry")], &w.trust).is_ok());
}

#[test]
fn a_corrupt_state_file_falls_back_to_embedded_assets() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    std::fs::write(w.layout().state_file(), b"{ this is not json").unwrap();

    // Refusing to start would be far worse than serving the embedded assets.
    let mut store = w.store();
    let outcome = store.boot().unwrap();
    assert!(outcome.layers.is_empty());
    assert_eq!(outcome.pointer, Pointer::Committed);
}

#[test]
fn an_unknown_state_spec_falls_back_to_embedded_assets() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    let raw = std::fs::read_to_string(w.layout().state_file()).unwrap();
    std::fs::write(
        w.layout().state_file(),
        raw.replace("tpk-state/1", "tpk-state/2"),
    )
    .unwrap();

    assert!(w.store().boot().unwrap().layers.is_empty());
}

#[test]
fn a_layer_file_deleted_underneath_us_is_reported_not_fatal() {
    let w = World::new();
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
    }

    for file in w.layer_files() {
        std::fs::remove_file(file).unwrap();
    }

    // boot still succeeds; the resolver reports the layer as failed.
    let outcome = w.store().boot().unwrap();
    assert_eq!(outcome.layers.len(), 1, "still referenced by the revision");
    let resolver = tpk_store::resolver_for(
        &outcome,
        Arc::clone(&w.trust),
        1,
        0,
        w.store().materialized(),
    );
    assert_eq!(resolver.failed_layers().len(), 1);
}

#[test]
fn the_collector_keeps_only_referenced_layers() {
    let w = World::new();
    // v1 committed, v2 staged: both are referenced.
    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
    }
    w.store().stage(vec![w.base(2, b"v2")], &w.trust).unwrap();

    let mut store = w.store();
    assert_eq!(store.gc().unwrap(), 0, "nothing is unreferenced yet");
    assert_eq!(w.layer_files().len(), 2);

    // After a reset nothing is referenced.
    store.reset(false).unwrap();
    assert!(w.layer_files().is_empty(), "the pool was collected");
}

#[test]
fn a_shared_layer_is_stored_once_and_kept_while_anything_needs_it() {
    let w = World::new();
    let shared = w.base(1, b"shared base");

    w.store().stage(vec![shared.clone()], &w.trust).unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
    }
    // Stage a revision that includes the very same base plus a patch.
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/index.html", b"patched").unwrap();
    });
    w.store().stage(vec![shared, patch], &w.trust).unwrap();

    // Content addressing means one copy, not two.
    assert_eq!(
        w.layer_files().len(),
        2,
        "base + patch, base not duplicated"
    );
    assert_eq!(w.store().gc().unwrap(), 0);
}

#[test]
fn a_patch_whose_parent_hash_does_not_match_is_refused() {
    let w = World::new();
    install(&w, vec![w.base(1, b"v1")]);

    // Same id and version_code, different bytes: a patch built against a v1
    // this device never installed. `tpk pack` would have caught it on the
    // build machine; nothing before this caught it here.
    let mut wrong = w.parent_ref("core", 1);
    wrong.manifest_sha256 = sha256_hex(b"another v1 manifest entirely");
    let patch = w.pack(PackKind::Patch, 2, Some(wrong), |b| {
        b.add_full("/index.html", b"patched").unwrap();
    });

    let mut store = w.store();
    let err = store.stage(vec![patch], &w.trust).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Parent);
    assert!(err.to_string().contains("parent manifest"), "{err}");
    assert!(
        store.state().staged.is_none(),
        "a patch on the wrong parent must not be staged"
    );
}

#[test]
fn opening_an_old_state_backfills_the_version_floor() {
    let w = World::new();
    let core = PackId::parse("core").unwrap();

    // The shape that makes the hole permanent: a version this device ran, then
    // condemned, then superseded by a revision for a different id. Once that
    // second commit lands, `write_revision` has dropped the condemned layer and
    // `core`'s version_code exists nowhere except the blacklist.
    let base = w.base(9, b"v9");
    let base_sha = sha256_hex(&base.bytes);
    install(&w, vec![base]);
    assert!(w
        .store()
        .record_failure(base_sha, Reason::Signature)
        .unwrap());
    let addon = w.pack_signed_by(&w.key, "extras", ADDON_KIND, 1, None, |b| {
        b.add_full("/extras.txt", b"addon").unwrap();
    });
    install(&w, vec![addon]);
    assert!(
        !committed_layers(&w).iter().any(|(id, _, _)| id == "core"),
        "the condemned base is gone from the revision"
    );

    // Now rewind to a state.json written before the floor existed.
    let state_path = w.layout().state_file();
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    doc.as_object_mut().unwrap().remove("version_floor");
    std::fs::write(&state_path, serde_json::to_vec(&doc).unwrap()).unwrap();
    assert_eq!(
        w.store().state().version_floor.get(&core),
        Some(&9),
        "the floor has to come back from the blacklist, the only place left"
    );

    // And a normal open writes nothing: the backfill is a one-off.
    let untouched = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&state_path)
        .unwrap()
        .set_modified(untouched)
        .unwrap();
    let _ = w.store();
    assert_eq!(
        std::fs::metadata(&state_path).unwrap().modified().unwrap(),
        untouched,
        "an open with nothing to backfill must not rewrite state.json"
    );
}

#[test]
fn a_rolled_back_version_does_not_backfill_the_floor() {
    let w = World::new();
    let core = PackId::parse("core").unwrap();
    install(&w, vec![w.base(2, b"v2")]);

    // Staged, put on trial, never acknowledged: condemned as NotAcknowledged.
    w.store().stage(vec![w.base(5, b"v5")], &w.trust).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }
    assert!(!w.store().blacklist().is_empty(), "v5 was condemned");

    let state_path = w.layout().state_file();
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    doc.as_object_mut().unwrap().remove("version_floor");
    std::fs::write(&state_path, serde_json::to_vec(&doc).unwrap()).unwrap();

    // v5 never ran, so it must not raise the bar against v2, which did — the
    // same rule the commit-only raise follows.
    assert_eq!(w.store().state().version_floor.get(&core), Some(&2));
}

#[test]
fn the_version_floor_survives_a_reopen_and_a_reset() {
    let w = World::new();
    let core = PackId::parse("core").unwrap();
    install(&w, vec![w.base(2, b"v2")]);
    assert_eq!(w.store().state().version_floor.get(&core), Some(&2));

    // A staged-but-never-committed revision must not raise it: it can still be
    // dropped, and a version that never ran must not bar the one that did.
    w.store().stage(vec![w.base(5, b"v5")], &w.trust).unwrap();
    assert_eq!(
        w.store().state().version_floor.get(&core),
        Some(&2),
        "staging is not committing"
    );

    let mut store = w.store();
    store.reset(true).unwrap();
    assert_eq!(
        store.state().version_floor.get(&core),
        Some(&2),
        "a support reset must not weaken the downgrade defence"
    );
    assert!(store.state().committed.is_none(), "content is gone");
    // And it is still there after the reopen a reset is usually followed by.
    assert_eq!(w.store().state().version_floor.get(&core), Some(&2));
}

#[test]
fn a_patch_on_a_blacklisted_base_says_so() {
    let w = World::new();
    let base = w.base(1, b"v1");
    let base_sha = sha256_hex(&base.bytes);
    install(&w, vec![base]);
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/index.html", b"patched").unwrap();
    });

    let mut store = w.store();
    assert!(store.record_failure(base_sha, Reason::Signature).unwrap());
    // The merge drops the condemned base, so the patch has nothing to land on.
    // The message has to say which of the two it is, or the log reads as though
    // the device had never installed the base at all.
    let err = store.stage(vec![patch], &w.trust).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Parent);
    assert!(err.to_string().contains("blacklisted"), "{err}");

    // And the store agrees with the planner-facing view: a condemned layer is
    // not installed, so the next check asks for a full base instead of looping
    // on the same doomed patch.
    let now = tpk_store::blacklist::now_secs();
    let committed = store.state().committed.clone().unwrap();
    assert!(committed.layers.iter().all(|l| store.blocks(l, now)));
}

#[test]
fn a_patch_with_nothing_beneath_it_is_refused() {
    let w = World::new();
    // Built so the parent link is well formed, then staged on an empty store.
    w.base(1, b"v1");
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/index.html", b"patched").unwrap();
    });

    let err = w.store().stage(vec![patch], &w.trust).unwrap_err();
    assert_eq!(err.code(), ErrorCode::Parent);
    assert!(err.to_string().contains("not installed"), "{err}");
}

#[test]
fn a_recorded_failure_survives_a_reopen_and_a_stage_clears_it() {
    let w = World::new();
    w.store()
        .record_last_error(ErrorCode::Network, "the channel could not be reached")
        .unwrap();

    // Reopened, which is what a cold start does.
    let recorded = w.store().state().last_error.clone().expect("persisted");
    assert_eq!(recorded.code, "E_NETWORK");
    assert!(recorded.message.contains("could not be reached"));

    w.store().stage(vec![w.base(1, b"v1")], &w.trust).unwrap();
    assert!(
        w.store().state().last_error.is_none(),
        "an update that landed clears the failure before it"
    );
}

#[test]
fn a_seed_is_committed_directly_and_only_once() {
    let w = World::new();
    let seed_dir = w.build.path().join("seed");
    std::fs::create_dir_all(&seed_dir).unwrap();

    let pack = w.base(1, b"shipped with the binary");
    std::fs::write(seed_dir.join("base-core-1.0.0.tpk"), &pack.bytes).unwrap();

    let mut store = w.store();
    assert!(store.seed_if_absent(&seed_dir, &w.trust).unwrap());
    // Content shipped inside the binary is already trusted; there is nothing to
    // roll back to, so putting it on trial would be theatre.
    assert_eq!(store.state().pointer, Pointer::Committed);
    assert!(store.state().committed.is_some());

    // Second call does nothing.
    assert!(!store.seed_if_absent(&seed_dir, &w.trust).unwrap());
}

#[test]
fn a_seed_is_ignored_once_content_exists() {
    let w = World::new();
    let seed_dir = w.build.path().join("seed");
    std::fs::create_dir_all(&seed_dir).unwrap();
    std::fs::write(seed_dir.join("base.tpk"), &w.base(1, b"the seed").bytes).unwrap();

    w.store()
        .stage(vec![w.base(2, b"downloaded")], &w.trust)
        .unwrap();
    let mut store = w.store();
    store.boot().unwrap();
    store.commit_booting().unwrap();

    assert!(
        !store.seed_if_absent(&seed_dir, &w.trust).unwrap(),
        "the seed must not overwrite newer downloaded content"
    );
}

#[test]
fn a_delta_is_materialized_at_stage_time() {
    let w = World::new();
    let base_body = b"the original file contents\n".repeat(20_000);
    let mut patched_body = base_body.clone();
    patched_body[0..10].copy_from_slice(b"CHANGED!!!");

    let base = w.pack(PackKind::Base, 1, None, |b| {
        b.add_full("/big.txt", &base_body).unwrap();
    });
    w.store().stage(vec![base], &w.trust).unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
    }

    let stream = tpk_delta::diff(&base_body, &patched_body).unwrap();
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_delta("/big.txt", &stream, &patched_body, sha256_hex(&base_body))
            .unwrap();
    });

    let mut store = w.store();
    store.stage(vec![patch], &w.trust).unwrap();

    // The reconstruction happened now, not at the next boot: doing it there
    // would block window creation and can trip the iOS launch watchdog.
    let expected: Sha256Hex = sha256_hex(&patched_body);
    assert_eq!(
        store.materialized().get(&expected).unwrap(),
        patched_body,
        "the result should already be on disk"
    );
}

#[test]
fn a_delta_against_the_wrong_base_is_refused_at_stage_time() {
    let w = World::new();
    let base_body = b"the original\n".repeat(20_000);
    let other_body = b"something else\n".repeat(20_000);
    let mut patched = base_body.clone();
    patched[0..5].copy_from_slice(b"EDIT!");

    let base = w.pack(PackKind::Base, 1, None, |b| {
        b.add_full("/big.txt", &base_body).unwrap();
    });
    w.store().stage(vec![base], &w.trust).unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
    }

    // The patch claims a base this stack does not have.
    let stream = tpk_delta::diff(&base_body, &patched).unwrap();
    let bad = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_delta("/big.txt", &stream, &patched, sha256_hex(&other_body))
            .unwrap();
    });

    let mut store = w.store();
    let err = store.stage(vec![bad], &w.trust).unwrap_err();
    assert!(err.to_string().contains("base"), "{err}");
    assert!(
        store.state().staged.is_none(),
        "a revision that cannot be materialized must not be staged"
    );
}

#[test]
fn recording_a_failure_blacklists_after_the_transient_threshold() {
    let w = World::new();
    let pack = w.base(1, b"v1");
    let sha = sha256_hex(&pack.bytes);
    w.store().stage(vec![pack], &w.trust).unwrap();

    let mut store = w.store();
    assert!(!store.record_failure(sha, Reason::Hash).unwrap());
    assert!(!store.record_failure(sha, Reason::Hash).unwrap());
    assert!(store.record_failure(sha, Reason::Hash).unwrap());

    // A signature failure is condemned on the first sighting instead.
    let other = w.base(2, b"v2");
    let other_sha = sha256_hex(&other.bytes);
    assert!(store.record_failure(other_sha, Reason::Signature).unwrap());
}

#[test]
fn a_pack_that_does_not_support_the_running_shell_is_refused() {
    let w = World::new();
    let path = w.build.path().join("future.tpk");
    let mut b = PackBuilder::new(
        PackKind::Base,
        PackId::parse("core").unwrap(),
        "1.0.0".parse().unwrap(),
        1,
        "2026-09-11T15:00:00Z",
    )
    .min_shell("3.0.0".parse().unwrap());
    b.add_full("/index.html", b"needs a newer shell").unwrap();
    b.build(&w.key, &path).unwrap();

    let pack = IncomingPack {
        bytes: std::fs::read(&path).unwrap(),
        id: PackId::parse("core").unwrap(),
        kind: PackKind::Base,
        version_code: 1,
    };

    // The planner skips this when the channel entry carries the range; the
    // store still checks the signed manifest for channels built elsewhere.
    let mut store = w.store();
    let shell: semver::Version = "2.3.0".parse().unwrap();
    let err = store
        .stage_for_shell(vec![pack.clone()], &w.trust, Some(&shell))
        .unwrap_err();
    assert!(err.to_string().contains("requires shell"), "{err}");
    assert_eq!(err.code(), tpk_format::error::ErrorCode::Shell);

    let newer: semver::Version = "3.1.0".parse().unwrap();
    assert!(store
        .stage_for_shell(vec![pack], &w.trust, Some(&newer))
        .is_ok());
}

#[test]
fn a_pack_pinned_below_the_running_shell_is_refused() {
    let w = World::new();
    let path = w.build.path().join("pinned.tpk");
    let mut b = PackBuilder::new(
        PackKind::Base,
        PackId::parse("core").unwrap(),
        "1.0.0".parse().unwrap(),
        1,
        "2026-09-11T15:00:00Z",
    )
    .max_shell("2.0.0".parse().unwrap());
    b.add_full("/index.html", b"only for old shells").unwrap();
    b.build(&w.key, &path).unwrap();

    let shell: semver::Version = "2.3.0".parse().unwrap();
    let err = w
        .store()
        .stage_for_shell(
            vec![IncomingPack {
                bytes: std::fs::read(&path).unwrap(),
                id: PackId::parse("core").unwrap(),
                kind: PackKind::Base,
                version_code: 1,
            }],
            &w.trust,
            Some(&shell),
        )
        .unwrap_err();
    assert!(err.to_string().contains("supports shell"), "{err}");
    assert_eq!(err.code(), tpk_format::error::ErrorCode::Shell);
}

#[test]
fn layers_are_stacked_base_then_patch() {
    let w = World::new();
    let base = w.pack(PackKind::Base, 1, None, |b| {
        b.add_full("/v.txt", b"base").unwrap();
    });
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/v.txt", b"patch").unwrap();
    });

    // Handed over in the wrong order on purpose.
    w.store().stage(vec![patch, base], &w.trust).unwrap();
    let outcome = w.store().boot().unwrap();

    let resolver = tpk_store::resolver_for(
        &outcome,
        Arc::clone(&w.trust),
        1,
        0,
        w.store().materialized(),
    );
    assert_eq!(&*resolver.get("/v.txt").unwrap(), b"patch");
}

#[test]
fn a_pack_signed_by_a_newer_epoch_is_staged_at_the_old_floor() {
    let w = World::new();
    let pack = w.pack_signed_by(&w.newer_key, "core", PackKind::Base, 1, None, |b| {
        b.add_full("/v.txt", b"rotated").unwrap();
    });
    assert_eq!(w.store().state().min_key_epoch, 1);

    w.store().stage(vec![pack], &w.trust).unwrap();
    let outcome = w.store().boot().unwrap();
    assert_eq!(outcome.layers.len(), 1);

    let resolver = tpk_store::resolver_for(
        &outcome,
        Arc::clone(&w.trust),
        1,
        0,
        w.store().materialized(),
    );
    assert!(resolver.failed_layers().is_empty());
    assert_eq!(&*resolver.get("/v.txt").unwrap(), b"rotated");
}

#[test]
fn a_seed_signed_by_a_newer_epoch_is_accepted_on_a_fresh_install() {
    let w = World::new();
    let seed_dir = w.build.path().join("seed");
    std::fs::create_dir_all(&seed_dir).unwrap();
    let pack = w.pack_signed_by(&w.newer_key, "core", PackKind::Base, 1, None, |b| {
        b.add_full("/index.html", b"rotated seed").unwrap();
    });
    std::fs::write(seed_dir.join("base.tpk"), &pack.bytes).unwrap();

    let mut store = w.store();
    assert!(store.seed_if_absent(&seed_dir, &w.trust).unwrap());
    assert!(store.state().committed.is_some());
}

#[test]
fn a_pack_below_the_floor_is_refused_everywhere() {
    let w = World::new();
    w.store()
        .stage(vec![w.base(1, b"old key")], &w.trust)
        .unwrap();
    {
        let mut store = w.store();
        store.boot().unwrap();
        store.commit_booting().unwrap();
        store.state_mut().min_key_epoch = 2;
        store.save().unwrap();
    }

    assert!(w
        .store()
        .stage(vec![w.base(2, b"still old key")], &w.trust)
        .is_err());

    let outcome = w.store().boot().unwrap();
    assert_eq!(
        outcome.layers.len(),
        1,
        "the committed layer is still listed"
    );
    let resolver = tpk_store::resolver_for(
        &outcome,
        Arc::clone(&w.trust),
        2,
        0,
        w.store().materialized(),
    );
    assert_eq!(resolver.failed_layers().len(), 1);
    assert_eq!(
        resolver.failed_layers()[0].code,
        tpk_format::error::ErrorCode::Signature
    );

    let out_dir = tempfile::tempdir().unwrap();
    assert!(tpk_store::materialize::materialize_layer(
        &outcome.layers[0],
        &[],
        &w.trust,
        2,
        out_dir.path(),
    )
    .is_err());
}

/// Stage, boot and acknowledge in one go.
fn install(w: &World, packs: Vec<IncomingPack>) {
    w.store().stage(packs, &w.trust).unwrap();
    let mut store = w.store();
    store.boot().unwrap();
    store.commit_booting().unwrap();
}

fn committed_layers(w: &World) -> Vec<(String, PackKind, u64)> {
    w.store()
        .state()
        .committed
        .as_ref()
        .unwrap()
        .layers
        .iter()
        .map(|l| (l.id.to_string(), l.kind, l.version_code))
        .collect()
}

#[test]
fn a_patch_only_update_keeps_the_base_layer() {
    let w = World::new();
    install(
        &w,
        vec![w.pack(PackKind::Base, 1, None, |b| {
            b.add_full("/a.txt", b"base a").unwrap();
            b.add_full("/b.txt", b"base b").unwrap();
        })],
    );
    install(
        &w,
        vec![
            w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
                b.add_full("/a.txt", b"patched a").unwrap();
            }),
        ],
    );

    assert_eq!(
        committed_layers(&w),
        vec![
            ("core".to_string(), PackKind::Base, 1),
            ("core".to_string(), PackKind::Patch, 2)
        ]
    );
    let outcome = w.store().boot().unwrap();
    let resolver = tpk_store::resolver_for(
        &outcome,
        Arc::clone(&w.trust),
        1,
        0,
        w.store().materialized(),
    );
    assert!(resolver.failed_layers().is_empty());
    assert_eq!(&*resolver.get("/a.txt").unwrap(), b"patched a");
    assert_eq!(&*resolver.get("/b.txt").unwrap(), b"base b");
}

#[test]
fn a_new_base_replaces_its_patches() {
    let w = World::new();
    install(&w, vec![w.base(1, b"v1")]);
    install(
        &w,
        vec![
            w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
                b.add_full("/index.html", b"v1 patched").unwrap();
            }),
        ],
    );
    install(&w, vec![w.base(3, b"v3")]);

    assert_eq!(
        committed_layers(&w),
        vec![("core".to_string(), PackKind::Base, 3)]
    );
}

#[test]
// The expected layer list is ordered by kind, so it only holds while the
// addon layer outranks a base — which it does not in an App Store build.
#[cfg(not(app_store))]
fn layers_of_other_ids_survive_an_update() {
    let w = World::new();
    let dlc = w.pack_signed_by(&w.key, "extras", ADDON_KIND, 1, None, |b| {
        b.add_full("/extras.txt", b"dlc").unwrap();
    });
    install(&w, vec![w.base(1, b"v1"), dlc]);
    install(&w, vec![w.base(2, b"v2")]);

    assert_eq!(
        committed_layers(&w),
        vec![
            ("core".to_string(), PackKind::Base, 2),
            ("extras".to_string(), ADDON_KIND, 1)
        ]
    );
}

#[test]
fn gc_keeps_inherited_layers() {
    let w = World::new();
    let base = w.base(1, b"v1");
    let base_file = w.layout().layer_file(&sha256_hex(&base.bytes));
    install(&w, vec![base]);
    install(
        &w,
        vec![
            w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
                b.add_full("/index.html", b"patched").unwrap();
            }),
        ],
    );

    // Nothing references the first revision any more, only the second.
    assert_eq!(w.store().gc().unwrap(), 0);
    assert!(base_file.exists(), "the inherited base is still referenced");
}

#[test]
fn a_commit_collects_superseded_layers() {
    let w = World::new();
    let body = b"the original file contents\n".repeat(20_000);
    let mut patched = body.clone();
    patched[0..10].copy_from_slice(b"CHANGED!!!");
    let base1 = w.pack(PackKind::Base, 1, None, |b| {
        b.add_full("/big.txt", &body).unwrap();
    });
    let base1_file = w.layout().layer_file(&sha256_hex(&base1.bytes));
    install(&w, vec![base1]);
    let stream = tpk_delta::diff(&body, &patched).unwrap();
    install(
        &w,
        vec![
            w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
                b.add_delta("/big.txt", &stream, &patched, sha256_hex(&body))
                    .unwrap();
            }),
        ],
    );
    let result = w.layout().materialized_file(&sha256_hex(&patched));
    assert!(result.exists(), "the patch's delta result is live");

    let base2 = w.base(3, b"v3");
    let base2_file = w.layout().layer_file(&sha256_hex(&base2.bytes));
    w.store().stage(vec![base2], &w.trust).unwrap();
    let mut store = w.store();
    store.boot().unwrap();
    assert!(base1_file.exists(), "still committed while v3 is on trial");
    assert!(result.exists());

    assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Committed);
    assert_eq!(
        w.layer_files(),
        vec![base2_file],
        "base1 and its patch are gone"
    );
    assert!(!result.exists(), "results of removed layers are gone");
}

#[test]
fn a_blacklisted_committed_layer_is_not_inherited() {
    let w = World::new();
    let dlc = w.pack_signed_by(&w.key, "extras", ADDON_KIND, 1, None, |b| {
        b.add_full("/extras.txt", b"dlc").unwrap();
    });
    let dlc_sha = sha256_hex(&dlc.bytes);
    install(&w, vec![w.base(1, b"v1"), dlc]);
    assert!(w
        .store()
        .record_failure(dlc_sha, Reason::Signature)
        .unwrap());

    install(&w, vec![w.base(2, b"v2")]);
    assert_eq!(
        committed_layers(&w),
        vec![("core".to_string(), PackKind::Base, 2)]
    );
}

#[test]
fn a_rollback_does_not_condemn_inherited_layers() {
    let w = World::new();
    let base = w.base(1, b"v1");
    let base_sha = sha256_hex(&base.bytes);
    install(&w, vec![base]);
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/index.html", b"bad patch").unwrap();
    });
    let patch_sha = sha256_hex(&patch.bytes);
    w.store().stage(vec![patch], &w.trust).unwrap();
    // Never acknowledged: the last of these rolls back.
    for _ in 0..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    let mut store = w.store();
    let outcome = store.boot().unwrap();
    assert_eq!(outcome.pointer, Pointer::Committed);
    let now = tpk_store::blacklist::now_secs();
    let core = PackId::parse("core").unwrap();
    assert!(!store.blacklist().blocks(&base_sha, &core, 1, now));
    assert!(store.blacklist().blocks(&patch_sha, &core, 2, now));

    assert_eq!(outcome.layers.len(), 1, "the base still loads");
    let resolver =
        tpk_store::resolver_for(&outcome, Arc::clone(&w.trust), 1, 0, store.materialized());
    assert_eq!(&*resolver.get("/index.html").unwrap(), b"v1");
}

const BIG: &str = "/big.txt";

fn big_base_body() -> Vec<u8> {
    b"the original file contents\n".repeat(20_000)
}

fn changed(body: &[u8], marker: &[u8; 10]) -> Vec<u8> {
    let mut out = body.to_vec();
    out[0..10].copy_from_slice(marker);
    out
}

/// A base carrying `/big.txt` and `/other.txt`.
fn big_base(w: &World) -> IncomingPack {
    w.pack(PackKind::Base, 1, None, |b| {
        b.add_full(BIG, &big_base_body()).unwrap();
        b.add_full("/other.txt", b"from the base").unwrap();
    })
}

/// A patch whose `/big.txt` is a delta from `from` to `to`.
fn delta_patch(w: &World, version_code: u64, parent: u64, from: &[u8], to: &[u8]) -> IncomingPack {
    let stream = tpk_delta::diff(from, to).unwrap();
    w.pack(
        PackKind::Patch,
        version_code,
        Some(w.parent_ref("core", parent)),
        |b| {
            b.add_delta(BIG, &stream, to, sha256_hex(from)).unwrap();
        },
    )
}

fn purge(w: &World) {
    std::fs::remove_dir_all(w.layout().materialized_dir()).unwrap();
}

fn resolver(w: &World, outcome: &tpk_store::BootOutcome) -> tpk_resolve::Resolver {
    tpk_store::resolver_for(
        outcome,
        Arc::clone(&w.trust),
        1,
        0,
        w.store().materialized(),
    )
}

#[test]
fn a_purged_cache_is_rebuilt_without_a_strike() {
    let w = World::new();
    let patched = changed(&big_base_body(), b"CHANGED!!!");
    install(
        &w,
        vec![
            big_base(&w),
            delta_patch(&w, 2, 1, &big_base_body(), &patched),
        ],
    );
    purge(&w);

    let mut store = w.store();
    let outcome = store.boot().unwrap();
    let r = resolver(&w, &outcome);
    let dir = w.layout().materialized_dir();
    assert!(r.failed_layers().is_empty());
    assert!(matches!(
        r.get(BIG),
        Err(ResolveMiss::NotMaterialized { .. })
    ));
    assert!(store.blacklist().is_empty(), "a purge is not a strike");
    assert!(tpk_store::missing_materialized(&r, &dir));

    assert_eq!(
        tpk_store::materialize_stack(&outcome.layers, &w.trust, 1, &dir).unwrap(),
        1
    );
    assert!(!tpk_store::missing_materialized(&r, &dir));
    // Same resolver: misses were not cached.
    assert_eq!(&*r.get(BIG).unwrap(), patched.as_slice());
}

#[test]
fn a_corrupted_cache_file_is_refused_and_rebuilt() {
    let w = World::new();
    let patched = changed(&big_base_body(), b"CHANGED!!!");
    install(
        &w,
        vec![
            big_base(&w),
            delta_patch(&w, 2, 1, &big_base_body(), &patched),
        ],
    );
    let result_file = w
        .layout()
        .materialized_dir()
        .join(sha256_hex(&patched).to_hex());
    std::fs::write(&result_file, b"bit rot").unwrap();

    let outcome = w.store().boot().unwrap();
    let r = resolver(&w, &outcome);
    let dir = w.layout().materialized_dir();
    assert!(tpk_store::missing_materialized(&r, &dir));
    assert_eq!(std::fs::read(&result_file).unwrap().as_slice(), b"bit rot");

    // A direct rebuild replaces the corrupt hash-named file.
    tpk_store::materialize_stack(&outcome.layers, &w.trust, 1, &dir).unwrap();
    assert!(!tpk_store::missing_materialized(&r, &dir));
    assert_eq!(&*r.get(BIG).unwrap(), patched.as_slice());
}

#[test]
fn staging_after_a_purge_repairs_the_committed_results() {
    let w = World::new();
    let v2 = changed(&big_base_body(), b"VERSION 2!");
    let v3 = changed(&v2, b"VERSION 3!");
    install(
        &w,
        vec![big_base(&w), delta_patch(&w, 2, 1, &big_base_body(), &v2)],
    );
    purge(&w);

    // The new patch's base is the purged v2 result.
    install(&w, vec![delta_patch(&w, 3, 2, &v2, &v3)]);

    let dir = w.layout().materialized_dir();
    assert!(
        dir.join(sha256_hex(&v2).to_hex()).exists(),
        "inherited result repaired"
    );
    let outcome = w.store().boot().unwrap();
    let r = resolver(&w, &outcome);
    assert!(!tpk_store::missing_materialized(&r, &dir));
    assert_eq!(&*r.get(BIG).unwrap(), v3.as_slice());
}

#[test]
fn a_patch_only_revision_is_repaired_after_a_purge() {
    let w = World::new();
    let patched = changed(&big_base_body(), b"CHANGED!!!");
    install(&w, vec![big_base(&w)]);
    install(&w, vec![delta_patch(&w, 2, 1, &big_base_body(), &patched)]);
    assert_eq!(committed_layers(&w).len(), 2, "the base is inherited");
    purge(&w);

    let outcome = w.store().boot().unwrap();
    let r = resolver(&w, &outcome);
    let dir = w.layout().materialized_dir();
    assert!(tpk_store::missing_materialized(&r, &dir));
    tpk_store::materialize_stack(&outcome.layers, &w.trust, 1, &dir).unwrap();

    assert_eq!(&*r.get(BIG).unwrap(), patched.as_slice());
    assert_eq!(&*r.get("/other.txt").unwrap(), b"from the base");
}

#[test]
// The expected layer list is ordered by kind, so it only holds while the
// addon layer outranks a base — which it does not in an App Store build.
#[cfg(not(app_store))]
fn staging_while_booting_keeps_the_unacknowledged_layers() {
    let w = World::new();
    install(&w, vec![w.base(1, b"v1")]);
    let dlc = w.pack_signed_by(&w.key, "extras", ADDON_KIND, 1, None, |b| {
        b.add_full("/extras.txt", b"dlc").unwrap();
    });
    w.store().stage(vec![dlc], &w.trust).unwrap();
    assert_eq!(w.store().boot().unwrap().pointer, Pointer::Booting);

    // A download during the trial, before the frontend acknowledged it.
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/index.html", b"patched").unwrap();
    });
    {
        let mut store = w.store();
        store.stage(vec![patch], &w.trust).unwrap();
        store.commit_booting().unwrap();
    }
    w.store().boot().unwrap();
    w.store().commit_booting().unwrap();

    assert_eq!(
        committed_layers(&w),
        vec![
            ("core".to_string(), PackKind::Base, 1),
            ("core".to_string(), PackKind::Patch, 2),
            ("extras".to_string(), ADDON_KIND, 1)
        ]
    );
}

#[test]
fn a_staged_revision_built_on_a_failed_boot_is_dropped() {
    let w = World::new();
    install(&w, vec![w.base(1, b"v1")]);
    let dlc = w.pack_signed_by(&w.key, "extras", ADDON_KIND, 1, None, |b| {
        b.add_full("/extras.txt", b"dlc").unwrap();
    });
    w.store().stage(vec![dlc], &w.trust).unwrap();
    w.store().boot().unwrap();

    // Staged on top of the trial, which is then never acknowledged.
    let patch = w.pack(PackKind::Patch, 2, Some(w.parent_ref("core", 1)), |b| {
        b.add_full("/index.html", b"patched").unwrap();
    });
    w.store().stage(vec![patch], &w.trust).unwrap();
    for _ in 1..MAX_BOOT_ATTEMPTS {
        w.store().boot().unwrap();
    }

    let store = w.store();
    assert_eq!(store.state().pointer, Pointer::Committed);
    assert!(store.state().booting.is_none());
    assert!(
        store.state().staged.is_none(),
        "it carried a condemned layer, so it is not put on trial"
    );
    assert_eq!(
        committed_layers(&w),
        vec![("core".to_string(), PackKind::Base, 1)]
    );
}

#[test]
fn gc_removes_stale_temp_files_but_keeps_fresh_ones() {
    let w = World::new();
    install(&w, vec![w.base(1, b"v1")]);
    let dir = w.layout().layers_dir();
    let stale = dir.join("abc.tpk.tmp.1.1");
    let fresh = dir.join("abc.tpk.tmp.1.2");
    std::fs::write(&stale, b"killed mid-write").unwrap();
    std::fs::write(&fresh, b"still writing").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60))
        .unwrap();

    w.store().gc().unwrap();
    assert!(
        !stale.exists(),
        "a leftover from a killed write is collected"
    );
    assert!(
        fresh.exists(),
        "a write that may be in flight is left alone"
    );
    assert_eq!(
        w.layer_files().len(),
        2,
        "the base plus the fresh temp file"
    );
}
