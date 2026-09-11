//! Where the store keeps things.
//!
//! Two roots, not one. `$APPLOCALDATA` on iOS is `Library/Application Support`,
//! which iCloud backs up by default — and Apple's review checklist names
//! *Optimizing Your App's Data for iCloud Backup* as a document apps are
//! expected to follow. A 50 MB layer pool that can be re-downloaded has no
//! business in a user's backup quota; on Android the equivalent is Auto Backup's
//! 25 MB ceiling, past which the whole app's backup is silently skipped.
//!
//! So: state and layers live in the data root (excluded from backup), while
//! anything re-derivable lives in the cache root, which the OS may purge at any
//! moment. That purge is exactly why a lazy materialization path has to exist.

use std::path::{Path, PathBuf};

use tpk_format::manifest::Sha256Hex;

/// Resolved directory layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    data_root: PathBuf,
    cache_root: PathBuf,
}

impl Layout {
    /// Build a layout from the two platform roots.
    ///
    /// Both are the plugin's own subdirectory, e.g. `$APPLOCALDATA/tpk`.
    pub fn new(data_root: impl Into<PathBuf>, cache_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            cache_root: cache_root.into(),
        }
    }

    /// The durable root.
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// The purgeable root.
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// `state.json`.
    pub fn state_file(&self) -> PathBuf {
        self.data_root.join("state.json")
    }

    /// `blacklist.json`.
    pub fn blacklist_file(&self) -> PathBuf {
        self.data_root.join("blacklist.json")
    }

    /// `keys-cache.json`, the audit trail for key epochs.
    pub fn keys_cache_file(&self) -> PathBuf {
        self.data_root.join("keys-cache.json")
    }

    /// The content-addressed layer pool.
    pub fn layers_dir(&self) -> PathBuf {
        self.data_root.join("layers")
    }

    /// One layer, named by its own hash.
    pub fn layer_file(&self, sha256: &Sha256Hex) -> PathBuf {
        self.layers_dir().join(format!("{sha256}.tpk"))
    }

    /// Materialized delta results.
    pub fn materialized_dir(&self) -> PathBuf {
        self.cache_root.join("materialized")
    }

    /// One materialized result, named by the hash of its content.
    pub fn materialized_file(&self, sha256: &Sha256Hex) -> PathBuf {
        self.materialized_dir().join(sha256.to_hex())
    }

    /// Scratch space for in-flight downloads.
    pub fn tmp_dir(&self) -> PathBuf {
        self.cache_root.join("tmp")
    }

    /// Create every directory the store needs.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if a directory cannot be created.
    pub fn create_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.data_root.clone(),
            self.layers_dir(),
            self.cache_root.clone(),
            self.materialized_dir(),
            self.tmp_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Directories the host should exclude from platform backups.
    ///
    /// Marking them needs `NSURLIsExcludedFromBackupKey` on Apple platforms and
    /// `<data-extraction-rules>` on Android — both host concerns, and neither
    /// worth pulling an Objective-C bridge into this crate for. The store names
    /// the directories; the plugin applies the flag, and treats failure as a
    /// warning rather than a reason not to start.
    pub fn backup_exclusions(&self) -> Vec<PathBuf> {
        // Only the layer pool: it is large and re-downloadable. `state.json` is
        // tiny and *should* survive a restore — carrying the blacklist and the
        // watermark floor across a device migration is correct behaviour.
        vec![self.layers_dir()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::sign::sha256_hex;

    fn layout() -> Layout {
        Layout::new("/data/tpk", "/cache/tpk")
    }

    #[test]
    fn durable_and_purgeable_paths_are_kept_apart() {
        let l = layout();
        let sha = sha256_hex(b"x");

        // Anything that cannot be re-derived belongs under the data root.
        for durable in [l.state_file(), l.blacklist_file(), l.layer_file(&sha)] {
            assert!(
                durable.starts_with(l.data_root()),
                "{} should be durable",
                durable.display()
            );
        }
        // Anything re-derivable belongs under the cache root.
        for purgeable in [l.materialized_file(&sha), l.tmp_dir()] {
            assert!(
                purgeable.starts_with(l.cache_root()),
                "{} should be purgeable",
                purgeable.display()
            );
        }
    }

    #[test]
    fn a_layer_is_named_by_its_own_hash() {
        let sha = sha256_hex(b"pack bytes");
        let path = layout().layer_file(&sha);
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("{sha}.tpk")
        );
    }

    #[test]
    fn create_dirs_makes_every_directory() {
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let l = Layout::new(data.path().join("tpk"), cache.path().join("tpk"));
        l.create_dirs().unwrap();

        assert!(l.layers_dir().is_dir());
        assert!(l.materialized_dir().is_dir());
        assert!(l.tmp_dir().is_dir());
    }

    #[test]
    fn only_the_layer_pool_is_excluded_from_backup() {
        let l = layout();
        assert_eq!(l.backup_exclusions(), vec![l.layers_dir()]);
        // state.json must keep being backed up: restoring the blacklist and the
        // watermark floor onto a new device is the behaviour we want.
        assert!(!l.backup_exclusions().contains(&l.state_file()));
    }

    #[test]
    fn create_dirs_is_idempotent() {
        let data = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let l = Layout::new(data.path(), cache.path());
        l.create_dirs().unwrap();
        l.create_dirs().unwrap();
    }
}
