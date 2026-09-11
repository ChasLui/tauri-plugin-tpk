//! `tpk-manifest.json` — the pack manifest (specification section 3.2).
//!
//! Every cross-field rule lives in [`PackManifest::parse`], never in callers:
//! a manifest that parses is a manifest that is internally consistent.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{FormatError, Result};
use crate::path::PackPath;

/// The only spec tag this parser accepts.
pub const SPEC_TAG: &str = "tpk/1";

/// A 32-byte SHA-256 digest, written as 64 lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Hex([u8; 32]);

impl Sha256Hex {
    /// Wrap raw digest bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse 64 lowercase hex characters.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Hash`] when the input is not exactly 64 lowercase
    /// hex digits. Uppercase is rejected so that a digest has one spelling and
    /// `blobs/<sha256>` entry names stay comparable as plain strings.
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 64 {
            return Err(FormatError::Hash(format!(
                "expected 64 hex characters, got {}",
                s.len()
            )));
        }
        let mut out = [0u8; 32];
        // `as_chunks::<2>` rather than `chunks_exact(2)`: the length is a
        // constant, so this gives `&[u8; 2]` and drops the bounds checks. The
        // remainder is empty by the length check above.
        for (i, [hi, lo]) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            out[i] = (hex_nibble(*hi)? << 4) | hex_nibble(*lo)?;
        }
        Ok(Self(out))
    }

    /// Render as 64 lowercase hex characters.
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
            s.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
        }
        s
    }
}

fn hex_nibble(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(FormatError::Hash(format!(
            "non lowercase-hex character {:?}",
            c as char
        ))),
    }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Sha256Hex {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A pack identifier: `[a-z0-9][a-z0-9-]{0,62}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackId(String);

impl PackId {
    /// Validate a pack id.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::PackId`] when the id is empty, too long, or
    /// contains anything outside `[a-z0-9-]`, or starts with `-`.
    pub fn parse(s: &str) -> Result<Self> {
        let bytes = s.as_bytes();
        let valid = (1..=63).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase_alnum()
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase_alnum() || *b == b'-');
        if valid {
            Ok(Self(s.to_string()))
        } else {
            Err(FormatError::PackId(s.to_string()))
        }
    }

    /// The id as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

trait AsciiLowerAlnum {
    fn is_ascii_lowercase_alnum(&self) -> bool;
}

impl AsciiLowerAlnum for u8 {
    fn is_ascii_lowercase_alnum(&self) -> bool {
        self.is_ascii_digit() || self.is_ascii_lowercase()
    }
}

impl fmt::Display for PackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for PackId {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for PackPath {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PackPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// What a pack contributes to the overlay.
///
/// Deliberately has no `#[serde(other)]` catch-all: an unknown kind must fail
/// with `E_SPEC` rather than degrade into a silently ignored layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackKind {
    /// A complete content tree. One per id, the bottom layer above embedded assets.
    Base,
    /// A file-level diff against a parent, applied in `version_code` order.
    Patch,
    /// Optional add-on content stacked above patches.
    ///
    /// Not available on App Store targets: DPLA §3.3.1(C) forbids enabling
    /// additional features through a non-App-Store distribution mechanism,
    /// regardless of whether they are paid.
    Dlc,
    /// Unsigned user content. Desktop only, and gated behind configuration.
    Mod,
}

/// What an entry does to the path it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// Replace the path with the decoded blob.
    Full,
    /// Reconstruct the path by patching the resolution of the layers below.
    Delta,
    /// Tombstone: hide the path, including everything below this layer.
    Delete,
}

/// How an entry's blob is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// Stored as-is.
    #[serde(rename = "identity")]
    Identity,
    /// zstd-compressed.
    #[serde(rename = "zstd")]
    Zstd,
    /// A bsdiff control stream, zstd-compressed.
    #[serde(rename = "zstd+bsdiff")]
    ZstdBsdiff,
}

/// A link from a patch to the pack it applies on top of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRef {
    /// The parent's pack id. Must equal this pack's id.
    pub id: PackId,
    /// The parent's version, for display.
    pub version: semver::Version,
    /// The parent's `version_code`. This is what comparisons use.
    pub version_code: u64,
    /// SHA-256 of the parent's `tpk-manifest.json` bytes.
    pub manifest_sha256: Sha256Hex,
}

