//! The publishing chain end to end: pack → sign → channel → verify.
//!
//! This is the sequence CI runs before anything reaches a CDN (spec section 10),
//! so it is exercised here as one flow rather than as isolated commands.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tpk_format::secret::SecretKey;

const CREATED_AT: &str = "2026-09-11T15:00:00Z";

struct Env {
    dir: tempfile::TempDir,
    key_file: String,
    pubkey: String,
}

impl Env {
    fn new() -> Self {
        let key = SecretKey::generate();
        Self {
            dir: tempfile::tempdir().unwrap(),
            key_file: key.to_key_file("e2e"),
            pubkey: key.public_key_base64(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn tpk(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tpk"))
            .args(args)
            .env("TPK_SIGNING_KEY", &self.key_file)
            .current_dir(self.dir.path())
            .output()
            .unwrap()
    }

    /// Run a command that is expected to succeed, returning stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.tpk(args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`tpk {}` failed\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn write_dist(&self, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let root = self.path(name);
        for (rel, content) in files {
            let full = root.join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, content).unwrap();
        }
        root
    }
}

fn field(stdout: &str, key: &str) -> String {
    stdout
        .lines()
        .find(|l| l.starts_with(key))
        .unwrap_or_else(|| panic!("no `{key}` line in:\n{stdout}"))
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

fn base_dist(env: &Env, name: &str) -> PathBuf {
    env.write_dist(
        name,
        &[
            (
                "index.html",
                br#"<!doctype html><html><head><script src="/app.js"></script></head><body></body></html>"#,
            ),
            ("app.js", b"export const version = 1;"),
            ("assets/big.txt", &b"the original payload line\n".repeat(20_000)),
        ],
    )
}

#[test]
fn pack_sign_channel_verify() {
    let env = Env::new();
    let dist = base_dist(&env, "dist");
    let pack = env.path("out/base-core-1.0.0.tpk");

    let out = env.ok(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--min-shell",
        "2.3.0",
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        pack.to_str().unwrap(),
    ]);
    assert!(out.contains("3 full"), "{out}");
    assert!(pack.exists());

    env.ok(&[
        "verify",
        "--pubkey",
        &env.pubkey,
        "--file",
        pack.to_str().unwrap(),
    ]);

    let channel = env.path("out/latest.json");
    let out = env.ok(&[
        "channel",
        "--channel",
        "stable",
        "--pack",
        pack.to_str().unwrap(),
        "--url-base",
        "https://cdn.example.com/tpk/core/",
        "--watermark",
        "202609111500",
        "--out",
        channel.to_str().unwrap(),
    ]);
    assert!(out.contains("watermark 202609111500"), "{out}");
    assert!(env.path("out/latest.json.minisig").exists());

    env.ok(&[
        "verify",
        "--pubkey",
        &env.pubkey,
        "--file",
        channel.to_str().unwrap(),
    ]);

    // The published manifest must describe the pack exactly, since the client
    // checks the downloaded bytes against it.
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&channel).unwrap()).unwrap();
    assert_eq!(manifest["packs"][0]["id"], "core");
    assert_eq!(manifest["packs"][0]["kind"], "base");
    assert_eq!(manifest["packs"][0]["version_code"], 20260911150000u64);
    assert_eq!(
        manifest["packs"][0]["url"],
        "https://cdn.example.com/tpk/core/base-core-1.0.0.tpk"
    );
    assert_eq!(manifest["min_shell"], "2.3.0", "inherited from the pack");
    assert_eq!(
        manifest["packs"][0]["size"].as_u64().unwrap(),
        std::fs::metadata(&pack).unwrap().len()
    );
}

