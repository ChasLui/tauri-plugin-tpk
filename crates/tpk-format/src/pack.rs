//! Building `.tpk` containers (`pack` feature). Build machines only.
//!
//! Output is **deterministic**: the same inputs produce a byte-identical file.
//! That is not a nicety — the blacklist matches packs by `sha256`, so a
//! container whose bytes shift between CI runs silently escapes it, and a
//! channel manifest's `sha256` stops being reproducible at the same time.
//!
//! Delta streams are supplied by the caller rather than generated here: bsdiff
//! lives in `tpk-delta`, which this crate deliberately does not depend on.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::error::{FormatError, Result};
use crate::manifest::{
    Encoding, Entry, Op, PackId, PackKind, PackManifest, PackPolicies, ParentRef, Sha256Hex,
    SPEC_TAG,
};
use crate::path::PackPath;
use crate::secret::SecretKey;
use crate::sign::sha256_hex;

/// zstd level used for every blob. Fixed so output stays reproducible.
const ZSTD_LEVEL: i32 = 19;

/// Below this, compression is not worth the decode cost at runtime.
const MIN_COMPRESS_BYTES: usize = 4096;

/// What went into a built pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSummary {
    /// SHA-256 of the finished `.tpk` file.
    pub file_sha256: Sha256Hex,
    /// SHA-256 of the manifest bytes.
    pub manifest_sha256: Sha256Hex,
    /// Size of the finished file.
    pub file_size: u64,
    /// Number of `full` entries.
    pub full_entries: usize,
    /// Number of `delta` entries.
    pub delta_entries: usize,
    /// Number of tombstones.
    pub delete_entries: usize,
}

/// Accumulates entries and writes a signed container.
pub struct PackBuilder {
    kind: PackKind,
    id: PackId,
    version: semver::Version,
    version_code: u64,
    min_shell: Option<semver::Version>,
    max_shell: Option<semver::Version>,
    parent: Option<ParentRef>,
    created_at: String,
    channel: Option<String>,
    /// Keyed by path so entries come out in the sorted order the format wants.
    entries: BTreeMap<String, (Entry, Option<Vec<u8>>)>,
}

