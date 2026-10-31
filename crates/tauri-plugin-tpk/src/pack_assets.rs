//! The `Assets` implementation that puts the overlay in front of the binary's
//! embedded content.
//!
//! Three constraints from Tauri's real API shape this file:
//!
//! * `attach` runs before `Builder::build`, where no path can be resolved yet —
//!   `app_local_data_dir()` goes through JNI on Android. So `attach` only swaps
//!   the assets provider; everything stateful happens later, in the plugin
//!   `setup` hook, which still runs before any window exists.
//! * `CspHash<'a>` borrows, so whatever backs those strings has to live as long
//!   as `&self`. They are leaked once, at startup, and there are a few dozen.
//! * Layers are frozen for the process lifetime by design, so the resolver goes
//!   in a `OnceLock` rather than a lock — a running WebView always sees one
//!   consistent index.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use tauri::{Assets, Runtime};
use tauri_utils::assets::{AssetKey, AssetsIter, CspHash};
use tpk_format::path::normalize_asset_key;
use tpk_resolve::{ResolveMiss, Resolver};

/// State shared between the assets provider and the plugin that fills it in.
#[derive(Debug, Default)]
pub struct Shared {
    resolver: OnceLock<Arc<Resolver>>,
    csp: OnceLock<Vec<&'static str>>,
}

impl Shared {
    /// Install the resolver. Subsequent calls are ignored.
    ///
    /// Returns whether this call was the one that installed it.
    pub fn set_resolver(&self, resolver: Arc<Resolver>) -> bool {
        self.resolver.set(resolver).is_ok()
    }

    /// The installed resolver, if the plugin got that far.
    pub fn resolver(&self) -> Option<&Arc<Resolver>> {
        self.resolver.get()
    }
}

/// Serves overlay content, falling back to the embedded assets.
pub struct PackAssets<R: Runtime> {
    fallback: Box<dyn Assets<R>>,
    shared: Arc<Shared>,
}

impl<R: Runtime> std::fmt::Debug for PackAssets<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackAssets")
            .field("resolver_installed", &self.shared.resolver().is_some())
            .finish()
    }
}

impl<R: Runtime> PackAssets<R> {
    /// Wrap the embedded assets.
    pub fn new(fallback: Box<dyn Assets<R>>, shared: Arc<Shared>) -> Self {
        Self { fallback, shared }
    }

    /// Try the overlay for one key.
    ///
    /// `Ok(None)` means "the overlay deliberately has nothing here, and the
    /// embedded assets must not be consulted either" — that is what a tombstone
    /// means, and falling through would make deletions impossible.
    fn overlay_get(&self, key: &str) -> Result<Option<Vec<u8>>, Tombstoned> {
        let Some(resolver) = self.shared.resolver() else {
            return Ok(None);
        };
        let Some(path) = normalize_asset_key(key) else {
            return Ok(None);
        };
        match resolver.get(path.as_str()) {
            Ok(bytes) => Ok(Some(bytes.to_vec())),
            Err(ResolveMiss::Deleted) => Err(Tombstoned),
            Err(ResolveMiss::NotFound) => Ok(None),
            Err(ResolveMiss::LayerCorrupt {
                file_sha256,
                reason,
            }) => {
                // The layer is bad, but a single broken asset must not take the
                // app down. Report it and let the embedded copy answer.
                log::error!("[tpk] layer {file_sha256} is corrupt: {reason}");
                Ok(None)
            }
            Err(ResolveMiss::NotMaterialized { file_sha256 }) => {
                // The cache was purged; setup rebuilds it in the background.
                log::warn!("[tpk] {path} from layer {file_sha256} is not materialized yet");
                Ok(None)
            }
        }
    }
}

/// The overlay says this path is gone.
struct Tombstoned;

impl<R: Runtime> Assets<R> for PackAssets<R> {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        let key_str = key.as_ref();

        // Mirrors Tauri's own resolution chain, which routed pages depend on.
        let trimmed = key_str.trim_end_matches('/');
        let candidates = [
            key_str.to_string(),
            format!("{trimmed}.html"),
            format!("{trimmed}/index.html"),
        ];

