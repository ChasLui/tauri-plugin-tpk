//! `tpk verify` — the gate CI runs before anything reaches a CDN.
//!
//! Verifies both `.tpk` packs and channel manifests. A channel manifest's
//! signature is detached, so it is looked for at `<file>.minisig`.

use std::path::{Path, PathBuf};

use tpk_format::channel::ChannelManifest;
use tpk_format::container::UnverifiedPack;
use tpk_format::sign::{TrustStore, TrustedKey};

use crate::{CliError, CliResult};

#[derive(clap::Args)]
pub struct Args {
    /// Trusted public key, base64. Repeat for a rotation window.
    #[arg(long = "pubkey", required = true)]
    pub pubkeys: Vec<String>,

    /// Epoch of the corresponding `--pubkey`, in the same order. Defaults to 1.
    #[arg(long = "epoch")]
    pub epochs: Vec<u32>,

    /// Files to verify: `.tpk` packs or channel manifests.
    #[arg(long = "file", required = true)]
    pub files: Vec<PathBuf>,

    /// Reject anything signed with a key older than this epoch.
    #[arg(long, default_value_t = 1)]
    pub min_epoch: u32,
}

pub fn run(args: &Args) -> CliResult {
    if !args.epochs.is_empty() && args.epochs.len() != args.pubkeys.len() {
        return Err(CliError::usage(format!(
            "{} --epoch values for {} --pubkey values",
            args.epochs.len(),
            args.pubkeys.len()
        )));
    }

    let keys: Vec<TrustedKey> = args
        .pubkeys
        .iter()
        .enumerate()
        .map(|(i, key)| TrustedKey {
            key: key.clone(),
            epoch: args.epochs.get(i).copied().unwrap_or(1),
        })
        .collect();
    let trust = TrustStore::new(&keys).map_err(|e| CliError::usage(e.to_string()))?;

    let mut failures = 0usize;
    for file in &args.files {
        match verify_one(&trust, file, args.min_epoch) {
            Ok(summary) => println!("ok    {}  {summary}", file.display()),
            Err(message) => {
                println!("FAIL  {}  {message}", file.display());
                failures += 1;
            }
        }
    }

    if failures > 0 {
        return Err(CliError::verification(format!(
            "{failures} of {} file(s) failed verification",
            args.files.len()
        )));
    }
    Ok(())
}

fn verify_one(trust: &TrustStore, path: &Path, min_epoch: u32) -> Result<String, String> {
    let is_pack = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tpk"));
    if is_pack {
        verify_pack(trust, path, min_epoch)
    } else {
        verify_channel(trust, path, min_epoch)
    }
}

fn verify_pack(trust: &TrustStore, path: &Path, min_epoch: u32) -> Result<String, String> {
    // A pack's manifest carries no epoch of its own; the channel manifest that
    // advertises it does. Try each epoch the store knows about. `verify`
    // consumes the reader, so the pack is reopened per attempt.
    let mut last = String::new();
    for epoch in min_epoch..=trust.max_epoch().max(min_epoch) {
        match UnverifiedPack::open(path)
            .map_err(|e| e.to_string())?
            .verify(trust, None, epoch, min_epoch)
        {
            Ok(pack) => {
                let m = pack.manifest();
                return Ok(format!(
                    "{:?} {} {} (version_code {}, {} entries, key epoch {epoch})",
                    m.kind,
                    m.id,
                    m.version,
                    m.version_code,
                    m.entries.len()
                ));
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn verify_channel(trust: &TrustStore, path: &Path, min_epoch: u32) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let sig_path = path.with_file_name(format!(
        "{}.minisig",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let signature = std::fs::read_to_string(&sig_path)
        .map_err(|e| format!("missing detached signature {}: {e}", sig_path.display()))?;

    // Parse first so a malformed document reports as such rather than as a
    // signature failure; the epoch we need to verify with lives inside it.
    let manifest = ChannelManifest::parse(&bytes).map_err(|e| e.to_string())?;
    trust
        .verify(&bytes, &signature, manifest.key_epoch, min_epoch)
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "channel {} watermark {} ({} pack(s), key epoch {})",
        manifest.channel,
        manifest.watermark,
        manifest.packs.len(),
        manifest.key_epoch
    ))
}
