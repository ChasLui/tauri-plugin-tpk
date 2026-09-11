const COMMANDS: &[&str] = &[
    "check",
    "download",
    "notify_ready",
    "status",
    "reset",
    "set_mod_enabled",
];

fn main() {
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
