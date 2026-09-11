//! `tpk sign` — produce a detached signature for a channel manifest.
//!
//! Packs are signed when they are built; this exists for the channel manifest,
//! whose signature travels beside it on the CDN.

use std::path::PathBuf;

use crate::key_source::{load_signing_key, write_detached_signature};
use crate::{CliError, CliResult};

#[derive(clap::Args)]
pub struct Args {
    /// Files to sign. Each gets a sibling `<name>.minisig`.
    #[arg(long = "file", required = true)]
    pub files: Vec<PathBuf>,

    /// Comment covered by the signature.
    #[arg(long, default_value = "tpk")]
    pub trusted_comment: String,
}

pub fn run(args: &Args) -> CliResult {
    let key = load_signing_key()?;

    for path in &args.files {
        let bytes = std::fs::read(path)
            .map_err(|e| CliError::usage(format!("cannot read {}: {e}", path.display())))?;
        let signature = key.sign(
            &bytes,
            &format!(
                "{} {}",
                args.trusted_comment,
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            "signature from tpk",
        );
        write_detached_signature(path, &signature)?;
    }
    Ok(())
}
