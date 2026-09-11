//! TPK/1 container, manifest and signature handling.
//!
//! This crate is the single source of truth for the on-disk pack format:
//! path rules, manifest parsing, the ZIP container and signature verification.
//! See `spec/tpk-v1.md` — and note that Appendix A of that document overrides
//! the body wherever they disagree.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod channel;
pub mod container;
pub mod error;
pub mod manifest;
#[cfg(feature = "pack")]
pub mod pack;
pub mod path;
#[cfg(feature = "pack")]
pub mod secret;
pub mod sign;

pub use channel::{ChannelManifest, PackRef, CHANNEL_SPEC_TAG, MAX_NOTES_LEN};
pub use container::{UnverifiedPack, VerifiedPack, MANIFEST_NAME, MANIFEST_SIG_NAME};
pub use error::{ErrorCode, FormatError, Result};
pub use manifest::{
    Encoding, Entry, Op, PackId, PackKind, PackManifest, PackPolicies, ParentRef, Sha256Hex,
    SPEC_TAG,
};
#[cfg(feature = "pack")]
pub use pack::{compressed_size, PackBuilder, PackSummary};
pub use path::{normalize_asset_key, PackPath};
#[cfg(feature = "pack")]
pub use secret::SecretKey;
pub use sign::{sha256_hex, verify_sha256, TrustStore, TrustedKey};
