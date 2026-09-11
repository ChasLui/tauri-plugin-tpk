//! Packaging rules for HTML, enforced at build time rather than documented.
//!
//! Two independent reasons, one rule:
//!
//! * **CSP.** Tauri injects the hashes of the *embedded* HTML's inline scripts.
//!   Once a pack replaces that HTML, those hashes no longer match and every
//!   inline script in the new page is blocked — silently, at runtime, on a
//!   user's device.
//! * **Google Play.** The Device and Network Abuse policy lists "a webview with
//!   added JavaScript Interface that loads untrusted web content" as a
//!   violation example, and Tauri's IPC bridge is exactly such an interface.
//!
//! Deliberately a byte scan rather than a real parser: the rule is "no inline
//! scripts and no remote script/iframe sources at all", which needs no DOM. A
//! scan can over-report on exotic markup, and for a build-time gate that fails
//! in the safe direction.

/// Why a file was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlViolation {
    /// The rule that was broken.
    pub reason: &'static str,
    /// A short excerpt showing where.
    pub excerpt: String,
}

impl std::fmt::Display for HtmlViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — near: {}", self.reason, self.excerpt)
    }
}

/// Check one HTML document.
///
/// # Errors
///
/// Returns the first violation found.
pub fn check_html(content: &[u8]) -> Result<(), HtmlViolation> {
    let text = String::from_utf8_lossy(content);
    let lower = text.to_lowercase();

    check_inline_scripts(&text, &lower)?;
    check_remote_sources(&text, &lower)?;
    Ok(())
}

fn check_inline_scripts(text: &str, lower: &str) -> Result<(), HtmlViolation> {
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find("<script") {
        let tag_start = cursor + rel;
        let Some(rel_gt) = lower[tag_start..].find('>') else {
            return Err(violation("unterminated <script> tag", text, tag_start));
        };
        let body_start = tag_start + rel_gt + 1;
        let body_end = lower[body_start..]
            .find("</script")
            .map(|r| body_start + r)
            .unwrap_or(text.len());

        if !text[body_start..body_end].trim().is_empty() {
            return Err(violation(
                "inline <script> is not allowed in a TPK pack; move it to an external file",
                text,
                tag_start,
            ));
        }
        cursor = body_end.max(body_start);
    }
    Ok(())
}

fn check_remote_sources(text: &str, lower: &str) -> Result<(), HtmlViolation> {
    for attr in ["src=\"http", "src='http", "href=\"http", "href='http"] {
        if let Some(at) = lower.find(attr) {
            // Stylesheets and anchors are fine; script and iframe are not. Look
            // back for the tag this attribute belongs to.
            let tag_start = lower[..at].rfind('<').unwrap_or(0);
            let tag = &lower[tag_start..at];
            if tag.starts_with("<script") || tag.starts_with("<iframe") {
                return Err(violation(
                    "remote <script>/<iframe> source is not allowed in a TPK pack",
                    text,
                    tag_start,
                ));
            }
        }
    }
    Ok(())
}

fn violation(reason: &'static str, text: &str, at: usize) -> HtmlViolation {
    let start = at.saturating_sub(20);
    let end = (at + 80).min(text.len());
    let excerpt = text
        .get(start..end)
        .unwrap_or("")
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string();
    HtmlViolation { reason, excerpt }
}

/// Files must keep a recognisable extension.
///
/// Tauri derives the response `Content-Type` from the request path, and its
/// extension table is short: an unknown extension falls back to `text/html`,
/// which breaks `WebAssembly.instantiateStreaming` and font loading in ways
/// that are painful to trace back to the packer.
pub fn check_extension(path: &str) -> Result<(), HtmlViolation> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.contains('.') && !name.ends_with('.') {
        return Ok(());
    }
    Err(HtmlViolation {
        reason: "file has no extension; Tauri would serve it as text/html",
        excerpt: path.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_html_with_only_external_scripts() {
        let html = br#"<!doctype html>
<html>
  <head>
    <link rel="stylesheet" href="/assets/app.css">
    <script type="module" src="/assets/app.js"></script>
  </head>
  <body><div id="root"></div></body>
</html>"#;
        assert!(check_html(html).is_ok());
    }

    #[test]
    fn rejects_inline_scripts() {
        for html in [
            br#"<script>console.log("hi")</script>"#.as_slice(),
            br#"<script type="module">import "./a.js"</script>"#.as_slice(),
            b"<SCRIPT>\n  var x = 1;\n</SCRIPT>",
            br#"<script>window.__INITIAL_STATE__ = {}</script>"#.as_slice(),
        ] {
            let err = check_html(html).unwrap_err();
            assert!(err.reason.contains("inline"), "{err}");
        }
    }

    #[test]
    fn accepts_empty_script_tags() {
        // `<script src=...></script>` has an empty body; that is the shape we
        // are steering people towards, so it must not trip the check.
        assert!(check_html(br#"<script src="/a.js"></script>"#).is_ok());
        assert!(check_html(b"<script src=\"/a.js\">\n\n</script>").is_ok());
    }

    #[test]
    fn rejects_remote_script_and_iframe_sources() {
        for html in [
            br#"<script src="https://cdn.example.com/a.js"></script>"#.as_slice(),
            br#"<script src='http://cdn.example.com/a.js'></script>"#.as_slice(),
            br#"<iframe src="https://example.com/embed"></iframe>"#.as_slice(),
        ] {
            let err = check_html(html).unwrap_err();
            assert!(err.reason.contains("remote"), "{err}");
        }
    }

    #[test]
    fn allows_remote_stylesheets_and_links() {
        // A remote stylesheet cannot execute; an anchor is just navigation.
        assert!(check_html(br#"<link rel="stylesheet" href="https://x.com/a.css">"#).is_ok());
        assert!(check_html(br#"<a href="https://example.com">docs</a>"#).is_ok());
    }

    #[test]
    fn reports_an_excerpt_to_locate_the_problem() {
        let html = b"<html><body><p>lots of text here</p><script>bad()</script></body></html>";
        let err = check_html(html).unwrap_err();
        assert!(err.excerpt.contains("script"), "{}", err.excerpt);
    }

    #[test]
    fn handles_unterminated_tags_without_panicking() {
        assert!(check_html(b"<script").is_err());
        assert!(check_html(b"<script>unclosed").is_err());
    }

    #[test]
    fn handles_non_utf8_without_panicking() {
        assert!(check_html(&[0xff, 0xfe, 0x00, 0x01]).is_ok());
    }

    #[test]
    fn extension_check() {
        for ok in ["/index.html", "/assets/app.js", "/a/b/c.woff2", "/x.wasm"] {
            assert!(check_extension(ok).is_ok(), "should accept {ok}");
        }
        for bad in ["/LICENSE", "/assets/hashedname", "/a/b/c."] {
            assert!(check_extension(bad).is_err(), "should reject {bad}");
        }
    }
}
