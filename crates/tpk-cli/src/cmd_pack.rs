//! `tpk pack` — turn a build output directory into a signed `.tpk`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tpk_delta::DeltaPolicy;
use tpk_format::container::{UnverifiedPack, VerifiedPack};
use tpk_format::error::FormatError;
use tpk_format::manifest::{Entry, Op, PackId, PackKind, PackManifest, ParentRef};
use tpk_format::pack::PackBuilder;
use tpk_format::sign::sha256_hex;

use crate::html_policy::{check_extension, check_html, check_worker_sources};
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
/// Largest decoded entry a `--parent` may declare.
///
/// Parents are not signature-checked, and both zstd decoding and bsdiff
/// reconstruction size their output from the manifest's `size`. This matches
/// the desktop `tpk_store::MAX_ASSET_BYTES`: no runtime would serve a larger
/// asset, so no real parent needs one.
const MAX_PARENT_ASSET_BYTES: u64 = 64 * 1024 * 1024;
/// Total bytes a `--parent` chain may decode, summed over every pack in it.
///
/// [`MAX_PARENT_ASSET_BYTES`] bounds one entry, not their number, so without
/// this a hostile parent listing many near-limit entries could still exhaust
/// memory. The count is cumulative — full contents, delta streams and rebuilt
/// files, including bytes a later patch replaces — so it bounds decode work as
/// well as the resolved tree. Every patch carries the whole dist (unchanged
/// files as `full`, spec §10.3), so a chain costs roughly dist size × links:
/// 1 GiB fits about 16 links of a 64 MiB dist or 100 links of a 10 MiB one.
/// A chain longer than that should be collapsed by publishing a new base.
const MAX_PARENT_CHAIN_BYTES: u64 = 1024 * 1024 * 1024;
/// Extensions whose contents are scanned for worker and remote-import sources.
///
/// What a Vite/Rollup, webpack or esbuild dist actually emits: `.js` for the
/// app and its chunks, `.mjs` when the output format is ESM and the host page
/// is not `type=module`, and `.cjs` for a CommonJS build or a Node-side
/// sidecar. Anything else in a dist is data, not a script Tauri will execute.
const JS_EXTENSIONS: [&str; 3] = [".js", ".mjs", ".cjs"];

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
    ///
    /// Repeat to give a chain, lowest first: the base, then each patch on it in
    /// order. Deltas and tombstones are computed against the resolved chain, and
    /// the new pack's parent link points at the last one.
    #[arg(long)]
    pub parent: Vec<PathBuf>,

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
    // Removed with `PackKind::{Dlc, Mod}` under the `app-store` feature; clap
    // derives `--kind`'s accepted values from these variants, so the usage
    // error names exactly the kinds the build supports.
    #[cfg(not(app_store))]
    Dlc,
    #[cfg(not(app_store))]
    Mod,
}

impl From<Kind> for PackKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Base => Self::Base,
            Kind::Patch => Self::Patch,
            #[cfg(not(app_store))]
            Kind::Dlc => Self::Dlc,
            #[cfg(not(app_store))]
            Kind::Mod => Self::Mod,
        }
    }
}