impl PackBuilder {
    /// Start a pack.
    ///
    /// `created_at` must be RFC 3339. It is an input rather than `now()` so a
    /// rebuild of the same content produces the same bytes.
    pub fn new(
        kind: PackKind,
        id: PackId,
        version: semver::Version,
        version_code: u64,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            id,
            version,
            version_code,
            min_shell: None,
            max_shell: None,
            parent: None,
            created_at: created_at.into(),
            channel: None,
            entries: BTreeMap::new(),
        }
    }

    /// Lowest shell version this pack supports.
    #[must_use]
    pub fn min_shell(mut self, v: semver::Version) -> Self {
        self.min_shell = Some(v);
        self
    }

    /// Highest shell version this pack supports.
    ///
    /// Leave unset unless a specific incompatibility is known: once a newer
    /// shell ships, every pack with a `max_shell` below it goes dark and those
    /// users silently fall back to the embedded assets.
    #[must_use]
    pub fn max_shell(mut self, v: semver::Version) -> Self {
        self.max_shell = Some(v);
        self
    }

    /// The pack this one patches. Required for `patch`.
    #[must_use]
    pub fn parent(mut self, parent: ParentRef) -> Self {
        self.parent = Some(parent);
        self
    }

    /// The channel this pack was built for.
    #[must_use]
    pub fn channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    /// Add a complete file.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Path`] for an invalid path and
    /// [`FormatError::Spec`] if the path was already added.
    pub fn add_full(&mut self, path: &str, content: &[u8]) -> Result<()> {
        let path = PackPath::parse(path)?;
        let content_sha = sha256_hex(content);

        let (blob_bytes, encoding) = if content.len() >= MIN_COMPRESS_BYTES {
            let compressed = zstd::encode_all(content, ZSTD_LEVEL)
                .map_err(|e| FormatError::Spec(format!("zstd encode failed: {e}")))?;
            // Compression that does not pay for itself just costs decode time.
            if compressed.len() < content.len() {
                (compressed, Encoding::Zstd)
            } else {
                (content.to_vec(), Encoding::Identity)
            }
        } else {
            (content.to_vec(), Encoding::Identity)
        };

        let blob_sha = sha256_hex(&blob_bytes);
        let entry = Entry {
            path: path.clone(),
            op: Op::Full,
            size: Some(content.len() as u64),
            sha256: Some(content_sha),
            blob: Some(blob_name(&blob_sha, encoding)),
            blob_sha256: Some(blob_sha),
            blob_size: Some(blob_bytes.len() as u64),
            encoding: Some(encoding),
            delta_base_sha256: None,
        };
        self.insert(path, entry, Some(blob_bytes))
    }

    /// Add a delta entry from a caller-generated bsdiff stream.
    ///
    /// `base_sha256` is what the layers below must resolve to, and
    /// `result_content` is what applying the patch must produce — both end up in
    /// the manifest so the runtime can check each independently.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Path`] for an invalid path and
    /// [`FormatError::Spec`] if the path was already added.
    pub fn add_delta(
        &mut self,
        path: &str,
        patch_stream: &[u8],
        result_content: &[u8],
        base_sha256: Sha256Hex,
    ) -> Result<()> {
        let path = PackPath::parse(path)?;
        // Always compressed: a raw bsdiff stream is roughly the size of the file
        // it rebuilds and is mostly zeros, so skipping zstd here would make
        // deltas pointless.
        let blob_bytes = zstd::encode_all(patch_stream, ZSTD_LEVEL)
            .map_err(|e| FormatError::Spec(format!("zstd encode failed: {e}")))?;
        let blob_sha = sha256_hex(&blob_bytes);

        let entry = Entry {
            path: path.clone(),
            op: Op::Delta,
            size: Some(result_content.len() as u64),
            sha256: Some(sha256_hex(result_content)),
            blob: Some(blob_name(&blob_sha, Encoding::ZstdBsdiff)),
            blob_sha256: Some(blob_sha),
            blob_size: Some(blob_bytes.len() as u64),
            encoding: Some(Encoding::ZstdBsdiff),
            delta_base_sha256: Some(base_sha256),
        };
        self.insert(path, entry, Some(blob_bytes))
    }

    /// Add a tombstone hiding a path present in the layers below.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Path`] for an invalid path and
    /// [`FormatError::Spec`] if the path was already added.
    pub fn add_delete(&mut self, path: &str) -> Result<()> {
        let path = PackPath::parse(path)?;
        let entry = Entry {
            path: path.clone(),
            op: Op::Delete,
            size: None,
            sha256: None,
            blob: None,
            blob_sha256: None,
            blob_size: None,
            encoding: None,
            delta_base_sha256: None,
        };
        self.insert(path, entry, None)
    }

    fn insert(&mut self, path: PackPath, entry: Entry, blob: Option<Vec<u8>>) -> Result<()> {
        if self
            .entries
            .insert(path.as_str().to_string(), (entry, blob))
            .is_some()
        {
            return Err(FormatError::Spec(format!("duplicate path {path}")));
        }
        Ok(())
    }

    /// How many entries have been added.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been added yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Write the signed container to `out`.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Spec`] when the resulting manifest would be
    /// invalid (which is checked by re-parsing it, so a builder bug cannot
    /// produce a pack the runtime would reject) and [`FormatError::Io`] on
    /// write failures.
    pub fn build(self, key: &SecretKey, out: &Path) -> Result<PackSummary> {
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut blobs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let (mut full, mut delta, mut delete) = (0usize, 0usize, 0usize);

        for (_, (entry, blob)) in self.entries {
            match entry.op {
                Op::Full => full += 1,
                Op::Delta => delta += 1,
                Op::Delete => delete += 1,
            }
            if let (Some(name), Some(bytes)) = (entry.blob.clone(), blob) {
                blobs.insert(name, bytes);
            }
            entries.push(entry);
        }

        let manifest = PackManifest {
            spec: SPEC_TAG.to_string(),
            kind: self.kind,
            id: self.id,
            version: self.version,
            version_code: self.version_code,
            min_shell: self.min_shell,
            max_shell: self.max_shell,
            parent: self.parent,
            created_at: self.created_at,
            channel: self.channel,
            policies: PackPolicies {
                trusted: self.kind != PackKind::Mod,
                ..PackPolicies::default()
            },
            entries,
        };

        // serde_json is deterministic for a struct with fixed field order.
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|e| FormatError::Spec(format!("cannot serialize manifest: {e}")))?;

        // Re-parse rather than trust the builder: a pack that the runtime would
        // reject must fail here, on the build machine, not on a user's device.
        PackManifest::parse(&manifest_bytes)?;

        let manifest_sha256 = sha256_hex(&manifest_bytes);
        let signature = key.sign(
            &manifest_bytes,
            &format!("tpk {} {}", manifest.id, manifest.version_code),
            "signature from tpk",
        );

        let file = std::fs::File::create(out)?;
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));

        for (name, bytes) in [
            (crate::container::MANIFEST_NAME, manifest_bytes.as_slice()),
            (crate::container::MANIFEST_SIG_NAME, signature.as_bytes()),
        ] {
            zip.start_file(name, deterministic_options())
                .map_err(zip_err)?;
            zip.write_all(bytes)?;
        }
        // BTreeMap iteration is sorted, so blob order is stable too.
        for (name, bytes) in &blobs {
            zip.start_file(name, deterministic_options())
                .map_err(zip_err)?;
            zip.write_all(bytes)?;
        }
        zip.finish().map_err(zip_err)?;

        let written = std::fs::read(out)?;
        Ok(PackSummary {
            file_sha256: sha256_hex(&written),
            manifest_sha256,
            file_size: written.len() as u64,
            full_entries: full,
            delta_entries: delta,
            delete_entries: delete,
        })
    }
}

