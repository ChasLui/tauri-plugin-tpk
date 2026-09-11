//! `state.json` — the single file whose atomic rewrite promotes a revision.
//!
//! The three states name their layers by content hash rather than living in
//! separate directories. Promotion is therefore one atomic write, not a
//! sequence of renames: a process killed midway through moving files would
//! otherwise leave the pointer claiming `committed` — a state with no
//! acknowledgement loop behind it — while the layer set was incomplete, which
//! is a permanent white screen.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tpk_format::manifest::{PackId, PackKind, Sha256Hex};

use crate::error::{Result, StoreError};

/// The only state spec tag this version understands.
pub const STATE_SPEC_TAG: &str = "tpk-state/1";

/// Which revision the running process loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pointer {
    /// A revision that has been acknowledged by the frontend.
    Committed,
    /// A revision on trial; not yet acknowledged.
    Booting,
}

/// One layer belonging to a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRecord {
    /// Pack identity.
    pub id: PackId,
    /// What it contributes.
    pub kind: PackKind,
    /// Monotonic ordering key.
    pub version_code: u64,
    /// SHA-256 of the `.tpk` file; also its name in `layers/`.
    pub file_sha256: Sha256Hex,
    /// File size, part of the cheap re-verification tuple.
    pub size: u64,
    /// Modification time in nanoseconds, part of the same tuple.
    #[serde(default)]
    pub mtime_ns: u128,
}

/// A named set of layers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    /// Identifier of the form `rev-<n>`.
    pub rev: String,
    /// The layers, in stack order, lowest first.
    pub layers: Vec<LayerRecord>,
}

impl Revision {
    /// Build a revision id from a counter.
    pub fn id(n: u64) -> String {
        format!("rev-{n}")
    }

    /// The counter embedded in a revision id.
    pub fn counter(&self) -> Option<u64> {
        self.rev.strip_prefix("rev-")?.parse().ok()
    }

    /// Whether the id has the frozen shape.
    pub fn has_valid_id(&self) -> bool {
        self.counter().is_some()
    }
}

/// The last failure worth reporting to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastError {
    /// One of the frozen `E_*` codes.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

/// The persisted store state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreState {
    /// Must be `tpk-state/1`.
    pub spec: String,
    /// Which revision is loaded.
    pub pointer: Pointer,
    /// Stable random identifier, used for staged-rollout bucketing.
    ///
    /// Survives `reset` on purpose: re-rolling it would move the device into a
    /// different bucket and hand it a release it had already been excluded from.
    pub install_id: String,
    /// Downloaded but not yet tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged: Option<Revision>,
    /// On trial this launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booting: Option<Revision>,
    /// Acknowledged and trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed: Option<Revision>,
    /// Launches of the current `booting` revision without an acknowledgement.
    ///
    /// Below the threshold the revision is retried rather than rolled back: an
    /// OS kill, a power loss or the user quitting are ordinary events, and
    /// treating them as "bad pack" blacklists perfectly good releases.
    #[serde(default)]
    pub boot_attempts: u32,
    /// Consecutive rollbacks. At the threshold, automatic updating stops.
    #[serde(default)]
    pub consecutive_rollbacks: u32,
    /// Highest watermark seen, per channel.
    ///
    /// Per channel because a single scalar makes a stable→beta→stable switch
    /// discard every stable manifest forever, with no error anywhere.
    #[serde(default)]
    pub last_watermark: BTreeMap<String, u64>,
    /// Monotonic floor on the signing key generation.
    #[serde(default = "default_min_key_epoch")]
    pub min_key_epoch: u32,
    /// The last failure, for `status()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<LastError>,
}

const fn default_min_key_epoch() -> u32 {
    1
}

impl StoreState {
    /// A fresh state for a device that has never updated.
    pub fn new() -> Self {
        Self {
            spec: STATE_SPEC_TAG.to_string(),
            pointer: Pointer::Committed,
            install_id: generate_install_id(),
            staged: None,
            booting: None,
            committed: None,
            boot_attempts: 0,
            consecutive_rollbacks: 0,
            last_watermark: BTreeMap::new(),
            min_key_epoch: 1,
            last_error: None,
        }
    }

