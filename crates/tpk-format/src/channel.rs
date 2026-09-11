//! The channel manifest (specification section 4) — a static JSON file on a CDN.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{FormatError, Result};
use crate::manifest::{PackId, PackKind, Sha256Hex};

/// The only channel spec tag this parser accepts.
pub const CHANNEL_SPEC_TAG: &str = "tpk-channel/1";

/// Upper bound on `notes` as stored. Longer values are truncated on parse.
///
/// `notes` is CDN-controlled text; anything that reaches a UI unbounded is an
/// unreviewed message channel into the app.
pub const MAX_NOTES_LEN: usize = 200;

/// One downloadable pack advertised by a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRef {
    /// Pack identity.
    pub id: PackId,
    /// What the pack contributes.
    pub kind: PackKind,
    /// Display version.
    pub version: semver::Version,
    /// Monotonic ordering key.
    pub version_code: u64,
    /// For patches: the `version_code` this one applies on top of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_version_code: Option<u64>,
    /// Absolute download URL.
    pub url: String,
    /// Exact size of the `.tpk` file.
    pub size: u64,
    /// SHA-256 of the `.tpk` file.
    pub sha256: Sha256Hex,
    /// Whether clients may skip this pack.
    #[serde(default)]
    pub optional: bool,
    /// Staged rollout percentage, 1..=100. Defaults to full rollout.
    #[serde(default = "default_rollout")]
    pub rollout: u8,
}

const fn default_rollout() -> u8 {
    100
}

/// A parsed channel manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelManifest {
    /// Must be `tpk-channel/1`.
    pub spec: String,
    /// The channel this manifest describes.
    pub channel: String,
    /// RFC 3339 publication timestamp.
    pub published_at: String,
    /// Monotonic freshness marker, compared per channel.
    pub watermark: u64,
    /// Signing key generation. Clients keep a monotonic floor and reject older ones.
    #[serde(default = "default_key_epoch")]
    pub key_epoch: u32,
    /// Lowest shell version any pack here supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_shell: Option<semver::Version>,
    /// If set and above the running shell, no pack is applied at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_shell: Option<semver::Version>,
    /// Human-readable release note. Truncated to [`MAX_NOTES_LEN`] on parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The packs on offer.
    pub packs: Vec<PackRef>,
}

const fn default_key_epoch() -> u32 {
    1
}

impl ChannelManifest {
    /// Parse and validate a channel manifest.
    ///
    /// # Errors
    ///
    /// - [`FormatError::SpecTag`] when `spec` is not `tpk-channel/1`
    /// - [`FormatError::Spec`] for a malformed document, an out-of-range
    ///   `rollout`, or a duplicate `(id, version_code)`
    /// - [`FormatError::Parent`] when a patch has no `parent_version_code`, or
    ///   a non-patch carries one
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut manifest: Self =
            serde_json::from_slice(raw).map_err(|e| FormatError::Spec(e.to_string()))?;
        manifest.validate()?;
        if let Some(notes) = &mut manifest.notes {
            truncate_chars(notes, MAX_NOTES_LEN);
        }
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.spec != CHANNEL_SPEC_TAG {
            return Err(FormatError::SpecTag {
                found: self.spec.clone(),
                expected: CHANNEL_SPEC_TAG,
            });
        }
        if self.channel.is_empty() {
            return Err(FormatError::Spec("channel must not be empty".into()));
        }
        if self.key_epoch == 0 {
            return Err(FormatError::Spec("key_epoch must be non-zero".into()));
        }

