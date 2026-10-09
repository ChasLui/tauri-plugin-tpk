//! The JSON Schemas in `spec/json-schema/` and this crate's parsers must agree.
//!
//! Two documents drifting apart is the failure mode these tests exist for: a
//! rule tightened in Rust but not in the schema silently blesses documents the
//! runtime will reject, and vice versa.
#![cfg(feature = "pack")]

use serde_json::Value;

use tpk_format::channel::ChannelManifest;
use tpk_format::manifest::PackManifest;

/// A named way of breaking a valid document.
type Mutation = (&'static str, Box<dyn Fn(&mut Value)>);

fn schema(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spec/json-schema")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

fn validator(name: &str) -> jsonschema::Validator {
    jsonschema::validator_for(&schema(name))
        .unwrap_or_else(|e| panic!("{name} is not a usable schema: {e}"))
}

const HASH_A: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
const HASH_B: &str = "bb00000000000000000000000000000000000000000000000000000000000002";
const HASH_C: &str = "cc00000000000000000000000000000000000000000000000000000000000003";

fn full_entry(path: &str) -> Value {
    serde_json::json!({
        "path": path,
        "op": "full",
        "size": 100,
        "sha256": HASH_A,
        "blob": format!("blobs/{HASH_B}.zst"),
        "blob_sha256": HASH_B,
        "blob_size": 40,
        "encoding": "zstd",
    })
}

fn base_pack() -> Value {
    serde_json::json!({
        "spec": "tpk/1",
        "kind": "base",
        "id": "core",
        "version": "1.0.0",
        "version_code": 10000,
        "created_at": "2026-09-11T15:00:00Z",
        "entries": [full_entry("/index.html")],
    })
}

fn channel_manifest() -> Value {
    serde_json::json!({
        "spec": "tpk-channel/1",
        "channel": "stable",
        "published_at": "2026-09-11T15:00:00Z",
        "watermark": 202609111500u64,
        "key_epoch": 1,
        "packs": [{
            "id": "core",
            "kind": "base",
            "version": "1.0.0",
            "version_code": 10000,
            "url": "https://cdn.example.com/tpk/core/base-core-1.0.0.tpk",
            "size": 8400000,
            "sha256": HASH_A,
            "rollout": 25,
        }],
    })
}

/// Assert the schema and the Rust parser reach the same verdict.
fn agree_pack(doc: &Value, expected_valid: bool, what: &str) {
    let by_schema = validator("pack-manifest.schema.json").is_valid(doc);
    let by_parser = PackManifest::parse(serde_json::to_vec(doc).unwrap().as_slice()).is_ok();
    assert_eq!(
        by_schema, expected_valid,
        "schema disagrees about {what} (said valid={by_schema})"
    );
    assert_eq!(
        by_parser, expected_valid,
        "parser disagrees about {what} (said valid={by_parser})"
    );
}

fn agree_channel(doc: &Value, expected_valid: bool, what: &str) {
    let by_schema = validator("channel-manifest.schema.json").is_valid(doc);
    let by_parser = ChannelManifest::parse(serde_json::to_vec(doc).unwrap().as_slice()).is_ok();
    assert_eq!(by_schema, expected_valid, "schema disagrees about {what}");
    assert_eq!(by_parser, expected_valid, "parser disagrees about {what}");
}

#[test]
fn all_three_schemas_are_usable() {
    for name in [
        "pack-manifest.schema.json",
        "channel-manifest.schema.json",
        "store-state.schema.json",
    ] {
        let _ = validator(name);
    }
}

#[test]
fn a_valid_pack_satisfies_both() {
    agree_pack(&base_pack(), true, "a minimal base pack");
}

#[test]
fn pack_rules_agree_on_rejection() {
    let cases: Vec<Mutation> = vec![
        (
            "an unknown spec tag",
            Box::new(|v: &mut Value| v["spec"] = serde_json::json!("tpk/2")),
        ),
        (
            "an unknown kind",
            Box::new(|v: &mut Value| v["kind"] = serde_json::json!("plugin")),
        ),
        (
            "an uppercase id",
            Box::new(|v: &mut Value| v["id"] = serde_json::json!("Core")),
        ),
        (
            "a zero version_code",
            Box::new(|v: &mut Value| v["version_code"] = serde_json::json!(0)),
        ),
        (
            "a relative entry path",
            Box::new(|v: &mut Value| v["entries"][0]["path"] = serde_json::json!("index.html")),
        ),
        (
            "an unknown encoding",
            Box::new(|v: &mut Value| v["entries"][0]["encoding"] = serde_json::json!("brotli")),
        ),
        (
            "an uppercase digest",
            Box::new(|v: &mut Value| {
                v["entries"][0]["blob_sha256"] = serde_json::json!(HASH_B.to_uppercase())
            }),
        ),
        (
            "a full entry missing blob_sha256",
            Box::new(|v: &mut Value| {
                v["entries"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("blob_sha256");
            }),
        ),
        (
            "a full entry missing blob_size",
            Box::new(|v: &mut Value| {
                v["entries"][0].as_object_mut().unwrap().remove("blob_size");
            }),
        ),
        (
            "a full entry carrying delta_base_sha256",
            Box::new(|v: &mut Value| {
                v["entries"][0]["delta_base_sha256"] = serde_json::json!(HASH_C)
            }),
        ),
        (
            "a full entry using bsdiff encoding",
            Box::new(|v: &mut Value| {
                v["entries"][0]["encoding"] = serde_json::json!("zstd+bsdiff")
            }),
        ),
        (
            "a delete entry carrying a blob",
            Box::new(|v: &mut Value| {
                v["entries"] = serde_json::json!([{
                    "path": "/gone.css",
                    "op": "delete",
                    "blob": format!("blobs/{HASH_B}"),
                }])
            }),
        ),
        (
            "a version that is not SemVer",
            Box::new(|v: &mut Value| v["version"] = serde_json::json!("1.0")),
        ),
        (
            "a min_shell that is not SemVer",
            Box::new(|v: &mut Value| v["min_shell"] = serde_json::json!("not-a-version")),
        ),
        (
            "a max_shell that is not SemVer",
            Box::new(|v: &mut Value| v["max_shell"] = serde_json::json!("2.0")),
        ),
        (
            "an empty entry list",
            Box::new(|v: &mut Value| v["entries"] = serde_json::json!([])),
        ),
        (
            "a base pack with a parent",
            Box::new(|v: &mut Value| {
                v["parent"] = serde_json::json!({
                    "id": "core", "version": "0.9.0", "version_code": 9000,
                    "manifest_sha256": HASH_C,
                })
            }),
        ),
    ];

    for (what, mutate) in cases {
        let mut doc = base_pack();
        mutate(&mut doc);
        agree_pack(&doc, false, what);
    }
}

#[test]
fn a_delta_entry_satisfies_both() {
    let mut doc = base_pack();
    doc["kind"] = serde_json::json!("patch");
    doc["parent"] = serde_json::json!({
        "id": "core", "version": "0.9.0", "version_code": 9000,
        "manifest_sha256": HASH_C,
    });
    doc["entries"] = serde_json::json!([{
        "path": "/assets/big.bin",
        "op": "delta",
        "size": 1048576,
        "sha256": HASH_A,
        "blob": format!("blobs/{HASH_B}.zst"),
        "blob_sha256": HASH_B,
        "blob_size": 4096,
        "encoding": "zstd+bsdiff",
        "delta_base_sha256": HASH_C,
    }]);
    agree_pack(&doc, true, "a well-formed delta entry");

    let mut missing_base = doc.clone();
    missing_base["entries"][0]
        .as_object_mut()
        .unwrap()
        .remove("delta_base_sha256");
    agree_pack(&missing_base, false, "a delta without delta_base_sha256");

    for kind in ["base", "dlc", "mod"] {
        let mut not_patch = doc.clone();
        not_patch["kind"] = serde_json::json!(kind);
        not_patch.as_object_mut().unwrap().remove("parent");
        if kind == "mod" {
            not_patch["policies"] = serde_json::json!({ "trusted": false });
        }
        agree_pack(
            &not_patch,
            false,
            &format!("a {kind} pack carrying a delta"),
        );
    }

    let mut dlc_with_parent = doc.clone();
    dlc_with_parent["kind"] = serde_json::json!("dlc");
    agree_pack(
        &dlc_with_parent,
        false,
        "a dlc pack with a parent carrying a delta",
    );
}

#[test]
fn a_delete_entry_satisfies_both() {
    let mut doc = base_pack();
    doc["entries"] = serde_json::json!([{ "path": "/gone.css", "op": "delete" }]);
    agree_pack(&doc, true, "a tombstone");
}

#[test]
fn a_valid_channel_satisfies_both() {
    agree_channel(&channel_manifest(), true, "a channel with one base pack");
}

#[test]
fn a_channel_entry_with_a_shell_range_satisfies_both() {
    let mut doc = channel_manifest();
    doc["packs"][0]["min_shell"] = serde_json::json!("1.2.0");
    doc["packs"][0]["max_shell"] = serde_json::json!("2.5.0");
    agree_channel(&doc, true, "a pack entry carrying min_shell and max_shell");

    let parsed = ChannelManifest::parse(serde_json::to_vec(&doc).unwrap().as_slice()).unwrap();
    assert_eq!(parsed.packs[0].min_shell, Some("1.2.0".parse().unwrap()));
    assert_eq!(parsed.packs[0].max_shell, Some("2.5.0".parse().unwrap()));
}

#[test]
fn channel_rules_agree_on_rejection() {
    let cases: Vec<Mutation> = vec![
        (
            "an unknown spec tag",
            Box::new(|v: &mut Value| v["spec"] = serde_json::json!("tpk-channel/2")),
        ),
        (
            "an empty channel name",
            Box::new(|v: &mut Value| v["channel"] = serde_json::json!("")),
        ),
        (
            "a zero key_epoch",
            Box::new(|v: &mut Value| v["key_epoch"] = serde_json::json!(0)),
        ),
        (
            "a rollout of zero",
            Box::new(|v: &mut Value| v["packs"][0]["rollout"] = serde_json::json!(0)),
        ),
        (
            "a rollout above 100",
            Box::new(|v: &mut Value| v["packs"][0]["rollout"] = serde_json::json!(101)),
        ),
        (
            "a base pack carrying parent_version_code",
            Box::new(|v: &mut Value| {
                v["packs"][0]["parent_version_code"] = serde_json::json!(9000)
            }),
        ),
        (
            "a force_shell that is not SemVer",
            Box::new(|v: &mut Value| v["force_shell"] = serde_json::json!("not-a-version")),
        ),
        (
            "a channel min_shell that is not SemVer",
            Box::new(|v: &mut Value| v["min_shell"] = serde_json::json!("not-a-version")),
        ),
        (
            "a pack min_shell that is not SemVer",
            Box::new(|v: &mut Value| {
                v["packs"][0]["min_shell"] = serde_json::json!("not-a-version")
            }),
        ),
        (
            "a pack max_shell with a leading zero",
            Box::new(|v: &mut Value| v["packs"][0]["max_shell"] = serde_json::json!("2.05.0")),
        ),
        (
            "a patch without parent_version_code",
            Box::new(|v: &mut Value| v["packs"][0]["kind"] = serde_json::json!("patch")),
        ),
    ];

    for (what, mutate) in cases {
        let mut doc = channel_manifest();
        mutate(&mut doc);
        agree_channel(&doc, false, what);
    }
}

#[test]
fn the_semver_pattern_agrees_with_semver() {
    // Every field that parses to `semver::Version` carries the semver.org regex;
    // it has to accept exactly what `semver::Version` parses, including what it
    // serializes back out.
    let channel = schema("channel-manifest.schema.json");
    let pack = schema("pack-manifest.schema.json");
    let pattern = channel["properties"]["min_shell"]["pattern"]
        .as_str()
        .expect("channel min_shell has a pattern");
    for (doc, pointer) in [
        (&channel, "/properties/min_shell"),
        (&channel, "/properties/force_shell"),
        (&channel, "/$defs/packRef/properties/version"),
        (&channel, "/$defs/packRef/properties/min_shell"),
        (&channel, "/$defs/packRef/properties/max_shell"),
        (&pack, "/properties/version"),
        (&pack, "/properties/min_shell"),
        (&pack, "/properties/max_shell"),
        (&pack, "/properties/parent/properties/version"),
    ] {
        let field = doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("no {pointer}"));
        assert_eq!(
            field["pattern"], pattern,
            "one pattern everywhere: {pointer}"
        );
    }
    let validator = jsonschema::validator_for(&serde_json::json!({
        "type": "string",
        "pattern": pattern,
    }))
    .unwrap();

    for raw in [
        "0.0.0",
        "2.3.0",
        "10.20.30",
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-0.3.7",
        "1.0.0-x.7.z.92",
        "1.0.0-x-y-z.--",
        "1.0.0+20130313144700",
        "1.0.0-beta+exp.sha.5114f85",
        "1.0.0+21AF26D3----117B344092BD",
        "1.0.0+001",
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-01",
        "1.2.3-",
        "1.2.3+",
        "1.2.3-a..b",
        "1.2.3 ",
        " 1.2.3",
        "v1.2.3",
        "not-a-version",
    ] {
        let parsed = raw.parse::<semver::Version>();
        let by_schema = validator.is_valid(&serde_json::json!(raw));
        assert_eq!(
            by_schema,
            parsed.is_ok(),
            "pattern and semver disagree on {raw:?}"
        );
        if let Ok(version) = parsed {
            let shown = version.to_string();
            assert!(validator.is_valid(&serde_json::json!(shown)), "{shown}");
        }
    }
}

#[test]
fn a_store_state_document_matches_its_schema() {
    // The Rust type (`StoreState`) lives in tpk-store, which depends on this
    // crate, so pin a representative document of what it writes instead.
    let doc = serde_json::json!({
        "spec": "tpk-state/1",
        "pointer": "committed",
        "install_id": "8f14e45f-ceea-467a-9a2f-0b8f5c8b3a21",
        "committed": {
            "rev": "rev-7",
            "layers": [{
                "id": "core",
                "kind": "base",
                "version_code": 10000,
                "file_sha256": HASH_A,
                "size": 8400000,
                "mtime_ns": 1757600000000000000u64,
            }],
        },
        "staged": null,
        "booting": null,
        "boot_attempts": 0,
        "consecutive_rollbacks": 0,
        "last_watermark": { "stable": 202609111500u64 },
        "min_key_epoch": 1,
        "last_error": null,
    });
    let v = validator("store-state.schema.json");
    if let Err(e) = v.validate(&doc) {
        panic!("representative state.json rejected: {e}");
    }

    let cases: Vec<Mutation> = vec![
        (
            "an unknown spec tag",
            Box::new(|d: &mut Value| d["spec"] = serde_json::json!("tpk-state/2")),
        ),
        (
            "an unknown pointer",
            Box::new(|d: &mut Value| d["pointer"] = serde_json::json!("staged")),
        ),
        (
            "a malformed rev",
            Box::new(|d: &mut Value| d["committed"]["rev"] = serde_json::json!("7")),
        ),
        (
            "a scalar last_watermark",
            Box::new(|d: &mut Value| d["last_watermark"] = serde_json::json!(202609111500u64)),
        ),
        (
            "a zero min_key_epoch",
            Box::new(|d: &mut Value| d["min_key_epoch"] = serde_json::json!(0)),
        ),
    ];
    for (what, mutate) in cases {
        let mut bad = doc.clone();
        mutate(&mut bad);
        assert!(!v.is_valid(&bad), "schema should reject {what}");
    }
}
