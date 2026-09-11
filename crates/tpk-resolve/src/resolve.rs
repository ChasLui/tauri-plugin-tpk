//! Resolving a path through the layer stack.

use std::sync::{Arc, Mutex, OnceLock};

use tpk_format::container::{UnverifiedPack, VerifiedPack};
use tpk_format::manifest::{Op, PackKind, Sha256Hex};
use tpk_format::sign::{sha256_hex, TrustStore};

use crate::cache::ByteLru;
use crate::csp;
use crate::index::Index;
use crate::layer::{FailedLayer, Layer, LayerSpec};

/// Supplies delta results that were materialized when their layer was staged.
///
/// Materialization deliberately does not happen here. Doing it during `boot`
/// would block window creation, and on a slow device a large patch can take
/// long enough to trip the iOS launch watchdog — which then looks exactly like
/// a bad pack and gets a perfectly good one blacklisted.
pub trait MaterializedSource: Send + Sync {
    /// Fetch a materialized result by its content hash.
    fn get(&self, sha256: &Sha256Hex) -> Option<Vec<u8>>;
}

/// A source that has nothing, for stacks with no delta entries.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoMaterialized;

impl MaterializedSource for NoMaterialized {
    fn get(&self, _sha256: &Sha256Hex) -> Option<Vec<u8>> {
        None
    }
}

/// Why a path could not be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveMiss {
    /// No layer provides the path; fall through to the embedded assets.
    NotFound,
    /// A tombstone hides it. The embedded assets must **not** be consulted.
    Deleted,
    /// The layer is corrupt and should be blacklisted.
    LayerCorrupt {
        /// SHA-256 of the offending pack file.
        file_sha256: Sha256Hex,
        /// What went wrong.
        reason: String,
    },
}

/// Builds an [`Index`] from a stack of layers, lowest first.
pub struct IndexBuilder {
    trust: Arc<TrustStore>,
    min_key_epoch: u32,
    allow_mods: bool,
    index: Index,
    readers: Vec<Mutex<VerifiedPack>>,
    failed: Vec<FailedLayer>,
}

impl IndexBuilder {
    /// Start a stack.
    pub fn new(trust: Arc<TrustStore>, min_key_epoch: u32) -> Self {
        Self {
            trust,
            min_key_epoch,
            allow_mods: false,
            index: Index::default(),
            readers: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Permit `mod` layers.
    ///
    /// Off by default and not reachable from configuration yet: an unsigned
    /// layer sits above everything else and can replace `/index.html`, and the
    /// JS it delivers inherits the main window's capabilities.
    #[must_use]
    pub fn allow_mods(mut self, allow: bool) -> Self {
        self.allow_mods = allow;
        self
    }

    /// Load one layer on top of the stack.
    ///
    /// A layer that cannot be loaded is recorded and skipped rather than
    /// aborting the build: one corrupt pack must degrade the overlay, not the
    /// application.
    pub fn push_layer(&mut self, spec: &LayerSpec) {
        match self.load(spec) {
            Ok((layer, reader)) => {
                if self.index.push(layer) {
                    self.readers.push(Mutex::new(reader));
                } else {
                    self.failed.push(FailedLayer {
                        path: spec.path.clone(),
                        file_sha256: Some(spec.file_sha256),
                        reason: "layer stack is full".to_string(),
                    });
                }
            }
            Err(reason) => self.failed.push(FailedLayer {
                path: spec.path.clone(),
                file_sha256: Some(spec.file_sha256),
                reason,
            }),
        }
    }

    fn load(&self, spec: &LayerSpec) -> Result<(Layer, VerifiedPack), String> {
        let bytes = std::fs::read(&spec.path).map_err(|e| e.to_string())?;
        let actual = sha256_hex(&bytes);
        if actual != spec.file_sha256 {
            return Err(format!(
                "file hash {actual} does not match the recorded {}",
                spec.file_sha256
            ));
        }
        drop(bytes);

        let unverified = UnverifiedPack::open(&spec.path).map_err(|e| e.to_string())?;
        // A pack's signature is checked against whichever epoch still verifies,
        // bounded below by the client's monotonic floor.
        let mut last = String::from("no trusted key");
        let mut verified = None;
        for epoch in self.min_key_epoch..=self.trust.max_epoch().max(self.min_key_epoch) {
            match UnverifiedPack::open(&spec.path)
                .map_err(|e| e.to_string())?
                .verify(&self.trust, None, epoch, self.min_key_epoch)
            {
                Ok(pack) => {
                    verified = Some(pack);
                    break;
                }
                Err(e) => last = e.to_string(),
            }
        }
        drop(unverified);
        let pack = verified.ok_or(last)?;

        if pack.manifest().kind == PackKind::Mod && !self.allow_mods {
            return Err("mod layers are disabled".to_string());
        }

        Ok((
            Layer {
                path: spec.path.clone(),
                file_sha256: spec.file_sha256,
                manifest: pack.manifest().clone(),
            },
            pack,
        ))
    }

    /// Layers that failed to load; their hashes belong on the blacklist.
    pub fn failed_layers(&self) -> &[FailedLayer] {
        &self.failed
    }

    /// Finish, producing a resolver.
    pub fn build(
        self,
        cache_budget_bytes: u64,
        materialized: Box<dyn MaterializedSource>,
    ) -> Resolver {
        Resolver {
            index: Arc::new(self.index),
            readers: self.readers,
            cache: Mutex::new(ByteLru::new(cache_budget_bytes)),
            materialized,
            failed: self.failed,
            csp_hashes: OnceLock::new(),
        }
    }
}

/// Serves paths from a frozen layer stack.
pub struct Resolver {
    index: Arc<Index>,
    readers: Vec<Mutex<VerifiedPack>>,
    cache: Mutex<ByteLru>,
    materialized: Box<dyn MaterializedSource>,
    failed: Vec<FailedLayer>,
    csp_hashes: OnceLock<Vec<String>>,
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("layers", &self.index.layers().len())
            .field("paths", &self.index.len())
            .field("failed_layers", &self.failed.len())
            .finish()
    }
}

impl Resolver {
    /// The frozen index.
    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }

