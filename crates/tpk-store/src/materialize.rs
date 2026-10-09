//! Reconstructing delta entries ahead of time.
//!
//! This runs when a revision is staged — after a download, inside an async task
//! that already has a progress indicator on screen — and never during `boot`.
//!
//! Doing it at boot would put an unbounded amount of bsdiff work in front of
//! window creation. On a mid-range phone a patch with a hundred delta entries
//! takes seconds to tens of seconds, and iOS kills a process that has not
//! finished launching in twenty (`0x8badf00d`). That kill is indistinguishable
//! from a crash, so the boot-attempt counter would count it, and after three
//! launches a perfectly good release would be blacklisted for being slow.
//!
//! The results live in the cache root, which the OS may purge. Callers must
//! therefore treat a missing result as ordinary and re-derive it, never as
//! corruption.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tpk_format::container::UnverifiedPack;
use tpk_format::manifest::{Op, Sha256Hex};
use tpk_format::sign::{sha256_hex, TrustStore};
use tpk_resolve::{IndexBuilder, LayerSpec, MaterializedSource, NoMaterialized, Resolver};

use crate::error::{Result, StoreError};
use crate::state::atomic_write_content_addressed;

/// Ceiling on a single reconstructed asset.
///
/// Mobile gets a much lower one: the bytes are copied again on their way
/// through the WebView bridge, and on Android that copy lands on the Dalvik
/// heap. Anything this large also cannot be streamed or range-requested over
/// `tauri://`, so the limit matches what the format can usefully carry anyway.
pub const MAX_ASSET_BYTES: u64 = if cfg!(any(target_os = "ios", target_os = "android")) {
    16 * 1024 * 1024
} else {
    64 * 1024 * 1024
};

/// Reads materialized results back out of the cache directory.
#[derive(Debug, Clone)]
pub struct MaterializedDir {
    dir: PathBuf,
}

impl MaterializedDir {
    /// Point at a directory of results.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory being read.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl MaterializedSource for MaterializedDir {
    fn get(&self, sha256: &Sha256Hex) -> Option<Vec<u8>> {
        let path = self.dir.join(sha256.to_hex());
        let mut file = std::fs::File::open(&path).ok()?.take(MAX_ASSET_BYTES + 1);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        // The cache directory is not a trust boundary — the OS, a backup tool
        // or a user can touch it — so what comes back is checked like anything
        // else. Readers leave bad copies alone; a writer replaces them atomically.
        if bytes.len() as u64 > MAX_ASSET_BYTES || sha256_hex(&bytes) != *sha256 {
            return None;
        }
        Some(bytes)
    }
}

/// Reconstruct every delta entry in `layer`, given the layers beneath it.
///
/// # Errors
///
/// Returns [`StoreError::Delta`] when a base cannot be resolved, its hash does
/// not match `delta_base_sha256`, or the patch does not rebuild the declared
/// content.
pub fn materialize_layer(
    layer: &LayerSpec,
    below: &[LayerSpec],
    trust: &Arc<TrustStore>,
    min_key_epoch: u32,
    out_dir: &Path,
) -> Result<usize> {
    let mut pack = UnverifiedPack::open(&layer.path)?.verify(trust, None, min_key_epoch)?;
    let materialized = MaterializedDir::new(out_dir.to_path_buf());
    // Only entries whose result is absent; when there are none, skip opening
    // and verifying every pack beneath.
    let deltas: Vec<_> = pack
        .manifest()
        .entries
        .iter()
        .filter(|e| e.op == Op::Delta)
        .filter(|e| e.sha256.is_none_or(|sha| materialized.get(&sha).is_none()))
        .cloned()
        .collect();
    if deltas.is_empty() {
        return Ok(0);
    }

    // One resolver for the whole layer rather than one per entry: building it
    // opens and verifies every pack beneath.
    let below_resolver = {
        let mut builder = IndexBuilder::new(Arc::clone(trust), min_key_epoch);
        for spec in below {
            builder.push_layer(spec);
        }
        // No cache: each entry is read once here, and holding megabytes of
        // decoded bases would defeat the point of doing this off the hot path.
        builder.build(0, Box::new(MaterializedDir::new(out_dir.to_path_buf())))
    };

    std::fs::create_dir_all(out_dir)?;
    let mut written = 0usize;

    for entry in &deltas {
        let expected = entry
            .sha256
            .ok_or_else(|| StoreError::Delta(format!("{} has no sha256", entry.path)))?;
        let out_path = out_dir.join(expected.to_hex());
        if materialized.get(&expected).is_some() {
            continue;
        }

        let base_sha = entry
            .delta_base_sha256
            .ok_or_else(|| StoreError::Delta(format!("{} has no delta_base_sha256", entry.path)))?;
        let base = below_resolver.get(entry.path.as_str()).map_err(|e| {
            StoreError::Delta(format!(
                "{} has no base in the layers below: {e:?}",
                entry.path
            ))
        })?;
        if sha256_hex(&base) != base_sha {
            return Err(StoreError::Delta(format!(
                "{} expects a base of {base_sha}, found {}",
                entry.path,
                sha256_hex(&base)
            )));
        }

        let size = entry
            .size
            .ok_or_else(|| StoreError::Delta(format!("{} has no size", entry.path)))?;
        let patch = pack
            .read_blob(entry)
            .map_err(|e| StoreError::Delta(format!("{}: {e}", entry.path)))?;

        let result = tpk_delta::apply(
            &base,
            &patch,
            usize::try_from(size).map_err(|_| {
                StoreError::Delta(format!("{} declares an impossible size", entry.path))
            })?,
            usize::try_from(MAX_ASSET_BYTES).unwrap_or(usize::MAX),
        )
        .map_err(|e| StoreError::Delta(format!("{}: {e}", entry.path)))?;

        if sha256_hex(&result) != expected {
            return Err(StoreError::Delta(format!(
                "{} rebuilt to the wrong content",
                entry.path
            )));
        }
        atomic_write_content_addressed(&out_path, &result)?;
        written += 1;
    }
    Ok(written)
}

