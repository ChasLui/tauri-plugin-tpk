//! The three-state machine from specification section 7, against real packs.
//!
//! "Killed" is simulated by dropping the `Store` and reopening it: everything
//! that matters is on disk, so a reopen is exactly what a cold start sees.
#![cfg(feature = "test-packs")]

use std::path::PathBuf;
use std::sync::Arc;

use tpk_format::manifest::{PackId, PackKind, ParentRef, Sha256Hex};
use tpk_format::pack::PackBuilder;
use tpk_format::secret::SecretKey;
use tpk_format::sign::{sha256_hex, TrustStore, TrustedKey};
use tpk_resolve::MaterializedSource;
use tpk_store::blacklist::Reason;
use tpk_store::{
    CommitOutcome, IncomingPack, Layout, Pointer, Store, MAX_BOOT_ATTEMPTS,
    MAX_CONSECUTIVE_ROLLBACKS,
};

struct World {
    data: tempfile::TempDir,
    cache: tempfile::TempDir,
    build: tempfile::TempDir,
    key: SecretKey,
    trust: Arc<TrustStore>,
}

impl World {
    fn new() -> Self {
        let key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        Self {
            data: tempfile::tempdir().unwrap(),
            cache: tempfile::tempdir().unwrap(),
            build: tempfile::tempdir().unwrap(),
            key,
            trust,
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

    fn pack(
        &self,
        kind: PackKind,
        version_code: u64,
        parent: Option<u64>,
        fill: impl FnOnce(&mut PackBuilder),
    ) -> IncomingPack {
        let path = self
            .build
            .path()
            .join(format!("{kind:?}-{version_code}.tpk"));
        let mut builder = PackBuilder::new(
            kind,
            PackId::parse("core").unwrap(),
            "1.0.0".parse().unwrap(),
            version_code,
            "2026-09-11T15:00:00Z",
        );
        if let Some(parent_code) = parent {
            builder = builder.parent(ParentRef {
                id: PackId::parse("core").unwrap(),
                version: "0.9.0".parse().unwrap(),
                version_code: parent_code,
                manifest_sha256: sha256_hex(b"parent"),
            });
        }
        fill(&mut builder);
        builder.build(&self.key, &path).unwrap();

        IncomingPack {
            bytes: std::fs::read(&path).unwrap(),
            id: PackId::parse("core").unwrap(),
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
    let patch = w.pack(PackKind::Patch, 2, Some(1), |b| {
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
    let patch = w.pack(PackKind::Patch, 2, Some(1), |b| {
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
    let bad = w.pack(PackKind::Patch, 2, Some(1), |b| {
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

    // A channel entry carries no shell range, so this can only be caught once
    // the signed manifest is in hand.
    let mut store = w.store();
    let shell: semver::Version = "2.3.0".parse().unwrap();
    let err = store
        .stage_for_shell(vec![pack.clone()], &w.trust, Some(&shell))
        .unwrap_err();
    assert!(err.to_string().contains("requires shell"), "{err}");

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
}

#[test]
fn layers_are_stacked_base_then_patch() {
    let w = World::new();
    let base = w.pack(PackKind::Base, 1, None, |b| {
        b.add_full("/v.txt", b"base").unwrap();
    });
    let patch = w.pack(PackKind::Patch, 2, Some(1), |b| {
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
