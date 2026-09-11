//! CLI behaviour, including the exit codes the specification assigns
//! (0 success, 2 verification failure, 3 usage error).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use tpk_format::secret::SecretKey;
use tpk_format::sign::sha256_hex;

fn tpk() -> Command {
    // Built by `cargo test`; CARGO_BIN_EXE_ is set for every bin target.
    Command::new(env!("CARGO_BIN_EXE_tpk"))
}

struct Fixture {
    dir: tempfile::TempDir,
    key: SecretKey,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            key: SecretKey::generate(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Write a signed single-entry base pack.
    fn write_pack(&self, name: &str, sign_with: Option<&SecretKey>) -> PathBuf {
        let content = b"<!doctype html>";
        let sha = sha256_hex(content);
        let manifest = serde_json::json!({
            "spec": "tpk/1",
            "kind": "base",
            "id": "core",
            "version": "1.2.3",
            "version_code": 10203,
            "created_at": "2026-09-11T15:00:00Z",
            "entries": [{
                "path": "/index.html",
                "op": "full",
                "size": content.len(),
                "sha256": sha.to_hex(),
                "blob": format!("blobs/{sha}"),
                "blob_sha256": sha.to_hex(),
                "blob_size": content.len(),
                "encoding": "identity",
            }],
        });
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let signer = sign_with.unwrap_or(&self.key);
        let sig = signer.sign(&bytes, "timestamp:0", "tpk test");

        let path = self.path(name);
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("tpk-manifest.json", opts).unwrap();
        zip.write_all(&bytes).unwrap();
        zip.start_file("tpk-manifest.json.minisig", opts).unwrap();
        zip.write_all(sig.as_bytes()).unwrap();
        zip.start_file(format!("blobs/{sha}"), opts).unwrap();
        zip.write_all(content).unwrap();
        zip.finish().unwrap();
        path
    }

    /// Write a signed channel manifest plus its detached signature.
    fn write_channel(&self, name: &str, key_epoch: u32) -> PathBuf {
        let manifest = serde_json::json!({
            "spec": "tpk-channel/1",
            "channel": "stable",
            "published_at": "2026-09-11T15:00:00Z",
            "watermark": 202609111500u64,
            "key_epoch": key_epoch,
            "packs": [],
        });
        let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let path = self.path(name);
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(
            self.path(&format!("{name}.minisig")),
            self.key.sign(&bytes, "timestamp:0", "tpk test"),
        )
        .unwrap();
        path
    }

    fn pubkey(&self) -> String {
        self.key.public_key_base64()
    }
}

fn exit_code(output: &std::process::Output) -> i32 {
    output.status.code().expect("process was not signalled")
}

#[test]
fn verify_accepts_a_well_formed_pack() {
    let f = Fixture::new();
    let pack = f.write_pack("base-core-1.2.3.tpk", None);
    let out = tpk()
        .args(["verify", "--pubkey", &f.pubkey(), "--file"])
        .arg(&pack)
        .output()
        .unwrap();
    assert_eq!(
        exit_code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ok "), "{stdout}");
    assert!(stdout.contains("version_code 10203"), "{stdout}");
}

#[test]
fn verify_rejects_a_pack_signed_by_another_key() {
    let f = Fixture::new();
    let stranger = SecretKey::generate();
    let pack = f.write_pack("base-core-1.2.3.tpk", Some(&stranger));
    let out = tpk()
        .args(["verify", "--pubkey", &f.pubkey(), "--file"])
        .arg(&pack)
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 2, "verification failures must exit 2");
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL"));
}

#[test]
fn verify_accepts_a_channel_manifest_with_its_detached_signature() {
    let f = Fixture::new();
    let channel = f.write_channel("latest.json", 1);
    let out = tpk()
        .args(["verify", "--pubkey", &f.pubkey(), "--file"])
        .arg(&channel)
        .output()
        .unwrap();
    assert_eq!(
        exit_code(&out),
        0,
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("watermark 202609111500"));
}

#[test]
fn verify_rejects_a_channel_below_the_epoch_floor() {
    let f = Fixture::new();
    let channel = f.write_channel("latest.json", 1);
    let out = tpk()
        .args([
            "verify",
            "--pubkey",
            &f.pubkey(),
            "--min-epoch",
            "2",
            "--file",
        ])
        .arg(&channel)
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 2);
}

#[test]
fn verify_reports_a_missing_detached_signature() {
    let f = Fixture::new();
    let channel = f.write_channel("latest.json", 1);
    std::fs::remove_file(f.path("latest.json.minisig")).unwrap();
    let out = tpk()
        .args(["verify", "--pubkey", &f.pubkey(), "--file"])
        .arg(&channel)
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stdout).contains("missing detached signature"));
}

#[test]
fn verify_rejects_mismatched_pubkey_and_epoch_counts() {
    let f = Fixture::new();
    let out = tpk()
        .args([
            "verify",
            "--pubkey",
            &f.pubkey(),
            "--epoch",
            "1",
            "--epoch",
            "2",
            "--file",
            "/nonexistent",
        ])
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 3, "usage errors must exit 3");
}

#[test]
fn verify_rejects_a_malformed_public_key() {
    let out = tpk()
        .args(["verify", "--pubkey", "nonsense", "--file", "/nonexistent"])
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 3);
}

#[test]
fn verify_counts_every_failing_file() {
    let f = Fixture::new();
    let good = f.write_pack("good.tpk", None);
    let bad = f.write_pack("bad.tpk", Some(&SecretKey::generate()));
    let out = tpk()
        .args(["verify", "--pubkey", &f.pubkey(), "--file"])
        .arg(&good)
        .arg("--file")
        .arg(&bad)
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1 of 2"), "{stderr}");
}

#[test]
fn inspect_prints_a_summary_without_a_key() {
    let f = Fixture::new();
    let pack = f.write_pack("base-core-1.2.3.tpk", None);
    let out = tpk().arg("inspect").arg(&pack).output().unwrap();
    assert_eq!(exit_code(&out), 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("id             core"), "{stdout}");
    assert!(stdout.contains("1 full"), "{stdout}");
    // The distinction from `verify` has to be visible in the output itself.
    assert!(stdout.contains("NOT verified"), "{stdout}");
}

#[test]
fn inspect_emits_json_on_request() {
    let f = Fixture::new();
    let pack = f.write_pack("base-core-1.2.3.tpk", None);
    let out = tpk()
        .arg("inspect")
        .arg(&pack)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 0);
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["id"], "core");
    assert_eq!(parsed["version_code"], 10203);
}

#[test]
fn inspect_fails_on_a_missing_file() {
    let out = tpk()
        .arg("inspect")
        .arg("/nonexistent/x.tpk")
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 2);
}

#[test]
fn keygen_writes_a_usable_key_and_refuses_to_clobber() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = dir.path().join("signing.key");

    let out = tpk()
        .arg("keygen")
        .arg("--out")
        .arg(&key_path)
        .output()
        .unwrap();
    assert_eq!(exit_code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"epoch\": 1"));

    let reloaded = SecretKey::parse(&std::fs::read_to_string(&key_path).unwrap()).unwrap();
    assert!(!reloaded.public_key_base64().is_empty());
    assert_secret_key_permissions(&key_path);

    let second = tpk()
        .arg("keygen")
        .arg("--out")
        .arg(&key_path)
        .output()
        .unwrap();
    assert_eq!(exit_code(&second), 3, "must not overwrite an existing key");
}

#[cfg(unix)]
fn assert_secret_key_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "secret keys must not be group/world readable"
    );
}

#[cfg(not(unix))]
fn assert_secret_key_permissions(_path: &Path) {}