pub fn run(args: &Args) -> CliResult {
    // Only a patch stacks on its parent; any other kind replaces the same-id
    // layers when staged, so its deltas and tombstones would have nothing below.
    if !args.parent.is_empty() && !matches!(args.kind, Kind::Patch) {
        return Err(CliError::verification(
            "--parent is only valid with --kind patch; base, dlc and mod packs \
             replace the layers of their id and cannot diff against a parent",
        ));
    }

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

    let mut unjudged_workers = 0usize;
    let mut unjudged_worker_files: Vec<&str> = Vec::new();
    for (path, content) in &files {
        check_extension(path).map_err(|v| CliError::verification(format!("{path}: {v}")))?;
        if path.ends_with(".html") || path.ends_with(".htm") {
            check_html(content).map_err(|v| CliError::verification(format!("{path}: {v}")))?;
        }
        if JS_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
            let unjudged = check_worker_sources(content)
                .map_err(|v| CliError::verification(format!("{path}: {v}")))?;
            if unjudged > 0 {
                unjudged_workers += unjudged;
                unjudged_worker_files.push(path);
            }
        }
    }
    if !unjudged_worker_files.is_empty() {
        eprintln!(
            "warning: {unjudged_workers} worker source(s) are computed at runtime and cannot \
             be checked for remote code; audit them in: {}",
            unjudged_worker_files.join(", ")
        );
    }

    let parent = load_parents(&args.parent, MAX_PARENT_CHAIN_BYTES)?;

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
            return Err(CliError::verification(format!(
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

/// Resolve a parent chain, lowest first, into the file tree it produces.
///
/// Each pack is applied over the previous result the way the runtime overlays
/// layers: `full` replaces, `delete` removes, `delta` patches the path's current
/// bytes. The returned manifest is the last parent's, which is what the new
/// pack links to.
///
/// Signatures are not checked here: `tpk verify` is the gate for that, and a
/// publisher diffing against their own artefacts is not a trust boundary. The
/// per-entry hashes inside the container are still enforced by `read_blob`,
/// every rebuilt delta is checked against its declared digest, and declared
/// sizes are capped at [`MAX_PARENT_ASSET_BYTES`] so a hostile file cannot make
/// the build machine allocate without bound. Every decoded buffer across the
/// whole chain also counts against `budget` (see [`MAX_PARENT_CHAIN_BYTES`]),
/// charged before the allocation wherever the manifest declares its size.
fn load_parents(paths: &[PathBuf], budget: u64) -> Result<Option<Parent>, CliError> {
    let mut resolved: Option<Parent> = None;
    let mut spent = 0u64;
    for path in paths {
        let fail =
            |e: &dyn std::fmt::Display| CliError::verification(format!("{}: {e}", path.display()));
        let charge = |spent: &mut u64, bytes: u64| {
            *spent = spent.saturating_add(bytes);
            if *spent > budget {
                return Err(fail(&format_args!(
                    "parent chain decodes to more than the {budget} byte budget"
                )));
            }
            Ok(())
        };
        // Decoding stops at what is left of the budget, so an oversized delta
        // stream is cut off mid-decode rather than charged once fully in memory.
        let read = |pack: &mut VerifiedPack, entry: &Entry, spent: u64| {
            pack.read_blob_bounded(entry, budget.saturating_sub(spent))
                .map_err(|e| match e {
                    FormatError::Spec(m) if m.starts_with("decode budget of ") => {
                        fail(&format_args!(
                            "parent chain decodes to more than the {budget} byte budget \
                         (while decoding {})",
                            entry.path
                        ))
                    }
                    e => fail(&e),
                })
        };
        let unverified = UnverifiedPack::open(path).map_err(|e| fail(&e))?;
        let manifest_sha256 = unverified.manifest_sha256();
        let manifest =
            PackManifest::parse(unverified.raw_manifest_bytes()).map_err(|e| fail(&e))?;
        manifest
            .validate_size_limit(MAX_PARENT_ASSET_BYTES)
            .map_err(|e| fail(&e))?;

        let mut contents = match resolved {
            None => {
                // A chain starts at a base; a patch alone would leave every
                // path it does not list, and every delta base, unresolved.
                if manifest.kind != PackKind::Base {
                    return Err(fail(&format_args!(
                        "the first --parent must be a base, not a {:?}; pass the \
                         chain it stacks on as earlier --parent flags",
                        manifest.kind
                    )));
                }
                BTreeMap::new()
            }
            Some(prev) => {
                let linked = manifest.kind == PackKind::Patch
                    && manifest.id == prev.manifest.id
                    && manifest.parent.as_ref().is_some_and(|p| {
                        p.version_code == prev.manifest.version_code
                            && p.manifest_sha256 == prev.manifest_sha256
                    });
                if !linked {
                    return Err(fail(&format_args!(
                        "not a patch on the preceding --parent ({} {})",
                        prev.manifest.id, prev.manifest.version_code
                    )));
                }
                prev.contents
            }
        };

        let mut pack = unverified.into_local_reader().map_err(|e| fail(&e))?;
        for entry in &manifest.entries {
            let key = entry.path.as_str().to_string();
            match entry.op {
                Op::Full => {
                    // An identity blob is read at `blob_size`, a zstd one
                    // decodes to `size`; charge the larger before either.
                    let declared = entry.size.max(entry.blob_size).unwrap_or(0);
                    let before = spent;
                    charge(&mut spent, declared)?;
                    let bytes = read(&mut pack, entry, before)?;
                    contents.insert(key, bytes);
                }
                Op::Delete => {
                    contents.remove(&key);
                }
                Op::Delta => {
                    let rebuilt = {
                        let base = contents
                            .get(&key)
                            .ok_or_else(|| fail(&format_args!("{key} has no delta base")))?;
                        if Some(sha256_hex(base)) != entry.delta_base_sha256 {
                            return Err(fail(&format_args!("{key} delta base does not match")));
                        }
                        let stream = read(&mut pack, entry, spent)?;
                        // The stream's decoded length is only known after reading.
                        charge(&mut spent, stream.len() as u64)?;
                        let size = entry
                            .size
                            .and_then(|s| usize::try_from(s).ok())
                            .ok_or_else(|| fail(&format_args!("{key} has no usable size")))?;
                        charge(&mut spent, size as u64)?;
                        let limit = usize::try_from(MAX_PARENT_ASSET_BYTES).unwrap_or(usize::MAX);
                        tpk_delta::apply(base, &stream, size, limit)
                            .map_err(|e| fail(&format_args!("{key}: {e}")))?
                    };
                    if Some(sha256_hex(&rebuilt)) != entry.sha256 {
                        return Err(fail(&format_args!("{key} rebuilt to the wrong content")));
                    }
                    contents.insert(key, rebuilt);
                }
            }
        }
        resolved = Some(Parent {
            manifest,
            manifest_sha256,
            contents,
        });
    }
    Ok(resolved)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tpk_format::secret::SecretKey;

    const SIZE: usize = 4096;

    /// Incompressible bytes, so the blob is stored at its full size.
    fn noise(seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..SIZE)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x.to_le_bytes()[0]
            })
            .collect()
    }

    /// Write a base holding `/a.js` = `base`, then a patch changing it to
    /// `next`, as a delta or as a full replacement.
    fn chain(dir: &Path, base: &[u8], next: &[u8], delta: bool) -> (PathBuf, PathBuf, usize) {
        let key = SecretKey::generate();
        let id = PackId::parse("app").unwrap();
        let base_path = dir.join("base.tpk");
        let mut b = PackBuilder::new(
            PackKind::Base,
            id.clone(),
            "1.0.0".parse().unwrap(),
            1,
            "2026-09-11T15:00:00Z",
        );
        b.add_full("/a.js", base).unwrap();
        b.build(&key, &base_path).unwrap();

        let manifest_sha256 = UnverifiedPack::open(&base_path).unwrap().manifest_sha256();
        let mut p = PackBuilder::new(
            PackKind::Patch,
            id.clone(),
            "1.0.1".parse().unwrap(),
            2,
            "2026-09-11T15:00:00Z",
        )
        .parent(ParentRef {
            id,
            version: "1.0.0".parse().unwrap(),
            version_code: 1,
            manifest_sha256,
        });
        let mut stream_len = 0;
        if delta {
            let stream = tpk_delta::diff(base, next).unwrap();
            stream_len = stream.len();
            p.add_delta("/a.js", &stream, next, sha256_hex(base))
                .unwrap();
        } else {
            p.add_full("/a.js", next).unwrap();
        }
        let patch_path = dir.join("patch.tpk");
        p.build(&key, &patch_path).unwrap();
        (base_path, patch_path, stream_len)
    }

    #[test]
    fn parent_budget_is_cumulative_across_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let (base, patch, _) = chain(dir.path(), &noise(1), &noise(2), false);
        let budget = (SIZE + SIZE / 2) as u64;

        assert!(load_parents(std::slice::from_ref(&base), budget).is_ok());
        // The patch replaces the only file, so the resolved tree never holds
        // more than one copy; the budget still counts both decodes.
        let Err(err) = load_parents(&[base, patch], budget) else {
            panic!("chain over budget must be refused");
        };
        assert_eq!(err.exit_code, 2);
        assert!(err.message.contains("byte budget"), "{}", err.message);
    }

    #[test]
    fn parent_budget_counts_delta_streams_and_rebuilt_output() {
        let dir = tempfile::tempdir().unwrap();
        let base = noise(3);
        let mut next = base.clone();
        next[..64].fill(0);
        let (base, patch, stream_len) = chain(dir.path(), &base, &next, true);
        let exact = (SIZE + stream_len + SIZE) as u64;

        assert!(load_parents(&[base.clone(), patch.clone()], exact).is_ok());
        let Err(err) = load_parents(&[base, patch], exact - 1) else {
            panic!("one byte over budget must be refused");
        };
        assert_eq!(err.exit_code, 2);
        assert!(err.message.contains("byte budget"), "{}", err.message);
    }

    #[test]
    fn parent_budget_cuts_a_delta_stream_off_mid_decode() {
        let dir = tempfile::tempdir().unwrap();
        let base = noise(4);
        let mut next = base.clone();
        next[..64].fill(0);
        let (base, patch, stream_len) = chain(dir.path(), &base, &next, true);
        // One byte short of the stream itself, before the rebuilt output.
        let budget = (SIZE + stream_len - 1) as u64;

        let Err(err) = load_parents(&[base, patch], budget) else {
            panic!("a stream longer than the remaining budget must be refused");
        };
        assert_eq!(err.exit_code, 2);
        assert!(
            err.message.contains("byte budget (while decoding /a.js)"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_blob_over_its_declared_size_is_not_blamed_on_the_budget() {
        use std::io::{Read, Write};

        let dir = tempfile::tempdir().unwrap();
        let honest = dir.path().join("honest.tpk");
        // Compressible and above the compression threshold, so stored as zstd.
        let content = vec![b'a'; 64 * 1024];
        let mut b = PackBuilder::new(
            PackKind::Base,
            PackId::parse("app").unwrap(),
            "1.0.0".parse().unwrap(),
            1,
            "2026-09-11T15:00:00Z",
        );
        b.add_full("/a.js", &content).unwrap();
        b.build(&SecretKey::generate(), &honest).unwrap();

        // Understate the size: the zstd frame now decodes past what it declares.
        let hostile = dir.path().join("hostile.tpk");
        let mut src = zip::ZipArchive::new(std::fs::File::open(&honest).unwrap()).unwrap();
        let mut dst = zip::ZipWriter::new(std::fs::File::create(&hostile).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for i in 0..src.len() {
            let mut entry = src.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == "tpk-manifest.json" {
                let mut m: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(m["entries"][0]["encoding"], "zstd");
                m["entries"][0]["size"] = serde_json::json!(1024);
                bytes = serde_json::to_vec(&m).unwrap();
            }
            dst.start_file(name, opts).unwrap();
            dst.write_all(&bytes).unwrap();
        }
        dst.finish().unwrap();

        let Err(err) = load_parents(std::slice::from_ref(&hostile), u64::MAX) else {
            panic!("a blob over its declared size must be refused");
        };
        assert_eq!(err.exit_code, 2);
        assert!(!err.message.contains("budget"), "{}", err.message);
        assert!(
            err.message.contains("declared 1024 byte limit"),
            "{}",
            err.message
        );
    }
}
