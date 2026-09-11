//! The overlay rules from specification section 6, against real packs.
#![cfg(feature = "test-packs")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tpk_format::manifest::{PackId, PackKind, Sha256Hex};
use tpk_format::pack::PackBuilder;
use tpk_format::secret::SecretKey;
use tpk_format::sign::{sha256_hex, TrustStore, TrustedKey};
use tpk_resolve::{IndexBuilder, LayerSpec, MaterializedSource, ResolveMiss, Resolver};

/// What a test layer should contain.
enum Content<'a> {
    Full(&'a str, &'a [u8]),
    Delete(&'a str),
    /// `(path, patch_stream, result, base_sha)`
    Delta(&'a str, &'a [u8], &'a [u8], Sha256Hex),
}

struct World {
    dir: tempfile::TempDir,
    key: SecretKey,
    trust: Arc<TrustStore>,
    specs: Vec<LayerSpec>,
}

impl World {
    fn new() -> Self {
        let key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        Self {
            dir: tempfile::tempdir().unwrap(),
            key,
            trust,
            specs: Vec::new(),
        }
    }

    /// Append a layer on top of the stack.
    fn layer(&mut self, id: &str, kind: PackKind, version_code: u64, items: Vec<Content<'_>>) {
        let path = self
            .dir
            .path()
            .join(format!("{id}-{version_code}-{kind:?}.tpk"));

        let mut builder = PackBuilder::new(
            kind,
            PackId::parse(id).unwrap(),
            "1.0.0".parse().unwrap(),
            version_code,
            "2026-09-11T15:00:00Z",
        );
        if kind == PackKind::Patch {
            builder = builder.parent(tpk_format::manifest::ParentRef {
                id: PackId::parse(id).unwrap(),
                version: "0.9.0".parse().unwrap(),
                version_code: version_code - 1,
                manifest_sha256: sha256_hex(b"parent"),
            });
        }
        for item in items {
            match item {
                Content::Full(p, c) => builder.add_full(p, c).unwrap(),
                Content::Delete(p) => builder.add_delete(p).unwrap(),
                Content::Delta(p, stream, result, base) => {
                    builder.add_delta(p, stream, result, base).unwrap()
                }
            }
        }
        let summary = builder.build(&self.key, &path).unwrap();
        self.specs.push(LayerSpec {
            path,
            file_sha256: summary.file_sha256,
        });
    }

    fn resolver(&self) -> Resolver {
        self.resolver_with(32 * 1024 * 1024, Box::new(Materialized::default()), false)
    }

    fn resolver_with(
        &self,
        budget: u64,
        materialized: Box<dyn MaterializedSource>,
        allow_mods: bool,
    ) -> Resolver {
        let mut builder = IndexBuilder::new(Arc::clone(&self.trust), 1).allow_mods(allow_mods);
        for spec in &self.specs {
            builder.push_layer(spec);
        }
        builder.build(budget, materialized)
    }

    fn last_path(&self) -> PathBuf {
        self.specs.last().unwrap().path.clone()
    }
}

/// An in-memory stand-in for the store's materialized-delta cache.
#[derive(Default)]
struct Materialized {
    by_hash: HashMap<Sha256Hex, Vec<u8>>,
}

impl Materialized {
    fn with(result: &[u8]) -> Self {
        let mut by_hash = HashMap::new();
        by_hash.insert(sha256_hex(result), result.to_vec());
        Self { by_hash }
    }
}

impl MaterializedSource for Materialized {
    fn get(&self, sha256: &Sha256Hex) -> Option<Vec<u8>> {
        self.by_hash.get(sha256).cloned()
    }
}

fn get(r: &Resolver, path: &str) -> Vec<u8> {
    r.get(path)
        .unwrap_or_else(|e| panic!("{path} should resolve, got {e:?}"))
        .to_vec()
}

#[test]
fn a_single_base_serves_its_own_files() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![
            Content::Full("/index.html", b"<!doctype html>"),
            Content::Full("/app.js", b"export const v = 1;"),
        ],
    );
    let r = w.resolver();

    assert_eq!(get(&r, "/index.html"), b"<!doctype html>");
    assert_eq!(get(&r, "/app.js"), b"export const v = 1;");
    assert_eq!(r.get("/absent.js").unwrap_err(), ResolveMiss::NotFound);
    assert!(r.failed_layers().is_empty());
}

