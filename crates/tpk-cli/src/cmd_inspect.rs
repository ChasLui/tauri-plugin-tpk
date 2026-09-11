//! `tpk inspect` — print a pack's manifest without verifying it.
//!
//! Deliberately does not require a public key: inspecting a pack you just built
//! is a routine debugging step. Every line of output is labelled as unverified
//! so the distinction from `tpk verify` stays visible.

use std::path::PathBuf;

use tpk_format::container::UnverifiedPack;
use tpk_format::manifest::{Op, PackManifest};

use crate::{CliError, CliResult};

#[derive(clap::Args)]
pub struct Args {
    /// The `.tpk` file to inspect.
    pub file: PathBuf,

    /// Print the manifest as JSON instead of a summary.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: &Args) -> CliResult {
    let pack = UnverifiedPack::open(&args.file)
        .map_err(|e| CliError::verification(format!("{}: {e}", args.file.display())))?;

    let manifest = PackManifest::parse(pack.raw_manifest_bytes())
        .map_err(|e| CliError::verification(format!("{}: {e}", args.file.display())))?;

    if args.json {
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| CliError::usage(format!("cannot render manifest: {e}")))?;
        println!("{json}");
        return Ok(());
    }

    println!("file           {}", args.file.display());
    println!("manifest_sha256 {}", pack.manifest_sha256());
    println!(
        "signature      {}",
        match pack.detached_signature() {
            Some(_) => "present (NOT verified — use `tpk verify`)",
            None => "absent",
        }
    );
    println!("spec           {}", manifest.spec);
    println!("id             {}", manifest.id);
    println!("kind           {:?}", manifest.kind);
    println!(
        "version        {} (version_code {})",
        manifest.version, manifest.version_code
    );
    if let Some(parent) = &manifest.parent {
        println!(
            "parent         {} version_code {}",
            parent.id, parent.version_code
        );
    }
    match (&manifest.min_shell, &manifest.max_shell) {
        (None, None) => println!("shell range    any"),
        (min, max) => println!(
            "shell range    {} .. {}",
            min.as_ref().map_or("any".to_string(), |v| v.to_string()),
            max.as_ref().map_or("any".to_string(), |v| v.to_string()),
        ),
    }
    if let Some(channel) = &manifest.channel {
        println!("channel        {channel}");
    }
    println!("created_at     {}", manifest.created_at);
    println!("trusted        {}", manifest.policies.trusted);

    let (mut full, mut delta, mut delete, mut bytes) = (0usize, 0usize, 0usize, 0u64);
    for entry in &manifest.entries {
        match entry.op {
            Op::Full => full += 1,
            Op::Delta => delta += 1,
            Op::Delete => delete += 1,
        }
        bytes += entry.size.unwrap_or(0);
    }
    println!(
        "entries        {} ({full} full, {delta} delta, {delete} delete), {bytes} bytes decoded",
        manifest.entries.len()
    );

    Ok(())
}
