const COMMANDS: &[&str] = &[
    "check",
    "download",
    "notify_ready",
    "status",
    "reset",
    "set_mod_enabled",
];

fn main() {
    // Tauri embeds Common-Controls v6 for its own tests; plugin crates need to
    // add the same activation manifest to avoid STATUS_ENTRYPOINT_NOT_FOUND.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-test-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }

    // Same `app_store` cfg as every other crate; see crates/tpk-format/build.rs.
    // Emitted before the packaging early-return so `cargo package` builds the
    // same code an ordinary build would.
    println!("cargo::rustc-check-cfg=cfg(app_store)");
    if std::env::var_os("CARGO_FEATURE_APP_STORE").is_some()
        && std::env::var_os("CARGO_FEATURE___ALL").is_none()
    {
        println!("cargo::rustc-cfg=app_store");
    }

    // Match on the parent directory name rather than the literal string
    // "target/package": the target directory is renameable via
    // CARGO_TARGET_DIR, and a string search misses it when it is.
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let packaging = std::path::Path::new(&manifest_dir)
            .parent()
            .and_then(std::path::Path::file_name)
            .is_some_and(|name| name == "package");
        if packaging {
            return;
        }
    }

    tauri_plugin::Builder::new(COMMANDS).build();
}
