//! On-disk layer pool, three-state pointer machine, blacklist and delta materialization.
//!
//! Layers are content-addressed in a single pool; the three states name the
//! layers they consist of rather than living in separate directories. Promotion
//! is one atomic write of `state.json`, which is what keeps a process killed
//! mid-promotion from leaving a trusted pointer over an incomplete layer set.
//!
//! See `spec/tpk-v1.md` sections 5 and 7, and Appendix A for the revisions.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod blacklist;
pub mod error;
pub mod layout;
pub mod materialize;
pub mod state;
pub mod store;

pub use blacklist::{Blacklist, Reason};
pub use error::{Result, StoreError};
pub use layout::Layout;
pub use materialize::{MaterializedDir, MAX_ASSET_BYTES};
pub use state::{LayerRecord, Pointer, Revision, StoreState};
pub use store::{
    empty_resolver, resolver_for, BootOutcome, CommitOutcome, IncomingPack, Store,
    MAX_BOOT_ATTEMPTS, MAX_CONSECUTIVE_ROLLBACKS,
};
