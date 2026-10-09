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
//! scripts and nothing that loads executable content from outside the pack",
//! which needs no DOM. Every `<letter` in the file is treated as a possible tag
//! start and parsed with the browser's attribute rules, so markup the browser
//! would not see as a tag (comments, raw text, attribute values) can only cause
//! over-rejection, never hide a real tag. A build-time gate that over-reports
//! fails in the safe direction.
//!
//! `<link>` is left alone: stylesheets cannot execute, and `modulepreload`
//! fetches and compiles a module without evaluating it.
//!
//! The one inline `<script>` body allowed is a JSON data block: the first
//! `type` attribute, trimmed of ASCII whitespace and compared case-insensitively,
//! is exactly `application/json` or `application/ld+json`. Browsers never
//! execute those and CSP `script-src` does not govern them, so SSR hydration
//! data and JSON-LD would otherwise be false positives. Everything else stays
//! rejected — no or empty `type`, `module`, JavaScript types, `importmap` and
//! `speculationrules` (both subject to CSP), unknown types, and MIME parameters
//! such as `application/json; charset=utf-8`, which a browser would also treat
//! as data but which nothing needs. Only the first `type` counts because the
//! HTML tokenizer drops duplicate attributes. Source rules apply regardless:
//! a data block with a remote `src` is still rejected.
//!
//! [`check_worker_sources`] extends the same "nothing executable from outside
//! the pack" rule to packed JavaScript, where `new Worker`, `new SharedWorker`,
//! `navigator.serviceWorker.register`, `importScripts()` and a dynamic
//! static and dynamic `import`, and `export … from` can all fetch code the HTML
//! never mentions. It is **a lint, not a sandbox**.
//!
//! What it judges: an argument that is a string literal — single-quoted,
//! double-quoted, or a template with no substitution — either directly or
//! inside `new URL(<literal>, import.meta.url)`, against the same
//! [`is_local_url`] rule the HTML check uses. `importScripts` is checked on
//! every argument, the others on the first. The constructor may be wrapped or
//! qualified (`new globalThis.Worker(…)`, `new (Worker)(…)`).
//!
//! What it only counts: anything it cannot read — a variable, a concatenation,
//! a template with a substitution, a computed constructor such as
//! `new g["Worker"]()`, and any file where a bare `/` was read as a regex
//! literal (the walk has no expression context, so that call is a guess). Those
//! are reported so a publisher can audit them, never rejected.
//!
//! What still gets through, by design: escapes are not decoded, so a scheme
//! spelled `"\x68ttps://…"` reads as local; a value assembled at runtime is
//! invisible; and a nested template literal or a misjudged `/` can still
//! desynchronise the walk — the regex guess narrows that to a warning rather
//! than silence, it does not close it. What this catches is the shape a bundler
//! actually emits.

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
    // ponytail: every candidate is parsed to its own `>`, so pathological input
    // (thousands of unterminated quotes) is O(n²); fine for a build-time lint.
    for start in 0..content.len() {
        if content[start] == b'<' && content.get(start + 1).is_some_and(u8::is_ascii_alphabetic) {
            check_tag(content, start, &parse_tag(content, start))?;
        }
    }
    Ok(())
}

struct Tag<'a> {
    name: &'a [u8],
    attrs: Vec<(&'a [u8], &'a [u8])>,
    /// Index just past the closing `>`, or `None` if the input ended first.
    end: Option<usize>,
}

impl Tag<'_> {
    fn attr<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s [u8]> + 's {
        self.attrs
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name.as_bytes()))
            .map(|(_, v)| *v)
    }
}

fn is_html_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Tokenize the tag starting at `b[start] == b'<'`, following the HTML
/// tokenizer's tag-name and attribute states.
fn parse_tag(b: &[u8], start: usize) -> Tag<'_> {
    let len = b.len();
    let mut i = start + 1;
    while i < len && !is_html_space(b[i]) && b[i] != b'/' && b[i] != b'>' {
        i += 1;
    }
    let name = &b[start + 1..i];
    let mut attrs = Vec::new();
    loop {
        while i < len && (is_html_space(b[i]) || b[i] == b'/') {
            i += 1;
        }
        if i >= len {
            return Tag {
                name,
                attrs,
                end: None,
            };
        }
        if b[i] == b'>' {
            return Tag {
                name,
                attrs,
                end: Some(i + 1),
            };
        }
        let name_start = i;
        // The first character of an attribute name may be `=`.
        i += 1;
        while i < len && !is_html_space(b[i]) && !matches!(b[i], b'/' | b'>' | b'=') {
            i += 1;
        }
        let attr_name = &b[name_start..i];
        let mut j = i;
        while j < len && is_html_space(b[j]) {
            j += 1;
        }
        if j >= len || b[j] != b'=' {
            attrs.push((attr_name, &b[i..i]));
            continue;
        }
        i = j + 1;
        while i < len && is_html_space(b[i]) {
            i += 1;
        }
        let value = match b.get(i) {
            Some(&quote @ (b'"' | b'\'')) => {
                let value_start = i + 1;
                let value_end = b[value_start..]
                    .iter()
                    .position(|&c| c == quote)
                    .map_or(len, |p| value_start + p);
                i = (value_end + 1).min(len);
                &b[value_start..value_end]
            }
            _ => {
                let value_start = i;
                while i < len && !is_html_space(b[i]) && b[i] != b'>' {
                    i += 1;
                }
                &b[value_start..i]
            }
        };
        attrs.push((attr_name, value));
    }
}

