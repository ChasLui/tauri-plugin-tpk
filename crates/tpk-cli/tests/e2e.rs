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