/// Override policy for a pack (specification section 3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackPolicies {
    /// Globs this pack is allowed to override. Default `["**"]`.
    #[serde(default = "default_can_override")]
    pub can_override: Vec<String>,
    /// Globs this pack must not override. Default empty.
    #[serde(default)]
    pub cannot_override: Vec<String>,
    /// Whether the pack came through the signed channel.
    #[serde(default = "default_trusted")]
    pub trusted: bool,
}

fn default_can_override() -> Vec<String> {
    vec!["**".to_string()]
}

const fn default_trusted() -> bool {
    true
}

impl Default for PackPolicies {
    fn default() -> Self {
        Self {
            can_override: default_can_override(),
            cannot_override: Vec::new(),
            trusted: default_trusted(),
        }
    }
}

impl PackPolicies {
    /// Whether the override globs are still at their defaults.
    ///
    /// Non-default globs are rejected until the policy engine exists, so that a
    /// future release never has to interpret historical packs carrying policies
    /// nothing ever enforced.
    pub fn globs_are_default(&self) -> bool {
        self.can_override == default_can_override() && self.cannot_override.is_empty()
    }
}

/// One path's contribution to a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The path this entry affects.
    pub path: PackPath,
    /// What to do with it.
    pub op: Op,
    /// Decoded size in bytes. Absent for `delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// SHA-256 of the decoded content. Absent for `delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256Hex>,
    /// Container-relative blob name, e.g. `blobs/<sha256>.zst`. Absent for `delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    /// SHA-256 of the blob's raw (still encoded) bytes. Absent for `delete`.
    ///
    /// Without this the blob of a `delta` entry has no signature-covered digest
    /// at all — its own name is the only reference, and nothing checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_sha256: Option<Sha256Hex>,
    /// Size of the blob's raw bytes. Absent for `delete`.
    ///
    /// Bounds the decoder: without it a 100 KB zstd frame can expand without
    /// limit before any other check runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_size: Option<u64>,
    /// How the blob is encoded. Absent for `delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<Encoding>,
    /// SHA-256 the layers below must resolve to. Present only for `delta`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_base_sha256: Option<Sha256Hex>,
}

/// A parsed, internally consistent `tpk-manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackManifest {
    /// Must be `tpk/1`.
    pub spec: String,
    /// What this pack contributes.
    pub kind: PackKind,
    /// Pack identity. Layers with the same id form one chain.
    pub id: PackId,
    /// Display version.
    pub version: semver::Version,
    /// Monotonic ordering key. All comparisons use this, never `version`.
    pub version_code: u64,
    /// Lowest shell version this pack supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_shell: Option<semver::Version>,
    /// Highest shell version this pack supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_shell: Option<semver::Version>,
    /// The pack this one patches. Required for `patch`, forbidden for `base`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentRef>,
    /// RFC 3339 build timestamp.
    pub created_at: String,
    /// The channel this pack was built for, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Override policy.
    #[serde(default)]
    pub policies: PackPolicies,
    /// Entries, sorted by path.
    pub entries: Vec<Entry>,
}