#[test]
fn a_tombstone_hides_a_lower_full() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/legacy.css", b"body{}")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Delete("/legacy.css")],
    );
    let r = w.resolver();

    // Deleted is distinct from NotFound on purpose: the caller must not fall
    // through to the embedded assets, or the deletion would never take effect.
    assert_eq!(r.get("/legacy.css").unwrap_err(), ResolveMiss::Deleted);
    assert!(!r.visible_paths().contains(&"/legacy.css".to_string()));
}

#[test]
fn a_higher_full_overrides_a_lower_delete() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Delete("/revived.js")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Full("/revived.js", b"back again")],
    );
    assert_eq!(get(&w.resolver(), "/revived.js"), b"back again");
}

#[test]
fn patches_apply_in_stack_order() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/v.txt", b"one")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Full("/v.txt", b"two")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        3,
        vec![Content::Full("/v.txt", b"three")],
    );
    assert_eq!(get(&w.resolver(), "/v.txt"), b"three");
}

#[test]
fn a_dlc_stacks_above_patches() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/theme.css", b"base")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Full("/theme.css", b"patched")],
    );
    w.layer(
        "maps",
        PackKind::Dlc,
        1,
        vec![Content::Full("/theme.css", b"from dlc")],
    );
    assert_eq!(get(&w.resolver(), "/theme.css"), b"from dlc");
}

#[test]
fn a_delta_is_served_from_the_materialized_cache() {
    let result = b"the reconstructed contents".repeat(10);
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/big.bin", b"the original contents")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Delta(
            "/big.bin",
            &[0u8; 64],
            &result,
            sha256_hex(b"the original contents"),
        )],
    );

    let r = w.resolver_with(
        32 * 1024 * 1024,
        Box::new(Materialized::with(&result)),
        false,
    );
    assert_eq!(get(&r, "/big.bin"), result);
}

#[test]
fn an_unmaterialized_delta_marks_the_layer_corrupt() {
    let result = b"never materialized".repeat(10);
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/big.bin", b"original")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Delta(
            "/big.bin",
            &[0u8; 64],
            &result,
            sha256_hex(b"original"),
        )],
    );

    // Empty cache: the OS may purge it at any time, so this must be an ordinary
    // recoverable outcome rather than a panic.
    let r = w.resolver();
    assert!(matches!(
        r.get("/big.bin").unwrap_err(),
        ResolveMiss::LayerCorrupt { .. }
    ));
}

#[test]
fn a_materialized_result_with_the_wrong_hash_is_refused() {
    let result = b"the expected result".repeat(10);
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/big.bin", b"original")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Delta(
            "/big.bin",
            &[0u8; 64],
            &result,
            sha256_hex(b"original"),
        )],
    );

    // The cache directory is not a trust boundary; content coming back from it
    // is checked like anything else.
    struct Liar;
    impl MaterializedSource for Liar {
        fn get(&self, _sha256: &Sha256Hex) -> Option<Vec<u8>> {
            Some(b"something else entirely".to_vec())
        }
    }

    let r = w.resolver_with(32 * 1024 * 1024, Box::new(Liar), false);
    assert!(matches!(
        r.get("/big.bin").unwrap_err(),
        ResolveMiss::LayerCorrupt { .. }
    ));
}

#[test]
fn a_layer_whose_file_hash_moved_is_skipped() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/a.js", b"good")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Full("/a.js", b"tampered")],
    );

    // Rewrite the top pack after it was recorded.
    let victim = w.last_path();
    let mut bytes = std::fs::read(&victim).unwrap();
    let n = bytes.len();
    bytes[n / 2] ^= 0xff;
    std::fs::write(&victim, &bytes).unwrap();

    let r = w.resolver();
    assert_eq!(r.failed_layers().len(), 1);
    assert!(r.failed_layers()[0].reason.contains("file hash"));
    // The stack degrades to the layers that are still intact.
    assert_eq!(get(&r, "/a.js"), b"good");
}

#[test]
fn a_layer_signed_by_a_stranger_is_skipped() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/a.js", b"trusted")],
    );

    // A pack built with a different key, recorded correctly otherwise.
    let stranger = SecretKey::generate();
    let path = w.dir.path().join("rogue.tpk");
    let mut b = PackBuilder::new(
        PackKind::Dlc,
        PackId::parse("rogue").unwrap(),
        "1.0.0".parse().unwrap(),
        1,
        "2026-09-11T15:00:00Z",
    );
    b.add_full("/a.js", b"untrusted").unwrap();
    let summary = b.build(&stranger, &path).unwrap();
    w.specs.push(LayerSpec {
        path,
        file_sha256: summary.file_sha256,
    });

    let r = w.resolver();
    assert_eq!(r.failed_layers().len(), 1);
    assert_eq!(get(&r, "/a.js"), b"trusted");
}

