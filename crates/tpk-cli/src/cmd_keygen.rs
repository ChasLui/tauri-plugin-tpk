//! `tpk keygen` — generate an unencrypted minisign key pair.

use std::path::PathBuf;

use tpk_format::secret::SecretKey;

use crate::{CliError, CliResult};

#[derive(clap::Args)]
pub struct Args {
    /// Where to write the secret key. Omit to print it to stdout instead.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Comment stored in the key file.
    #[arg(long, default_value = "tpk signing key")]
    pub comment: String,
}

pub fn run(args: &Args) -> CliResult {
    let key = SecretKey::generate();
    let key_file = key.to_key_file(&args.comment);

    match &args.out {
        Some(path) => {
            if path.exists() {
                return Err(CliError::usage(format!(
                    "{} already exists; refusing to overwrite a signing key",
                    path.display()
                )));
            }
            write_private(path, &key_file)?;
            eprintln!("secret key written to {}", path.display());
        }
        None => {
            // stdout so it can be piped straight into a secret manager.
            print!("{key_file}");
        }
    }

    eprintln!();
    eprintln!("Add this to tauri.conf.json under plugins.tpk.pubkeys:");
    println!(
        "  {{ \"key\": \"{}\", \"epoch\": 1 }}",
        key.public_key_base64()
    );
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, contents: &str) -> CliResult {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| CliError::usage(format!("cannot create {}: {e}", path.display())))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| CliError::usage(format!("cannot write {}: {e}", path.display())))
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, contents: &str) -> CliResult {
    // No mode bits to set; the parent directory's ACL governs access.
    std::fs::write(path, contents)
        .map_err(|e| CliError::usage(format!("cannot write {}: {e}", path.display())))
}
