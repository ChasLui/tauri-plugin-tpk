//! End-to-end container tests: build a real signed `.tpk`, then read it back.
//!
//! These exercise the one path that matters most — untrusted ZIP bytes in,
//! verified content out — against containers this crate produced itself.
#![cfg(feature = "pack")]

use std::io::Write;
use std::path::Path;

use tpk_format::container::UnverifiedPack;
use tpk_format::error::FormatError;
use tpk_format::secret::SecretKey;
use tpk_format::sign::{sha256_hex, TrustStore, TrustedKey};

/// How a test wants the container deliberately broken.
#[derive(Default, Clone, Copy)]
struct Damage {
    tamper_manifest_after_signing: bool,
    tamper_blob: bool,
    add_unreferenced_blob: bool,
    omit_blob: bool,
    add_illegal_entry: bool,
    set_archive_comment: bool,
    omit_signature: bool,
}

struct Built {
    path: std::path::PathBuf,
    trust: TrustStore,
    _dir: tempfile::TempDir,
}

/// Build a one-entry base pack containing `content` at `/index.html`.
fn build(content: &[u8], damage: Damage) -> Built {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("base-core-1.0.0.tpk");
    let key = SecretKey::generate();
    let trust = TrustStore::new(&[TrustedKey {
        key: key.public_key_base64(),
        epoch: 1,
    }])
    .unwrap();

    let blob_sha = sha256_hex(content);
    let blob_name = format!("blobs/{blob_sha}");
    let manifest = serde_json::json!({
        "spec": "tpk/1",
        "kind": "base",
        "id": "core",
        "version": "1.0.0",
        "version_code": 10000,
        "created_at": "2026-09-11T15:00:00Z",
        "entries": [{
            "path": "/index.html",
            "op": "full",
            "size": content.len(),
            "sha256": blob_sha.to_hex(),
            "blob": blob_name,
            "blob_sha256": blob_sha.to_hex(),
            "blob_size": content.len(),
            "encoding": "identity",
        }],
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let signature = key.sign(&manifest_bytes, "timestamp:0", "tpk test");

    let written_manifest = if damage.tamper_manifest_after_signing {
        let mut tampered = manifest;
        tampered["version_code"] = serde_json::json!(99999);
        serde_json::to_vec(&tampered).unwrap()
    } else {
        manifest_bytes
    };

    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("tpk-manifest.json", opts).unwrap();
    zip.write_all(&written_manifest).unwrap();

    if !damage.omit_signature {
        zip.start_file("tpk-manifest.json.minisig", opts).unwrap();
        zip.write_all(signature.as_bytes()).unwrap();
    }

    if !damage.omit_blob {
        zip.start_file(format!("blobs/{blob_sha}"), opts).unwrap();
        if damage.tamper_blob {
            zip.write_all(b"replaced after signing").unwrap();
        } else {
            zip.write_all(content).unwrap();
        }
    }

    if damage.add_unreferenced_blob {
        let extra = sha256_hex(b"stowaway");
        zip.start_file(format!("blobs/{extra}"), opts).unwrap();
        zip.write_all(b"stowaway").unwrap();
    }
    if damage.add_illegal_entry {
        zip.start_file("README.md", opts).unwrap();
        zip.write_all(b"not part of the format").unwrap();
    }
    if damage.set_archive_comment {
        zip.set_comment("smuggled").unwrap();
    }

    zip.finish().unwrap();
    Built {
        path,
        trust,
        _dir: dir,
    }
}

fn open_and_verify(built: &Built) -> Result<tpk_format::container::VerifiedPack, FormatError> {
    UnverifiedPack::open(&built.path)?.verify(&built.trust, None, 1, 1)
}

#[test]
fn a_freshly_built_pack_verifies_and_reads_back() {
    let content = b"<!doctype html><title>hello</title>";
    let built = build(content, Damage::default());

    let unverified = UnverifiedPack::open(&built.path).unwrap();
    assert!(unverified.detached_signature().is_some());
    let expected_manifest_sha = unverified.manifest_sha256();

    let mut pack = unverified.verify(&built.trust, None, 1, 1).unwrap();
    assert_eq!(pack.manifest_sha256(), expected_manifest_sha);
    assert_eq!(pack.manifest().id.as_str(), "core");
    assert_eq!(pack.manifest().version_code, 10000);

    let entry = pack.manifest().entries[0].clone();
    assert_eq!(pack.read_blob(&entry).unwrap(), content);
}

#[test]
fn tampering_with_the_manifest_breaks_the_signature() {
    let built = build(
        b"x",
        Damage {
            tamper_manifest_after_signing: true,
            ..Default::default()
        },
    );
    assert!(matches!(
        open_and_verify(&built).unwrap_err(),
        FormatError::Signature(_)
    ));
}

#[test]
fn tampering_with_a_blob_is_caught_on_read() {
    let built = build(
        b"original content",
        Damage {
            tamper_blob: true,
            ..Default::default()
        },
    );
    // The manifest still verifies — only the blob changed.
    let mut pack = open_and_verify(&built).unwrap();
    let entry = pack.manifest().entries[0].clone();
    assert!(matches!(
        pack.read_blob(&entry).unwrap_err(),
        FormatError::Hash(_)
    ));
}

#[test]
fn an_unreferenced_blob_is_rejected() {
    let built = build(
        b"x",
        Damage {
            add_unreferenced_blob: true,
            ..Default::default()
        },
    );
    let err = open_and_verify(&built).unwrap_err().to_string();
    assert!(err.contains("unreferenced blob"), "{err}");
}

#[test]
fn a_missing_blob_is_rejected() {
    let built = build(
        b"x",
        Damage {
            omit_blob: true,
            ..Default::default()
        },
    );
    let err = open_and_verify(&built).unwrap_err().to_string();
    assert!(err.contains("missing blob"), "{err}");
}

#[test]
fn an_illegal_entry_name_is_rejected_before_verification() {
    let built = build(
        b"x",
        Damage {
            add_illegal_entry: true,
            ..Default::default()
        },
    );
    let err = UnverifiedPack::open(&built.path).unwrap_err().to_string();
    assert!(err.contains("illegal container entry"), "{err}");
}

#[test]
fn an_archive_comment_is_rejected() {
    let built = build(
        b"x",
        Damage {
            set_archive_comment: true,
            ..Default::default()
        },
    );
    let err = UnverifiedPack::open(&built.path).unwrap_err().to_string();
    assert!(err.contains("archive comment"), "{err}");
}

#[test]
fn a_pack_without_a_signature_cannot_be_verified() {
    let built = build(
        b"x",
        Damage {
            omit_signature: true,
            ..Default::default()
        },
    );
    let unverified = UnverifiedPack::open(&built.path).unwrap();
    assert!(unverified.detached_signature().is_none());
    assert!(matches!(
        unverified.verify(&built.trust, None, 1, 1).unwrap_err(),
        FormatError::Signature(_)
    ));
}

#[test]
fn a_detached_signature_can_be_supplied_out_of_band() {
    let built = build(
        b"x",
        Damage {
            omit_signature: true,
            ..Default::default()
        },
    );
    // Recreate the signature the way a channel manifest carries one: separate
    // from the container.
    let unverified = UnverifiedPack::open(&built.path).unwrap();
    let key = SecretKey::generate();
    let trust = TrustStore::new(&[TrustedKey {
        key: key.public_key_base64(),
        epoch: 1,
    }])
    .unwrap();
    let sig = key.sign(unverified.raw_manifest_bytes(), "timestamp:0", "c");
    assert!(unverified.verify(&trust, Some(&sig), 1, 1).is_ok());
}

#[test]
fn a_truncated_container_is_rejected() {
    let built = build(b"some reasonably sized content", Damage::default());
    let bytes = std::fs::read(&built.path).unwrap();
    let truncated = &bytes[..bytes.len() / 2];
    let path = built.path.with_extension("truncated.tpk");
    std::fs::write(&path, truncated).unwrap();
    assert!(UnverifiedPack::open(&path).is_err());
}

#[test]
fn a_nonexistent_file_reports_io() {
    let err = UnverifiedPack::open(Path::new("/nonexistent/nope.tpk")).unwrap_err();
    assert!(matches!(err, FormatError::Io(_)));
}
