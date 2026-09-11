//! The `.tpk` ZIP container.
//!
//! Opening a pack is the real trust boundary: to read the manifest we must let
//! the ZIP parser walk fully untrusted bytes before any signature has been
//! checked. The type states below make the required order un-bypassable —
//! [`UnverifiedPack`] exposes only the bytes needed to verify, and entry access
//! exists exclusively on [`VerifiedPack`].

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use crate::error::{FormatError, Result};
use crate::manifest::{Encoding, Entry, Op, PackManifest, Sha256Hex};
use crate::sign::{sha256_hex, verify_sha256, TrustStore};

/// Container-relative name of the manifest.
pub const MANIFEST_NAME: &str = "tpk-manifest.json";
/// Container-relative name of the detached manifest signature.
pub const MANIFEST_SIG_NAME: &str = "tpk-manifest.json.minisig";

/// Hard ceiling on container entries. A real pack is bounded by its content
/// tree; anything past this is a resource-exhaustion attempt.
pub const MAX_CONTAINER_ENTRIES: usize = 65_536;

/// A pack whose bytes have been located but whose signature is unchecked.
///
/// The only thing you can do with one is [`verify`](Self::verify).
pub struct UnverifiedPack {
    archive: zip::ZipArchive<File>,
    manifest_bytes: Vec<u8>,
    manifest_sha256: Sha256Hex,
    signature: Option<String>,
}

impl std::fmt::Debug for UnverifiedPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnverifiedPack")
            .field("manifest_sha256", &self.manifest_sha256)
            .field("has_detached_signature", &self.signature.is_some())
            .finish()
    }
}

impl UnverifiedPack {
    /// Open a `.tpk` file and read its manifest bytes.
    ///
    /// Structural checks that do not need a signature happen here: the entry
    /// name whitelist, duplicate names, the entry count ceiling, and a non-empty
    /// archive comment.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Io`] when the file cannot be read and
    /// [`FormatError::Spec`] for any structural violation.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        Self::from_reader(file)
    }

    fn from_reader(reader: File) -> Result<Self> {
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|e| FormatError::Spec(format!("not a readable ZIP: {e}")))?;

        if !archive.comment().is_empty() {
            return Err(FormatError::Spec(
                "archive comment must be empty; it is not covered by the manifest".into(),
            ));
        }
        if archive.len() > MAX_CONTAINER_ENTRIES {
            return Err(FormatError::Spec(format!(
                "{} entries exceeds the {MAX_CONTAINER_ENTRIES} limit",
                archive.len()
            )));
        }

        // ZIP permits duplicate names; `by_name` then silently picks one of
        // them, which is the classic signed-one-thing/read-another split.
        let mut names = std::collections::HashSet::new();
        for i in 0..archive.len() {
            let name = archive
                .by_index_raw(i)
                .map_err(|e| FormatError::Spec(format!("unreadable entry {i}: {e}")))?
                .name()
                .to_string();
            if !is_allowed_entry_name(&name) {
                return Err(FormatError::Spec(format!(
                    "illegal container entry {name:?}"
                )));
            }
            if !names.insert(name.clone()) {
                return Err(FormatError::Spec(format!(
                    "duplicate container entry {name:?}"
                )));
            }
        }

        let manifest_bytes = read_whole_entry(&mut archive, MANIFEST_NAME)?;
        let manifest_sha256 = sha256_hex(&manifest_bytes);
        let signature = match read_whole_entry(&mut archive, MANIFEST_SIG_NAME) {
            Ok(bytes) => Some(
                String::from_utf8(bytes)
                    .map_err(|e| FormatError::Signature(format!("signature is not UTF-8: {e}")))?,
            ),
            Err(FormatError::Spec(_)) => None,
            Err(e) => return Err(e),
        };

        Ok(Self {
            archive,
            manifest_bytes,
            manifest_sha256,
            signature,
        })
    }

    /// The exact manifest bytes the signature covers.
    pub fn raw_manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// SHA-256 of the manifest bytes, used by child packs' `parent` links.
    pub fn manifest_sha256(&self) -> Sha256Hex {
        self.manifest_sha256
    }

    /// The embedded detached signature, if the pack carries one.
    pub fn detached_signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }

    /// Verify the manifest signature and parse it.
    ///
    /// `signature` overrides any signature embedded in the container — channel
    /// manifests carry the signature out of band.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Signature`] when no signature is available or none
    /// verifies, and propagates [`PackManifest::parse`] failures. Also returns
    /// [`FormatError::Spec`] when the manifest references a blob the container
    /// does not hold, or the container holds a blob nothing references.
    pub fn verify(
        self,
        trust: &TrustStore,
        signature: Option<&str>,
        declared_epoch: u32,
        min_epoch: u32,
    ) -> Result<VerifiedPack> {
        let signature = signature
            .or(self.signature.as_deref())
            .ok_or_else(|| FormatError::Signature("pack carries no signature".into()))?;
        trust.verify(&self.manifest_bytes, signature, declared_epoch, min_epoch)?;

        let manifest = PackManifest::parse(&self.manifest_bytes)?;

        let referenced: std::collections::HashSet<&str> = manifest
            .entries
            .iter()
            .filter_map(|e| e.blob.as_deref())
            .collect();
        let mut present = std::collections::HashSet::new();
        for i in 0..self.archive.len() {
            let name = self
                .archive
                .name_for_index(i)
                .ok_or_else(|| FormatError::Spec(format!("unreadable entry {i}")))?;
            if name.starts_with("blobs/") {
                present.insert(name.to_string());
            }
        }
        for blob in &referenced {
            if !present.contains(*blob) {
                return Err(FormatError::Spec(format!("missing blob {blob:?}")));
            }
        }
        for blob in &present {
            if !referenced.contains(blob.as_str()) {
                // Unreferenced blobs are covered by the pack's sha256 but by
                // nothing in the manifest — a free-ride smuggling channel.
                return Err(FormatError::Spec(format!("unreferenced blob {blob:?}")));
            }
        }

        Ok(VerifiedPack {
            archive: self.archive,
            manifest,
            manifest_sha256: self.manifest_sha256,
        })
    }
}

