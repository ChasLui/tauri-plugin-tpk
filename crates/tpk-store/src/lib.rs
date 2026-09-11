//! On-disk layer pool, three-state pointer machine, blacklist and delta materialization.
//!
//! Implementation lands in step 4; see `spec/tpk-v1.md` sections 5 and 7.
#![deny(missing_docs)]
#![forbid(unsafe_code)]