#[test]
fn a_patch_carries_deltas_and_tombstones() {
    let env = Env::new();
    let base_pack = env.path("out/base.tpk");
    env.ok(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        base_dist(&env, "dist-v1").to_str().unwrap(),
        "--out",
        base_pack.to_str().unwrap(),
    ]);

    // v2: edit the big file, change a small one, drop another.
    let mut edited = b"the original payload line\n".repeat(20_000);
    edited[0..10].copy_from_slice(b"CHANGED!!!");
    let dist2 = env.write_dist(
        "dist-v2",
        &[
            (
                "index.html",
                br#"<!doctype html><html><head><script src="/app.js"></script></head><body></body></html>"#,
            ),
            ("assets/big.txt", &edited),
        ],
    );

    let patch = env.path("out/patch.tpk");
    let out = env.ok(&[
        "pack",
        "--kind",
        "patch",
        "--id",
        "core",
        "--version",
        "1.0.1",
        "--version-code",
        "20260911160000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist2.to_str().unwrap(),
        "--parent",
        base_pack.to_str().unwrap(),
        "--out",
        patch.to_str().unwrap(),
    ]);
    assert!(out.contains("1 delta"), "big.txt should be a delta: {out}");
    assert!(
        out.contains("1 delete"),
        "app.js should be a tombstone: {out}"
    );

    env.ok(&[
        "verify",
        "--pubkey",
        &env.pubkey,
        "--file",
        patch.to_str().unwrap(),
    ]);

    let inspected = env.ok(&["inspect", patch.to_str().unwrap(), "--json"]);
    let manifest: serde_json::Value = serde_json::from_str(&inspected).unwrap();
    assert_eq!(manifest["kind"], "patch");
    assert_eq!(manifest["parent"]["version_code"], 20260911150000u64);

    let ops: Vec<&str> = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["op"].as_str().unwrap())
        .collect();
    assert!(ops.contains(&"delta"));
    assert!(ops.contains(&"delete"));

    // A delta must be materially smaller than the file it rebuilds — otherwise
    // the patch is pure overhead.
    let delta = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "delta")
        .unwrap();
    let (blob, size) = (
        delta["blob_size"].as_u64().unwrap(),
        delta["size"].as_u64().unwrap(),
    );
    assert!(blob * 10 < size, "delta blob {blob} vs file {size}");
}

#[test]
fn packing_twice_produces_identical_bytes() {
    let env = Env::new();
    let dist = base_dist(&env, "dist");

    let pack_to = |name: &str| -> (String, Vec<u8>) {
        let out_path = env.path(name);
        let stdout = env.ok(&[
            "pack",
            "--kind",
            "base",
            "--id",
            "core",
            "--version",
            "1.0.0",
            "--version-code",
            "20260911150000",
            "--created-at",
            CREATED_AT,
            "--dist",
            dist.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ]);
        (field(&stdout, "sha256"), std::fs::read(&out_path).unwrap())
    };

    let (sha_a, bytes_a) = pack_to("a.tpk");
    let (sha_b, bytes_b) = pack_to("b.tpk");

    // The blacklist matches packs by this hash. If it moved between runs, a bad
    // pack would escape it simply by being rebuilt.
    assert_eq!(
        sha_a, sha_b,
        "identical inputs must produce identical sha256"
    );
    assert_eq!(bytes_a, bytes_b);
}

#[test]
fn inline_scripts_are_rejected_at_pack_time() {
    let env = Env::new();
    let dist = env.write_dist(
        "dist",
        &[(
            "index.html",
            br#"<!doctype html><script>window.x = 1</script>"#,
        )],
    );
    let out = env.tpk(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        env.path("out.tpk").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("inline"), "{stderr}");
}

#[test]
fn remote_scripts_are_rejected_at_pack_time() {
    let env = Env::new();
    let dist = env.write_dist(
        "dist",
        &[(
            "index.html",
            br#"<!doctype html><script src="https://cdn.example.com/x.js"></script>"#,
        )],
    );
    let out = env.tpk(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        env.path("out.tpk").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("remote"));
}

#[test]
fn extensionless_files_are_rejected_at_pack_time() {
    let env = Env::new();
    let dist = env.write_dist("dist", &[("LICENSE", b"MIT")]);
    let out = env.tpk(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        env.path("out.tpk").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("extension"));
}

