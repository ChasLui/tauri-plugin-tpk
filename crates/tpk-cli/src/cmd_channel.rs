//! `tpk channel` — build and sign the channel manifest a CDN serves.
//!
//! Every per-pack field is read out of the pack itself. There is deliberately no
//! `--id` / `--kind` / `--version-code` flag: one mistyped CI variable would
//! otherwise publish a DLC as a core patch, with a valid signature, and `kind`
//! decides layer order — a pack mislabelled `dlc` shadows every patch beneath
//! it, security fixes included.

use std::path::PathBuf;

use tpk_format::channel::{ChannelManifest, PackRef, CHANNEL_SPEC_TAG};
use tpk_format::container::UnverifiedPack;
use tpk_format::manifest::{PackKind, PackManifest};
use tpk_format::sign::sha256_hex;

use crate::key_source::{load_signing_key, write_detached_signature};
use crate::{CliError, CliResult};

#[derive(clap::Args)]
pub struct Args {
    /// Channel name, e.g. `stable`.
    #[arg(long)]
    pub channel: String,

    /// Packs to advertise. Repeat once per pack.
    #[arg(long = "pack", required = true)]
    pub packs: Vec<PathBuf>,

    /// URL prefix the packs are served from, e.g. `https://cdn.example.com/tpk/core/`.
    #[arg(long)]
    pub url_base: String,

    /// Freshness marker. `auto` uses UTC `YYYYMMDDHHMM`.
    #[arg(long, default_value = "auto")]
    pub watermark: String,

    /// Signing key generation this manifest is signed with.
    #[arg(long, default_value_t = 1)]
    pub key_epoch: u32,

    /// RFC 3339 publication timestamp. Defaults to the watermark's minute.
    #[arg(long)]
    pub published_at: Option<String>,

    /// Staged rollout percentage applied to every pack.
    #[arg(long, default_value_t = 100)]
    pub rollout: u8,

    /// Lowest shell version. Defaults to the highest `min_shell` among the packs.
    #[arg(long)]
    pub min_shell: Option<String>,

    /// Block all content for shells below this version.
    ///
    /// A blunt instrument: it stops hotfixes too. Use it when the shell should
    /// not receive content at all, not to nudge people to upgrade.
    #[arg(long)]
    pub force_shell: Option<String>,

    /// Release note. Truncated to 200 characters, and not meant for end users.
    #[arg(long)]
    pub notes: Option<String>,

    /// Where to write the manifest.
    #[arg(long)]
    pub out: PathBuf,
}

