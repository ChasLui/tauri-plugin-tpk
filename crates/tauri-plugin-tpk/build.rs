const COMMANDS: &[&str] = &[
    "check",
    "download",
    "notify_ready",
    "status",
    "reset",
    "set_mod_enabled",
];

fn main() {
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
