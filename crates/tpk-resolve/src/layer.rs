//! One pack participating in the overlay.

use std::path::PathBuf;

use tpk_format::error::ErrorCode;
use tpk_format::manifest::{PackId, PackKind, PackManifest, Sha256Hex};

/// Where a layer sits in the stack.
///
/// Ordering is decided by the store and handed to the resolver, which only
/// walks the slice it is given. Keeping the policy out of here is what lets the
/// overlay be tested without a disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LayerOrder {
    /// A complete content tree. One per id.
    Base,
    /// Applied over a base in `version_code` order.
    Patch,
    /// Optional content stacked above patches.
    ///
    /// Removed by the `app-store` feature, along with [`PackKind::Dlc`].
    #[cfg(not(app_store))]
    Dlc,
    /// Unsigned user content. Highest, and only when explicitly enabled.
    ///
    /// Removed by the `app-store` feature, along with [`PackKind::Mod`].
    #[cfg(not(app_store))]
    Mod,
}

impl From<PackKind> for LayerOrder {
    fn from(kind: PackKind) -> Self {
        match kind {
            PackKind::Base => Self::Base,
            PackKind::Patch => Self::Patch,
            #[cfg(not(app_store))]
            PackKind::Dlc => Self::Dlc,
            #[cfg(not(app_store))]
            PackKind::Mod => Self::Mod,
        }
    }
}

/// A layer the store wants loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerSpec {
    /// Path to the `.tpk` file.
    pub path: PathBuf,
    /// Expected SHA-256 of that file, as recorded when it was staged.
    pub file_sha256: Sha256Hex,
}

/// A layer that loaded successfully.
#[derive(Debug)]
pub struct Layer {
    /// Where it came from.
    pub path: PathBuf,
    /// SHA-256 of the file, used to blacklist it if it turns out to be bad.
    pub file_sha256: Sha256Hex,
    /// Its verified manifest.
    pub manifest: PackManifest,
}

impl Layer {
    /// Pack identity.
    pub fn id(&self) -> &PackId {
        &self.manifest.id
    }

    /// Ordering class.
    pub fn order(&self) -> LayerOrder {
        self.manifest.kind.into()
    }

    /// Monotonic ordering key within an id.
    pub fn version_code(&self) -> u64 {
        self.manifest.version_code
    }
}

/// A layer that could not be loaded, and why.
#[derive(Debug, Clone)]
pub struct FailedLayer {
    /// Where it came from.
    pub path: PathBuf,
    /// SHA-256 of the file, if it could be computed.
    pub file_sha256: Option<Sha256Hex>,
    /// What went wrong.
    pub reason: String,
    /// The frozen code for it: `Io` for an unreadable file, `Hash` for a hash
    /// mismatch, the verifier's own code for a failed verification, `Disabled`
    /// for a mod layer while mods are off, and `State` for a full stack.
    pub code: ErrorCode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_order_stacks_low_to_high() {
        #[cfg(not(app_store))]
        let (mut orders, expected) = (
            [
                LayerOrder::Mod,
                LayerOrder::Base,
                LayerOrder::Dlc,
                LayerOrder::Patch,
            ],
            [
                LayerOrder::Base,
                LayerOrder::Patch,
                LayerOrder::Dlc,
                LayerOrder::Mod,
            ],
        );
        #[cfg(app_store)]
        let (mut orders, expected) = (
            [LayerOrder::Patch, LayerOrder::Base],
            [LayerOrder::Base, LayerOrder::Patch],
        );

        orders.sort();
        assert_eq!(orders, expected);
    }

    #[test]
    fn pack_kind_maps_to_order() {
        assert_eq!(LayerOrder::from(PackKind::Base), LayerOrder::Base);
        assert_eq!(LayerOrder::from(PackKind::Patch), LayerOrder::Patch);
        #[cfg(not(app_store))]
        {
            assert_eq!(LayerOrder::from(PackKind::Dlc), LayerOrder::Dlc);
            assert_eq!(LayerOrder::from(PackKind::Mod), LayerOrder::Mod);
        }
    }
}
