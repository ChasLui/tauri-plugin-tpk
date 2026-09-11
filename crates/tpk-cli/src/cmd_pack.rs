//! `tpk pack` — turn a build output directory into a signed `.tpk`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tpk_delta::DeltaPolicy;
use tpk_format::container::UnverifiedPack;
use tpk_format::manifest::{Op, PackId, PackKind, PackManifest, ParentRef};
use tpk_format::pack::PackBuilder;
use tpk_format::sign::sha256_hex;

use crate::html_policy::{check_extension, check_html};
use crate::key_source;
use crate::{CliError, CliResult};

/// Ceiling on how much of a pack may be deltas.
///
/// Every delta costs a bsdiff run when the pack is staged, and that work is
/// bounded by whatever the publisher put in the pack. Capping it here makes the
/// worst case on the slowest device finite.
const MAX_DELTA_ENTRIES: usize = 64;
/// Companion ceiling on the reconstructed bytes those deltas represent.
const MAX_DELTA_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(clap::Args)]
pub struct Args {
    /// What this pack contributes.
    #[arg(long, value_enum)]
    pub kind: Kind,

    /// Pack id: `[a-z0-9][a-z0-9-]{0,62}`.
    #[arg(long)]
    pub id: String,

    /// Display version (SemVer).
    #[arg(long)]
    pub version: String,

    /// Monotonic ordering key. Never reuse or lower it.
    ///
    /// UTC `YYYYMMDDHHMMSS` is the recommended source: monotonic, stateless,
    /// and unaffected by moving the repository or rebuilding CI.
    #[arg(long)]
    pub version_code: u64,

    /// RFC 3339 build timestamp.
    ///
    /// Required rather than defaulted to `now()` so packing the same input twice
    /// produces the same bytes — which is what the blacklist and the channel
    /// manifest's sha256 depend on. Use `date -u +%Y-%m-%dT%H:%M:%SZ`.
    #[arg(long)]
    pub created_at: String,

    /// Lowest shell version this pack supports.
    #[arg(long)]
    pub min_shell: Option<String>,

    /// Highest shell version. Leave unset unless a specific break is known.
    #[arg(long)]
    pub max_shell: Option<String>,

    /// Channel this pack is built for.
    #[arg(long)]
    pub channel: Option<String>,

    /// Directory whose contents become the pack.
    #[arg(long)]
    pub dist: PathBuf,

    /// Parent `.tpk`. Required for `--kind patch`; enables delta entries.
    #[arg(long)]
    pub parent: Option<PathBuf>,

    /// Files below this size are never deltified.
    #[arg(long, default_value_t = 262_144)]
    pub delta_threshold: usize,

    /// Where to write the pack.
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Kind {
    Base,
    Patch,
    Dlc,
    Mod,
}

impl From<Kind> for PackKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Base => Self::Base,
            Kind::Patch => Self::Patch,
            Kind::Dlc => Self::Dlc,
            Kind::Mod => Self::Mod,
        }
    }
}

pub fn run(args: &Args) -> CliResult {
    let key = key_source::load_signing_key()?;
    let id = PackId::parse(&args.id).map_err(|e| CliError::usage(e.to_string()))?;
    let version = args
        .version
        .parse()
        .map_err(|e| CliError::usage(format!("--version is not SemVer: {e}")))?;

    let files = collect_files(&args.dist)?;
    if files.is_empty() {
        return Err(CliError::usage(format!(
            "{} contains no files",
            args.dist.display()
        )));
    }

    for (path, content) in &files {
        check_extension(path).map_err(|v| CliError::verification(format!("{path}: {v}")))?;
        if path.ends_with(".html") || path.ends_with(".htm") {
            check_html(content).map_err(|v| CliError::verification(format!("{path}: {v}")))?;
        }
    }

    let parent = match &args.parent {
        Some(path) => Some(load_parent(path)?),
        None => None,
    };

    let mut builder = PackBuilder::new(
        args.kind.into(),
        id.clone(),
        version,
        args.version_code,
        args.created_at.clone(),
    );
    if let Some(v) = &args.min_shell {
        builder = builder.min_shell(
            v.parse()
                .map_err(|e| CliError::usage(format!("--min-shell is not SemVer: {e}")))?,
        );
    }
    if let Some(v) = &args.max_shell {
        eprintln!(
            "warning: --max-shell pins this pack to shells at or below {v}; \
             once a newer shell ships, those users silently fall back to embedded assets"
        );
        builder = builder.max_shell(
            v.parse()
                .map_err(|e| CliError::usage(format!("--max-shell is not SemVer: {e}")))?,
        );
    }
    if let Some(c) = &args.channel {
        builder = builder.channel(c.clone());
    }
    if let Some(parent) = &parent {
        if parent.manifest.id != id {
            return Err(CliError::usage(format!(
                "parent id {} does not match --id {id}",
                parent.manifest.id
            )));
        }
        builder = builder.parent(ParentRef {
            id: parent.manifest.id.clone(),
            version: parent.manifest.version.clone(),
            version_code: parent.manifest.version_code,
            manifest_sha256: parent.manifest_sha256,
        });
    }

    let policy = DeltaPolicy {
        min_file_size: args.delta_threshold,
        ..DeltaPolicy::default()
    };
    let mut delta_count = 0usize;
    let mut delta_bytes = 0u64;
    let mut deltas_declined = 0usize;

    for (path, content) in &files {
        let from_parent = parent.as_ref().and_then(|p| p.contents.get(path));

        let use_delta = match from_parent {
            Some(base) if policy.is_candidate(content.len()) && base != content => {
                if delta_count >= MAX_DELTA_ENTRIES
                    || delta_bytes + content.len() as u64 > MAX_DELTA_OUTPUT_BYTES
                {
                    deltas_declined += 1;
                    None
                } else {
                    let stream = tpk_delta::diff(base, content)
                        .map_err(|e| CliError::verification(format!("{path}: {e}")))?;
                    // Compare against the compressed size the blob will actually
                    // have; a raw bsdiff stream is about as large as the file.
                    let compressed = zstd_len(&stream)?;
                    if policy.is_worthwhile(compressed, content.len()) {
                        Some((stream, sha256_hex(base)))
                    } else {
                        deltas_declined += 1;
                        None
                    }
                }
            }
            _ => None,
        };

        match use_delta {
            Some((stream, base_sha)) => {
                builder
                    .add_delta(path, &stream, content, base_sha)
                    .map_err(|e| CliError::verification(format!("{path}: {e}")))?;
                delta_count += 1;
                delta_bytes += content.len() as u64;
            }
            None => builder
                .add_full(path, content)
                .map_err(|e| CliError::verification(format!("{path}: {e}")))?,
        }
    }

    // Anything the parent had and this build does not is a deletion; without
    // tombstones the old file would keep showing through from the layer below.
    if let Some(parent) = &parent {
        for path in parent.contents.keys() {
            if !files.contains_key(path) {
                builder
                    .add_delete(path)
                    .map_err(|e| CliError::verification(format!("{path}: {e}")))?;
            }
        }
    }

    if let Some(dir) = args.out.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|e| CliError::usage(format!("cannot create {}: {e}", dir.display())))?;
        }
    }

    let summary = builder
        .build(&key, &args.out)
        .map_err(|e| CliError::verification(e.to_string()))?;

    println!("wrote    {}", args.out.display());
    println!("sha256   {}", summary.file_sha256);
    println!("size     {} bytes", summary.file_size);
    println!(
        "entries  {} full, {} delta, {} delete",
        summary.full_entries, summary.delta_entries, summary.delete_entries
    );
    if deltas_declined > 0 {
        println!("note     {deltas_declined} candidate(s) shipped as full instead of delta");
    }
    Ok(())
}

