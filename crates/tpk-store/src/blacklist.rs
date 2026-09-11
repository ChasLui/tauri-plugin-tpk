//! The blacklist of packs that must not be loaded again.
//!
//! Two failure modes shaped this, and they pull in opposite directions:
//!
//! * **Escaping it.** Matching only on `sha256` means republishing the same
//!   content produces a different hash — a CI rerun is enough to walk straight
//!   past the list. Entries therefore also match on `(id, version_code)`, which
//!   forces a publisher to bump the code to ship again. That bump is also the
//!   observable signal that something was actually fixed.
//! * **Being trapped by it.** A transient disk error must not disable a release
//!   permanently. Entries carry the reason that put them there, and only the
//!   reasons that cannot be transient are permanent.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tpk_format::error::ErrorCode;
use tpk_format::manifest::{PackId, Sha256Hex};

use crate::error::Result;
use crate::state::atomic_write_json;

/// How many strikes a transient failure gets before it sticks.
pub const TRANSIENT_STRIKES: u32 = 3;
/// How long a transient entry survives, in seconds (30 days).
pub const TRANSIENT_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Upper bound on entries, oldest dropped first.
pub const MAX_ENTRIES: usize = 256;

/// Why a pack was blacklisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The signature did not verify. Never transient.
    Signature,
    /// The document violated the schema. Never transient.
    Spec,
    /// A path was illegal. Never transient.
    Path,
    /// A parent link was unsatisfiable. Never transient.
    Parent,
    /// Filesystem trouble. Possibly transient.
    Io,
    /// Content did not match its digest. Possibly transient (bit rot, partial write).
    Hash,
    /// A delta failed to apply. Possibly transient.
    Delta,
    /// The revision was never acknowledged. Possibly transient — an OS kill
    /// looks exactly like this.
    NotAcknowledged,
}

impl Reason {
    /// Whether this reason can be caused by something other than a bad pack.
    pub const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Io | Self::Hash | Self::Delta | Self::NotAcknowledged
        )
    }

    /// Best-effort mapping from an error code.
    pub const fn from_code(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Signature => Self::Signature,
            ErrorCode::Path => Self::Path,
            ErrorCode::Parent => Self::Parent,
            ErrorCode::Hash => Self::Hash,
            ErrorCode::Delta => Self::Delta,
            ErrorCode::Io => Self::Io,
            _ => Self::Spec,
        }
    }
}

/// One blacklisted pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Hash of the pack file.
    pub sha256: Sha256Hex,
    /// Pack identity, so a rebuild of the same version cannot slip through.
    pub id: PackId,
    /// Version code, same purpose.
    pub version_code: u64,
    /// Why it is here.
    pub reason: Reason,
    /// How many times it has failed.
    pub count: u32,
    /// Unix seconds when it was first recorded.
    pub first_seen: u64,
}

impl Entry {
    /// Whether this entry currently blocks loading.
    fn is_active(&self, now: u64) -> bool {
        if !self.reason.is_transient() {
            return true;
        }
        if self.count < TRANSIENT_STRIKES {
            return false;
        }
        now.saturating_sub(self.first_seen) < TRANSIENT_TTL_SECS
    }

    /// Whether this entry can be dropped entirely.
    fn is_expired(&self, now: u64) -> bool {
        self.reason.is_transient() && now.saturating_sub(self.first_seen) >= TRANSIENT_TTL_SECS
    }
}

/// The persisted blacklist.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Blacklist {
    #[serde(default)]
    entries: Vec<Entry>,
}