    /// Parse a state document.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::State`] for an unknown spec tag or a malformed
    /// revision id. Callers treat that as "no state" and fall back to the
    /// embedded assets rather than failing the launch.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let state: Self =
            serde_json::from_slice(raw).map_err(|e| StoreError::State(e.to_string()))?;
        if state.spec != STATE_SPEC_TAG {
            return Err(StoreError::State(format!(
                "unknown state spec {:?}",
                state.spec
            )));
        }
        for (name, rev) in [
            ("staged", &state.staged),
            ("booting", &state.booting),
            ("committed", &state.committed),
        ] {
            if let Some(rev) = rev {
                if !rev.has_valid_id() {
                    return Err(StoreError::State(format!(
                        "{name} has a malformed revision id {:?}",
                        rev.rev
                    )));
                }
            }
        }
        Ok(state)
    }

    /// The revision the resolver should load this launch.
    pub fn active(&self) -> Option<&Revision> {
        match self.pointer {
            Pointer::Committed => self.committed.as_ref(),
            Pointer::Booting => self.booting.as_ref(),
        }
    }

    /// Highest revision counter in use, for allocating the next one.
    pub fn highest_counter(&self) -> u64 {
        [&self.staged, &self.booting, &self.committed]
            .into_iter()
            .flatten()
            .filter_map(Revision::counter)
            .max()
            .unwrap_or(0)
    }

    /// Every layer hash any revision still refers to.
    ///
    /// This is the garbage collector's root set.
    pub fn referenced_layers(&self) -> std::collections::HashSet<Sha256Hex> {
        [&self.staged, &self.booting, &self.committed]
            .into_iter()
            .flatten()
            .flat_map(|rev| rev.layers.iter().map(|l| l.file_sha256))
            .collect()
    }

    /// Record the highest watermark seen for a channel.
    pub fn observe_watermark(&mut self, channel: &str, watermark: u64) {
        let slot = self.last_watermark.entry(channel.to_string()).or_insert(0);
        *slot = (*slot).max(watermark);
    }

    /// The floor a channel's manifests must reach.
    pub fn watermark_floor(&self, channel: &str) -> u64 {
        self.last_watermark.get(channel).copied().unwrap_or(0)
    }
}

impl Default for StoreState {
    fn default() -> Self {
        Self::new()
    }
}

/// A random, stable per-installation identifier.
///
/// Shaped like a UUIDv4 but seeded from `RandomState`, which std already
/// randomizes per process — enough for bucketing, and it avoids taking a
/// dependency for one value written once in a device's lifetime.
fn generate_install_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut halves = [0u64; 2];
    for (i, half) in halves.iter_mut().enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_usize(i);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        );
        *half = hasher.finish();
    }
    let b = [halves[0].to_be_bytes(), halves[1].to_be_bytes()].concat();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5],
        b[6] & 0x0f, b[7], (b[8] & 0x3f) | 0x80, b[9],
        b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Write JSON atomically: temp file, fsync, rename, fsync the directory.
///
/// The directory fsync is the step people skip. Without it the rename may not
/// be visible after a power loss even though the file's own contents were
/// flushed — reproducible on both ext4 and APFS.
///
/// # Errors
///
/// Returns [`StoreError::Io`] if any step fails.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| StoreError::State(e.to_string()))?;
    atomic_write(path, &bytes)
}