/// A pack whose manifest signature verified against a trusted key.
pub struct VerifiedPack {
    archive: zip::ZipArchive<File>,
    manifest: PackManifest,
    manifest_sha256: Sha256Hex,
}

impl std::fmt::Debug for VerifiedPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedPack")
            .field("id", &self.manifest.id)
            .field("kind", &self.manifest.kind)
            .field("version_code", &self.manifest.version_code)
            .field("entries", &self.manifest.entries.len())
            .finish()
    }
}

impl VerifiedPack {
    /// The verified manifest.
    pub fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// SHA-256 of the manifest bytes.
    pub fn manifest_sha256(&self) -> Sha256Hex {
        self.manifest_sha256
    }

    /// Read and decode an entry's blob.
    ///
    /// For `full` entries the result is the file content, checked against
    /// `sha256`. For `delta` entries it is the bsdiff control stream; the
    /// reconstructed content is checked after patching, by the resolver.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Hash`] when the raw blob or the decoded content
    /// does not match its digest, and [`FormatError::Spec`] when the entry has
    /// no blob or decoding overruns the declared size.
    pub fn read_blob(&mut self, entry: &Entry) -> Result<Vec<u8>> {
        let blob = entry
            .blob
            .as_deref()
            .ok_or_else(|| FormatError::Spec(format!("entry {} has no blob", entry.path)))?;
        let blob_sha256 = entry
            .blob_sha256
            .ok_or_else(|| FormatError::Spec(format!("entry {} has no blob_sha256", entry.path)))?;
        let blob_size = entry
            .blob_size
            .ok_or_else(|| FormatError::Spec(format!("entry {} has no blob_size", entry.path)))?;

        let raw = read_whole_entry(&mut self.archive, blob)?;
        if raw.len() as u64 != blob_size {
            return Err(FormatError::Hash(format!(
                "blob {blob:?} is {} bytes, manifest says {blob_size}",
                raw.len()
            )));
        }
        // Checked before decoding: the decoder must never see bytes that are
        // not the ones the manifest was signed over.
        verify_sha256(&raw, &blob_sha256)?;

        let declared_size = entry
            .size
            .ok_or_else(|| FormatError::Spec(format!("entry {} has no size", entry.path)))?;
        let encoding = entry
            .encoding
            .ok_or_else(|| FormatError::Spec(format!("entry {} has no encoding", entry.path)))?;

        let decoded = match encoding {
            Encoding::Identity => raw,
            // For a delta the decoded bytes are the patch stream, whose length
            // is unrelated to `size`; bound it by the compressed length instead,
            // which is what a zstd bomb would have to inflate past.
            Encoding::Zstd => decode_zstd(&raw, declared_size)?,
            Encoding::ZstdBsdiff => decode_zstd(&raw, max_patch_len(blob_size))?,
        };

        if entry.op == Op::Full {
            let expected = entry
                .sha256
                .ok_or_else(|| FormatError::Spec(format!("entry {} has no sha256", entry.path)))?;
            verify_sha256(&decoded, &expected)?;
        }
        Ok(decoded)
    }
}