impl PackManifest {
    /// Parse and fully validate a manifest.
    ///
    /// # Errors
    ///
    /// - [`FormatError::SpecTag`] when `spec` is not `tpk/1`
    /// - [`FormatError::Spec`] for structural problems, including any
    ///   `op`/`encoding` field combination the specification does not allow
    /// - [`FormatError::Parent`] when the parent link is missing, forbidden or
    ///   inconsistent with this pack
    /// - [`FormatError::Policy`] when policy globs are set but unsupported
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let manifest: Self =
            serde_json::from_slice(raw).map_err(|e| FormatError::Spec(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.spec != SPEC_TAG {
            return Err(FormatError::SpecTag {
                found: self.spec.clone(),
                expected: SPEC_TAG,
            });
        }
        if self.version_code == 0 {
            return Err(FormatError::Spec("version_code must be non-zero".into()));
        }
        if !is_rfc3339_shaped(&self.created_at) {
            return Err(FormatError::Spec(format!(
                "created_at {:?} is not RFC 3339",
                self.created_at
            )));
        }
        if let (Some(min), Some(max)) = (&self.min_shell, &self.max_shell) {
            if min > max {
                return Err(FormatError::Spec(format!(
                    "min_shell {min} is above max_shell {max}"
                )));
            }
        }

        self.validate_parent()?;
        self.validate_policies()?;
        self.validate_entries()
    }

    fn validate_parent(&self) -> Result<()> {
        match (self.kind, &self.parent) {
            (PackKind::Base, Some(_)) => Err(FormatError::Parent(
                "a base pack must not have a parent".into(),
            )),
            (PackKind::Patch, None) => Err(FormatError::Parent(
                "a patch pack must have a parent".into(),
            )),
            (_, Some(parent)) => {
                if parent.id != self.id {
                    return Err(FormatError::Parent(format!(
                        "parent id {} does not match pack id {}",
                        parent.id, self.id
                    )));
                }
                if parent.version_code >= self.version_code {
                    return Err(FormatError::Parent(format!(
                        "parent version_code {} is not below {}",
                        parent.version_code, self.version_code
                    )));
                }
                Ok(())
            }
            (_, None) => Ok(()),
        }
    }

    fn validate_policies(&self) -> Result<()> {
        if self.kind == PackKind::Mod && self.policies.trusted {
            return Err(FormatError::Policy(
                "a mod pack must declare trusted = false".into(),
            ));
        }
        // Reject rather than ignore: a pack carrying globs nothing enforces
        // would become a compatibility problem the day the engine ships.
        if !self.policies.globs_are_default() {
            return Err(FormatError::Policy(
                "can_override/cannot_override are not supported yet and must be left at their defaults"
                    .into(),
            ));
        }
        Ok(())
    }

    fn validate_entries(&self) -> Result<()> {
        if self.entries.is_empty() {
            return Err(FormatError::Spec("entries must not be empty".into()));
        }

        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for entry in &self.entries {
            let path = entry.path.as_str();
            if !seen.insert(path) {
                return Err(FormatError::Spec(format!("duplicate path {path:?}")));
            }
            if let Some(prev) = previous {
                if prev > path {
                    return Err(FormatError::Spec(format!(
                        "entries must be sorted by path; {path:?} follows {prev:?}"
                    )));
                }
            }
            previous = Some(path);
            validate_entry_shape(entry)?;
        }
        Ok(())
    }

    /// Reject entries whose decoded size exceeds `max_asset_bytes`.
    ///
    /// Separate from [`Self::parse`] because the limit is runtime configuration,
    /// not part of the format.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Policy`] naming the first oversized entry.
    pub fn validate_size_limit(&self, max_asset_bytes: u64) -> Result<()> {
        for entry in &self.entries {
            if let Some(size) = entry.size {
                if size > max_asset_bytes {
                    return Err(FormatError::Policy(format!(
                        "entry {} is {size} bytes, over the {max_asset_bytes} byte limit",
                        entry.path
                    )));
                }
            }
        }
        Ok(())
    }
}

fn validate_entry_shape(entry: &Entry) -> Result<()> {
    let path = entry.path.as_str();
    let require = |present: bool, field: &str| -> Result<()> {
        if present {
            Ok(())
        } else {
            Err(FormatError::Spec(format!(
                "entry {path:?} with op {:?} requires {field}",
                entry.op
            )))
        }
    };
    let forbid = |present: bool, field: &str| -> Result<()> {
        if present {
            Err(FormatError::Spec(format!(
                "entry {path:?} with op {:?} must not carry {field}",
                entry.op
            )))
        } else {
            Ok(())
        }
    };

    match entry.op {
        Op::Delete => {
            forbid(entry.size.is_some(), "size")?;
            forbid(entry.sha256.is_some(), "sha256")?;
            forbid(entry.blob.is_some(), "blob")?;
            forbid(entry.blob_sha256.is_some(), "blob_sha256")?;
            forbid(entry.blob_size.is_some(), "blob_size")?;
            forbid(entry.encoding.is_some(), "encoding")?;
            forbid(entry.delta_base_sha256.is_some(), "delta_base_sha256")?;
        }
        Op::Full | Op::Delta => {
            require(entry.size.is_some(), "size")?;
            require(entry.sha256.is_some(), "sha256")?;
            require(entry.blob.is_some(), "blob")?;
            require(entry.blob_sha256.is_some(), "blob_sha256")?;
            require(entry.blob_size.is_some(), "blob_size")?;
            require(entry.encoding.is_some(), "encoding")?;

            let encoding = entry.encoding.expect("checked just above");
            if entry.op == Op::Delta {
                require(entry.delta_base_sha256.is_some(), "delta_base_sha256")?;
                if encoding != Encoding::ZstdBsdiff {
                    return Err(FormatError::Spec(format!(
                        "entry {path:?} is a delta but encoding is {encoding:?}"
                    )));
                }
            } else {
                forbid(entry.delta_base_sha256.is_some(), "delta_base_sha256")?;
                if encoding == Encoding::ZstdBsdiff {
                    return Err(FormatError::Spec(format!(
                        "entry {path:?} is not a delta but uses zstd+bsdiff"
                    )));
                }
            }

            // `blobs/<sha256>` is a naming convention in the spec; tying it to
            // blob_sha256 here is what makes it checkable.
            let blob = entry.blob.as_deref().expect("checked just above");
            let expected = entry.blob_sha256.expect("checked just above").to_hex();
            let stem = blob
                .strip_prefix("blobs/")
                .map(|rest| rest.split('.').next().unwrap_or(rest));
            if stem != Some(expected.as_str()) {
                return Err(FormatError::Spec(format!(
                    "entry {path:?} blob name {blob:?} does not match blob_sha256"
                )));
            }
        }
    }
    Ok(())
}

/// A cheap shape check: `YYYY-MM-DDTHH:MM:SS` plus an offset or `Z`.
///
/// Deliberately does not pull in a date library — the value is only ever
/// displayed and compared as an opaque string.
fn is_rfc3339_shaped(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let digits = |start: usize, end: usize| (start..end).all(|i| b[i].is_ascii_digit());
    digits(0, 4)
        && b[4] == b'-'
        && digits(5, 7)
        && b[7] == b'-'
        && digits(8, 10)
        && (b[10] == b'T' || b[10] == b't')
        && digits(11, 13)
        && b[13] == b':'
        && digits(14, 16)
        && b[16] == b':'
        && digits(17, 19)
        && (s.ends_with('Z') || s.ends_with('z') || s.contains('+') || s[19..].contains('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob_name(hex: &str, ext: &str) -> String {
        format!("blobs/{hex}{ext}")
    }

    const HASH_A: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const HASH_B: &str = "bb00000000000000000000000000000000000000000000000000000000000002";
    const HASH_C: &str = "cc00000000000000000000000000000000000000000000000000000000000003";

    fn full_entry(path: &str, blob_hash: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "op": "full",
            "size": 100,
            "sha256": HASH_A,
            "blob": blob_name(blob_hash, ".zst"),
            "blob_sha256": blob_hash,
            "blob_size": 40,
            "encoding": "zstd",
        })
    }