        for candidate in &candidates {
            match self.overlay_get(candidate) {
                Ok(Some(bytes)) => return Some(Cow::Owned(bytes)),
                Ok(None) => {}
                Err(Tombstoned) => return None,
            }
        }
        self.fallback.get(key)
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        let Some(resolver) = self.shared.resolver() else {
            return self.fallback.iter();
        };
        // The union, with overlay paths taking precedence. Only
        // `AssetResolver::iter` reaches this; nothing inside Tauri depends on it.
        // Both sides are normalized through `AssetKey` before being compared:
        // pack paths always carry a leading slash, an `Assets` implementation
        // need not, and comparing the two spellings directly would silently let
        // the embedded copy shadow the overlay.
        let normalize = |k: &str| AssetKey::from(k).as_ref().to_string();

        let overlay: Vec<String> = resolver.visible_paths();
        let mut seen: std::collections::HashSet<String> =
            overlay.iter().map(|p| normalize(p)).collect();

        let mut items: Vec<(Cow<'_, str>, Cow<'_, [u8]>)> = Vec::new();
        for path in &overlay {
            if let Ok(Some(bytes)) = self.overlay_get(path) {
                items.push((Cow::Owned(normalize(path)), Cow::Owned(bytes)));
            }
        }
        for (key, bytes) in self.fallback.iter() {
            let key = normalize(&key);
            if seen.insert(key.clone()) {
                items.push((Cow::Owned(key), Cow::Owned(bytes.to_vec())));
            }
        }
        Box::new(items.into_iter())
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        let Some(resolver) = self.shared.resolver() else {
            return self.fallback.csp_hashes(html_path);
        };

        let hashes = self.shared.csp.get_or_init(|| {
            // Leaked deliberately: `CspHash` borrows, and these live for the
            // process anyway. A few dozen short strings, computed once.
            resolver
                .csp_script_hashes()
                .iter()
                .map(|h| &*Box::leak(h.clone().into_boxed_str()))
                .collect()
        });

        // If the overlay serves this HTML, the embedded document's inline-script
        // hashes describe a page that no longer exists; carrying them over would
        // widen the policy for scripts nobody serves. Packs may not contain
        // inline scripts at all — `tpk pack` rejects them — so there is nothing
        // to replace them with.
        let overlay_owns_html = normalize_asset_key(html_path.as_ref())
            .is_some_and(|p| matches!(resolver.get(p.as_str()), Ok(_) | Err(ResolveMiss::Deleted)));

        let own = hashes.iter().copied().map(CspHash::Script);
        if overlay_owns_html {
            Box::new(own)
        } else {
            Box::new(self.fallback.csp_hashes(html_path).chain(own))
        }
    }
}

/// Handle to the overlay, returned by [`attach`] and consumed by the plugin.
#[derive(Debug, Clone)]
pub struct TpkHandle {
    shared: Arc<Shared>,
}

impl TpkHandle {
    /// Turn Tauri's SPA fallback into an empty 404 for an explicitly deleted asset.
    ///
    /// Wire this into each `WebviewWindowBuilder::on_web_resource_request` callback
    /// **before building the webview**. Save the request URI before passing the
    /// request to any host callback, then call this method after that callback so
    /// it filters the final response. Installing the plugin does not wire this hook.
    ///
    /// Only the local Tauri origins (`tauri://localhost`, `http://tauri.localhost`,
    /// and `https://tauri.localhost`) and the exact canonical asset path are checked.
    /// Ordinary missing routes keep their SPA fallback. The frozen resolver index
    /// is consulted without IO; a higher layer's full entry restores a deleted path.
    /// Mobile development requests are left alone because they use the dev server.
    ///
    /// Tauri does not call the response hook when core asset resolution fails
    /// (for example, when no fallback `index.html` exists), so this method cannot
    /// force a 404 in that case.
    pub fn filter_web_resource_response(
        &self,
        uri: &tauri::http::Uri,
        response: &mut tauri::http::Response<Cow<'static, [u8]>>,
    ) {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if tauri::is_dev() {
            return;
        }

        let local_origin = matches!(
            (uri.scheme_str(), uri.authority().map(|a| a.as_str())),
            (Some("tauri"), Some("localhost")) | (Some("http" | "https"), Some("tauri.localhost"))
        );
        if !local_origin {
            return;
        }
        let Some(resolver) = self.shared.resolver() else {
            return;
        };

        // Mirror core protocol resolution: remove one literal trailing slash
        // before decoding, decode once, and map the root to index.html.
        let path = uri.path().strip_suffix('/').unwrap_or(uri.path());
        let decoded = percent_encoding::percent_decode(path.as_bytes()).decode_utf8_lossy();
        let key = if decoded.is_empty() {
            "index.html"
        } else {
            decoded.strip_prefix('/').unwrap_or(&decoded)
        };
        let key = AssetKey::from(key);
        let Some(path) = normalize_asset_key(key.as_ref()) else {
            return;
        };
        if !resolver
            .index()
            .lookup(path.as_str())
            .is_some_and(|loc| loc.op == tpk_format::manifest::Op::Delete)
        {
            return;
        }

        *response.status_mut() = tauri::http::StatusCode::NOT_FOUND;
        *response.body_mut() = Cow::Borrowed(&[]);
        // Remove metadata for the discarded entity, retaining CORS and other
        // security headers that still apply to the empty response.
        let headers = response.headers_mut();
        for name in [
            "content-type",
            "content-length",
            "content-encoding",
            "content-range",
            "content-language",
            "content-location",
            "content-disposition",
            "content-security-policy",
            "content-security-policy-report-only",
            "etag",
            "last-modified",
            "expires",
            "age",
            "accept-ranges",
            "transfer-encoding",
            "trailer",
            "digest",
            "content-digest",
            "repr-digest",
            "content-md5",
        ] {
            headers.remove(name);
        }
        headers.insert(
            tauri::http::header::CACHE_CONTROL,
            tauri::http::HeaderValue::from_static("no-store"),
        );
    }