impl Blacklist {
    /// Load from disk, treating a missing or unreadable file as empty.
    ///
    /// Failing to read the blacklist must not stop the app from starting; the
    /// cost is at worst retrying a bad pack once more.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Persist to disk.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::StoreError::Io`] if the write fails.
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }

    /// Number of recorded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, including ones not currently active.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Whether a pack is currently blocked.
    ///
    /// Matches on the file hash **or** on `(id, version_code)`; the latter is
    /// what stops a rebuild of the same release from slipping through.
    pub fn blocks(&self, sha256: &Sha256Hex, id: &PackId, version_code: u64, now: u64) -> bool {
        self.entries.iter().any(|e| {
            e.is_active(now)
                && (e.sha256 == *sha256 || (e.id == *id && e.version_code == version_code))
        })
    }

    /// Record a failure, returning whether the pack is now blocked.
    pub fn record(
        &mut self,
        sha256: Sha256Hex,
        id: PackId,
        version_code: u64,
        reason: Reason,
        now: u64,
    ) -> bool {
        self.prune(now);

        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.sha256 == sha256 || (e.id == id && e.version_code == version_code))
        {
            existing.count = existing.count.saturating_add(1);
            // A permanent reason overrides a transient one; the reverse must not
            // happen, or one flaky IO error would downgrade a bad signature.
            if !reason.is_transient() {
                existing.reason = reason;
            }
            return existing.is_active(now);
        }

        self.entries.push(Entry {
            sha256,
            id,
            version_code,
            reason,
            count: 1,
            first_seen: now,
        });
        // Oldest first; the list is capped so a pathological device cannot grow
        // it without bound.
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..excess);
        }
        self.entries.last().is_some_and(|e| e.is_active(now))
    }

    /// Record a failure that a higher layer has already confirmed.
    ///
    /// `record` counts one strike, for callers that cannot tell a bad pack from
    /// a flaky moment. `condemn` is for callers that already ran that
    /// distinction themselves — the boot-attempt counter being the one that
    /// matters. Without this the two mechanisms would compose into nine
    /// launches before a genuinely broken release stopped being retried.
    ///
    /// A transient reason still keeps its TTL, so a release condemned by three
    /// interrupted launches is not condemned forever.
    pub fn condemn(
        &mut self,
        sha256: Sha256Hex,
        id: PackId,
        version_code: u64,
        reason: Reason,
        now: u64,
    ) -> bool {
        self.record(sha256, id.clone(), version_code, reason, now);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.sha256 == sha256 || (e.id == id && e.version_code == version_code))
        {
            entry.count = entry.count.max(TRANSIENT_STRIKES);
            return entry.is_active(now);
        }
        false
    }

    /// Drop entries whose TTL has passed.
    pub fn prune(&mut self, now: u64) {
        self.entries.retain(|e| !e.is_expired(now));
    }

    /// Forget everything. Support path, behind `tpk:allow-reset`.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Current time in Unix seconds, saturating at zero for clocks before the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::sign::sha256_hex;

    const NOW: u64 = 1_789_140_600;

    fn id(name: &str) -> PackId {
        PackId::parse(name).unwrap()
    }

    #[test]
    fn a_permanent_reason_blocks_immediately() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"bad");
        assert!(bl.record(sha, id("core"), 1, Reason::Signature, NOW));
        assert!(bl.blocks(&sha, &id("core"), 1, NOW));
    }

    #[test]
    fn a_transient_reason_needs_three_strikes() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"maybe-bad");

        // An OS kill during boot looks identical to a bad pack; one occurrence
        // must not condemn the release.
        assert!(!bl.record(sha, id("core"), 1, Reason::NotAcknowledged, NOW));
        assert!(!bl.blocks(&sha, &id("core"), 1, NOW));
        assert!(!bl.record(sha, id("core"), 1, Reason::NotAcknowledged, NOW));
        assert!(!bl.blocks(&sha, &id("core"), 1, NOW));
        assert!(bl.record(sha, id("core"), 1, Reason::NotAcknowledged, NOW));
        assert!(bl.blocks(&sha, &id("core"), 1, NOW));
    }

    #[test]
    fn rebuilding_the_same_release_does_not_escape_the_list() {
        let mut bl = Blacklist::default();
        bl.record(sha256_hex(b"build-1"), id("core"), 10003, Reason::Spec, NOW);

        // Same id and version_code, different bytes: a CI rerun.
        assert!(
            bl.blocks(&sha256_hex(b"build-2"), &id("core"), 10003, NOW),
            "a rebuild must still be blocked"
        );
        // Bumping the version code is the way out, and the way we can tell
        // something was actually changed.
        assert!(!bl.blocks(&sha256_hex(b"build-2"), &id("core"), 10004, NOW));
    }

    #[test]
    fn a_different_pack_id_is_unaffected() {
        let mut bl = Blacklist::default();
        bl.record(sha256_hex(b"bad"), id("core"), 1, Reason::Signature, NOW);
        assert!(!bl.blocks(&sha256_hex(b"other"), &id("maps"), 1, NOW));
    }

    #[test]
    fn a_transient_entry_expires() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"flaky");
        for _ in 0..TRANSIENT_STRIKES {
            bl.record(sha, id("core"), 1, Reason::Io, NOW);
        }
        assert!(bl.blocks(&sha, &id("core"), 1, NOW));

        let later = NOW + TRANSIENT_TTL_SECS + 1;
        assert!(!bl.blocks(&sha, &id("core"), 1, later));
        bl.prune(later);
        assert!(bl.is_empty(), "expired entries are dropped");
    }

    #[test]
    fn a_permanent_entry_never_expires() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"forged");
        bl.record(sha, id("core"), 1, Reason::Signature, NOW);

        let much_later = NOW + TRANSIENT_TTL_SECS * 100;
        assert!(bl.blocks(&sha, &id("core"), 1, much_later));
        bl.prune(much_later);
        assert_eq!(bl.len(), 1);
    }

    #[test]
    fn a_permanent_reason_upgrades_a_transient_entry() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"x");
        bl.record(sha, id("core"), 1, Reason::Io, NOW);
        assert!(!bl.blocks(&sha, &id("core"), 1, NOW), "one IO strike only");

        bl.record(sha, id("core"), 1, Reason::Signature, NOW);
        assert!(bl.blocks(&sha, &id("core"), 1, NOW), "now permanent");
    }

    #[test]
    fn a_transient_reason_does_not_downgrade_a_permanent_entry() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"x");
        bl.record(sha, id("core"), 1, Reason::Signature, NOW);
        bl.record(sha, id("core"), 1, Reason::Io, NOW);
        assert!(bl.blocks(&sha, &id("core"), 1, NOW));
        assert_eq!(bl.entries()[0].reason, Reason::Signature);
    }

    #[test]
    fn condemning_takes_effect_immediately() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"three-failed-launches");
        // The boot-attempt counter already gave this release its chances.
        assert!(bl.condemn(sha, id("core"), 1, Reason::NotAcknowledged, NOW));
        assert!(bl.blocks(&sha, &id("core"), 1, NOW));
    }

    #[test]
    fn a_condemned_transient_entry_still_expires() {
        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"x");
        bl.condemn(sha, id("core"), 1, Reason::NotAcknowledged, NOW);
        assert!(!bl.blocks(&sha, &id("core"), 1, NOW + TRANSIENT_TTL_SECS + 1));
    }

    #[test]
    fn the_list_is_capped() {
        let mut bl = Blacklist::default();
        for i in 0..(MAX_ENTRIES + 50) {
            bl.record(
                sha256_hex(format!("pack-{i}").as_bytes()),
                id("core"),
                i as u64 + 1,
                Reason::Signature,
                NOW,
            );
        }
        assert_eq!(bl.len(), MAX_ENTRIES);
        // The newest survive; the oldest were dropped.
        assert!(bl.blocks(
            &sha256_hex(format!("pack-{}", MAX_ENTRIES + 49).as_bytes()),
            &id("core"),
            (MAX_ENTRIES + 50) as u64,
            NOW
        ));
    }

    #[test]
    fn reason_transience_is_classified_correctly() {
        for permanent in [
            Reason::Signature,
            Reason::Spec,
            Reason::Path,
            Reason::Parent,
        ] {
            assert!(!permanent.is_transient(), "{permanent:?}");
        }
        for transient in [
            Reason::Io,
            Reason::Hash,
            Reason::Delta,
            Reason::NotAcknowledged,
        ] {
            assert!(transient.is_transient(), "{transient:?}");
        }
    }

    #[test]
    fn survives_a_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blacklist.json");

        let mut bl = Blacklist::default();
        let sha = sha256_hex(b"bad");
        bl.record(sha, id("core"), 1, Reason::Signature, NOW);
        bl.save(&path).unwrap();

        let loaded = Blacklist::load(&path);
        assert!(loaded.blocks(&sha, &id("core"), 1, NOW));
    }

    #[test]
    fn a_missing_or_corrupt_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Blacklist::load(&dir.path().join("absent.json")).is_empty());

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{not json").unwrap();
        assert!(
            Blacklist::load(&corrupt).is_empty(),
            "an unreadable blacklist must not stop the app from starting"
        );
    }

    #[test]
    fn clear_forgets_everything() {
        let mut bl = Blacklist::default();
        bl.record(sha256_hex(b"x"), id("core"), 1, Reason::Signature, NOW);
        bl.clear();
        assert!(bl.is_empty());
    }
}