/// Materialize a whole stack, lowest layer first, each against the layers before it.
///
/// Used to rebuild results after the OS purged the cache. Layers whose results
/// are all present cost one pack verification each.
///
/// # Errors
///
/// Stops at the first layer [`materialize_layer`] fails on.
pub fn materialize_stack(
    specs: &[LayerSpec],
    trust: &Arc<TrustStore>,
    min_key_epoch: u32,
    out_dir: &Path,
) -> Result<usize> {
    let mut written = 0;
    for (i, spec) in specs.iter().enumerate() {
        written += materialize_layer(spec, &specs[..i], trust, min_key_epoch, out_dir)?;
    }
    Ok(written)
}

/// Whether any delta entry in the resolver's layers lacks a valid result in `dir`.
///
/// Checks content hashes and stops at the first missing or corrupt result. The
/// plugin calls this on its background repair thread, away from window startup.
pub fn missing_materialized(resolver: &Resolver, dir: &Path) -> bool {
    let materialized = MaterializedDir::new(dir.to_path_buf());
    resolver
        .index()
        .layers()
        .iter()
        .flat_map(|layer| layer.manifest.entries.iter())
        .filter(|e| e.op == Op::Delta)
        .filter_map(|e| e.sha256)
        .any(|sha| materialized.get(&sha).is_none())
}

/// Cache of materialized results held in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryMaterialized {
    by_hash: HashMap<Sha256Hex, Vec<u8>>,
}

impl MemoryMaterialized {
    /// Record a result.
    pub fn insert(&mut self, bytes: Vec<u8>) {
        self.by_hash.insert(sha256_hex(&bytes), bytes);
    }
}

impl MaterializedSource for MemoryMaterialized {
    fn get(&self, sha256: &Sha256Hex) -> Option<Vec<u8>> {
        self.by_hash.get(sha256).cloned()
    }
}

/// A source that has nothing.
pub type Empty = NoMaterialized;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mobile_asset_ceiling_is_lower() {
        if cfg!(any(target_os = "ios", target_os = "android")) {
            assert_eq!(MAX_ASSET_BYTES, 16 * 1024 * 1024);
        } else {
            assert_eq!(MAX_ASSET_BYTES, 64 * 1024 * 1024);
        }
    }

    #[test]
    fn reads_back_a_result_by_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"materialized bytes".to_vec();
        let sha = sha256_hex(&content);
        std::fs::write(dir.path().join(sha.to_hex()), &content).unwrap();

        let source = MaterializedDir::new(dir.path());
        assert_eq!(source.get(&sha).unwrap(), content);
    }

    #[test]
    fn a_missing_result_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // The OS purges this directory whenever it likes; absence is ordinary.
        assert!(MaterializedDir::new(dir.path())
            .get(&sha256_hex(b"absent"))
            .is_none());
    }

    #[test]
    fn a_tampered_result_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"the real content";
        let sha = sha256_hex(content);
        let path = dir.path().join(sha.to_hex());
        std::fs::write(&path, b"something else").unwrap();
        let source = MaterializedDir::new(dir.path());
        assert!(source.get(&sha).is_none());
        assert_eq!(std::fs::read(&path).unwrap().as_slice(), b"something else");
        atomic_write_content_addressed(&path, content).unwrap();
        assert_eq!(source.get(&sha).unwrap().as_slice(), content);
    }

    #[test]
    fn an_oversized_sparse_result_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sha = sha256_hex(b"small result");
        let path = dir.path().join(sha.to_hex());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_ASSET_BYTES + 1)
            .unwrap();
        assert!(MaterializedDir::new(dir.path()).get(&sha).is_none());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), MAX_ASSET_BYTES + 1);
    }

    #[test]
    fn memory_source_round_trips() {
        let mut m = MemoryMaterialized::default();
        m.insert(b"abc".to_vec());
        assert_eq!(m.get(&sha256_hex(b"abc")).unwrap(), b"abc");
        assert!(m.get(&sha256_hex(b"xyz")).is_none());
    }
}