struct Parent {
    manifest: PackManifest,
    manifest_sha256: tpk_format::manifest::Sha256Hex,
    contents: BTreeMap<String, Vec<u8>>,
}

/// Read a parent pack's decoded contents so this build can diff against them.
///
/// The signature is not checked here: `tpk verify` is the gate for that, and a
/// publisher diffing against their own artefact is not a trust boundary. The
/// per-entry hashes inside the container are still enforced by `read_blob`.
fn load_parent(path: &Path) -> Result<Parent, CliError> {
    let unverified = UnverifiedPack::open(path)
        .map_err(|e| CliError::verification(format!("{}: {e}", path.display())))?;
    let manifest_sha256 = unverified.manifest_sha256();
    let manifest = PackManifest::parse(unverified.raw_manifest_bytes())
        .map_err(|e| CliError::verification(format!("{}: {e}", path.display())))?;

    if manifest.entries.iter().any(|e| e.op == Op::Delta) {
        return Err(CliError::usage(format!(
            "{} contains delta entries; diff against a base pack, or against the \
             resolved tree, not against a patch",
            path.display()
        )));
    }

    let mut pack = unverified
        .into_local_reader()
        .map_err(|e| CliError::verification(format!("{}: {e}", path.display())))?;

    let mut contents = BTreeMap::new();
    for entry in &manifest.entries {
        if entry.op == Op::Full {
            let bytes = pack
                .read_blob(entry)
                .map_err(|e| CliError::verification(format!("{}: {e}", path.display())))?;
            contents.insert(entry.path.as_str().to_string(), bytes);
        }
    }
    Ok(Parent {
        manifest,
        manifest_sha256,
        contents,
    })
}

/// Walk `dist`, returning POSIX pack paths mapped to file contents.
fn collect_files(dist: &Path) -> Result<BTreeMap<String, Vec<u8>>, CliError> {
    if !dist.is_dir() {
        return Err(CliError::usage(format!(
            "{} is not a directory",
            dist.display()
        )));
    }
    let mut out = BTreeMap::new();
    walk(dist, dist, &mut out)?;
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<(), CliError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| CliError::usage(format!("cannot read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| CliError::usage(e.to_string()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| CliError::usage(e.to_string()))?;

        if file_type.is_symlink() {
            // A symlink has no representation in the format, and following one
            // could pull in bytes from outside the build directory.
            return Err(CliError::usage(format!(
                "{} is a symlink; packs contain plain files only",
                path.display()
            )));
        }
        if file_type.is_dir() {
            walk(root, &path, out)?;
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .map_err(|e| CliError::usage(e.to_string()))?;
        let mut pack_path = String::from("/");
        let mut first = true;
        for component in rel.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(CliError::usage(format!(
                    "unexpected path component in {}",
                    path.display()
                )));
            };
            let name = name
                .to_str()
                .ok_or_else(|| CliError::usage(format!("{} is not valid UTF-8", path.display())))?;
            if !first {
                pack_path.push('/');
            }
            pack_path.push_str(name);
            first = false;
        }

        let content = std::fs::read(&path)
            .map_err(|e| CliError::usage(format!("cannot read {}: {e}", path.display())))?;
        out.insert(pack_path, content);
    }
    Ok(())
}

fn zstd_len(bytes: &[u8]) -> Result<usize, CliError> {
    tpk_format::pack::compressed_size(bytes).map_err(|e| CliError::verification(e.to_string()))
}