    /// The shared slot the plugin fills in during `setup`.
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }
}

/// Put the overlay in front of the binary's embedded assets.
///
/// Call this on the context from `generate_context!` **before** building the
/// app, and pass the handle to `init`.
///
/// Nothing stateful happens here on purpose: `app_local_data_dir()` goes
/// through JNI on Android and is not available this early, so the store is
/// opened later in the plugin `setup` hook — which still runs before any window
/// exists.
///
/// Warns, and otherwise does nothing different, if the app uses the isolation
/// pattern; see the log message for why that combination is unlikely to work.
pub fn attach<R: Runtime>(context: &mut tauri::Context<R>) -> TpkHandle {
    // The config carries the pattern in a form that is always present, unlike
    // `tauri::Pattern::Isolation`, which only exists with tauri's `isolation`
    // feature and so cannot be matched on from here.
    if matches!(
        context.config().app.security.pattern,
        tauri_utils::config::PatternKind::Isolation { .. }
    ) {
        log::warn!(
            "[tpk] the isolation pattern is enabled; packs and isolation are not \
             known to work together. Tauri resolves the isolation frame's \
             `index.html` itself and injects a hook generated at build time, so \
             HTML shipped in a pack may not be served, and pack content will not \
             carry that hook."
        );
    }

    // Two swaps: the first takes the embedded provider out (leaving a
    // placeholder), the second puts it back inside the overlay as its fallback.
    let embedded = context.set_assets(Box::new(EmptyAssets));
    let shared = Arc::new(Shared::default());
    context.set_assets(Box::new(PackAssets::new(embedded, Arc::clone(&shared))));
    TpkHandle { shared }
}

/// An assets provider that answers nothing.
///
/// Only used as the placeholder value while swapping the real one out.
pub struct EmptyAssets;

impl<R: Runtime> Assets<R> for EmptyAssets {
    fn get(&self, _key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        None
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }

