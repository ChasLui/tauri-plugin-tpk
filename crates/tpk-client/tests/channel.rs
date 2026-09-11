//! Verifying a fetched channel manifest: signature, key epoch, watermark.
#![cfg(feature = "test-keys")]

use std::sync::Arc;

use tpk_client::{verify_channel, ClientError, SignedBytes};
use tpk_format::secret::SecretKey;
use tpk_format::sign::{TrustStore, TrustedKey};

fn manifest_json(watermark: u64, key_epoch: u32) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "spec": "tpk-channel/1",
        "channel": "stable",
        "published_at": "2026-09-11T15:00:00Z",
        "watermark": watermark,
        "key_epoch": key_epoch,
        "packs": [],
    }))
    .unwrap()
}

fn signed(key: &SecretKey, body: Vec<u8>) -> SignedBytes {
    let signature = key.sign(&body, "timestamp:0", "tpk test");
    SignedBytes { body, signature }
}

fn trust(keys: &[(&SecretKey, u32)]) -> Arc<TrustStore> {
    Arc::new(
        TrustStore::new(
            &keys
                .iter()
                .map(|(k, epoch)| TrustedKey {
                    key: k.public_key_base64(),
                    epoch: *epoch,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    )
}

#[test]
fn a_well_formed_manifest_verifies() {
    let key = SecretKey::generate();
    let m = verify_channel(
        &signed(&key, manifest_json(202609111500, 1)),
        &trust(&[(&key, 1)]),
        1,
        0,
    )
    .unwrap();
    assert_eq!(m.channel, "stable");
    assert_eq!(m.watermark, 202609111500);
}

#[test]
fn a_tampered_body_does_not_verify() {
    let key = SecretKey::generate();
    let mut s = signed(&key, manifest_json(202609111500, 1));
    s.body = manifest_json(202609111600, 1);

    let err = verify_channel(&s, &trust(&[(&key, 1)]), 1, 0).unwrap_err();
    assert!(matches!(err, ClientError::Format(_)), "{err}");
}

#[test]
fn a_stranger_key_does_not_verify() {
    let publisher = SecretKey::generate();
    let stranger = SecretKey::generate();
    let s = signed(&stranger, manifest_json(202609111500, 1));

    assert!(verify_channel(&s, &trust(&[(&publisher, 1)]), 1, 0).is_err());
}

#[test]
fn an_older_watermark_is_refused() {
    let key = SecretKey::generate();
    let s = signed(&key, manifest_json(202609111400, 1));

    let err = verify_channel(&s, &trust(&[(&key, 1)]), 1, 202609111500).unwrap_err();
    assert!(matches!(err, ClientError::Watermark { .. }), "{err}");
}

#[test]
fn an_equal_watermark_is_accepted() {
    let key = SecretKey::generate();
    let s = signed(&key, manifest_json(202609111500, 1));

    // Rejecting equality would let one same-minute double publish invalidate a
    // manifest permanently for every device that saw the other one — and that
    // failure is silent. The real downgrade defence is the per-pack version_code
    // check, not this.
    assert!(verify_channel(&s, &trust(&[(&key, 1)]), 1, 202609111500).is_ok());
}

#[test]
fn a_retired_key_epoch_is_refused() {
    let k1 = SecretKey::generate();
    let k2 = SecretKey::generate();
    let store = trust(&[(&k1, 1), (&k2, 2)]);

    // Both keys are still listed, but the device has moved its floor to 2.
    let old = signed(&k1, manifest_json(202609111500, 1));
    assert!(verify_channel(&old, &store, 2, 0).is_err());

    let new = signed(&k2, manifest_json(202609111500, 2));
    assert!(verify_channel(&new, &store, 2, 0).is_ok());
}

#[test]
fn a_leaked_key_cannot_claim_a_newer_epoch() {
    let k1 = SecretKey::generate();
    let k2 = SecretKey::generate();
    let store = trust(&[(&k1, 1), (&k2, 2)]);

    // The whole point of the epoch: whoever holds the retired key cannot sign a
    // manifest that claims to be from the new generation, so they can only push
    // the floor up — which retires them faster.
    let forged = signed(&k1, manifest_json(202609111500, 2));
    assert!(verify_channel(&forged, &store, 1, 0).is_err());
}

#[test]
fn an_epoch_with_no_key_is_refused() {
    let key = SecretKey::generate();
    let s = signed(&key, manifest_json(202609111500, 5));
    let err = verify_channel(&s, &trust(&[(&key, 1)]), 1, 0).unwrap_err();
    assert!(err.to_string().contains("epoch"), "{err}");
}

#[test]
fn a_malformed_document_reports_as_malformed_not_as_a_signature_failure() {
    let key = SecretKey::generate();
    let body = br#"{"spec":"tpk-channel/1","channel":"stable"}"#.to_vec();
    let s = signed(&key, body);

    // Parsing first is deliberate: the epoch the signature must be checked
    // against lives inside the document.
    let err = verify_channel(&s, &trust(&[(&key, 1)]), 1, 0)
        .unwrap_err()
        .to_string();
    assert!(!err.contains("signature"), "{err}");
}

#[test]
fn an_unknown_spec_tag_is_refused() {
    let key = SecretKey::generate();
    let body = serde_json::to_vec(&serde_json::json!({
        "spec": "tpk-channel/2",
        "channel": "stable",
        "published_at": "2026-09-11T15:00:00Z",
        "watermark": 1,
        "packs": [],
    }))
    .unwrap();
    assert!(verify_channel(&signed(&key, body), &trust(&[(&key, 1)]), 1, 0).is_err());
}