/// Compressed size a blob would have, for deciding whether a delta pays off.
///
/// Lives here so `zstd` stays confined to this crate — `deny.toml` enforces
/// that boundary, and routing the estimate through the same encoder also means
/// the estimate matches what the packer will actually write.
///
/// # Errors
///
/// Returns [`FormatError::Spec`] if the encoder fails.
pub fn compressed_size(bytes: &[u8]) -> Result<usize> {
    zstd::encode_all(bytes, ZSTD_LEVEL)
        .map(|v| v.len())
        .map_err(|e| FormatError::Spec(format!("zstd encode failed: {e}")))
}

/// Fixed timestamp, fixed host system and no extra fields, so two runs produce
/// identical bytes — including two runs on different operating systems.
fn deterministic_options() -> zip::write::FileOptions<'static, ()> {
    zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(
            zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).expect("valid DOS epoch"),
        )
        // Without this the `zip` crate stamps the *building* host into the high
        // byte of each central directory header's "version made by": 3 on Unix,
        // 0 on Windows. Everything else about the archive matches, so a pack
        // built on Windows differed from the same pack built on Linux by
        // exactly one byte per entry — enough to change the file's SHA-256,
        // which is what the channel manifest publishes and what the blacklist
        // matches on.
        .system(zip::System::Unix)
        // `system` alone is not enough: the external attributes are derived
        // from the host too, so pin the mode rather than inheriting an umask.
        .unix_permissions(0o644)
}

fn blob_name(sha: &Sha256Hex, encoding: Encoding) -> String {
    match encoding {
        Encoding::Identity => format!("blobs/{sha}"),
        Encoding::Zstd | Encoding::ZstdBsdiff => format!("blobs/{sha}.zst"),
    }
}

