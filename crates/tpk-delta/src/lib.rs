//! bsdiff patch application (runtime) and generation (CLI) for TPK delta entries.
//!
//! This crate handles only the bsdiff control stream. The zstd layer that wraps
//! it on disk is `tpk-format`'s concern, so `encoding = "zstd+bsdiff"` decomposes
//! into two independent, separately testable steps.
//!
//! See `spec/tpk-v1.md` section 3.4.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::fmt;

/// Errors raised while applying or generating a patch.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeltaError {
    /// The patch stream is malformed or truncated.
    #[error("malformed patch stream: {0}")]
    Malformed(String),

    /// Applying the patch produced a different length than the manifest declared.
    #[error("patch produced {actual} bytes, manifest declared {expected}")]
    LengthMismatch {
        /// What the manifest said the result would be.
        expected: usize,
        /// What actually came out.
        actual: usize,
    },

    /// The declared output length is above the configured ceiling.
    #[error("declared output of {declared} bytes exceeds the {limit} byte limit")]
    TooLarge {
        /// The length the manifest declared.
        declared: usize,
        /// The configured ceiling.
        limit: usize,
    },

    /// The patch routine panicked on hostile input.
    #[error("patch routine failed on malformed input")]
    Panicked,
}

impl DeltaError {
    /// The frozen error code this failure maps to (`E_DELTA`).
    pub const fn code(&self) -> &'static str {
        "E_DELTA"
    }
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, DeltaError>;

/// Apply a bsdiff control stream to `base`.
///
/// `expect_out_len` comes from the entry's signature-covered `size`, so the
/// output buffer is allocated exactly once at exactly the right size. `limit`
/// bounds that allocation independently: a manifest is signed, but a signing key
/// can still be used to ship something that would exhaust memory on a phone.
///
/// # Errors
///
/// - [`DeltaError::TooLarge`] when `expect_out_len` is above `limit`
/// - [`DeltaError::Malformed`] when the stream cannot be applied
/// - [`DeltaError::LengthMismatch`] when the result is not `expect_out_len` bytes
/// - [`DeltaError::Panicked`] if the patch routine panics on hostile input
pub fn apply(
    base: &[u8],
    patch_stream: &[u8],
    expect_out_len: usize,
    limit: usize,
) -> Result<Vec<u8>> {
    if expect_out_len > limit {
        return Err(DeltaError::TooLarge {
            declared: expect_out_len,
            limit,
        });
    }

    // bsdiff appends through `Write` and reads back through `DerefMut` (its add
    // operations need the bytes already emitted), so the buffer must start
    // empty. Reserving exactly `expect_out_len` keeps the allocation single and
    // exact rather than amortised at 2x — `size` is signature-covered and was
    // bounds-checked above, so trusting it here is safe.
    let mut out = Vec::with_capacity(expect_out_len);
    let mut reader = patch_stream;

    // The crate is safe Rust with checked arithmetic throughout, but a corrupt
    // layer must degrade to "skip this layer", never to a process abort.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bsdiff::patch(base, &mut reader, &mut out)
    }));

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(DeltaError::Malformed(e.to_string())),
        Err(_) => return Err(DeltaError::Panicked),
    }

    if out.len() != expect_out_len {
        return Err(DeltaError::LengthMismatch {
            expected: expect_out_len,
            actual: out.len(),
        });
    }
    Ok(out)
}

/// Generate a bsdiff control stream turning `old` into `new`.
///
/// Build-machine only.
///
/// # Errors
///
/// Returns [`DeltaError::Malformed`] if the generator fails.
#[cfg(feature = "encode")]
pub fn diff(old: &[u8], new: &[u8]) -> Result<Vec<u8>> {
    let mut patch = Vec::new();
    bsdiff::diff(old, new, &mut patch).map_err(|e| DeltaError::Malformed(e.to_string()))?;
    Ok(patch)
}

/// Whether a delta is worth shipping instead of the whole file.
///
/// Mirrors specification section 3.4: below the size threshold, or once the
/// patch approaches the size of the file itself, `full` wins — it decodes
/// faster, needs no base, and cannot fail with a base-hash mismatch.
#[derive(Debug, Clone, Copy)]
pub struct DeltaPolicy {
    /// Files below this size are never deltified.
    pub min_file_size: usize,
    /// Reject a delta whose stream reaches this fraction of the new file.
    pub max_ratio: f64,
}

impl Default for DeltaPolicy {
    fn default() -> Self {
        Self {
            min_file_size: 262_144,
            max_ratio: 0.7,
        }
    }
}

