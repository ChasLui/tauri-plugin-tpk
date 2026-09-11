//! Layered overlay resolution for TPK packs: tombstones, index, LRU, CSP hashes.
//!
//! Layers stack lowest-first in the order the store hands them over; the highest
//! entry for a path wins, whether it adds, replaces or hides. Delta entries are
//! reconstructed when their layer is staged, not here — see
//! [`resolve::MaterializedSource`].
//!
//! See `spec/tpk-v1.md` section 6, and Appendix A for where it was revised.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod cache;
pub mod csp;
pub mod index;
pub mod layer;
pub mod resolve;

pub use cache::ByteLru;
pub use csp::{is_script_path, normalize_script_for_csp, script_hash};
pub use index::{Index, Loc};
pub use layer::{FailedLayer, Layer, LayerOrder, LayerSpec};
pub use resolve::{IndexBuilder, MaterializedSource, NoMaterialized, ResolveMiss, Resolver};
