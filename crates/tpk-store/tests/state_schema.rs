//! `state.json` as the store actually writes it must satisfy its JSON Schema.
//!
//! `crates/tpk-format/tests/schema.rs` can only pin a hand-written document:
//! tpk-store depends on tpk-format, not the other way round, so `StoreState` is
//! invisible from there. This is the other half — the real type, serialized by
//! the real persistence path, against `spec/json-schema/store-state.schema.json`.
#![cfg(feature = "test-packs")]

use std::sync::Arc;

use serde_json::Value;
use tpk_format::manifest::{PackId, PackKind};
use tpk_format::pack::PackBuilder;
use tpk_format::secret::SecretKey;
use tpk_format::sign::{sha256_hex, TrustStore, TrustedKey};
use tpk_store::state::LastError;
use tpk_store::{CommitOutcome, IncomingPack, Layout, Pointer, Reason, Store};

fn schema() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spec/json-schema/store-state.schema.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

/// Everything the schema lists under `required`, at that position.
fn required(at: &Value) -> Vec<String> {
    at.get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn base_pack(dir: &std::path::Path, key: &SecretKey, version_code: u64) -> IncomingPack {
    let path = dir.join(format!("core-{version_code}.tpk"));
    let mut builder = PackBuilder::new(
        PackKind::Base,
        PackId::parse("core").unwrap(),
        "1.0.0".parse().unwrap(),
        version_code,
        "2026-09-11T15:00:00Z",
    );
    builder
        .add_full("/index.html", format!("<p>{version_code}</p>").as_bytes())
        .unwrap();
    builder.build(key, &path).unwrap();
    IncomingPack {
        bytes: std::fs::read(&path).unwrap(),
        id: PackId::parse("core").unwrap(),
        kind: PackKind::Base,
        version_code,
    }
}

#[test]
fn the_state_the_store_writes_matches_its_schema() {
    let data = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let build = tempfile::tempdir().unwrap();
    let key = SecretKey::generate();
    let trust = Arc::new(
        TrustStore::new(&[TrustedKey {
            key: key.public_key_base64(),
            epoch: 1,
        }])
        .unwrap(),
    );
    let layout = Layout::new(data.path().join("tpk"), cache.path().join("tpk"));
    let mut store = Store::open(layout.clone()).unwrap();

    // Drive all three pointer slots into use: commit one revision, put a second
    // on trial, leave a third staged. A document with only `committed` filled in
    // exercises one third of the schema.
    store
        .stage(vec![base_pack(build.path(), &key, 10_000)], &trust)
        .unwrap();
    store.boot().unwrap();
    assert_eq!(store.commit_booting().unwrap(), CommitOutcome::Committed);
    store
        .stage(vec![base_pack(build.path(), &key, 20_000)], &trust)
        .unwrap();
    store.boot().unwrap();
    assert_eq!(store.state().pointer, Pointer::Booting);
    store
        .stage(vec![base_pack(build.path(), &key, 30_000)], &trust)
        .unwrap();

    // A blacklist entry, and the three fields only the plugin layer ever writes.
    store
        .record_failure(sha256_hex(b"a layer that never loaded"), Reason::Hash)
        .unwrap();
    let state = store.state_mut();
    state.observe_watermark("stable", 202_609_111_500);
    state.min_key_epoch = 2;
    state.last_error = Some(LastError {
        code: "E_HASH".to_string(),
        message: "content did not match its digest".to_string(),
    });
    store.save().unwrap();

    // The bytes on disk, not a re-serialization: `atomic_write_json` is what a
    // real device reads back on the next launch.
    let raw = std::fs::read(layout.state_file()).unwrap();
    let doc: Value = serde_json::from_slice(&raw).unwrap();

    // Committing above raised the per-id floor, so the field is exercised by the
    // same sequence rather than being poked in by hand.
    assert_eq!(
        doc["version_floor"]["core"], 10_000,
        "committing a revision should raise the version floor: {doc:#}"
    );

    let schema = schema();
    let validator = jsonschema::validator_for(&schema).expect("unusable schema");
    if let Err(e) = validator.validate(&doc) {
        panic!("state.json written by the store was rejected: {e}\n{doc:#}");
    }

    // The other direction: a required property the store never writes would
    // still validate above only if the schema and the type agreed by accident.
    for name in required(&schema) {
        assert!(doc.get(&name).is_some(), "state.json is missing {name:?}");
    }
    for slot in ["staged", "booting", "committed"] {
        let rev = doc
            .get(slot)
            .unwrap_or_else(|| panic!("{slot} should be populated by this sequence"));
        for name in required(&schema["$defs"]["revision"]) {
            assert!(rev.get(&name).is_some(), "{slot} is missing {name:?}");
        }
        for layer in rev["layers"].as_array().expect("layers is an array") {
            for name in required(&schema["$defs"]["revision"]["properties"]["layers"]["items"]) {
                assert!(layer.get(&name).is_some(), "{slot} layer missing {name:?}");
            }
        }
    }

    // And the document the store wrote is one the store can read.
    assert_eq!(tpk_store::StoreState::parse(&raw).unwrap(), *store.state());
}
