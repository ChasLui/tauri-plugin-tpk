//! Layered overlay resolution for TPK packs: tombstones, index, LRU cache, CSP hashes.
//!
//! Implementation lands in step 3; see `spec/tpk-v1.md` section 6.
#![deny(missing_docs)]
#![forbid(unsafe_code)]