#[test]
fn packing_without_a_signing_key_fails_with_guidance() {
    let env = Env::new();
    let dist = base_dist(&env, "dist");
    let out = Command::new(env!("CARGO_BIN_EXE_tpk"))
        .args([
            "pack",
            "--kind",
            "base",
            "--id",
            "core",
            "--version",
            "1.0.0",
            "--version-code",
            "20260911150000",
            "--created-at",
            CREATED_AT,
            "--dist",
            dist.to_str().unwrap(),
            "--out",
            env.path("out.tpk").to_str().unwrap(),
        ])
        .env_remove("TPK_SIGNING_KEY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("TPK_SIGNING_KEY"), "{stderr}");
    assert!(
        stderr.contains("tpk keygen"),
        "must say how to fix it: {stderr}"
    );
}

#[test]
fn channel_refuses_a_plaintext_url_base() {
    let env = Env::new();
    let dist = base_dist(&env, "dist");
    let pack = env.path("p.tpk");
    env.ok(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        pack.to_str().unwrap(),
    ]);
    let out = env.tpk(&[
        "channel",
        "--channel",
        "stable",
        "--pack",
        pack.to_str().unwrap(),
        "--url-base",
        "http://cdn.example.com/tpk/core/",
        "--out",
        env.path("latest.json").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("https"));
}

#[test]
fn a_symlink_in_dist_is_refused() {
    let env = Env::new();
    let dist = env.write_dist("dist", &[("index.html", b"<!doctype html>")]);
    symlink_for_test(Path::new("/etc/hosts"), &dist.join("leak.txt"));

    let out = env.tpk(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        env.path("out.tpk").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("symlink"));
}

#[cfg(unix)]
fn symlink_for_test(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn symlink_for_test(target: &Path, link: &Path) {
    // Requires developer mode; skip the assertion rather than fail the suite.
    let _ = std::os::windows::fs::symlink_file(target, link);
}

#[test]
fn parent_is_refused_for_every_kind_but_patch() {
    let env = Env::new();
    let base_pack = env.path("out/base.tpk");
    env.ok(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        base_dist(&env, "dist-v1").to_str().unwrap(),
        "--out",
        base_pack.to_str().unwrap(),
    ]);

    // Same big file with an edit: without the guard this would become a delta.
    let mut edited = b"the original payload line\n".repeat(20_000);
    edited[0..10].copy_from_slice(b"CHANGED!!!");
    let dist2 = env.write_dist("dist-v2", &[("assets/big.txt", &edited)]);

    // `dlc` and `mod` are not values `--kind` accepts in an App Store build,
    // so there the usage error comes from clap, not from this rule.
    #[cfg(not(app_store))]
    let kinds = ["dlc", "mod", "base"];
    #[cfg(app_store)]
    let kinds = ["base"];

    for kind in kinds {
        let out_path = env.path(&format!("out/{kind}-with-parent.tpk"));
        let out = env.tpk(&[
            "pack",
            "--kind",
            kind,
            "--id",
            "core",
            "--version",
            "1.0.1",
            "--version-code",
            "20260911160000",
            "--created-at",
            CREATED_AT,
            "--dist",
            dist2.to_str().unwrap(),
            "--parent",
            base_pack.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{kind}: {stderr}");
        assert!(
            stderr.contains("--parent is only valid with --kind patch"),
            "{kind}: {stderr}"
        );
        assert!(!out_path.exists(), "{kind}: no pack may be written");
    }
}

/// `tpk pack` arguments shared by the chain tests.
fn pack_args<'a>(kind: &'a str, code: &'a str, dist: &'a str, out: &'a str) -> Vec<&'a str> {
    vec![
        "pack",
        "--kind",
        kind,
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        code,
        "--created-at",
        CREATED_AT,
        "--dist",
        dist,
        "--out",
        out,
    ]
}

/// Overlay packs lowest first, the way the runtime stacks layers.
fn resolve_chain(packs: &[&Path]) -> std::collections::BTreeMap<String, Vec<u8>> {
    use tpk_format::manifest::{Op, PackManifest};
    let mut tree = std::collections::BTreeMap::new();
    for path in packs {
        let unverified = tpk_format::container::UnverifiedPack::open(path).unwrap();
        let manifest = PackManifest::parse(unverified.raw_manifest_bytes()).unwrap();
        let mut reader = unverified.into_local_reader().unwrap();
        for entry in &manifest.entries {
            let key = entry.path.as_str().to_string();
            match entry.op {
                Op::Full => {
                    tree.insert(key, reader.read_blob(entry).unwrap());
                }
                Op::Delete => {
                    tree.remove(&key);
                }
                Op::Delta => {
                    let stream = reader.read_blob(entry).unwrap();
                    let size = entry.size.unwrap() as usize;
                    let rebuilt = tpk_delta::apply(&tree[&key], &stream, size, usize::MAX).unwrap();
                    assert_eq!(Some(tpk_format::sign::sha256_hex(&rebuilt)), entry.sha256);
                    tree.insert(key, rebuilt);
                }
            }
        }
    }
    tree
}

/// base1 → patch2 (delta on big.txt) → patch3, packed against the resolved chain.
fn build_chain(env: &Env) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let base1 = env.path("out/base1.tpk");
    let dist1 = base_dist(env, "dist1");
    env.ok(&pack_args(
        "base",
        "100",
        dist1.to_str().unwrap(),
        base1.to_str().unwrap(),
    ));

    let index: &[u8] =
        br#"<!doctype html><html><head><script src="/app.js"></script></head><body></body></html>"#;
    let mut big2 = b"the original payload line\n".repeat(20_000);
    big2[0..10].copy_from_slice(b"CHANGED!!!");
    let dist2 = env.write_dist(
        "dist2",
        &[
            ("index.html", index),
            ("app.js", b"export const version = 2;"),
            ("assets/big.txt", &big2),
        ],
    );
    let patch2 = env.path("out/patch2.tpk");
    let mut args = pack_args(
        "patch",
        "200",
        dist2.to_str().unwrap(),
        patch2.to_str().unwrap(),
    );
    args.extend(["--parent", base1.to_str().unwrap()]);
    let out = env.ok(&args);
    assert!(out.contains("1 delta"), "patch2 must carry a delta: {out}");

    // v3 edits big.txt again, so its delta base only exists after applying
    // patch2's delta; drops app.js and adds a file.
    let mut big3 = big2.clone();
    let tail = big3.len() - 10;
    big3[tail..].copy_from_slice(b"AGAIN!!!!\n");
    let dist3 = env.write_dist(
        "dist3",
        &[
            ("index.html", index),
            ("assets/big.txt", &big3),
            ("assets/new.css", b"body{}"),
        ],
    );
    let patch3 = env.path("out/patch3.tpk");
    let mut args = pack_args(
        "patch",
        "300",
        dist3.to_str().unwrap(),
        patch3.to_str().unwrap(),
    );
    args.extend([
        "--parent",
        base1.to_str().unwrap(),
        "--parent",
        patch2.to_str().unwrap(),
    ]);
    let out = env.ok(&args);
    assert!(out.contains("1 delta"), "patch3 must carry a delta: {out}");
    assert!(out.contains("1 delete"), "app.js must be tombstoned: {out}");

    (base1, patch2, patch3, dist3)
}

#[test]
fn a_patch_packs_against_a_chain_that_contains_deltas() {
    let env = Env::new();
    let (base1, patch2, patch3, dist3) = build_chain(&env);

    let inspected = env.ok(&["inspect", patch3.to_str().unwrap(), "--json"]);
    let manifest: serde_json::Value = serde_json::from_str(&inspected).unwrap();
    assert_eq!(
        manifest["parent"]["version_code"], 200,
        "links to the last parent"
    );
    let delta = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["op"] == "delta")
        .expect("a delta entry");
    assert_eq!(delta["path"], "/assets/big.txt");

    let tree = resolve_chain(&[&base1, &patch2, &patch3]);
    let expected: std::collections::BTreeMap<String, Vec<u8>> =
        ["index.html", "assets/big.txt", "assets/new.css"]
            .iter()
            .map(|rel| (format!("/{rel}"), std::fs::read(dist3.join(rel)).unwrap()))
            .collect();
    assert_eq!(tree, expected, "base1 + patch2 + patch3 must rebuild dist3");
}

#[test]
fn a_parent_chain_with_a_gap_is_refused() {
    let env = Env::new();
    let (base1, _patch2, patch3, dist3) = build_chain(&env);
    let out_path = env.path("out/patch4.tpk");
    let mut args = pack_args(
        "patch",
        "400",
        dist3.to_str().unwrap(),
        out_path.to_str().unwrap(),
    );
    args.extend([
        "--parent",
        base1.to_str().unwrap(),
        "--parent",
        patch3.to_str().unwrap(),
    ]);
    let out = env.tpk(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("not a patch on the preceding --parent"),
        "{stderr}"
    );
    assert!(!out_path.exists());
}

#[test]
fn a_chain_must_start_at_a_base() {
    let env = Env::new();
    let (_base1, patch2, _patch3, dist3) = build_chain(&env);
    let out_path = env.path("out/patch4.tpk");
    let mut args = pack_args(
        "patch",
        "400",
        dist3.to_str().unwrap(),
        out_path.to_str().unwrap(),
    );
    args.extend(["--parent", patch2.to_str().unwrap()]);
    let out = env.tpk(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("first --parent must be a base"), "{stderr}");
    assert!(!out_path.exists());
}

/// Copy a pack, rewriting its manifest. The signature goes stale, which is the
/// point: `--parent` does not check it.
fn tamper_manifest(from: &Path, to: &Path, edit: impl Fn(&mut serde_json::Value)) {
    use std::io::{Read, Write};
    let mut src = zip::ZipArchive::new(std::fs::File::open(from).unwrap()).unwrap();
    let mut dst = zip::ZipWriter::new(std::fs::File::create(to).unwrap());
    let opts: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for i in 0..src.len() {
        let mut entry = src.by_index(i).unwrap();
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if name == "tpk-manifest.json" {
            let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            edit(&mut manifest);
            bytes = serde_json::to_vec(&manifest).unwrap();
        }
        dst.start_file(name, opts).unwrap();
        dst.write_all(&bytes).unwrap();
    }
    if !src.comment().is_empty() {
        dst.set_raw_comment(src.comment().to_vec().into()).unwrap();
    }
    dst.finish().unwrap();
}

#[test]
fn a_parent_declaring_an_oversized_delta_is_refused_before_allocating() {
    let env = Env::new();
    let (base1, patch2, _patch3, dist3) = build_chain(&env);
    let hostile = env.path("out/hostile-patch2.tpk");
    tamper_manifest(&patch2, &hostile, |m| {
        let delta = m["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|e| e["op"] == "delta")
            .unwrap();
        // 1 TiB: bsdiff would try to reserve all of it up front.
        delta["size"] = serde_json::json!(1u64 << 40);
    });

    let out_path = env.path("out/patch4.tpk");
    let mut args = pack_args(
        "patch",
        "400",
        dist3.to_str().unwrap(),
        out_path.to_str().unwrap(),
    );
    args.extend([
        "--parent",
        base1.to_str().unwrap(),
        "--parent",
        hostile.to_str().unwrap(),
    ]);
    let out = env.tpk(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("byte limit"), "{stderr}");
    assert!(!out_path.exists());
}

#[test]
fn a_parent_with_another_id_is_refused_as_a_bad_link() {
    let env = Env::new();
    let (base1, _patch2, _patch3, dist3) = build_chain(&env);
    let out_path = env.path("out/other.tpk");
    let mut args = pack_args(
        "patch",
        "400",
        dist3.to_str().unwrap(),
        out_path.to_str().unwrap(),
    );
    let id = args.iter().position(|a| *a == "--id").unwrap() + 1;
    args[id] = "other";
    args.extend(["--parent", base1.to_str().unwrap()]);
    let out = env.tpk(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("does not match --id"), "{stderr}");
    assert!(!out_path.exists());
}

#[test]
fn channel_rollout_gates_only_the_newest_pack_of_an_id() {
    let env = Env::new();
    let (base1, patch2, patch3, _) = build_chain(&env);
    let channel = env.path("out/latest.json");
    env.ok(&[
        "channel",
        "--channel",
        "stable",
        "--pack",
        base1.to_str().unwrap(),
        "--pack",
        patch3.to_str().unwrap(),
        "--pack",
        patch2.to_str().unwrap(),
        "--url-base",
        "https://cdn.example.com/tpk/core/",
        "--watermark",
        "202609111500",
        "--rollout",
        "10",
        "--out",
        channel.to_str().unwrap(),
    ]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&channel).unwrap()).unwrap();
    let rollouts: Vec<(u64, u64)> = manifest["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["version_code"].as_u64().unwrap(),
                p["rollout"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(rollouts, [(100, 100), (300, 10), (200, 100)]);
}

#[test]
fn channel_entries_carry_each_pack_shell_range() {
    let env = Env::new();
    let dist = base_dist(&env, "dist");
    let pack = env.path("out/base-core-1.0.0.tpk");
    env.ok(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--min-shell",
        "1.2.0",
        "--max-shell",
        "2.5.0",
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        pack.to_str().unwrap(),
    ]);

    let channel = env.path("out/latest.json");
    env.ok(&[
        "channel",
        "--channel",
        "stable",
        "--pack",
        pack.to_str().unwrap(),
        "--url-base",
        "https://cdn.example.com/tpk/core/",
        "--watermark",
        "202609111500",
        "--out",
        channel.to_str().unwrap(),
    ]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&channel).unwrap()).unwrap();
    // Signed with the rest of the manifest, so the planner can skip a pack the
    // running shell could not stage without downloading it first.
    assert_eq!(manifest["packs"][0]["min_shell"], "1.2.0");
    assert_eq!(manifest["packs"][0]["max_shell"], "2.5.0");
}

#[test]
fn remote_workers_are_rejected_at_pack_time() {
    let env = Env::new();
    let dist = env.write_dist(
        "dist",
        &[
            ("index.html", br#"<script src="/app.js"></script>"#),
            ("app.js", br#"new Worker("https://evil.example.com/w.js");"#),
        ],
    );
    let out = env.tpk(&[
        "pack",
        "--kind",
        "base",
        "--id",
        "core",
        "--version",
        "1.0.0",
        "--version-code",
        "20260911150000",
        "--created-at",
        CREATED_AT,
        "--dist",
        dist.to_str().unwrap(),
        "--out",
        env.path("out.tpk").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Worker"), "{stderr}");
}