/// How far a bsdiff patch stream may expand past its compressed size.
///
/// bsdiff control streams compress well but not without bound; 64x plus a
/// constant covers real packs while keeping a decompression bomb finite.
fn max_patch_len(blob_size: u64) -> u64 {
    blob_size.saturating_mul(64).saturating_add(1 << 20)
}

fn decode_zstd(raw: &[u8], limit: u64) -> Result<Vec<u8>> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(raw)
        .map_err(|e| FormatError::Spec(format!("not a zstd frame: {e}")))?;
    // limit + 1 so an over-long stream is detected rather than silently cut.
    let mut out = Vec::new();
    let read = std::io::Read::take(&mut decoder, limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| FormatError::Spec(format!("zstd decode failed: {e}")))?;
    if read as u64 > limit {
        return Err(FormatError::Spec(format!(
            "zstd stream expands past its declared {limit} byte limit"
        )));
    }
    Ok(out)
}

fn read_whole_entry<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Vec<u8>> {
    let mut entry = archive
        .by_name(name)
        .map_err(|e| FormatError::Spec(format!("missing container entry {name:?}: {e}")))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

/// The container may only hold the manifest, its signature, and blobs whose
/// names are their own digests.
fn is_allowed_entry_name(name: &str) -> bool {
    if name == MANIFEST_NAME || name == MANIFEST_SIG_NAME {
        return true;
    }
    let Some(rest) = name.strip_prefix("blobs/") else {
        return false;
    };
    let stem = rest.strip_suffix(".zst").unwrap_or(rest);
    stem.len() == 64
        && stem
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_name_whitelist() {
        let hex = "a".repeat(64);
        assert!(is_allowed_entry_name(MANIFEST_NAME));
        assert!(is_allowed_entry_name(MANIFEST_SIG_NAME));
        assert!(is_allowed_entry_name(&format!("blobs/{hex}")));
        assert!(is_allowed_entry_name(&format!("blobs/{hex}.zst")));

        for bad in [
            "README.md",
            "blobs/",
            "blobs/short",
            "blobs/../escape",
            &format!("blobs/{}", "A".repeat(64)),
            &format!("blobs/{hex}.exe"),
            &format!("nested/blobs/{hex}"),
            &format!("blobs/{hex}/inner"),
        ] {
            assert!(!is_allowed_entry_name(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn patch_limit_is_finite_and_saturating() {
        assert_eq!(max_patch_len(0), 1 << 20);
        assert_eq!(max_patch_len(1000), 64_000 + (1 << 20));
        // No overflow panic at the top of the range.
        assert_eq!(max_patch_len(u64::MAX), u64::MAX);
    }

    #[test]
    fn zstd_decode_rejects_garbage() {
        assert!(decode_zstd(b"not a zstd frame at all", 1024).is_err());
    }

    #[test]
    fn zstd_decode_enforces_the_limit() {
        // A frame that legitimately decodes to more than we allow must error
        // rather than return truncated content.
        let payload = vec![7u8; 4096];
        let frame = zstd_frame(&payload);
        assert_eq!(decode_zstd(&frame, 4096).unwrap(), payload);
        let err = decode_zstd(&frame, 100).unwrap_err().to_string();
        assert!(err.contains("expands past"), "{err}");
    }

    /// Build a real zstd frame without pulling in an encoder: store a single
    /// raw block. Frame header: magic, FHD with single-segment + no checksum,
    /// then the window/content size byte.
    fn zstd_frame(payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 0x1_0000, "test helper handles small inputs");
        let mut out = vec![0x28, 0xB5, 0x2F, 0xFD];
        // FHD: single segment (0x20), frame content size field size = 1 (0x00)
        // means the size byte holds len-0 for values < 256; use the 2-byte form.
        out.push(0x20 | 0x40); // single segment + FCS field size code 1 (2 bytes)
        let fcs = (payload.len() as u16).wrapping_sub(256);
        out.extend_from_slice(&fcs.to_le_bytes());
        // Block header: last-block bit, block type 0 (raw) in bits 1-2, size << 3
        let header = 1u32 | ((payload.len() as u32) << 3);
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(payload);
        out
    }
}