#[test]
fn a_missing_layer_file_is_skipped() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/a.js", b"present")],
    );
    w.specs.push(LayerSpec {
        path: w.dir.path().join("does-not-exist.tpk"),
        file_sha256: sha256_hex(b"nothing"),
    });

    let r = w.resolver();
    assert_eq!(r.failed_layers().len(), 1);
    assert_eq!(get(&r, "/a.js"), b"present");
}

#[test]
fn mod_layers_are_skipped_unless_explicitly_enabled() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/index.html", b"the real page")],
    );
    w.layer(
        "skin",
        PackKind::Mod,
        1,
        vec![Content::Full("/index.html", b"replaced by a mod")],
    );

    // Default: the mod does not load at all.
    let r = w.resolver();
    assert_eq!(r.failed_layers().len(), 1);
    assert_eq!(get(&r, "/index.html"), b"the real page");

    // Explicitly enabled, it wins — which is exactly why it is off by default.
    let r = w.resolver_with(32 * 1024 * 1024, Box::new(Materialized::default()), true);
    assert!(r.failed_layers().is_empty());
    assert_eq!(get(&r, "/index.html"), b"replaced by a mod");
}

#[test]
fn visible_paths_is_the_union_minus_tombstones() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![
            Content::Full("/a.js", b"a"),
            Content::Full("/b.js", b"b"),
            Content::Full("/c.js", b"c"),
        ],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Delete("/b.js"), Content::Full("/d.js", b"d")],
    );
    assert_eq!(w.resolver().visible_paths(), ["/a.js", "/c.js", "/d.js"]);
}

#[test]
fn repeated_reads_are_served_from_cache() {
    let mut w = World::new();
    let content = b"cache me".repeat(100);
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/a.js", &content)],
    );
    let r = w.resolver();

    let first = r.get("/a.js").unwrap();
    let second = r.get("/a.js").unwrap();
    assert_eq!(&*first, &*second);
    assert!(Arc::ptr_eq(&first, &second), "second read should be a hit");
}

#[test]
fn a_zero_budget_still_serves_every_read() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/a.js", b"no cache")],
    );
    let r = w.resolver_with(0, Box::new(Materialized::default()), false);
    assert_eq!(get(&r, "/a.js"), b"no cache");
    assert_eq!(get(&r, "/a.js"), b"no cache");
}

#[test]
fn csp_hashes_cover_scripts_the_overlay_serves() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![
            Content::Full("/app.js", b"console.log(1);"),
            Content::Full("/mod.mjs", b"export default 1;"),
            Content::Full("/index.html", b"<!doctype html>"),
            Content::Full("/style.css", b"body{}"),
        ],
    );
    let r = w.resolver();
    let hashes = r.csp_script_hashes();

    // Two scripts, and nothing for the HTML or the stylesheet.
    assert_eq!(hashes.len(), 2, "{hashes:?}");
    assert!(hashes.iter().all(|h| h.starts_with("'sha256-")));
    assert_eq!(hashes, r.csp_script_hashes(), "must be computed once");
}

#[test]
fn csp_hashes_follow_the_top_layer() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/app.js", b"console.log('old');")],
    );
    let before = w.resolver().csp_script_hashes().to_vec();

    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Full("/app.js", b"console.log('new');")],
    );
    let after = w.resolver().csp_script_hashes().to_vec();

    assert_ne!(
        before, after,
        "the hash must describe what is actually served"
    );
    assert_eq!(after.len(), 1);
}

#[test]
fn a_deleted_script_drops_out_of_the_csp_hashes() {
    let mut w = World::new();
    w.layer(
        "core",
        PackKind::Base,
        1,
        vec![Content::Full("/gone.js", b"console.log(1);")],
    );
    w.layer(
        "core",
        PackKind::Patch,
        2,
        vec![Content::Delete("/gone.js")],
    );
    assert!(w.resolver().csp_script_hashes().is_empty());
}

#[test]
fn an_empty_stack_resolves_nothing_without_failing() {
    let w = World::new();
    let r = w.resolver();
    assert_eq!(r.get("/anything").unwrap_err(), ResolveMiss::NotFound);
    assert!(r.visible_paths().is_empty());
    assert!(r.failed_layers().is_empty());
}
