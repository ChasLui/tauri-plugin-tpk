//! A pack built once, committed, and verified forever.
//!
//! The rest of the suite signs and verifies with the same build, so it stays
//! green through any change that alters the format *consistently* — a different
//! hash, a reordered container, a new signature encoding. Every one of those
//! would leave already-published packs unverifiable while the tests said
//! nothing.
//!
//! This fixture is the control for that. It was produced by `tpk pack` at
//! v0.1.0 and must keep verifying, byte-for-byte unchanged, under every later
//! build.
//!
//! If it fails you have made a breaking format change. That may be deliberate —
//! but it means a spec version bump and a migration story, not a new fixture.

use tpk_format::container::UnverifiedPack;
use tpk_format::manifest::PackKind;
use tpk_format::sign::{TrustStore, TrustedKey};

const GOLDEN: &[u8] = include_bytes!("fixtures/golden-v1.tpk");

/// The public half of the key that signed the fixture. The secret half was
/// discarded on purpose: nothing should ever be signed with it again.
const GOLDEN_PUBKEY: &str = "RWSGEIZgc+grZ4YQhmBz6CtnSLRcol0J9MRJO4Y0KG0Gc0wZvKF+zW96";

/// SHA-256 of the fixture as committed.
const GOLDEN_SHA256: &str = "e8a6485e9f8d5eb083fef4b625511fe06c906ac1ff99fa35ed5a01c3c14e6d86";

fn trust() -> TrustStore {
    TrustStore::new(&[TrustedKey {
        key: GOLDEN_PUBKEY.to_string(),
        epoch: 1,
    }])
    .expect("the golden public key must parse")
}

#[test]
fn the_committed_fixture_is_the_one_that_was_measured() {
    // Guards the other assertions: if the file were replaced, "it still
    // verifies" would prove nothing about compatibility.
    let actual = tpk_format::sign::sha256_hex(GOLDEN);
    assert_eq!(
        actual.to_hex(),
        GOLDEN_SHA256,
        "the fixture on disk is not the one this test was written against"
    );
}

#[test]
fn a_pack_published_at_v0_1_0_still_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("golden.tpk");
    std::fs::write(&path, GOLDEN).unwrap();

    let pack = UnverifiedPack::open(&path)
        .expect("the golden pack must still parse")
        .verify(&trust(), None, 1, 1)
        .expect(
            "the golden pack must still verify: a failure here means every \
                 already-published pack has become unverifiable",
        );

    let m = pack.manifest();
    assert_eq!(m.spec, "tpk/1");
    assert_eq!(m.kind, PackKind::Base);
    assert_eq!(m.id.as_str(), "core");
    assert_eq!(m.version_code, 1);
    assert_eq!(m.entries.len(), 2);

    // The digest the signed manifest carries for a known payload. A change in
    // the content-hash algorithm would show up right here.
    let index = m
        .entries
        .iter()
        .find(|e| e.path.as_str() == "/index.html")
        .expect("fixture has /index.html");
    assert_eq!(
        index.sha256.unwrap().to_hex(),
        // sha256("hello\n")
        "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
    );
}

#[test]
fn the_golden_pack_is_refused_by_an_untrusted_key() {
    // Positive control. Without it, a `verify` that had degenerated into
    // "always Ok" would make the test above pass while proving nothing.
    let other = TrustStore::new(&[TrustedKey {
        key: "RWTHaKkibC2F1MdoqSJsLYXUPc9WKK9CDDqZVTJiSuFt/XHiBFORvBZG".to_string(),
        epoch: 1,
    }])
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("golden.tpk");
    std::fs::write(&path, GOLDEN).unwrap();

    let result = UnverifiedPack::open(&path)
        .unwrap()
        .verify(&other, None, 1, 1);
    assert!(
        result.is_err(),
        "a pack signed by a different key must not verify"
    );
}

#[test]
fn the_golden_pack_is_refused_below_its_key_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("golden.tpk");
    std::fs::write(&path, GOLDEN).unwrap();

    // The fixture was signed at epoch 1; a client whose floor has moved to 2
    // must refuse it.
    let result = UnverifiedPack::open(&path)
        .unwrap()
        .verify(&trust(), None, 1, 2);
    assert!(result.is_err(), "a retired key epoch must be refused");
}

#[test]
fn tampering_with_one_byte_is_caught() {
    // Second control: prove the check reads the bytes it claims to.
    let mut tampered = GOLDEN.to_vec();
    let last = tampered.len() - 40;
    tampered[last] ^= 0xff;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tampered.tpk");
    std::fs::write(&path, &tampered).unwrap();

    let result = UnverifiedPack::open(&path).and_then(|p| p.verify(&trust(), None, 1, 1));
    assert!(result.is_err(), "a flipped byte must not verify");
}
