//! Turns the `app-store` / `__all` feature pair into a single `app_store` cfg.
//!
//! Identical to `crates/tpk-format/build.rs`, which explains why `__all` is
//! there: the `PackKind` variants and the arms matching on them have to be
//! added and removed together, so every crate needs the same cfg.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(app_store)");
    if std::env::var_os("CARGO_FEATURE_APP_STORE").is_some()
        && std::env::var_os("CARGO_FEATURE___ALL").is_none()
    {
        println!("cargo::rustc-cfg=app_store");
    }
}