/// Write bytes atomically.
///
/// # Errors
///
/// Returns [`StoreError::Io`] if any step fails.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let parent = path
        .parent()
        .ok_or_else(|| StoreError::State(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(parent)?;

    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;

    // Durability of the rename itself, not just of the bytes.
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::sign::sha256_hex;

    fn layer(id: &str, code: u64) -> LayerRecord {
        LayerRecord {
            id: PackId::parse(id).unwrap(),
            kind: PackKind::Base,
            version_code: code,
            file_sha256: sha256_hex(format!("{id}{code}").as_bytes()),
            size: 1234,
            mtime_ns: 0,
        }
    }

    #[test]
    fn a_fresh_state_is_committed_and_empty() {
        let s = StoreState::new();
        assert_eq!(s.pointer, Pointer::Committed);
        assert!(s.active().is_none());
        assert_eq!(s.min_key_epoch, 1);
        assert_eq!(s.highest_counter(), 0);
        assert!(s.referenced_layers().is_empty());
    }

    #[test]
    fn install_ids_look_like_uuid_v4_and_differ() {
        let a = StoreState::new().install_id;
        let b = StoreState::new().install_id;
        assert_ne!(a, b);
        assert_eq!(a.len(), 36, "{a}");
        assert_eq!(a.chars().filter(|c| *c == '-').count(), 4, "{a}");
        assert_eq!(a.as_bytes()[14], b'4', "version nibble: {a}");
        assert!(
            matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "variant nibble: {a}"
        );
    }

    #[test]
    fn round_trips_through_json() {
        let mut s = StoreState::new();
        s.committed = Some(Revision {
            rev: Revision::id(7),
            layers: vec![layer("core", 10000)],
        });
        s.observe_watermark("stable", 202609111500);

        let bytes = serde_json::to_vec(&s).unwrap();
        assert_eq!(StoreState::parse(&bytes).unwrap(), s);
    }

    #[test]
    fn rejects_an_unknown_spec_tag() {
        let mut s = StoreState::new();
        s.spec = "tpk-state/2".to_string();
        let bytes = serde_json::to_vec(&s).unwrap();
        assert!(matches!(
            StoreState::parse(&bytes).unwrap_err(),
            StoreError::State(_)
        ));
    }

    #[test]
    fn rejects_a_malformed_revision_id() {
        let mut s = StoreState::new();
        s.committed = Some(Revision {
            rev: "7".to_string(),
            layers: vec![],
        });
        let bytes = serde_json::to_vec(&s).unwrap();
        assert!(StoreState::parse(&bytes).is_err());
    }

    #[test]
    fn active_follows_the_pointer() {
        let mut s = StoreState::new();
        s.committed = Some(Revision {
            rev: Revision::id(1),
            layers: vec![layer("core", 1)],
        });
        s.booting = Some(Revision {
            rev: Revision::id(2),
            layers: vec![layer("core", 2)],
        });

        assert_eq!(s.active().unwrap().rev, "rev-1");
        s.pointer = Pointer::Booting;
        assert_eq!(s.active().unwrap().rev, "rev-2");
    }

    #[test]
    fn referenced_layers_spans_all_three_states() {
        let mut s = StoreState::new();
        s.committed = Some(Revision {
            rev: Revision::id(1),
            layers: vec![layer("core", 1)],
        });
        s.staged = Some(Revision {
            rev: Revision::id(2),
            layers: vec![layer("core", 1), layer("core", 2)],
        });
        // The shared base counts once; the gc must not delete it while either
        // revision still points at it.
        assert_eq!(s.referenced_layers().len(), 2);
        assert_eq!(s.highest_counter(), 2);
    }

    #[test]
    fn watermarks_are_per_channel_and_monotonic() {
        let mut s = StoreState::new();
        s.observe_watermark("stable", 100);
        s.observe_watermark("beta", 500);
        // Switching to beta and back must not wedge stable.
        assert_eq!(s.watermark_floor("stable"), 100);
        assert_eq!(s.watermark_floor("beta"), 500);
        assert_eq!(s.watermark_floor("canary"), 0);

        s.observe_watermark("stable", 50);
        assert_eq!(s.watermark_floor("stable"), 100, "must never go down");
        s.observe_watermark("stable", 200);
        assert_eq!(s.watermark_floor("stable"), 200);
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        atomic_write_json(&path, &StoreState::new()).unwrap();
        let first = std::fs::read(&path).unwrap();

        let mut second_state = StoreState::new();
        second_state.boot_attempts = 3;
        atomic_write_json(&path, &second_state).unwrap();
        let second = std::fs::read(&path).unwrap();

        assert_ne!(first, second);
        assert_eq!(StoreState::parse(&second).unwrap().boot_attempts, 3);
        assert!(
            !path.with_extension("tmp").exists(),
            "temp file left behind"
        );
    }

    #[test]
    fn atomic_write_creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/state.json");
        atomic_write_json(&path, &StoreState::new()).unwrap();
        assert!(path.exists());
    }
}