    /// Layers that failed to load.
    pub fn failed_layers(&self) -> &[FailedLayer] {
        &self.failed
    }

    /// Every path the overlay serves.
    pub fn visible_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.index.visible_paths().map(str::to_string).collect();
        paths.sort_unstable();
        paths
    }

    /// Resolve a path.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveMiss::Deleted`] for a tombstone — the caller must not
    /// fall back to embedded assets in that case, or deletions would never take
    /// effect — and [`ResolveMiss::LayerCorrupt`] when a layer must be
    /// blacklisted.
    pub fn get(&self, path: &str) -> Result<Arc<[u8]>, ResolveMiss> {
        let Some(loc) = self.index.lookup(path) else {
            return Err(ResolveMiss::NotFound);
        };
        if loc.op == Op::Delete {
            return Err(ResolveMiss::Deleted);
        }

        if let Ok(mut cache) = self.cache.lock() {
            if let Some(hit) = cache.get(path) {
                return Ok(hit);
            }
        }

        let entry = self
            .index
            .entry(loc)
            .ok_or_else(|| self.corrupt(loc.layer, "entry is missing from its layer"))?;
        let expected = entry
            .sha256
            .ok_or_else(|| self.corrupt(loc.layer, "entry has no sha256"))?;

        let bytes = match entry.op {
            Op::Full => {
                let mut reader = self
                    .readers
                    .get(loc.layer as usize)
                    .ok_or_else(|| self.corrupt(loc.layer, "layer has no reader"))?
                    .lock()
                    .map_err(|_| self.corrupt(loc.layer, "layer reader is poisoned"))?;
                reader
                    .read_blob(entry)
                    .map_err(|e| self.corrupt(loc.layer, &e.to_string()))?
            }
            Op::Delta => {
                // Already reconstructed at stage time; this is a cache read.
                self.materialized.get(&expected).ok_or_else(|| {
                    self.corrupt(loc.layer, "delta result has not been materialized")
                })?
            }
            Op::Delete => unreachable!("handled above"),
        };

        // The container checks `full` blobs on read, but a materialized delta
        // comes from a cache directory the OS may purge or a tool may touch.
        // Checking both keeps one rule rather than two.
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(self.corrupt(
                loc.layer,
                &format!("content hash {actual} does not match {expected}"),
            ));
        }

        let bytes: Arc<[u8]> = bytes.into();
        if let Ok(mut cache) = self.cache.lock() {
            return Ok(cache.insert(path, bytes));
        }
        Ok(bytes)
    }

    fn corrupt(&self, layer: u16, reason: &str) -> ResolveMiss {
        let file_sha256 = self
            .index
            .layers()
            .get(layer as usize)
            .map(|l| l.file_sha256)
            .unwrap_or_else(|| sha256_hex(b""));
        ResolveMiss::LayerCorrupt {
            file_sha256,
            reason: reason.to_string(),
        }
    }

    /// CSP script hashes for every `.js`/`.mjs` the overlay serves.
    ///
    /// Computed once, lazily: an app without a configured CSP never calls this,
    /// which is the default.
    pub fn csp_script_hashes(&self) -> &[String] {
        self.csp_hashes.get_or_init(|| {
            let mut hashes = Vec::new();
            for path in self.index.visible_paths() {
                if !csp::is_script_path(path) {
                    continue;
                }
                if let Ok(bytes) = self.get(path) {
                    hashes.push(csp::script_hash(&bytes));
                }
            }
            hashes.sort_unstable();
            hashes.dedup();
            hashes
        })
    }
}
