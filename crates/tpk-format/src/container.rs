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

/// Ceiling on `tpk-manifest.json`, read before any signature check. Matches the
/// client's limit on channel manifests.
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
/// Ceiling on the detached signature entry; a minisign signature is a few
/// hundred bytes.
const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;
/// Largest up-front allocation for an entry read.
const PREALLOC_CAP: u64 = 8 * 1024 * 1024;

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

        let manifest_bytes = read_bounded_entry(&mut archive, MANIFEST_NAME, MAX_MANIFEST_BYTES)?;
        let manifest_sha256 = sha256_hex(&manifest_bytes);
        let signature = if archive.file_names().any(|name| name == MANIFEST_SIG_NAME) {
            let bytes = read_bounded_entry(&mut archive, MANIFEST_SIG_NAME, MAX_SIGNATURE_BYTES)?;
            Some(
                String::from_utf8(bytes)
                    .map_err(|e| FormatError::Signature(format!("signature is not UTF-8: {e}")))?,
            )
        } else {
            None
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
    /// A pack carries no key epoch of its own, so any trusted key whose epoch is
    /// at or above `min_epoch` is accepted (see [`TrustStore::verify_at_or_above`]).
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
        min_epoch: u32,
    ) -> Result<VerifiedPack> {
        let signature = signature
            .or(self.signature.as_deref())
            .ok_or_else(|| FormatError::Signature("pack carries no signature".into()))?;
        trust.verify_at_or_above(&self.manifest_bytes, signature, min_epoch)?;

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

#[cfg(feature = "pack")]
impl UnverifiedPack {
    /// Read a pack's entries **without** checking its signature.
    ///
    /// Gated behind `pack`, which only build tooling enables — the runtime
    /// cannot reach this even by mistake. It exists so `tpk pack` can diff
    /// against a parent artefact it just produced itself, which is not a trust
    /// boundary. Per-blob hashes are still enforced by [`VerifiedPack::read_blob`].
    pub fn into_local_reader(self) -> Result<VerifiedPack> {
        let manifest = PackManifest::parse(&self.manifest_bytes)?;
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
        self.read_blob_bounded(entry, u64::MAX)
    }

    /// [`read_blob`](Self::read_blob) with a caller-imposed ceiling on the
    /// decoded length.
    ///
    /// Decoding stops at `max_decoded` or at the entry's own limit (`size` for
    /// a zstd `full` blob, a multiple of `blob_size` for a delta stream),
    /// whichever is lower. That lets a caller with a running byte budget refuse
    /// a delta stream mid-decode instead of learning its length only after the
    /// whole stream is in memory. An identity blob is not decoded; it is refused
    /// after the read if it is longer than `max_decoded`.
    ///
    /// # Errors
    ///
    /// As [`read_blob`](Self::read_blob). Exceeding `max_decoded` is also
    /// [`FormatError::Spec`], but only when `max_decoded` was the binding
    /// ceiling is the message prefixed with `decode budget of {max_decoded}
    /// bytes exceeded: `, so a caller can tell its own budget apart from a
    /// blob overrunning its declared size.
    pub fn read_blob_bounded(&mut self, entry: &Entry, max_decoded: u64) -> Result<Vec<u8>> {
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

        let raw = read_whole_entry(&mut self.archive, blob, blob_size)?;
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
            Encoding::Identity if blob_size > max_decoded => {
                return Err(FormatError::Spec(format!(
                    "decode budget of {max_decoded} bytes exceeded: blob {blob:?} is {blob_size} bytes"
                )));
            }
            Encoding::Identity => raw,
            // For a delta the decoded bytes are the patch stream, whose length
            // is unrelated to `size`; bound it by the compressed length instead,
            // which is what a zstd bomb would have to inflate past.
            Encoding::Zstd => decode_zstd(&raw, declared_size, max_decoded)?,
            Encoding::ZstdBsdiff => decode_zstd(&raw, max_patch_len(blob_size), max_decoded)?,
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

/// Decode at most `min(own_limit, max_decoded)` bytes, saying which of the two
/// an over-long stream ran into.
fn decode_zstd(raw: &[u8], own_limit: u64, max_decoded: u64) -> Result<Vec<u8>> {
    let limit = own_limit.min(max_decoded);
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(raw)
        .map_err(|e| FormatError::Spec(format!("not a zstd frame: {e}")))?;
    // limit + 1 so an over-long stream is detected rather than silently cut.
    let mut out = Vec::new();
    let read = std::io::Read::take(&mut decoder, limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| FormatError::Spec(format!("zstd decode failed: {e}")))?;
    if read as u64 > limit {
        return Err(FormatError::Spec(if max_decoded < own_limit {
            // The stream may still be within its own limit; only the budget is known to be hit.
            format!("decode budget of {max_decoded} bytes exceeded: zstd stream is longer")
        } else {
            format!("zstd stream expands past its declared {own_limit} byte limit")
        }));
    }
    Ok(out)
}

/// Read one entry, stopping one byte past `limit` so the caller can tell an
/// overrun apart. The size in the ZIP header is attacker-controlled, so it is
/// neither trusted for allocation nor as a cap.
fn read_whole_entry<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let entry = archive
        .by_name(name)
        .map_err(|e| FormatError::Spec(format!("missing container entry {name:?}: {e}")))?;
    // Only a small preallocation is trusted; beyond it the buffer grows with
    // the bytes that actually arrive.
    let mut buf = Vec::with_capacity(entry.size().min(limit).min(PREALLOC_CAP) as usize);
    entry.take(limit.saturating_add(1)).read_to_end(&mut buf)?;
    Ok(buf)
}

/// [`read_whole_entry`] for entries with a fixed ceiling rather than a signed size.
fn read_bounded_entry<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let buf = read_whole_entry(archive, name, limit)?;
    if buf.len() as u64 > limit {
        return Err(FormatError::Spec(format!(
            "container entry {name:?} exceeds {limit} bytes"
        )));
    }
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
    fn entry_reads_are_bounded_regardless_of_the_zip_header() {
        use std::io::{Cursor, Write};
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file(MANIFEST_NAME, stored).unwrap();
        zip.write_all(&[b' '; 64]).unwrap();
        let mut archive = zip::ZipArchive::new(zip.finish().unwrap()).unwrap();

        assert_eq!(
            read_bounded_entry(&mut archive, MANIFEST_NAME, 64)
                .unwrap()
                .len(),
            64
        );
        // A blob read stops just past its signed size and lets the caller report it.
        assert_eq!(
            read_whole_entry(&mut archive, MANIFEST_NAME, 10)
                .unwrap()
                .len(),
            11
        );
        let err = read_bounded_entry(&mut archive, MANIFEST_NAME, 63)
            .unwrap_err()
            .to_string();
        assert!(err.contains("exceeds 63 bytes"), "{err}");
    }

    #[test]
    fn present_oversized_signature_is_not_treated_as_missing() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized-signature.tpk");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file(MANIFEST_NAME, stored).unwrap();
        zip.write_all(b"{}").unwrap();
        zip.start_file(MANIFEST_SIG_NAME, stored).unwrap();
        zip.write_all(&vec![b'x'; MAX_SIGNATURE_BYTES as usize + 1])
            .unwrap();
        zip.finish().unwrap();

        let err = UnverifiedPack::open(&path).unwrap_err().to_string();
        assert!(err.contains("exceeds 65536 bytes"), "{err}");
    }

    #[test]
    fn zstd_decode_rejects_garbage() {
        assert!(decode_zstd(b"not a zstd frame at all", 1024, u64::MAX).is_err());
    }

    #[test]
    fn zstd_decode_enforces_the_limit() {
        // A frame that legitimately decodes to more than we allow must error
        // rather than return truncated content.
        let payload = vec![7u8; 4096];
        let frame = zstd_frame(&payload);
        assert_eq!(decode_zstd(&frame, 4096, u64::MAX).unwrap(), payload);
        let err = decode_zstd(&frame, 100, u64::MAX).unwrap_err().to_string();
        assert!(err.contains("expands past"), "{err}");
    }

    #[test]
    fn zstd_decode_names_the_ceiling_it_hit() {
        let frame = zstd_frame(&[7u8; 4096]);
        // Caller budget below the declared size: the budget is to blame.
        let Err(FormatError::Spec(err)) = decode_zstd(&frame, 4096, 100) else {
            panic!("over budget must be a spec error");
        };
        assert!(
            err.starts_with("decode budget of 100 bytes exceeded: "),
            "{err}"
        );
        // Stream longer than its declared size, budget unlimited: the blob is.
        let err = decode_zstd(&frame, 100, u64::MAX).unwrap_err().to_string();
        assert!(!err.contains("decode budget"), "{err}");
        assert!(err.contains("declared 100 byte limit"), "{err}");
        // Both ceilings exceeded, declared one lower: still the blob's fault.
        let err = decode_zstd(&frame, 100, 200).unwrap_err().to_string();
        assert!(!err.contains("decode budget"), "{err}");
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
