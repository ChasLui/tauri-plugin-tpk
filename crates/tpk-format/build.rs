//! Turns the `app-store` / `__all` feature pair into a single `app_store` cfg.
//!
//! `--all-features` enables every feature of every workspace member, which
//! would silently put the whole CI matrix into App Store mode and delete the
//! `PackKind` variants the rest of the suite needs. `__all` exists only to be
//! swept up by `--all-features` and cancel `app-store` again; an explicit
//! `--features app-store` leaves it off.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(app_store)");
    if std::env::var_os("CARGO_FEATURE_APP_STORE").is_some()
        && std::env::var_os("CARGO_FEATURE___ALL").is_none()
    {
        println!("cargo::rustc-cfg=app_store");
    }
}