    fn csp_hashes(&self, _html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tauri::test::MockRuntime;
    use tpk_format::manifest::{PackId, PackKind};
    use tpk_format::pack::PackBuilder;
    use tpk_format::secret::SecretKey;
    use tpk_format::sign::{TrustStore, TrustedKey};
    use tpk_resolve::{IndexBuilder, LayerSpec, NoMaterialized};

    /// Stands in for the binary's compiled-in assets.
    #[derive(Default)]
    struct MockAssets {
        files: HashMap<String, Vec<u8>>,
        csp: Vec<&'static str>,
    }

    impl MockAssets {
        fn with(files: &[(&str, &[u8])]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(k, v)| (AssetKey::from(*k).as_ref().to_string(), v.to_vec()))
                    .collect(),
                csp: Vec::new(),
            }
        }
    }

    impl Assets<MockRuntime> for MockAssets {
        fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
            // `AssetKey::from` prepends a root, so stored keys carry it too —
            // exactly what the generated `EmbeddedAssets` does.
            self.files.get(key.as_ref()).map(|v| Cow::Borrowed(&v[..]))
        }

        fn iter(&self) -> Box<AssetsIter<'_>> {
            Box::new(
                self.files
                    .iter()
                    .map(|(k, v)| (Cow::Borrowed(k.as_str()), Cow::Borrowed(&v[..]))),
            )
        }

        fn csp_hashes(&self, _html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
            Box::new(self.csp.iter().copied().map(CspHash::Script))
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        assets: PackAssets<MockRuntime>,
    }

    /// Build a one-layer overlay over the given embedded files.
    fn fixture(embedded: &[(&str, &[u8])], overlay: &[(&str, &[u8])], deleted: &[&str]) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );

        let shared = Arc::new(Shared::default());
        let assets = PackAssets::new(Box::new(MockAssets::with(embedded)), Arc::clone(&shared));

        if !overlay.is_empty() || !deleted.is_empty() {
            let path = dir.path().join("base.tpk");
            let mut builder = PackBuilder::new(
                PackKind::Base,
                PackId::parse("core").unwrap(),
                "1.0.0".parse().unwrap(),
                1,
                "2026-09-11T15:00:00Z",
            );
            for (p, c) in overlay {
                builder.add_full(p, c).unwrap();
            }
            for p in deleted {
                builder.add_delete(p).unwrap();
            }
            let summary = builder.build(&key, &path).unwrap();

            let mut ib = IndexBuilder::new(trust, 1);
            ib.push_layer(&LayerSpec {
                path,
                file_sha256: summary.file_sha256,
            });
            shared.set_resolver(Arc::new(ib.build(0, Box::new(NoMaterialized))));
        }

        Fixture { _dir: dir, assets }
    }

    fn spa_response() -> tauri::http::Response<Cow<'static, [u8]>> {
        tauri::http::Response::builder()
            .header("content-type", "text/html")
            .header("content-length", "16")
            .header("content-encoding", "gzip")
            .header("content-security-policy", "script-src 'self'")
            .header("etag", "\"index\"")
            .header("last-modified", "Wed, 01 Jan 2025 00:00:00 GMT")
            .header("cache-control", "max-age=3600")
            .header("access-control-allow-origin", "tauri://localhost")
            .header("x-content-type-options", "nosniff")
            .header("cross-origin-resource-policy", "same-origin")
            .body(Cow::Borrowed(&b"<html>SPA</html>"[..]))
            .unwrap()
    }

    fn filter(f: &Fixture, uri: &str, response: &mut tauri::http::Response<Cow<'static, [u8]>>) {
        TpkHandle {
            shared: Arc::clone(&f.assets.shared),
        }
        .filter_web_resource_response(&uri.parse().unwrap(), response);
    }

    #[test]
    fn response_guard_replaces_deleted_spa_and_retains_security_headers() {
        let f = fixture(&[], &[], &["/removed.txt"]);
        let mut response = spa_response();
        filter(&f, "tauri://localhost/removed.txt", &mut response);
        assert_eq!(response.status(), tauri::http::StatusCode::NOT_FOUND);
        assert!(response.body().is_empty());
        for header in [
            "content-type",
            "content-length",
            "content-encoding",
            "content-security-policy",
            "etag",
            "last-modified",
        ] {
            assert!(!response.headers().contains_key(header), "{header}");
        }
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "tauri://localhost"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.headers()["cross-origin-resource-policy"],
            "same-origin"
        );
    }

    #[test]
    fn response_guard_matches_core_path_normalization_and_local_origins() {
        let f = fixture(&[], &[], &["/removed.txt", "/index.html", "/caf\u{e9}.txt"]);
        for uri in [
            "tauri://localhost/removed.txt",
            "tauri://localhost/removed%2Etxt/?query=ignored",
            "http://tauri.localhost/removed.txt",
            "https://tauri.localhost/removed.txt",
            "tauri://localhost/",
            "tauri://localhost/cafe%CC%81.txt",
        ] {
            let mut response = spa_response();
            filter(&f, uri, &mut response);
            assert_eq!(
                response.status(),
                tauri::http::StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    #[test]
    fn response_guard_preserves_spa_and_does_not_infer_aliases_or_decode_twice() {
        let f = fixture(
            &[],
            &[],
            &["/removed.txt", "/about.html", "/docs/index.html"],
        );
        for uri in [
            "tauri://localhost/missing.txt",
            "tauri://localhost/client/route",
            "tauri://localhost/about",
            "tauri://localhost/docs/",
            "tauri://localhost/removed%252Etxt",
        ] {
            let mut response = spa_response();
            let before = spa_response();
            filter(&f, uri, &mut response);
            assert_eq!(response.status(), before.status(), "{uri}");
            assert_eq!(response.headers(), before.headers(), "{uri}");
            assert_eq!(response.body(), before.body(), "{uri}");
        }
    }

    #[test]
    fn response_guard_uses_platform_asset_key_for_remaining_trailing_slashes() {
        let f = fixture(&[], &[], &["/removed.txt"]);
        for uri in [
            "tauri://localhost/removed.txt%2F",
            "tauri://localhost/removed.txt//",
        ] {
            let mut response = spa_response();
            filter(&f, uri, &mut response);
            // Windows AssetKey reconstructs path components, removing remaining
            // trailing slashes; Unix AssetKey retains them, as core Tauri does.
            let expected = if cfg!(windows) {
                tauri::http::StatusCode::NOT_FOUND
            } else {
                tauri::http::StatusCode::OK
            };
            assert_eq!(response.status(), expected, "{uri}");
        }
    }

    #[test]
    fn response_guard_leaves_external_origins_and_uninitialized_handle_unchanged() {
        let f = fixture(&[], &[], &["/removed.txt"]);
        for uri in [
            "https://example.com/removed.txt",
            "https://localhost/removed.txt",
            "tauri://example.com/removed.txt",
            "https://tauri.localhost.example.com/removed.txt",
            "https://tauri.localhost:8443/removed.txt",
            "/removed.txt",
        ] {
            let mut response = spa_response();
            let before = spa_response();
            filter(&f, uri, &mut response);
            assert_eq!(response.status(), before.status(), "{uri}");
            assert_eq!(response.headers(), before.headers(), "{uri}");
            assert_eq!(response.body(), before.body(), "{uri}");
        }
        let f = fixture(&[], &[], &[]);
        let mut response = spa_response();
        filter(&f, "tauri://localhost/removed.txt", &mut response);
        assert_eq!(response.headers(), spa_response().headers());
        assert_eq!(response.body(), spa_response().body());
        assert_eq!(response.status(), tauri::http::StatusCode::OK);
    }

    #[test]
    fn response_guard_uses_winning_operation_when_a_higher_layer_restores_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let mut index = IndexBuilder::new(trust, 1);
        for (version, delete) in [(1, true), (2, false)] {
            let path = dir.path().join(format!("{version}.tpk"));
            let mut builder = PackBuilder::new(
                PackKind::Base,
                PackId::parse("core").unwrap(),
                "1.0.0".parse().unwrap(),
                version,
                "2026-09-11T15:00:00Z",
            );
            if delete {
                builder.add_delete("/removed.txt").unwrap();
            } else {
                builder.add_full("/removed.txt", b"restored").unwrap();
            }
            let summary = builder.build(&key, &path).unwrap();
            index.push_layer(&LayerSpec {
                path,
                file_sha256: summary.file_sha256,
            });
        }
        let resolver = Arc::new(index.build(0, Box::new(NoMaterialized)));
        assert!(resolver.failed_layers().is_empty());
        assert_eq!(
            resolver.index().lookup("/removed.txt").unwrap().op,
            tpk_format::manifest::Op::Full
        );
        let shared = Arc::new(Shared::default());
        shared.set_resolver(resolver);
        let handle = TpkHandle { shared };
        let mut response = spa_response();
        handle.filter_web_resource_response(
            &"tauri://localhost/removed.txt".parse().unwrap(),
            &mut response,
        );
        assert_eq!(response.status(), tauri::http::StatusCode::OK);
        assert_eq!(response.headers(), spa_response().headers());
        assert_eq!(response.body(), spa_response().body());
    }

    /// `CspHash` is `#[non_exhaustive]`, so the wildcard arm is required: a new
    /// variant in a Tauri point release must not break the build.
    fn hash_text(h: CspHash<'_>) -> String {
        match h {
            CspHash::Script(s) | CspHash::Style(s) => s.to_string(),
            _ => String::new(),
        }
    }

    fn get(f: &Fixture, key: &str) -> Option<Vec<u8>> {
        Assets::<MockRuntime>::get(&f.assets, &key.into()).map(|c| c.to_vec())
    }

    #[test]
    fn the_overlay_wins_over_embedded_content() {
        let f = fixture(
            &[("index.html", b"embedded")],
            &[("/index.html", b"from the overlay")],
            &[],
        );
        assert_eq!(get(&f, "index.html").unwrap(), b"from the overlay");
    }

    #[test]
    fn embedded_content_answers_what_the_overlay_lacks() {
        let f = fixture(
            &[("only-embedded.js", b"embedded")],
            &[("/index.html", b"overlay")],
            &[],
        );
        assert_eq!(get(&f, "only-embedded.js").unwrap(), b"embedded");
    }

    #[test]
    fn without_a_resolver_everything_comes_from_embedded() {
        // This is the state during `attach`, and after a disk failure in setup.
        let f = fixture(&[("index.html", b"embedded")], &[], &[]);
        assert_eq!(get(&f, "index.html").unwrap(), b"embedded");
        assert!(get(&f, "missing.js").is_none());
    }

    #[test]
    fn the_html_fallback_chain_is_preserved() {
        let f = fixture(
            &[],
            &[
                ("/about.html", b"about page"),
                ("/docs/index.html", b"docs index"),
            ],
            &[],
        );
        // Routed pages depend on these two rewrites.
        assert_eq!(get(&f, "about").unwrap(), b"about page");
        assert_eq!(get(&f, "docs").unwrap(), b"docs index");
        assert_eq!(get(&f, "docs/").unwrap(), b"docs index");
    }

    #[test]
    fn an_exact_match_beats_the_html_fallback() {
        let f = fixture(
            &[],
            &[("/about", b"exact"), ("/about.html", b"fallback")],
            &[],
        );
        assert_eq!(get(&f, "about").unwrap(), b"exact");
    }

    #[test]
    fn a_tombstone_does_not_fall_through_to_embedded() {
        let f = fixture(&[("legacy.css", b"embedded copy")], &[], &["/legacy.css"]);
        // The whole point of a tombstone is to remove something the binary
        // still ships. Falling through would make deletion impossible.
        assert!(get(&f, "legacy.css").is_none());
    }

    #[test]
    fn a_traversal_key_resolves_nothing() {
        let f = fixture(&[], &[("/app.js", b"overlay")], &[]);
        assert!(get(&f, "../../../etc/passwd").is_none());
    }

    #[test]
    fn an_empty_key_lands_on_the_index() {
        // `AssetKey::from("")` is `/`, and `/` falls through to `/index.html` —
        // the same rewrite Tauri applies for a bare origin request.
        let f = fixture(&[], &[("/index.html", b"the index")], &[]);
        assert_eq!(get(&f, "").unwrap(), b"the index");
    }

    #[test]
    fn iter_is_the_union_with_the_overlay_winning() {
        let f = fixture(
            &[("index.html", b"embedded"), ("only-embedded.js", b"e")],
            &[("/index.html", b"overlay"), ("/only-overlay.js", b"o")],
            &[],
        );
        let items: HashMap<String, Vec<u8>> = Assets::<MockRuntime>::iter(&f.assets)
            .map(|(k, v)| (k.trim_start_matches('/').to_string(), v.to_vec()))
            .collect();

        assert_eq!(items.len(), 3, "{:?}", items.keys().collect::<Vec<_>>());
        assert_eq!(items["index.html"], b"overlay");
        assert_eq!(items["only-embedded.js"], b"e");
        assert_eq!(items["only-overlay.js"], b"o");
    }

    #[test]
    fn iter_falls_back_entirely_without_a_resolver() {
        let f = fixture(&[("a.js", b"x"), ("b.js", b"y")], &[], &[]);
        assert_eq!(Assets::<MockRuntime>::iter(&f.assets).count(), 2);
    }

    #[test]
    fn csp_hashes_cover_overlay_scripts() {
        let f = fixture(&[], &[("/app.js", b"console.log(1);")], &[]);
        let hashes: Vec<String> =
            Assets::<MockRuntime>::csp_hashes(&f.assets, &"index.html".into())
                .map(hash_text)
                .collect();
        assert_eq!(hashes.len(), 1, "{hashes:?}");
        assert!(hashes[0].starts_with("'sha256-"), "{hashes:?}");
    }

    #[test]
    fn embedded_hashes_are_dropped_once_the_overlay_owns_the_html() {
        let mut embedded = MockAssets::with(&[("index.html", b"embedded page")]);
        embedded.csp = vec!["'sha256-EMBEDDEDINLINESCRIPT'"];

        let dir = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        let trust = Arc::new(
            TrustStore::new(&[TrustedKey {
                key: key.public_key_base64(),
                epoch: 1,
            }])
            .unwrap(),
        );
        let shared = Arc::new(Shared::default());
        let assets = PackAssets::new(Box::new(embedded), Arc::clone(&shared));

        let path = dir.path().join("base.tpk");
        let mut builder = PackBuilder::new(
            PackKind::Base,
            PackId::parse("core").unwrap(),
            "1.0.0".parse().unwrap(),
            1,
            "2026-09-11T15:00:00Z",
        );
        builder.add_full("/index.html", b"overlay page").unwrap();
        let summary = builder.build(&key, &path).unwrap();
        let mut ib = IndexBuilder::new(trust, 1);
        ib.push_layer(&LayerSpec {
            path,
            file_sha256: summary.file_sha256,
        });
        shared.set_resolver(Arc::new(ib.build(0, Box::new(NoMaterialized))));

        let hashes: Vec<String> = Assets::<MockRuntime>::csp_hashes(&assets, &"index.html".into())
            .map(hash_text)
            .collect();
        // The embedded document's inline-script hash describes a page that is no
        // longer being served; keeping it would widen the policy for nothing.
        assert!(
            !hashes.iter().any(|h| h.contains("EMBEDDEDINLINESCRIPT")),
            "{hashes:?}"
        );
    }

    #[test]
    fn csp_hashes_fall_back_when_the_overlay_does_not_own_the_html() {
        let mut embedded = MockAssets::with(&[("index.html", b"embedded page")]);
        embedded.csp = vec!["'sha256-EMBEDDEDINLINESCRIPT'"];
        let shared = Arc::new(Shared::default());
        let assets = PackAssets::new(Box::new(embedded), shared);

        let hashes: Vec<String> = Assets::<MockRuntime>::csp_hashes(&assets, &"index.html".into())
            .map(hash_text)
            .collect();
        assert_eq!(hashes, ["'sha256-EMBEDDEDINLINESCRIPT'"]);
    }

    #[test]
    fn the_resolver_can_only_be_installed_once() {
        let shared = Shared::default();
        let make = || {
            Arc::new(
                IndexBuilder::new(
                    Arc::new(
                        TrustStore::new(&[TrustedKey {
                            key: SecretKey::generate().public_key_base64(),
                            epoch: 1,
                        }])
                        .unwrap(),
                    ),
                    1,
                )
                .build(0, Box::new(NoMaterialized)),
            )
        };
        assert!(shared.set_resolver(make()));
        // Layers are frozen for the process lifetime by design; a second install
        // would mean a running WebView could see two different indexes.
        assert!(!shared.set_resolver(make()));
    }

    #[test]
    fn attach_wraps_the_embedded_assets_without_losing_them() {
        let mut context: tauri::Context<MockRuntime> =
            tauri::test::mock_context(tauri::test::noop_assets());
        let handle = attach(&mut context);

        // Nothing is resolved yet — that is the plugin's job, later.
        assert!(handle.shared().resolver().is_none());
        // And the provider is in place and answering (with nothing, since the
        // mock has nothing).
        assert!(context.assets().get(&"index.html".into()).is_none());
    }

    #[test]
    fn attach_warns_but_still_works_under_the_isolation_pattern() {
        let mut context: tauri::Context<MockRuntime> =
            tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().app.security.pattern = tauri_utils::config::PatternKind::Isolation {
            dir: "../dist-isolation".into(),
        };

        // Only that it degrades to a warning: `log` has no capture here, and a
        // logger installed just to read one string back is not worth it.
        let handle = attach(&mut context);
        assert!(handle.shared().resolver().is_none());
        assert!(context.assets().get(&"index.html".into()).is_none());
    }

    #[test]
    fn empty_assets_answer_nothing() {
        let empty = EmptyAssets;
        assert!(Assets::<MockRuntime>::get(&empty, &"index.html".into()).is_none());
        assert_eq!(Assets::<MockRuntime>::iter(&empty).count(), 0);
        assert_eq!(
            Assets::<MockRuntime>::csp_hashes(&empty, &"index.html".into()).count(),
            0
        );
    }
}