impl DeltaPolicy {
    /// Whether a file of this size is a delta candidate at all.
    pub fn is_candidate(&self, new_size: usize) -> bool {
        new_size >= self.min_file_size
    }

    /// Whether a generated stream is worth keeping.
    pub fn is_worthwhile(&self, patch_len: usize, new_size: usize) -> bool {
        if new_size == 0 {
            return false;
        }
        (patch_len as f64) < self.max_ratio * (new_size as f64)
    }
}

impl fmt::Display for DeltaPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "min_file_size={} max_ratio={}",
            self.min_file_size, self.max_ratio
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 64 * 1024 * 1024;

    #[cfg(feature = "encode")]
    #[test]
    fn round_trips_a_realistic_edit() {
        let old = b"the quick brown fox jumps over the lazy dog".repeat(500);
        let mut new = old.clone();
        new.extend_from_slice(b"... and then keeps going for a while longer");
        new[100..110].copy_from_slice(b"CHANGEDXYZ");

        let patch = diff(&old, &new).unwrap();
        assert_eq!(apply(&old, &patch, new.len(), LIMIT).unwrap(), new);

        // A raw bsdiff control stream is *not* smaller than the file: its diff
        // stream carries one byte per position regardless of whether that
        // position changed. What it is, is overwhelmingly zeros — which is why
        // the format wraps it in zstd (`encoding = "zstd+bsdiff"`, spec 3.4)
        // rather than shipping it bare. Anyone tempted to drop that wrapper
        // should read this assertion first.
        let zeros = patch.iter().filter(|b| **b == 0).count();
        assert!(
            zeros * 10 > patch.len() * 9,
            "expected a mostly-zero stream, got {zeros} zeros in {} bytes",
            patch.len()
        );
    }

    #[cfg(feature = "encode")]
    #[test]
    fn round_trips_edge_case_inputs() {
        for (old, new) in [
            (vec![], vec![]),
            (vec![], b"appeared from nothing".to_vec()),
            (b"vanished into nothing".to_vec(), vec![]),
            (vec![0u8; 1024], vec![255u8; 1024]),
            (b"same".to_vec(), b"same".to_vec()),
        ] {
            let patch = diff(&old, &new).unwrap();
            assert_eq!(
                apply(&old, &patch, new.len(), LIMIT).unwrap(),
                new,
                "failed for {} -> {} bytes",
                old.len(),
                new.len()
            );
        }
    }

    #[cfg(feature = "encode")]
    #[test]
    fn applying_to_the_wrong_base_does_not_silently_succeed() {
        let old = b"original content here".repeat(100);
        let new = b"modified content here".repeat(100);
        let patch = diff(&old, &new).unwrap();

        let wrong_base = b"something else entirely".repeat(100);
        // bsdiff is not authenticated, so a wrong base can still produce output.
        // That is exactly why the caller must check the result hash — this test
        // pins the fact that the delta layer alone does not catch it.
        if let Ok(out) = apply(&wrong_base, &patch, new.len(), LIMIT) {
            assert_ne!(out, new, "wrong base must not reconstruct the file");
        }
    }

    #[test]
    fn rejects_a_declared_length_above_the_limit() {
        let err = apply(b"base", &[], 100, 10).unwrap_err();
        assert!(matches!(err, DeltaError::TooLarge { .. }));
        assert_eq!(err.code(), "E_DELTA");
    }

    #[test]
    fn rejects_a_truncated_stream() {
        // Nothing at all is not a valid control stream for a non-empty output.
        assert!(apply(b"base", &[], 4096, LIMIT).is_err());
    }

    #[test]
    fn rejects_random_bytes_as_a_patch() {
        let garbage: Vec<u8> = (0..512u32).map(|i| (i * 7 % 251) as u8).collect();
        // Must fail cleanly rather than panic or hang.
        let _ = apply(b"some base content", &garbage, 1024, LIMIT);
    }

    #[test]
    fn policy_skips_small_files() {
        let p = DeltaPolicy::default();
        assert!(!p.is_candidate(1024));
        assert!(!p.is_candidate(262_143));
        assert!(p.is_candidate(262_144));
    }

    #[test]
    fn policy_rejects_patches_that_approach_the_file_size() {
        let p = DeltaPolicy::default();
        assert!(p.is_worthwhile(10, 1000));
        assert!(!p.is_worthwhile(700, 1000), "at the ratio, full wins");
        assert!(!p.is_worthwhile(900, 1000));
        assert!(
            !p.is_worthwhile(0, 0),
            "a zero-byte file is never a candidate"
        );
    }
}