        let mut seen: HashSet<(&str, u64)> = HashSet::new();
        for pack in &self.packs {
            if pack.version_code == 0 {
                return Err(FormatError::Spec("version_code must be non-zero".into()));
            }
            if !(1..=100).contains(&pack.rollout) {
                return Err(FormatError::Spec(format!(
                    "pack {} rollout {} is outside 1..=100",
                    pack.id, pack.rollout
                )));
            }
            if !seen.insert((pack.id.as_str(), pack.version_code)) {
                return Err(FormatError::Spec(format!(
                    "duplicate pack {} version_code {}",
                    pack.id, pack.version_code
                )));
            }
            match (pack.kind, pack.parent_version_code) {
                (PackKind::Patch, None) => {
                    return Err(FormatError::Parent(format!(
                        "patch {} has no parent_version_code",
                        pack.id
                    )))
                }
                (PackKind::Patch, Some(parent)) if parent >= pack.version_code => {
                    return Err(FormatError::Parent(format!(
                        "patch {} parent_version_code {parent} is not below {}",
                        pack.id, pack.version_code
                    )))
                }
                (kind, Some(_)) if kind != PackKind::Patch => {
                    return Err(FormatError::Parent(format!(
                        "{kind:?} pack {} must not carry parent_version_code",
                        pack.id
                    )))
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether this manifest may replace one already seen at `last_watermark`.
    ///
    /// Equal watermarks are accepted. Rejecting them would let a single
    /// same-minute double publish silently and permanently invalidate a
    /// manifest for every client that saw the other one — and the real
    /// downgrade defence is the per-pack `version_code` check, not this.
    pub fn is_fresh_enough(&self, last_watermark: u64) -> bool {
        self.watermark >= last_watermark
    }
}

fn truncate_chars(s: &mut String, max: usize) {
    if s.chars().count() > max {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        s.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "aa00000000000000000000000000000000000000000000000000000000000001";

    fn pack(kind: &str, code: u64, parent: Option<u64>) -> serde_json::Value {
        let mut p = serde_json::json!({
            "id": "core",
            "kind": kind,
            "version": "1.0.0",
            "version_code": code,
            "url": "https://cdn.example.com/tpk/core/x.tpk",
            "size": 1234,
            "sha256": HASH,
        });
        if let Some(parent) = parent {
            p["parent_version_code"] = serde_json::json!(parent);
        }
        p
    }

    fn manifest(packs: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "spec": "tpk-channel/1",
            "channel": "stable",
            "published_at": "2026-09-11T15:00:00Z",
            "watermark": 202609111500u64,
            "packs": packs,
        })
    }

    fn parse(v: &serde_json::Value) -> Result<ChannelManifest> {
        ChannelManifest::parse(serde_json::to_string(v).unwrap().as_bytes())
    }

    #[test]
    fn parses_a_minimal_channel() {
        let m = parse(&manifest(vec![pack("base", 10000, None)])).unwrap();
        assert_eq!(m.channel, "stable");
        assert_eq!(m.key_epoch, 1, "defaults to the first generation");
        assert_eq!(m.packs[0].rollout, 100, "defaults to full rollout");
        assert!(!m.packs[0].optional);
        assert!(m.force_shell.is_none());
    }

    #[test]
    fn rejects_unknown_spec() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["spec"] = serde_json::json!("tpk-channel/2");
        assert!(matches!(
            parse(&v).unwrap_err(),
            FormatError::SpecTag { .. }
        ));
    }

    #[test]
    fn rejects_zero_key_epoch() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["key_epoch"] = serde_json::json!(0);
        assert!(parse(&v).is_err());
    }

    #[test]
    fn patch_requires_a_lower_parent_version_code() {
        assert!(parse(&manifest(vec![pack("patch", 10003, None)])).is_err());
        assert!(parse(&manifest(vec![pack("patch", 10003, Some(10003))])).is_err());
        assert!(parse(&manifest(vec![pack("patch", 10003, Some(20000))])).is_err());
        assert!(parse(&manifest(vec![pack("patch", 10003, Some(10000))])).is_ok());
    }

    #[test]
    fn non_patch_must_not_carry_parent_version_code() {
        assert!(parse(&manifest(vec![pack("base", 10000, Some(9000))])).is_err());
        assert!(parse(&manifest(vec![pack("dlc", 20000, Some(9000))])).is_err());
    }

    #[test]
    fn rejects_duplicate_id_and_version_code() {
        let v = manifest(vec![pack("base", 10000, None), pack("base", 10000, None)]);
        assert!(parse(&v).is_err());
    }

    #[test]
    fn base_and_patch_for_one_id_coexist() {
        let v = manifest(vec![
            pack("base", 10000, None),
            pack("patch", 10003, Some(10000)),
        ]);
        assert_eq!(parse(&v).unwrap().packs.len(), 2);
    }

    #[test]
    fn rollout_must_be_in_range() {
        for bad in [0u8, 101, 255] {
            let mut v = manifest(vec![pack("base", 10000, None)]);
            v["packs"][0]["rollout"] = serde_json::json!(bad);
            assert!(parse(&v).is_err(), "should reject rollout {bad}");
        }
        for ok in [1u8, 50, 100] {
            let mut v = manifest(vec![pack("base", 10000, None)]);
            v["packs"][0]["rollout"] = serde_json::json!(ok);
            assert_eq!(parse(&v).unwrap().packs[0].rollout, ok);
        }
    }

    #[test]
    fn notes_are_truncated_not_rejected() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("x".repeat(500));
        let m = parse(&v).unwrap();
        assert_eq!(m.notes.unwrap().chars().count(), MAX_NOTES_LEN);
    }

    #[test]
    fn notes_truncation_respects_char_boundaries() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("修复登录页样式".repeat(100));
        let m = parse(&v).unwrap();
        assert_eq!(m.notes.unwrap().chars().count(), MAX_NOTES_LEN);
    }

    #[test]
    fn equal_watermark_is_accepted() {
        let m = parse(&manifest(vec![pack("base", 10000, None)])).unwrap();
        assert!(m.is_fresh_enough(202609111500), "equal must be accepted");
        assert!(m.is_fresh_enough(202609111400), "newer must be accepted");
        assert!(
            !m.is_fresh_enough(202609111600),
            "strictly older must be rejected"
        );
    }

    #[test]
    fn empty_pack_list_is_valid() {
        // A channel with nothing on offer is how you stop a rollout.
        assert!(parse(&manifest(vec![])).unwrap().packs.is_empty());
    }
}