fn check_tag(b: &[u8], start: usize, tag: &Tag<'_>) -> Result<(), HtmlViolation> {
    const REMOTE: &str = "remote or non-local <script>/<iframe>/<frame>/<embed>/<object> source is not allowed in a TPK pack";
    let name = tag.name.to_ascii_lowercase();
    let loaders: &[&str] = match name.as_slice() {
        b"script" => &["src", "href", "xlink:href"],
        b"iframe" | b"frame" | b"embed" => &["src"],
        b"object" => &["data"],
        _ => &[],
    };
    for attr in loaders {
        if tag.attr(attr).any(|v| !is_local_url(v)) {
            return Err(violation(REMOTE, b, start));
        }
    }

    match name.as_slice() {
        b"script" => {
            let Some(body_start) = tag.end else {
                return Err(violation("unterminated <script> tag", b, start));
            };
            // `</script` only ends the element when followed by whitespace,
            // `/`, `>` or EOF; `</scriptFOO>` is still script text.
            let body_end = (body_start..b.len())
                .find(|&i| {
                    b[i..].len() >= 8
                        && b[i..i + 8].eq_ignore_ascii_case(b"</script")
                        && b.get(i + 8)
                            .is_none_or(|&c| is_html_space(c) || matches!(c, b'/' | b'>'))
                })
                .unwrap_or(b.len());
            // `attr` yields in source order, and the tokenizer keeps the first
            // of duplicate attributes.
            let data_block = tag.attr("type").next().is_some_and(|t| {
                let t = t.trim_ascii();
                t.eq_ignore_ascii_case(b"application/json")
                    || t.eq_ignore_ascii_case(b"application/ld+json")
            });
            if !data_block && !b[body_start..body_end].iter().all(u8::is_ascii_whitespace) {
                return Err(violation(
                    "inline <script> is not allowed in a TPK pack; move it to an external file",
                    b,
                    start,
                ));
            }
        }
        b"iframe" if tag.attr("srcdoc").next().is_some() => {
            return Err(violation(
                "<iframe srcdoc> is not allowed in a TPK pack",
                b,
                start,
            ));
        }
        b"base" if tag.attr("href").next().is_some() => {
            return Err(violation(
                "<base href> is not allowed in a TPK pack; it redirects relative script URLs",
                b,
                start,
            ));
        }
        b"meta"
            if tag
                .attr("http-equiv")
                .any(|v| v.trim_ascii().eq_ignore_ascii_case(b"refresh")) =>
        {
            return Err(violation(
                "<meta http-equiv=refresh> is not allowed in a TPK pack",
                b,
                start,
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Whether a URL attribute value stays inside the pack.
///
/// Mirrors what the URL parser sees: tab/LF/CR are removed and leading C0
/// controls and spaces are dropped. Anything with a scheme (`https:`, `data:`,
/// `javascript:`, `blob:` ...), a protocol-relative prefix (`//`, `\\`, `/\`),
/// or a character reference that could spell either is treated as non-local.
fn is_local_url(value: &[u8]) -> bool {
    let cleaned: Vec<u8> = value
        .iter()
        .copied()
        .filter(|c| !matches!(c, b'\t' | b'\n' | b'\r'))
        .collect();
    let v = &cleaned[cleaned
        .iter()
        .position(|&c| c > b' ')
        .unwrap_or(cleaned.len())..];

    let before_query = &v[..v
        .iter()
        .position(|&c| matches!(c, b'?' | b'#'))
        .unwrap_or(v.len())];
    if before_query.contains(&b'&') {
        return false;
    }
    if matches!(v, [b'/' | b'\\', b'/' | b'\\', ..]) {
        return false;
    }
    let first_delimiter = v
        .iter()
        .position(|&c| matches!(c, b':' | b'/' | b'\\' | b'?' | b'#'));
    !matches!(first_delimiter, Some(p) if v[p] == b':')
}

/// Check the worker and remote-import sources in one packed script.
///
/// Returns how many call sites carry an argument no static check can judge; the
/// caller is expected to report that count so a publisher can audit them. See
/// the module docs for what this deliberately does not catch.
///
/// # Errors
///
/// Returns the first non-local source found.
pub fn check_worker_sources(content: &[u8]) -> Result<usize, HtmlViolation> {
    const REMOTE: &str = "remote or non-local Worker/SharedWorker/serviceWorker/importScripts/\
                          import() source is not allowed in a TPK pack";
    // Cheap prefilter: every form below spells one of these.
    if !mentions_any(content, &[b"Worker", b"import", b"export"]) {
        return Ok(0);
    }

    let mut dynamic = 0usize;
    // Set when the walk guessed "regex literal" for a bare `/`; the guess can
    // be wrong either way, so the file is reported for audit.
    let mut guessed_a_regex = false;
    let (mut prev, mut prev2) = (None, None);
    let mut i = 0;
    loop {
        i = skip_trivia(content, i);
        let token_start = i;
        let Some(&c) = content.get(i) else { break };
        let token = if let Some((name, next)) = ident_at(content, i) {
            i = next;
            Token::Ident(name)
        } else if matches!(c, b'"' | b'\'' | b'`') {
            i = skip_string(content, i);
            // Any string is one opaque token; its contents never form a call.
            Token::Punct(b'"')
        } else if c == b'/' && !prev.is_some_and(ends_an_operand) {
            // `skip_trivia` already took comments, so this `/` is either
            // division or a regex. Nothing before it can end an operand, so
            // read it as a regex and keep the walk in sync.
            guessed_a_regex = true;
            i = skip_regex(content, i);
            Token::Punct(b'/')
        } else {
            i += 1;
            Token::Punct(c)
        };

        let call = if ident_is(Some(token), b"new") {
            match new_callee(content, i) {
                Callee::Named(name, paren) if name == b"Worker" || name == b"SharedWorker" => {
                    Some(single(first_arg_url(content, paren + 1)))
                }
                // A computed constructor cannot be read, and is only worth an
                // audit line when the expression mentions a worker at all.
                Callee::Computed if mentions_worker_near(content, token_start) => {
                    Some((Vec::new(), true))
                }
                _ => None,
            }
        } else if ident_is(Some(token), b"register")
            && prev == Some(Token::Punct(b'.'))
            && ident_is(prev2, b"serviceWorker")
        {
            arg_paren(content, i).map(|p| single(first_arg_url(content, p + 1)))
        } else if ident_is(Some(token), b"importScripts") {
            // Takes any number of specifiers; every one of them loads code.
            arg_paren(content, i).map(|p| literal_args(content, p))
        } else if ident_is(Some(token), b"import") {
            // `import(...)`; `import.meta` and a static import have no `(`.
            arg_paren(content, i).map(|p| single(first_arg_url(content, p + 1)))
        } else {
            None
        };
        // Static imports and re-exports load a module just like dynamic import.
        // `from` is the only common syntax preceding their source literal;
        // side-effect imports put the literal directly after `import`.
        if matches!(content.get(token_start), Some(b'\'' | b'"'))
            && (ident_is(prev, b"from") || ident_is(prev, b"import"))
            && string_literal(content, token_start).is_some_and(|url| !is_local_url(url))
        {
            return Err(violation(REMOTE, content, token_start));
        }
        if let Some((urls, unread)) = call {
            if urls.iter().any(|u| !is_local_url(u)) {
                return Err(violation(REMOTE, content, token_start));
            }
            if unread {
                dynamic += 1;
            }
        }

        prev2 = prev;
        prev = Some(token);
    }
    Ok(dynamic + usize::from(guessed_a_regex))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Ident(&'a [u8]),
    Punct(u8),
}

/// A call argument, as far as it can be read statically.
enum Arg<'a> {
    Literal(&'a [u8]),
    Dynamic,
}

/// How far a `new` expression's constructor could be read.
enum Callee<'a> {
    /// A plain or dotted name, plus the index of the `(` opening its arguments.
    Named(&'a [u8], usize),
    /// A computed constructor — `new x[k]()`, `new (0, f)()` — or truncated input.
    Computed,
    /// Nothing the walk needs to look at: `new Map` with no arguments, `new.target`.
    Plain,
}

fn single(arg: Arg<'_>) -> (Vec<&[u8]>, bool) {
    match arg {
        Arg::Literal(url) => (vec![url], false),
        Arg::Dynamic => (Vec::new(), true),
    }
}

fn ident_is(token: Option<Token<'_>>, name: &[u8]) -> bool {
    matches!(token, Some(Token::Ident(n)) if n == name)
}

/// Whether a token can end an operand, which makes a following `/` division
/// rather than the start of a regex literal.
fn ends_an_operand(token: Token<'_>) -> bool {
    match token {
        Token::Ident(_) => true,
        Token::Punct(c) => matches!(c, b')' | b']' | b'"') || c.is_ascii_digit(),
    }
}

fn mentions_any(b: &[u8], needles: &[&[u8]]) -> bool {
    needles.iter().any(|n| b.windows(n.len()).any(|w| w == *n))
}

/// Whether the expression starting at `from` names a worker within reach.
fn mentions_worker_near(b: &[u8], from: usize) -> bool {
    const WINDOW: usize = 128;
    mentions_any(&b[from..(from + WINDOW).min(b.len())], &[b"Worker"])
}

/// Bytes JavaScript accepts inside an identifier. Non-ASCII is included so a
/// name with an accented letter stays one token; [`js_space_len`] takes the
/// non-ASCII whitespace back out.
fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$') || c >= 0x80
}

fn read_ident(b: &[u8], from: usize) -> (&[u8], usize) {
    let mut i = from;
    while i < b.len() && is_ident_byte(b[i]) && js_space_len(b, i) == 0 {
        i += 1;
    }
    (&b[from..i], i)
}

/// Read the identifier at `b[from]`, if one starts there.
fn ident_at(b: &[u8], from: usize) -> Option<(&[u8], usize)> {
    let c = *b.get(from)?;
    if !is_ident_byte(c) || c.is_ascii_digit() || js_space_len(b, from) > 0 {
        return None;
    }
    Some(read_ident(b, from))
}

/// Byte length of the ECMAScript WhiteSpace or LineTerminator at `b[from]`,
/// or 0 if there is none.
///
/// The non-ASCII ones matter: `new\u{a0}Worker(…)` runs, and without this the
/// NBSP would glue `new` and `Worker` into one identifier.
fn js_space_len(b: &[u8], from: usize) -> usize {
    match &b[from..] {
        [c, ..] if c.is_ascii_whitespace() || *c == 0x0b => 1,
        [0xc2, 0xa0, ..] => 2,                                // U+00A0
        [0xef, 0xbb, 0xbf, ..] | [0xe1, 0x9a, 0x80, ..] => 3, // U+FEFF, U+1680
        // U+2000..=U+200A, U+2028, U+2029, U+202F
        [0xe2, 0x80, c, ..] if (0x80..=0x8a).contains(c) || matches!(c, 0xa8 | 0xa9 | 0xaf) => 3,
        [0xe2, 0x81, 0x9f, ..] | [0xe3, 0x80, 0x80, ..] => 3, // U+205F, U+3000
        _ => 0,
    }
}

fn is_line_terminator(b: &[u8], from: usize) -> bool {
    matches!(b[from], b'\n' | b'\r')
        || matches!(b.get(from..from + 3), Some([0xe2, 0x80, 0xa8 | 0xa9]))
}

/// Skip whitespace, `//` and `/* */` between two tokens.
fn skip_trivia(b: &[u8], from: usize) -> usize {
    let mut i = from;
    loop {
        while i < b.len() {
            let n = js_space_len(b, i);
            if n == 0 {
                break;
            }
            i += n;
        }
        match b.get(i..i + 2) {
            Some(b"//") => {
                i += 2;
                while i < b.len() && !is_line_terminator(b, i) {
                    i += 1;
                }
            }
            Some(b"/*") => {
                i = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + p + 4);
            }
            _ => return i,
        }
    }
}

/// Index just past the string starting at `b[from]`, which is a quote.
fn skip_string(b: &[u8], from: usize) -> usize {
    let quote = b[from];
    let mut i = from + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == quote => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Index just past the regex literal starting at `b[from] == b'/'`.
fn skip_regex(b: &[u8], from: usize) -> usize {
    let mut i = from + 1;
    let mut in_class = false;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => return i + 1,
            // A regex literal cannot span lines; leave the walk at the break.
            b'\n' | b'\r' => return i,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Read the raw bytes of the string literal at `b[from]`, if that is one.
///
/// A template counts only when it has no substitution, which makes it a
/// compile-time constant like any other literal. Escapes are left as written: a
/// scheme spelled with them is not decoded, and one spelled plainly still trips
/// [`is_local_url`].
fn string_literal(b: &[u8], from: usize) -> Option<&[u8]> {
    let quote = *b.get(from)?;
    if !matches!(quote, b'"' | b'\'' | b'`') {
        return None;
    }
    let end = skip_string(b, from);
    // An unterminated literal ran to EOF and has no closing quote to trim.
    if b.get(end - 1) != Some(&quote) || end - 1 <= from {
        return None;
    }
    let body = &b[from + 1..end - 1];
    (quote != b'`' || !has_substitution(body)).then_some(body)
}

fn has_substitution(body: &[u8]) -> bool {
    let mut i = 0;
    while i + 1 < body.len() {
        match body[i] {
            b'\\' => i += 2,
            b'$' if body[i + 1] == b'{' => return true,
            _ => i += 1,
        }
    }
    false
}

/// Index of the `(` that opens an argument list at `from`, if there is one.
fn arg_paren(b: &[u8], from: usize) -> Option<usize> {
    let i = skip_trivia(b, from);
    (b.get(i) == Some(&b'(')).then_some(i)
}

/// Read `new <callee>(` starting just past the `new` keyword.
fn new_callee(b: &[u8], from: usize) -> Callee<'_> {
    /// `new ((Worker))(…)` is legal; a deeper nest is not worth following.
    const MAX_WRAPPERS: usize = 4;

    let mut i = skip_trivia(b, from);
    let mut wrappers = 0;
    while b.get(i) == Some(&b'(') && wrappers < MAX_WRAPPERS {
        wrappers += 1;
        i = skip_trivia(b, i + 1);
    }
    let Some((mut name, mut end)) = ident_at(b, i) else {
        // `new.target` is a meta-property, not a construction.
        return if b.get(i) == Some(&b'.') {
            Callee::Plain
        } else {
            Callee::Computed
        };
    };
    // `new globalThis.Worker(…)` — the last name in the chain is the class.
    loop {
        let dot = skip_trivia(b, end);
        if b.get(dot) != Some(&b'.') {
            break;
        }
        let Some((next, next_end)) = ident_at(b, skip_trivia(b, dot + 1)) else {
            return Callee::Computed;
        };
        (name, end) = (next, next_end);
    }
    let mut i = skip_trivia(b, end);
    if b.get(i) == Some(&b'[') {
        return Callee::Computed;
    }
    for _ in 0..wrappers {
        if b.get(i) != Some(&b')') {
            return Callee::Computed;
        }
        i = skip_trivia(b, i + 1);
    }
    if b.get(i) == Some(&b'(') {
        Callee::Named(name, i)
    } else {
        Callee::Plain
    }
}

/// Read a call's first argument, starting just past the `(`.
fn first_arg_url(b: &[u8], from: usize) -> Arg<'_> {
    let i = skip_trivia(b, from);
    if let Some(url) = string_literal(b, i) {
        return if arg_ends_at(b, skip_string(b, i)) {
            Arg::Literal(url)
        } else {
            Arg::Dynamic
        };
    }
    // `new URL(<literal>, import.meta.url)` — the bundler form. Local unless
    // the literal itself is not.
    let (kw, i) = read_ident(b, i);
    if kw != b"new" {
        return Arg::Dynamic;
    }
    let (ctor, i) = read_ident(b, skip_trivia(b, i));
    if ctor != b"URL" {
        return Arg::Dynamic;
    }
    let Some(paren) = arg_paren(b, i) else {
        return Arg::Dynamic;
    };
    let url_at = skip_trivia(b, paren + 1);
    let Some(url) = string_literal(b, url_at) else {
        return Arg::Dynamic;
    };
    let after_url = skip_trivia(b, skip_string(b, url_at));
    if b.get(after_url) != Some(&b',') {
        return Arg::Dynamic;
    }
    let (base, base_end) = read_ident(b, skip_trivia(b, after_url + 1));
    let meta_dot = skip_trivia(b, base_end);
    if base != b"import" || b.get(meta_dot) != Some(&b'.') {
        return Arg::Dynamic;
    }
    let (meta, meta_end) = read_ident(b, skip_trivia(b, meta_dot + 1));
    let url_dot = skip_trivia(b, meta_end);
    if meta != b"meta" || b.get(url_dot) != Some(&b'.') {
        return Arg::Dynamic;
    }
    let (base_url, url_end) = read_ident(b, skip_trivia(b, url_dot + 1));
    let close = skip_trivia(b, url_end);
    if base_url != b"url" || b.get(close) != Some(&b')') || !arg_ends_at(b, close + 1) {
        return Arg::Dynamic;
    }
    Arg::Literal(url)
}

/// Whether a literal or call ends the complete first argument.
fn arg_ends_at(b: &[u8], from: usize) -> bool {
    matches!(b.get(skip_trivia(b, from)), Some(b',' | b')'))
}

/// Read every string-literal argument of the call whose `(` is at `paren`.
///
/// The bool says whether an argument was left unread, which no static check
/// can judge.
fn literal_args(b: &[u8], paren: usize) -> (Vec<&[u8]>, bool) {
    let mut urls = Vec::new();
    let mut i = skip_trivia(b, paren + 1);
    loop {
        if b.get(i) == Some(&b')') {
            return (urls, false);
        }
        let Some(url) = string_literal(b, i) else {
            return (urls, true);
        };
        urls.push(url);
        i = skip_trivia(b, skip_string(b, i));
        match b.get(i) {
            Some(b',') => i = skip_trivia(b, i + 1),
            Some(b')') => return (urls, false),
            _ => return (urls, true),
        }
    }
}

fn violation(reason: &'static str, b: &[u8], at: usize) -> HtmlViolation {
    let start = at.saturating_sub(20);
    let end = (at + 80).min(b.len());
    let excerpt = String::from_utf8_lossy(&b[start..end])
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
    fn policy_table() {
        let reject: &[(&str, &[u8])] = &[
            (
                "second_occurrence_after_img",
                br#"<img src="https://a/b.png"><script src="https://evil/x.js"></script>"#,
            ),
            (
                "img_in_comment_hides_script",
                br#"<!-- <img src="https://a/b.png"> --><script src="https://evil/x.js"></script>"#,
            ),
            (
                "abrupt_comment_close",
                b"<!--><script src=//evil/x.js></script>",
            ),
            (
                "quote_in_comment",
                b"<!-- \" --><script src=//evil/x.js></script>",
            ),
            (
                "unquoted_protocol_relative_upper",
                b"<SCRIPT SRC=//evil/x.js></SCRIPT>",
            ),
            (
                "spaced_equals",
                b"<script src = \"https://evil/x.js\"></script>",
            ),
            (
                "leading_space_in_value",
                b"<script src=\"  https://evil/x.js\"></script>",
            ),
            (
                "tab_inside_scheme",
                b"<iframe src=\"java\tscript:alert(1)\"></iframe>",
            ),
            (
                "backslash_protocol_relative",
                br#"<script src="/\evil/x.js"></script>"#,
            ),
            (
                "entity_encoded_scheme",
                br#"<script src="https&#58;//evil/x.js"></script>"#,
            ),
            ("slash_separated_attr", b"<script/src=//evil/x.js></script>"),
            (
                "data_script",
                br#"<script src="data:text/javascript,alert(1)"></script>"#,
            ),
            (
                "javascript_iframe",
                br#"<iframe src="javascript:alert(1)"></iframe>"#,
            ),
            (
                "blob_script",
                br#"<script src="blob:https://x/uuid"></script>"#,
            ),
            (
                "svg_script_href",
                br#"<svg><script href="https://evil/x.js"></script></svg>"#,
            ),
            (
                "base_href",
                br#"<base href="https://evil/"><script src="/app.js"></script>"#,
            ),
            ("embed", br#"<embed src="https://evil/x.swf">"#),
            ("object_data", br#"<object data="https://evil/x"></object>"#),
            ("frame", br#"<frame src="https://evil/">"#),
            (
                "meta_refresh",
                br#"<meta http-equiv="Refresh" content="0;url=https://evil/">"#,
            ),
            (
                "iframe_srcdoc",
                br#"<iframe srcdoc="&lt;script&gt;alert(1)&lt;/script&gt;"></iframe>"#,
            ),
            (
                "lt_inside_earlier_attr",
                br#"<script data-x="a<b" src="https://evil/x.js"></script>"#,
            ),
            (
                "multibyte_prefix_inline",
                "İİİİ<script>bad()</script>".as_bytes(),
            ),
            (
                "end_tag_suffix_upper",
                b"<script>   </scriptFOO>evil()</script>",
            ),
            (
                "end_tag_suffix_dash",
                b"<script> </script-x>evil()</script>",
            ),
            ("end_tag_suffix_s", b"<script> </scripts>evil()</script>"),
            // Over-reject: the browser treats this as textarea text, but
            // contexts are deliberately not tracked (see module docs).
            (
                "script_inside_textarea",
                b"<textarea><script>x()</script></textarea>",
            ),
            (
                "importmap_inline",
                br#"<script type="importmap">{"imports":{}}</script>"#,
            ),
            (
                "json_with_params",
                br#"<script type="application/json; charset=utf-8">{}</script>"#,
            ),
            ("module_inline", br#"<script type="module">run()</script>"#),
            (
                "json_type_duplicate_second",
                br#"<script type="module" type="application/json">run()</script>"#,
            ),
            (
                "json_block_remote_src",
                br#"<script type="application/json" src="https://evil/x.json"></script>"#,
            ),
            ("empty_type_inline", br#"<script type="">run()</script>"#),
            (
                "text_javascript_inline",
                br#"<script type="text/javascript">run()</script>"#,
            ),
            (
                "speculationrules_inline",
                br#"<script type="speculationrules">{}</script>"#,
            ),
            (
                "unknown_type_inline",
                br#"<script type="text/x-template">x</script>"#,
            ),
        ];
        for (name, html) in reject {
            assert!(check_html(html).is_err(), "{name} must be rejected");
        }

        let accept: &[(&str, &[u8])] = &[
            ("absolute_local", br#"<script src="/app.js"></script>"#),
            ("dot_relative", br#"<script src="./x.js"></script>"#),
            (
                "query_with_ampersand",
                br#"<script src="/a.js?x=1&amp;y=2"></script>"#,
            ),
            ("remote_img", br#"<img src="https://cdn/x.png">"#),
            ("remote_anchor", br#"<a href="https://site">x</a>"#),
            (
                "modulepreload",
                br#"<link rel="modulepreload" href="https://cdn/x.js">"#,
            ),
            ("empty_inline", b"<script></script>"),
            ("uppercase", br#"<SCRIPT SRC="/APP.JS"></SCRIPT>"#),
            (
                "multibyte_prefix",
                "İİİİ<script src=\"/a.js\"></script>".as_bytes(),
            ),
            (
                "gt_inside_quotes",
                br#"<div title="a > b"><script src="/a.js"></script></div>"#,
            ),
            ("local_iframe", br#"<iframe src="/embed.html"></iframe>"#),
            (
                "meta_charset",
                br#"<meta http-equiv="content-type" content="text/html">"#,
            ),
            ("text_lt", b"<p>a < b</p>"),
            ("end_tag_space", b"<script>  </script >"),
            ("end_tag_upper", b"<script> </SCRIPT>"),
            ("end_tag_slash", b"<script></script/>"),
            (
                "json_data_block",
                br#"<script type="application/json" id="__DATA__">{"a":"</b>"}</script>"#,
            ),
            (
                "ld_json_block",
                br#"<script type="application/ld+json">{"@context":"https://schema.org"}</script>"#,
            ),
            (
                "json_type_uppercase_and_spaces",
                b"<script type=\" Application/JSON \t\">{}</script>",
            ),
        ];
        for (name, html) in accept {
            assert!(
                check_html(html).is_ok(),
                "{name} must be accepted: {:?}",
                check_html(html)
            );
        }
    }

    #[test]
    fn worker_policy_table() {
        let reject: &[(&str, &[u8])] = &[
            ("worker_https", br#"new Worker("https://evil/x.js")"#),
            (
                "shared_worker_protocol_relative",
                b"new SharedWorker('//evil/x')",
            ),
            (
                "service_worker_register",
                br#"navigator.serviceWorker.register("https://evil/sw.js")"#,
            ),
            (
                "worker_new_url_remote",
                br#"new Worker(new URL("https://evil/x", import.meta.url))"#,
            ),
            (
                "comment_between_tokens",
                b"new /* w */ Worker /* ( */ ( // go\n  'https://evil/x.js')",
            ),
            (
                "newline_between_tokens",
                b"new\n  Worker\n  (\n  \"//evil/x\"\n)",
            ),
            ("extra_spacing", br#"new  Worker ( "https://evil/x.js" )"#),
            (
                "line_comment_before_ctor",
                b"new // c\nWorker(\"data:,alert(1)\")",
            ),
            (
                "after_an_accepted_call",
                br#"new Worker("/a.js"); new Worker("https://evil/x.js")"#,
            ),
            (
                "service_worker_new_url_remote",
                br#"navigator.serviceWorker.register(new URL('//evil/sw.js', import.meta.url))"#,
            ),
            // R6 bypasses, each rejected now.
            (
                "nbsp_between_new_and_worker",
                "new\u{a0}Worker(\"https://evil/x.js\")".as_bytes(),
            ),
            (
                "zwnbsp_between_new_and_worker",
                "new\u{feff}Worker('//evil/x.js')".as_bytes(),
            ),
            (
                "qualified_global_this",
                br#"new globalThis.Worker("https://evil/x.js")"#,
            ),
            (
                "qualified_window",
                br#"new window.Worker("https://evil/x.js")"#,
            ),
            (
                "parenthesised_callee",
                br#"new (Worker)("https://evil/x.js")"#,
            ),
            (
                "template_without_substitution",
                b"new Worker(`https://evil/x.js`)",
            ),
            (
                "import_scripts_first_arg",
                br#"importScripts("https://evil/a.js")"#,
            ),
            (
                "import_scripts_later_arg",
                br#"importScripts("/a.js", './b.js', "https://evil/c.js")"#,
            ),
            ("dynamic_import", br#"import("https://evil/x.js")"#),
            ("static_import", br#"import "https://evil/x.js""#),
            (
                "static_import_from",
                br#"import value from "https://evil/x.js""#,
            ),
            (
                "re_export_from",
                br#"export { value } from "https://evil/x.js""#,
            ),
            (
                "dynamic_import_new_url",
                br#"import(new URL("//evil/x.js", import.meta.url))"#,
            ),
            (
                "regex_no_longer_hides_the_call",
                br#"/["]/; new Worker("https://evil/x.js");"#,
            ),
        ];
        for (name, js) in reject {
            let err = check_worker_sources(js).unwrap_err();
            assert!(err.reason.contains("Worker"), "{name}: {err}");
        }

        // Accepted with nothing to audit.
        let accept: &[(&str, &[u8])] = &[
            ("local_absolute", br#"new Worker("/assets/w.js")"#),
            (
                "new_url_relative",
                br#"new Worker(new URL("./w.js", import.meta.url))"#,
            ),
            (
                "service_worker_local",
                br#"navigator.serviceWorker.register("/sw.js")"#,
            ),
            (
                "identifier_prefix",
                b"const f = MyWorkerFactory(\"https://cdn/x\");",
            ),
            (
                "worker_in_a_string",
                br#"const s = "Worker(\"https://cdn/x\")";"#,
            ),
            (
                "register_without_service_worker",
                br#"router.register("https://x/y")"#,
            ),
            ("worker_without_new", br#"Worker("https://evil/x.js")"#),
            ("no_worker_at_all", b"export const a = 1;"),
            (
                "local_import_scripts",
                br#"importScripts("/a.js", "./b.js")"#,
            ),
            ("local_dynamic_import", br#"import("./chunk.js")"#),
            ("local_static_import", br#"import value from "./chunk.js""#),
            (
                "tagged_template_after_from_identifier",
                b"const from = s => s[0]; export { from }; from`https://data`;",
            ),
            ("import_meta_is_not_a_call", b"const u = import.meta.url;"),
            (
                "accented_identifier_survives",
                "const wörker = 1;".as_bytes(),
            ),
            (
                "plain_new_without_arguments",
                b"const m = new Map; new Worker(\"/w.js\");",
            ),
            (
                "division_is_not_a_regex",
                br#"const r = a / b; new Worker("/w.js")"#,
            ),
        ];
        for (name, js) in accept {
            assert_eq!(check_worker_sources(js).unwrap(), 0, "{name} must be clean");
        }

        // Unjudgeable: reported for audit, never rejected.
        let warn: &[(&str, &[u8])] = &[
            ("variable", b"new Worker(workerUrl)"),
            ("concatenation", br#"new Worker(base + "/w.js")"#),
            ("template", b"new Worker(`${base}/w.js`)"),
            (
                "new_url_variable",
                b"new Worker(new URL(u, import.meta.url))",
            ),
            (
                "service_worker_variable",
                b"navigator.serviceWorker.register(swUrl)",
            ),
            ("template_with_substitution", b"new Worker(`${base}/w.js`)"),
            (
                "computed_constructor",
                br#"new globalThis["Worker"]("/w.js")"#,
            ),
            ("import_scripts_variable", b"importScripts(a, b)"),
            ("dynamic_import_variable", b"import(chunk)"),
            (
                "local_prefix_worker_concatenation",
                br#"new Worker("/" + "/evil/x.js")"#,
            ),
            (
                "local_prefix_dynamic_import_concatenation",
                br#"import("/" + "/evil/x.js")"#,
            ),
            (
                "new_url_literal_concatenation",
                br#"new Worker(new URL("/" + "/evil/x.js", import.meta.url))"#,
            ),
            (
                "new_url_expression_concatenation",
                br#"new Worker(new URL("/x.js", import.meta.url) + "/evil")"#,
            ),
            // The `/` has to be guessed at, so the file is flagged for audit
            // even though the call itself reads clean.
            (
                "regex_guess_flags_the_file",
                br#"/["]/; new Worker("/w.js");"#,
            ),
        ];
        for (name, js) in warn {
            assert_eq!(
                check_worker_sources(js).unwrap(),
                1,
                "{name} must warn once"
            );
        }
    }

    #[test]
    fn worker_scan_handles_truncated_input_without_panicking() {
        for js in [
            b"new Worker(".as_slice(),
            b"new Worker(\"https://evil/x".as_slice(),
            b"new Worker(new URL(".as_slice(),
            b"new Worker(/* unterminated".as_slice(),
            b"new Worker(`unterminated".as_slice(),
            b"new (((((Worker".as_slice(),
            b"importScripts(".as_slice(),
            b"import(".as_slice(),
            b"new \xc2".as_slice(),
            b"Worker".as_slice(),
            &[0xff, 0xfe, b'W', b'o', b'r', b'k', b'e', b'r'],
        ] {
            let _ = check_worker_sources(js);
        }
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
