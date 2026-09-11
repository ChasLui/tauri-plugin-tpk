//! Path rules for TPK entries (specification section 3.2).
//!
//! Entry paths are POSIX-style, always absolute, always NFC, and never contain
//! `.`, `..`, empty segments, backslashes or drive letters.
//!
//! In the overlay model pack contents never touch the filesystem — a path is
//! only a lookup key into the index. The rules are still enforced strictly:
//! they keep two spellings of the same path from becoming two distinct keys,
//! and they keep the rules meaningful for the one place that *does* write to
//! disk (seed copying).

use unicode_normalization::{is_nfc, UnicodeNormalization};

use crate::error::{FormatError, Result};

/// Path prefixes reserved for the host runtime. A pack may not shadow them.
const RESERVED_PREFIXES: &[&str] = &["/.tauri", "/__tauri"];

/// A validated TPK entry path: absolute, POSIX, NFC, no traversal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackPath(String);

impl PackPath {
    /// Validate a path as written in a `tpk-manifest.json`.
    ///
    /// # Errors
    ///
    /// Returns [`FormatError::Path`] if any rule in section 3.2 is violated.
    pub fn parse(raw: &str) -> Result<Self> {
        let reject = |reason: &'static str| -> FormatError {
            FormatError::Path {
                path: raw.to_string(),
                reason,
            }
        };

        if raw.is_empty() {
            return Err(reject("empty"));
        }
        if !raw.starts_with('/') {
            return Err(reject("must start with '/'"));
        }
        if raw.contains('\\') {
            return Err(reject("backslash is not a separator"));
        }
        if raw.contains('\0') {
            return Err(reject("contains NUL"));
        }
        // Normalization is checked, never applied: silently rewriting a signed
        // manifest's bytes would make the path differ from what was signed.
        if !is_nfc(raw) {
            return Err(reject("not Unicode NFC"));
        }

        for segment in raw[1..].split('/') {
            match segment {
                "" => return Err(reject("empty path segment")),
                "." => return Err(reject("'.' segment")),
                ".." => return Err(reject("'..' segment")),
                _ => {}
            }
            if is_drive_letter(segment) {
                return Err(reject("drive letter segment"));
            }
        }

        for reserved in RESERVED_PREFIXES {
            if raw == *reserved || raw.starts_with(&format!("{reserved}/")) {
                return Err(reject("reserved prefix"));
            }
        }

        Ok(Self(raw.to_string()))
    }

    /// The path as written, including the leading `/`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume into the owned `String`.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for PackPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `C:` and friends. Rejected anywhere in a path, not just at the front.
fn is_drive_letter(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Convert a Tauri asset key into a [`PackPath`], returning `None` when the key
/// cannot name a pack entry.
///
/// Tauri hands us keys without a leading slash (`index.html`,
/// `assets/app.js`); a leading slash is tolerated. Unlike [`PackPath::parse`]
/// this normalizes to NFC rather than rejecting, because the key comes from the
/// WebView rather than from signed bytes.
pub fn normalize_asset_key(key: &str) -> Option<PackPath> {
    let trimmed = key.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let normalized: String = if is_nfc(trimmed) {
        trimmed.to_string()
    } else {
        trimmed.nfc().collect()
    };
    PackPath::parse(&format!("/{normalized}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_paths() {
        for ok in [
            "/index.html",
            "/assets/app.js",
            "/a/b/c/d.txt",
            "/file with spaces.txt",
            "/dot.in.name.js",
            "/..leading-dots-are-fine.txt",
            "/trailing..txt",
        ] {
            assert!(PackPath::parse(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn rejects_relative_paths() {
        assert!(PackPath::parse("index.html").is_err());
        assert!(PackPath::parse("").is_err());
    }

    #[test]
    fn rejects_parent_dir() {
        for bad in ["/../etc/passwd", "/a/../b", "/a/b/..", "/.."] {
            assert!(PackPath::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_current_dir() {
        for bad in ["/./a", "/a/./b", "/."] {
            assert!(PackPath::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_empty_segment() {
        for bad in ["//a", "/a//b", "/a/"] {
            assert!(PackPath::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_backslash() {
        for bad in [r"/a\b", r"/..\..\windows", r"\a"] {
            assert!(PackPath::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_drive_letter() {
        for bad in ["/C:/windows", "/a/C:/b", "/c:"] {
            assert!(PackPath::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_nul() {
        assert!(PackPath::parse("/a\0b").is_err());
    }

    #[test]
    fn rejects_reserved_prefix() {
        for bad in [
            "/.tauri",
            "/.tauri/ipc.js",
            "/__tauri",
            "/__tauri/bridge.js",
        ] {
            assert!(PackPath::parse(bad).is_err(), "should reject {bad:?}");
        }
        // A longer name that merely starts with the same letters is fine.
        assert!(PackPath::parse("/__tauriesque.js").is_ok());
    }

    #[test]
    fn accepts_nfc_unicode() {
        // U+00E9 — composed, already NFC.
        assert!(PackPath::parse("/caf\u{e9}.html").is_ok());
    }

    #[test]
    fn rejects_nfd_unicode() {
        // "e" + U+0301 combining acute — decomposed, not NFC.
        let nfd = "/cafe\u{301}.html";
        assert!(!is_nfc(nfd));
        assert!(PackPath::parse(nfd).is_err());
    }

    #[test]
    fn asset_key_gets_a_leading_slash() {
        assert_eq!(
            normalize_asset_key("index.html").unwrap().as_str(),
            "/index.html"
        );
        assert_eq!(
            normalize_asset_key("/index.html").unwrap().as_str(),
            "/index.html"
        );
        assert_eq!(
            normalize_asset_key("assets/app.js").unwrap().as_str(),
            "/assets/app.js"
        );
    }

    #[test]
    fn asset_key_rejects_traversal_and_empties() {
        for bad in ["", "/", "../etc/passwd", "a/../../b", "./x", "a//b"] {
            assert!(
                normalize_asset_key(bad).is_none(),
                "should reject key {bad:?}"
            );
        }
    }

    #[test]
    fn asset_key_normalizes_rather_than_rejecting() {
        // The WebView may hand us a decomposed key; map it onto the NFC entry.
        let from_nfd = normalize_asset_key("cafe\u{301}.html").unwrap();
        let from_nfc = normalize_asset_key("caf\u{e9}.html").unwrap();
        assert_eq!(from_nfd, from_nfc);
    }
}
