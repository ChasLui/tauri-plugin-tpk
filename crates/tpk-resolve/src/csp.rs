//! CSP script hashes for overlaid content.
//!
//! What Tauri actually does is narrower than it first appears, and getting this
//! wrong is easy:
//!
//! * The compile-time "global" hashes are computed over the **contents of each
//!   `.js`/`.mjs` file**, not over the HTML.
//! * Inline `<script>` hashes are keyed by HTML path, and once a pack replaces
//!   that HTML the stored hashes describe a page that no longer exists. Packs
//!   therefore may not contain inline scripts at all — enforced at pack time by
//!   the CLI, not merely documented.
//! * Whenever any hash is present, Tauri also injects `'self'` into the
//!   directive. Same-origin `tauri://` scripts are already allowed by that, so
//!   the hashes we add here change nothing for scripts we serve. They exist to
//!   keep the directive consistent with what is actually being served, not to
//!   unblock anything.
//!
//! Consequently this module is cheap and lazy: when the app has no CSP
//! configured — the default — `csp_hashes` is never called and none of this
//! runs.

use sha2::{Digest, Sha256};

/// Normalize a script the way `tauri-utils` does before hashing.
///
/// Reimplemented rather than imported: the upstream helper sits behind the
/// `html-manipulation` feature, which is not available at runtime. The rule is
/// small and stable — CRLF and lone CR both become LF — but it must match
/// exactly or every hash we produce is wrong.
pub fn normalize_script_for_csp(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'\r' => {
                out.push(b'\n');
                // Swallow the LF of a CRLF pair.
                if input.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    out
}

/// The `'sha256-…'` token for one script's contents.
pub fn script_hash(content: &[u8]) -> String {
    use base64::Engine as _;
    let normalized = normalize_script_for_csp(content);
    let digest = Sha256::digest(&normalized);
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// Whether a path names a script Tauri would hash.
pub fn is_script_path(path: &str) -> bool {
    path.ends_with(".js") || path.ends_with(".mjs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_and_lone_cr_both_become_lf() {
        assert_eq!(normalize_script_for_csp(b"a\r\nb"), b"a\nb");
        assert_eq!(normalize_script_for_csp(b"a\rb"), b"a\nb");
        assert_eq!(normalize_script_for_csp(b"a\nb"), b"a\nb");
        assert_eq!(normalize_script_for_csp(b"a\r\n\r\nb"), b"a\n\nb");
        assert_eq!(normalize_script_for_csp(b"trailing\r\n"), b"trailing\n");
        assert_eq!(normalize_script_for_csp(b"\r"), b"\n");
        assert_eq!(normalize_script_for_csp(b""), b"");
    }

    #[test]
    fn line_endings_do_not_change_the_hash() {
        // The whole point of normalizing: the same source checked out on
        // Windows and on Linux must produce the same CSP token.
        assert_eq!(
            script_hash(b"console.log(1);\r\nconsole.log(2);\r\n"),
            script_hash(b"console.log(1);\nconsole.log(2);\n")
        );
    }

    #[test]
    fn hash_has_the_csp_token_shape() {
        let h = script_hash(b"console.log(1);");
        assert!(h.starts_with("'sha256-"), "{h}");
        assert!(h.ends_with('\''), "{h}");
        // base64 of 32 bytes is 44 characters including padding.
        assert_eq!(h.len(), "'sha256-".len() + 44 + 1, "{h}");
    }

    #[test]
    fn different_content_hashes_differently() {
        assert_ne!(script_hash(b"a();"), script_hash(b"b();"));
    }

    #[test]
    fn recognizes_script_paths() {
        assert!(is_script_path("/app.js"));
        assert!(is_script_path("/assets/chunk-abc123.mjs"));
        assert!(!is_script_path("/index.html"));
        assert!(!is_script_path("/style.css"));
        assert!(!is_script_path("/data.json"));
        // Not a script despite the substring.
        assert!(!is_script_path("/js/readme.txt"));
    }
}