pub fn run(args: &Args) -> CliResult {
    if !(1..=100).contains(&args.rollout) {
        return Err(CliError::usage(format!(
            "--rollout {} is outside 1..=100",
            args.rollout
        )));
    }
    let key = load_signing_key()?;
    let watermark = resolve_watermark(&args.watermark)?;

    let url_base = if args.url_base.ends_with('/') {
        args.url_base.clone()
    } else {
        format!("{}/", args.url_base)
    };
    if !url_base.starts_with("https://") {
        return Err(CliError::usage(
            "--url-base must be https; clients reject plaintext sources",
        ));
    }

    let mut refs = Vec::with_capacity(args.packs.len());
    let mut highest_min_shell: Option<semver::Version> = None;

    for path in &args.packs {
        let bytes = std::fs::read(path)
            .map_err(|e| CliError::usage(format!("cannot read {}: {e}", path.display())))?;
        let unverified = UnverifiedPack::open(path)
            .map_err(|e| CliError::verification(format!("{}: {e}", path.display())))?;
        let manifest = PackManifest::parse(unverified.raw_manifest_bytes())
            .map_err(|e| CliError::verification(format!("{}: {e}", path.display())))?;

        let file_name = path
            .file_name()
            .ok_or_else(|| CliError::usage(format!("{} has no file name", path.display())))?
            .to_string_lossy()
            .to_string();

        if let Some(min) = &manifest.min_shell {
            if highest_min_shell.as_ref().is_none_or(|cur| min > cur) {
                highest_min_shell = Some(min.clone());
            }
        }

        refs.push(PackRef {
            id: manifest.id.clone(),
            kind: manifest.kind,
            version: manifest.version.clone(),
            version_code: manifest.version_code,
            parent_version_code: match manifest.kind {
                PackKind::Patch => manifest.parent.as_ref().map(|p| p.version_code),
                _ => None,
            },
            url: format!("{url_base}{file_name}"),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            optional: manifest.kind == PackKind::Dlc,
            rollout: args.rollout,
        });
    }

    let min_shell = match &args.min_shell {
        Some(v) => Some(
            v.parse()
                .map_err(|e| CliError::usage(format!("--min-shell is not SemVer: {e}")))?,
        ),
        None => highest_min_shell,
    };
    let force_shell = match &args.force_shell {
        Some(v) => Some(
            v.parse()
                .map_err(|e| CliError::usage(format!("--force-shell is not SemVer: {e}")))?,
        ),
        None => None,
    };

    let manifest = ChannelManifest {
        spec: CHANNEL_SPEC_TAG.to_string(),
        channel: args.channel.clone(),
        published_at: args
            .published_at
            .clone()
            .unwrap_or_else(|| watermark_to_rfc3339(watermark)),
        watermark,
        key_epoch: args.key_epoch,
        min_shell,
        force_shell,
        notes: args.notes.clone(),
        packs: refs,
    };

    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| CliError::usage(format!("cannot serialize manifest: {e}")))?;

    // Round-trip before publishing: a manifest the client would reject must fail
    // here, not after it is live on the CDN.
    ChannelManifest::parse(&bytes).map_err(|e| CliError::verification(e.to_string()))?;

    if let Some(dir) = args.out.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|e| CliError::usage(format!("cannot create {}: {e}", dir.display())))?;
        }
    }
    std::fs::write(&args.out, &bytes)
        .map_err(|e| CliError::usage(format!("cannot write {}: {e}", args.out.display())))?;

    let signature = key.sign(
        &bytes,
        &format!("tpk channel {} watermark {watermark}", args.channel),
        "signature from tpk",
    );
    write_detached_signature(&args.out, &signature)?;

    println!("wrote    {}", args.out.display());
    println!("channel  {}", manifest.channel);
    println!("watermark {watermark}");
    println!("packs    {}", manifest.packs.len());
    if args.rollout < 100 {
        println!("rollout  {}%", args.rollout);
    }
    Ok(())
}

fn resolve_watermark(spec: &str) -> Result<u64, CliError> {
    if spec != "auto" {
        return spec
            .parse()
            .map_err(|e| CliError::usage(format!("--watermark must be a number or `auto`: {e}")));
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| CliError::usage(format!("system clock is before the epoch: {e}")))?
        .as_secs();
    let (y, mo, d, h, mi) = civil_from_unix(secs);
    Ok(y as u64 * 100_000_000
        + mo as u64 * 1_000_000
        + d as u64 * 10_000
        + h as u64 * 100
        + mi as u64)
}

fn watermark_to_rfc3339(watermark: u64) -> String {
    let (y, rest) = (watermark / 100_000_000, watermark % 100_000_000);
    let (mo, rest) = (rest / 1_000_000, rest % 1_000_000);
    let (d, rest) = (rest / 10_000, rest % 10_000);
    let (h, mi) = (rest / 100, rest % 100);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:00Z")
}

/// Days-from-civil, inverted — Howard Hinnant's algorithm.
///
/// Avoids a date-library dependency for the one place a timestamp is produced.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    (
        y,
        m,
        d,
        (secs_of_day / 3600) as u32,
        ((secs_of_day % 3600) / 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_matches_known_instants() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0));
        // 2026-09-11T15:30:00Z
        assert_eq!(civil_from_unix(1_789_140_600), (2026, 9, 11, 15, 30));
        // A leap day.
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0));
    }

    #[test]
    fn watermark_and_timestamp_agree() {
        assert_eq!(
            watermark_to_rfc3339(202_609_111_530),
            "2026-09-11T15:30:00Z"
        );
    }

    #[test]
    fn explicit_watermark_is_passed_through() {
        assert_eq!(resolve_watermark("202609111500").unwrap(), 202_609_111_500);
        assert!(resolve_watermark("not-a-number").is_err());
    }

    #[test]
    fn auto_watermark_has_the_expected_shape() {
        let w = resolve_watermark("auto").unwrap();
        assert!(
            (202_000_000_000..300_000_000_000).contains(&w),
            "unexpected watermark {w}"
        );
    }
}