    fn base_manifest(entries: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "spec": "tpk/1",
            "kind": "base",
            "id": "core",
            "version": "1.0.0",
            "version_code": 10000,
            "created_at": "2026-09-11T15:00:00Z",
            "entries": entries,
        })
    }

    fn parse(v: &serde_json::Value) -> Result<PackManifest> {
        PackManifest::parse(serde_json::to_string(v).unwrap().as_bytes())
    }

    #[test]
    fn parses_a_minimal_base() {
        let m = parse(&base_manifest(vec![full_entry("/index.html", HASH_B)])).unwrap();
        assert_eq!(m.kind, PackKind::Base);
        assert_eq!(m.id.as_str(), "core");
        assert_eq!(m.version_code, 10000);
        assert!(m.parent.is_none());
        assert_eq!(m.policies, PackPolicies::default());
    }

    #[test]
    fn rejects_unknown_spec() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["spec"] = serde_json::json!("tpk/2");
        assert!(matches!(
            parse(&v).unwrap_err(),
            FormatError::SpecTag { .. }
        ));
    }

    #[test]
    fn rejects_unknown_kind_and_op_and_encoding() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["kind"] = serde_json::json!("plugin");
        assert!(parse(&v).is_err());

        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["entries"][0]["op"] = serde_json::json!("append");
        assert!(parse(&v).is_err());

        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["entries"][0]["encoding"] = serde_json::json!("brotli");
        assert!(parse(&v).is_err());
    }

    #[test]
    fn rejects_zero_version_code() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["version_code"] = serde_json::json!(0);
        assert!(parse(&v).is_err());
    }

    #[test]
    fn rejects_duplicate_path() {
        let v = base_manifest(vec![
            full_entry("/a.html", HASH_B),
            full_entry("/a.html", HASH_C),
        ]);
        let err = parse(&v).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn rejects_unsorted_entries() {
        let v = base_manifest(vec![
            full_entry("/b.html", HASH_B),
            full_entry("/a.html", HASH_C),
        ]);
        let err = parse(&v).unwrap_err().to_string();
        assert!(err.contains("sorted"), "{err}");
    }

    #[test]
    fn rejects_empty_entries() {
        assert!(parse(&base_manifest(vec![])).is_err());
    }

    #[test]
    fn rejects_bad_path() {
        let v = base_manifest(vec![full_entry("/../escape", HASH_B)]);
        assert!(parse(&v).is_err());
    }

    #[test]
    fn base_with_parent_is_error() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["parent"] = serde_json::json!({
            "id": "core", "version": "0.9.0", "version_code": 9000, "manifest_sha256": HASH_C,
        });
        assert!(matches!(parse(&v).unwrap_err(), FormatError::Parent(_)));
    }

    #[test]
    fn patch_without_parent_is_error() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["kind"] = serde_json::json!("patch");
        assert!(matches!(parse(&v).unwrap_err(), FormatError::Parent(_)));
    }

    #[test]
    fn patch_parent_must_be_same_id_and_lower_code() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["kind"] = serde_json::json!("patch");
        v["parent"] = serde_json::json!({
            "id": "maps", "version": "0.9.0", "version_code": 9000, "manifest_sha256": HASH_C,
        });
        assert!(parse(&v).is_err(), "parent id must match");

        v["parent"] = serde_json::json!({
            "id": "core", "version": "2.0.0", "version_code": 20000, "manifest_sha256": HASH_C,
        });
        assert!(parse(&v).is_err(), "parent must be older");

        v["parent"] = serde_json::json!({
            "id": "core", "version": "0.9.0", "version_code": 9000, "manifest_sha256": HASH_C,
        });
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn delete_must_not_carry_payload_fields() {
        for field in [
            "size",
            "sha256",
            "blob",
            "blob_sha256",
            "blob_size",
            "encoding",
        ] {
            let mut entry = serde_json::json!({ "path": "/gone.css", "op": "delete" });
            entry[field] = match field {
                "size" | "blob_size" => serde_json::json!(1),
                "encoding" => serde_json::json!("zstd"),
                "blob" => serde_json::json!(blob_name(HASH_B, ".zst")),
                _ => serde_json::json!(HASH_B),
            };
            let v = base_manifest(vec![entry]);
            assert!(parse(&v).is_err(), "delete must reject {field}");
        }
        let v = base_manifest(vec![
            serde_json::json!({ "path": "/gone.css", "op": "delete" }),
        ]);
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn full_requires_blob_sha256_and_blob_size() {
        for field in [
            "blob_sha256",
            "blob_size",
            "sha256",
            "size",
            "blob",
            "encoding",
        ] {
            let mut entry = full_entry("/a.html", HASH_B);
            entry.as_object_mut().unwrap().remove(field);
            let v = base_manifest(vec![entry]);
            assert!(parse(&v).is_err(), "full must require {field}");
        }
    }

    #[test]
    fn delta_requires_base_hash_and_bsdiff_encoding() {
        let delta = |encoding: &str, with_base: bool| {
            let mut e = serde_json::json!({
                "path": "/big.bin",
                "op": "delta",
                "size": 1048576,
                "sha256": HASH_A,
                "blob": blob_name(HASH_B, ".zst"),
                "blob_sha256": HASH_B,
                "blob_size": 4096,
                "encoding": encoding,
            });
            if with_base {
                e["delta_base_sha256"] = serde_json::json!(HASH_C);
            }
            base_manifest(vec![e])
        };
        assert!(
            parse(&delta("zstd+bsdiff", false)).is_err(),
            "needs base hash"
        );
        assert!(
            parse(&delta("zstd", true)).is_err(),
            "needs bsdiff encoding"
        );
        assert!(parse(&delta("zstd+bsdiff", true)).is_ok());
    }

    #[test]
    fn full_must_not_use_bsdiff_encoding() {
        let mut entry = full_entry("/a.html", HASH_B);
        entry["encoding"] = serde_json::json!("zstd+bsdiff");
        assert!(parse(&base_manifest(vec![entry])).is_err());
    }

    #[test]
    fn blob_name_must_match_blob_sha256() {
        let mut entry = full_entry("/a.html", HASH_B);
        entry["blob"] = serde_json::json!(blob_name(HASH_C, ".zst"));
        let err = parse(&base_manifest(vec![entry])).unwrap_err().to_string();
        assert!(err.contains("blob name"), "{err}");
    }

    #[test]
    fn accepts_blob_without_extension() {
        let mut entry = full_entry("/a.html", HASH_B);
        entry["blob"] = serde_json::json!(blob_name(HASH_B, ""));
        entry["encoding"] = serde_json::json!("identity");
        assert!(parse(&base_manifest(vec![entry])).is_ok());
    }

    #[test]
    fn mod_must_declare_untrusted() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["kind"] = serde_json::json!("mod");
        assert!(parse(&v).is_err(), "trusted defaults to true");
        v["policies"] = serde_json::json!({ "trusted": false });
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn non_default_override_globs_are_rejected() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["policies"] = serde_json::json!({ "cannot_override": ["/index.html"] });
        assert!(matches!(parse(&v).unwrap_err(), FormatError::Policy(_)));

        v["policies"] = serde_json::json!({ "can_override": ["/assets/**"] });
        assert!(matches!(parse(&v).unwrap_err(), FormatError::Policy(_)));

        // The explicit defaults are accepted.
        v["policies"] = serde_json::json!({ "can_override": ["**"], "cannot_override": [] });
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn rejects_min_shell_above_max_shell() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["min_shell"] = serde_json::json!("3.0.0");
        v["max_shell"] = serde_json::json!("2.0.0");
        assert!(parse(&v).is_err());
    }

    #[test]
    fn rejects_malformed_created_at() {
        for bad in ["2026-09-11", "not a date", "20260911T150000Z", ""] {
            let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
            v["created_at"] = serde_json::json!(bad);
            assert!(parse(&v).is_err(), "should reject created_at {bad:?}");
        }
        for ok in ["2026-09-11T15:00:00Z", "2026-09-11T15:00:00+08:00"] {
            let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
            v["created_at"] = serde_json::json!(ok);
            assert!(parse(&v).is_ok(), "should accept created_at {ok:?}");
        }
    }

    #[test]
    fn size_limit_is_separate_from_parsing() {
        let mut entry = full_entry("/big.bin", HASH_B);
        entry["size"] = serde_json::json!(100 * 1024 * 1024);
        let m = parse(&base_manifest(vec![entry])).unwrap();
        assert!(m.validate_size_limit(64 * 1024 * 1024).is_err());
        assert!(m.validate_size_limit(128 * 1024 * 1024).is_ok());
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let mut v = base_manifest(vec![full_entry("/a.html", HASH_B)]);
        v["future_field"] = serde_json::json!("whatever");
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn pack_id_rules() {
        for ok in ["core", "a", "maps-hd", "x9", &"a".repeat(63)] {
            assert!(PackId::parse(ok).is_ok(), "should accept {ok:?}");
        }
        for bad in ["", "-core", "Core", "core_hd", "core.hd", &"a".repeat(64)] {
            assert!(PackId::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn sha256_hex_roundtrip_and_rejections() {
        let h = Sha256Hex::parse(HASH_A).unwrap();
        assert_eq!(h.to_hex(), HASH_A);
        assert_eq!(Sha256Hex::from_bytes(*h.as_bytes()), h);

        assert!(Sha256Hex::parse(&HASH_A[..63]).is_err(), "too short");
        assert!(
            Sha256Hex::parse(&HASH_A.to_uppercase()).is_err(),
            "uppercase"
        );
        assert!(
            Sha256Hex::parse(&format!("g{}", &HASH_A[1..])).is_err(),
            "non-hex"
        );
    }

    #[test]
    fn serializes_back_to_a_parseable_document() {
        let m = parse(&base_manifest(vec![
            serde_json::json!({ "path": "/gone.css", "op": "delete" }),
            full_entry("/index.html", HASH_B),
        ]))
        .unwrap();
        let round = PackManifest::parse(serde_json::to_vec(&m).unwrap().as_slice()).unwrap();
        assert_eq!(m, round);
    }
}