fn zip_err(e: zip::result::ZipError) -> FormatError {
    FormatError::Io(std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::UnverifiedPack;
    use crate::sign::{TrustStore, TrustedKey};

    fn builder() -> PackBuilder {
        PackBuilder::new(
            PackKind::Base,
            PackId::parse("core").unwrap(),
            "1.0.0".parse().unwrap(),
            10000,
            "2026-09-11T15:00:00Z",
        )
    }

    fn trust(key: &SecretKey) -> TrustStore {
        TrustStore::new(&[TrustedKey {
            key: key.public_key_base64(),
            epoch: 1,
        }])
        .unwrap()
    }

    #[test]
    fn a_built_pack_verifies_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("base-core-1.0.0.tpk");
        let key = SecretKey::generate();

        let small = b"<!doctype html><title>hi</title>";
        let large = b"body { color: rebeccapurple; }\n".repeat(500);

        let mut b = builder();
        b.add_full("/index.html", small).unwrap();
        b.add_full("/app.css", &large).unwrap();
        b.add_delete("/old.js").unwrap();
        let summary = b.build(&key, &out).unwrap();

        assert_eq!(summary.full_entries, 2);
        assert_eq!(summary.delete_entries, 1);
        assert_eq!(summary.delta_entries, 0);

        let mut pack = UnverifiedPack::open(&out)
            .unwrap()
            .verify(&trust(&key), None, 1, 1)
            .unwrap();
        assert_eq!(pack.manifest_sha256(), summary.manifest_sha256);
        assert_eq!(pack.manifest().entries.len(), 3);

        let by_path: std::collections::HashMap<_, _> = pack
            .manifest()
            .entries
            .iter()
            .map(|e| (e.path.as_str().to_string(), e.clone()))
            .collect();

        assert_eq!(pack.read_blob(&by_path["/index.html"]).unwrap(), small);
        assert_eq!(pack.read_blob(&by_path["/app.css"]).unwrap(), large);
        assert_eq!(by_path["/old.js"].op, Op::Delete);
    }

    #[test]
    fn entries_come_out_sorted_regardless_of_insertion_order() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("p.tpk");
        let key = SecretKey::generate();

        let mut b = builder();
        for path in ["/z.js", "/a.js", "/m/nested.js", "/b.css"] {
            b.add_full(path, path.as_bytes()).unwrap();
        }
        b.build(&key, &out).unwrap();

        let pack = UnverifiedPack::open(&out)
            .unwrap()
            .verify(&trust(&key), None, 1, 1)
            .unwrap();
        let paths: Vec<_> = pack
            .manifest()
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(paths, ["/a.js", "/b.css", "/m/nested.js", "/z.js"]);
    }

    #[test]
    fn the_same_inputs_produce_byte_identical_packs() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();

        let build_one = |name: &str| {
            let out = dir.path().join(name);
            let mut b = builder();
            b.add_full("/index.html", b"<!doctype html>").unwrap();
            b.add_full("/app.css", &b"a{}".repeat(4000)).unwrap();
            b.add_delete("/gone.js").unwrap();
            let summary = b.build(&key, &out).unwrap();
            (summary, std::fs::read(&out).unwrap())
        };

        let (first, first_bytes) = build_one("one.tpk");
        let (second, second_bytes) = build_one("two.tpk");

        // The blacklist matches on this hash, and channel manifests publish it.
        assert_eq!(
            first.file_sha256, second.file_sha256,
            "packing the same input twice must produce the same sha256"
        );
        assert_eq!(first_bytes, second_bytes);
    }

    #[test]
    fn small_files_are_stored_and_large_compressible_ones_are_zstd() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("p.tpk");
        let key = SecretKey::generate();

        let mut b = builder();
        b.add_full("/tiny.txt", b"small").unwrap();
        b.add_full("/big.css", &b"a { color: red; }\n".repeat(1000))
            .unwrap();
        b.build(&key, &out).unwrap();

        let pack = UnverifiedPack::open(&out)
            .unwrap()
            .verify(&trust(&key), None, 1, 1)
            .unwrap();
        let by_path: std::collections::HashMap<_, _> = pack
            .manifest()
            .entries
            .iter()
            .map(|e| (e.path.as_str().to_string(), e.clone()))
            .collect();

        assert_eq!(by_path["/tiny.txt"].encoding, Some(Encoding::Identity));
        assert_eq!(by_path["/big.css"].encoding, Some(Encoding::Zstd));
        assert!(
            by_path["/big.css"].blob_size.unwrap() < by_path["/big.css"].size.unwrap(),
            "compression should have paid for itself"
        );
    }

    #[test]
    fn incompressible_data_falls_back_to_identity() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("p.tpk");
        let key = SecretKey::generate();

        // xorshift64: genuinely incompressible, unlike a multiplicative hash
        // whose high bits keep enough structure for zstd to exploit.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let noise: Vec<u8> = (0..20_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let mut b = builder();
        b.add_full("/noise.bin", &noise).unwrap();
        b.build(&key, &out).unwrap();

        let pack = UnverifiedPack::open(&out)
            .unwrap()
            .verify(&trust(&key), None, 1, 1)
            .unwrap();
        assert_eq!(
            pack.manifest().entries[0].encoding,
            Some(Encoding::Identity)
        );
    }

    #[test]
    fn a_delta_entry_round_trips_through_the_container() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("patch.tpk");
        let key = SecretKey::generate();

        let base = b"the original file contents".repeat(200);
        let result = b"the modified file contents".repeat(200);
        // Stand-in for a bsdiff stream; the container does not interpret it.
        let patch_stream = vec![0u8; 4096];

        let mut b = PackBuilder::new(
            PackKind::Patch,
            PackId::parse("core").unwrap(),
            "1.0.1".parse().unwrap(),
            10001,
            "2026-09-11T15:00:00Z",
        )
        .parent(ParentRef {
            id: PackId::parse("core").unwrap(),
            version: "1.0.0".parse().unwrap(),
            version_code: 10000,
            manifest_sha256: sha256_hex(b"pretend parent manifest"),
        });
        b.add_delta("/big.bin", &patch_stream, &result, sha256_hex(&base))
            .unwrap();
        let summary = b.build(&key, &out).unwrap();
        assert_eq!(summary.delta_entries, 1);

        let mut pack = UnverifiedPack::open(&out)
            .unwrap()
            .verify(&trust(&key), None, 1, 1)
            .unwrap();
        let entry = pack.manifest().entries[0].clone();
        assert_eq!(entry.delta_base_sha256, Some(sha256_hex(&base)));
        assert_eq!(entry.encoding, Some(Encoding::ZstdBsdiff));
        // read_blob returns the patch stream, not the result: reconstruction
        // happens in the resolver, against the layers below.
        assert_eq!(pack.read_blob(&entry).unwrap(), patch_stream);
    }

    #[test]
    fn duplicate_paths_are_rejected() {
        let mut b = builder();
        b.add_full("/index.html", b"one").unwrap();
        assert!(b.add_full("/index.html", b"two").is_err());
        assert!(b.add_delete("/index.html").is_err());
    }

    #[test]
    fn invalid_paths_are_rejected() {
        let mut b = builder();
        assert!(b.add_full("../escape", b"x").is_err());
        assert!(b.add_full("/../escape", b"x").is_err());
        assert!(b.add_delete("/.tauri/ipc.js").is_err());
    }

    #[test]
    fn a_patch_without_a_parent_fails_at_build_time() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let mut b = PackBuilder::new(
            PackKind::Patch,
            PackId::parse("core").unwrap(),
            "1.0.1".parse().unwrap(),
            10001,
            "2026-09-11T15:00:00Z",
        );
        b.add_full("/index.html", b"x").unwrap();
        // The builder re-parses its own manifest, so this surfaces here rather
        // than on a user's device.
        assert!(matches!(
            b.build(&key, &dir.path().join("p.tpk")).unwrap_err(),
            FormatError::Parent(_)
        ));
    }

    #[test]
    fn an_empty_pack_fails_at_build_time() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let b = builder();
        assert!(b.is_empty());
        assert!(b.build(&key, &dir.path().join("p.tpk")).is_err());
    }

    #[test]
    fn a_mod_pack_is_marked_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("mod.tpk");
        let key = SecretKey::generate();
        let mut b = PackBuilder::new(
            PackKind::Mod,
            PackId::parse("skin").unwrap(),
            "1.0.0".parse().unwrap(),
            10000,
            "2026-09-11T15:00:00Z",
        );
        b.add_full("/skin.css", b"body{}").unwrap();
        b.build(&key, &out).unwrap();

        let pack = UnverifiedPack::open(&out)
            .unwrap()
            .verify(&trust(&key), None, 1, 1)
            .unwrap();
        assert!(!pack.manifest().policies.trusted);
    }

    #[test]
    fn the_container_records_a_fixed_host_system_not_the_building_one() {
        // The `zip` crate defaults "version made by" to whatever host is
        // building: 3 (Unix) on macOS and Linux, 0 (FAT) on Windows. Every
        // other byte of the archive matched, so a pack built on Windows
        // differed from the same pack built on Linux by one byte per central
        // directory entry — enough to change the file's SHA-256, which is what
        // the channel manifest publishes and what the blacklist matches on.
        //
        // Caught by building the same fixture on macOS and in a Windows 11 VM;
        // this test is the part of that check that fits in CI.
        const Z_UNIX: u8 = 3;

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("host.tpk");
        let key = SecretKey::generate();
        let mut b = builder();
        b.add_full("/index.html", b"<!DOCTYPE html><html></html>")
            .unwrap();
        b.add_full("/a/app.js", b"export const n = 1;").unwrap();
        b.build(&key, &out).unwrap();

        let bytes = std::fs::read(&out).unwrap();
        let mut headers = 0usize;
        // Central directory headers start with PK\x01\x02, then a two-byte
        // "version made by" whose high byte is the host system.
        for i in 0..bytes.len().saturating_sub(6) {
            if &bytes[i..i + 4] == b"PK\x01\x02" {
                headers += 1;
                assert_eq!(
                    bytes[i + 5],
                    Z_UNIX,
                    "central directory header {headers} records host system {} rather than a fixed {Z_UNIX}; \
                     packs built on different operating systems will not be byte-identical",
                    bytes[i + 5],
                );
            }
        }
        assert!(headers >= 3, "expected several entries, found {headers}");
    }
}
