//! The channel manifest (specification section 4) — a static JSON file on a CDN.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{FormatError, Result};
use crate::manifest::{PackId, PackKind, Sha256Hex};

/// The only channel spec tag this parser accepts.
pub const CHANNEL_SPEC_TAG: &str = "tpk-channel/1";

/// Upper bound on `notes` as stored, counted in visible characters after
/// markup is stripped. Longer values are truncated on parse.
///
/// `notes` is CDN-controlled text; anything that reaches a UI unbounded is an
/// unreviewed message channel into the app.
pub const MAX_NOTES_LEN: usize = 200;

/// One downloadable pack advertised by a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRef {
    /// Pack identity.
    pub id: PackId,
    /// What the pack contributes.
    pub kind: PackKind,
    /// Display version.
    pub version: semver::Version,
    /// Monotonic ordering key.
    pub version_code: u64,
    /// For patches: the `version_code` this one applies on top of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_version_code: Option<u64>,
    /// Absolute download URL.
    pub url: String,
    /// Exact size of the `.tpk` file.
    pub size: u64,
    /// SHA-256 of the `.tpk` file.
    pub sha256: Sha256Hex,
    /// Whether clients may skip this pack.
    #[serde(default)]
    pub optional: bool,
    /// Staged rollout percentage, 1..=100. Defaults to full rollout.
    #[serde(default = "default_rollout")]
    pub rollout: u8,
    /// Lowest shell version the pack supports, copied from its signed manifest.
    ///
    /// Lets a client skip a pack it could not stage without downloading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_shell: Option<semver::Version>,
    /// Highest shell version the pack supports, copied from its signed manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_shell: Option<semver::Version>,
}

const fn default_rollout() -> u8 {
    100
}

/// A parsed channel manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelManifest {
    /// Must be `tpk-channel/1`.
    pub spec: String,
    /// The channel this manifest describes.
    pub channel: String,
    /// RFC 3339 publication timestamp.
    pub published_at: String,
    /// Monotonic freshness marker, compared per channel.
    pub watermark: u64,
    /// Signing key generation. Clients keep a monotonic floor and reject older ones.
    #[serde(default = "default_key_epoch")]
    pub key_epoch: u32,
    /// Lowest shell version any pack here supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_shell: Option<semver::Version>,
    /// If set and above the running shell, no pack is applied at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_shell: Option<semver::Version>,
    /// Human-readable release note. Markup is stripped and the result
    /// truncated to [`MAX_NOTES_LEN`] on parse; a note left empty by that
    /// becomes `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The packs on offer.
    ///
    /// In an `app-store` build, entries whose `kind` is `dlc` or `mod` are
    /// dropped here rather than failing the document. A publisher serves one
    /// channel to every platform, so rejecting the manifest wholesale would let
    /// a single DLC entry block base and patch updates — including security
    /// fixes — for every App Store client.
    #[cfg_attr(app_store, serde(deserialize_with = "skip_absent_kinds"))]
    pub packs: Vec<PackRef>,
}

/// Drop `dlc` / `mod` entries, then parse the rest exactly as usual.
///
/// Deliberately narrow: only these two kinds are skipped. Any other unknown
/// `kind`, or a malformed entry, still fails the whole manifest, so this cannot
/// become a general "ignore what you do not understand" rule.
#[cfg(app_store)]
fn skip_absent_kinds<'de, D>(d: D) -> std::result::Result<Vec<PackRef>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    Vec::<serde_json::Value>::deserialize(d)?
        .into_iter()
        .filter(|v| {
            !matches!(
                v.get("kind").and_then(serde_json::Value::as_str),
                Some("dlc" | "mod")
            )
        })
        .map(|v| serde_json::from_value(v).map_err(D::Error::custom))
        .collect()
}

const fn default_key_epoch() -> u32 {
    1
}

impl ChannelManifest {
    /// Parse and validate a channel manifest.
    ///
    /// # Errors
    ///
    /// - [`FormatError::SpecTag`] when `spec` is not `tpk-channel/1`
    /// - [`FormatError::Spec`] for a malformed document, an out-of-range
    ///   `rollout`, or a duplicate `(id, version_code)`
    /// - [`FormatError::Parent`] when a patch has no `parent_version_code`, or
    ///   a non-patch carries one
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let mut manifest: Self =
            serde_json::from_slice(raw).map_err(|e| FormatError::Spec(e.to_string()))?;
        manifest.validate()?;
        manifest.notes = manifest.notes.take().and_then(|notes| {
            // Strip first: the length budget is meant to bound what a user
            // sees, and markup would otherwise spend it. A note that was
            // nothing but markup becomes `None`, not an empty note.
            let mut notes = strip_markup(&notes);
            truncate_chars(&mut notes, MAX_NOTES_LEN);
            (!notes.is_empty()).then_some(notes)
        });
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.spec != CHANNEL_SPEC_TAG {
            return Err(FormatError::SpecTag {
                found: self.spec.clone(),
                expected: CHANNEL_SPEC_TAG,
            });
        }
        if self.channel.is_empty() {
            return Err(FormatError::Spec("channel must not be empty".into()));
        }
        if self.key_epoch == 0 {
            return Err(FormatError::Spec("key_epoch must be non-zero".into()));
        }

        let mut seen: HashSet<(&str, u64)> = HashSet::new();
        for pack in &self.packs {
            if pack.version_code == 0 {
                return Err(FormatError::Spec("version_code must be non-zero".into()));
            }
            if !(1..=100).contains(&pack.rollout) {
                return Err(FormatError::Spec(format!(
                    "pack {} rollout {} is outside 1..=100",
                    pack.id, pack.rollout
                )));
            }
            if !seen.insert((pack.id.as_str(), pack.version_code)) {
                return Err(FormatError::Spec(format!(
                    "duplicate pack {} version_code {}",
                    pack.id, pack.version_code
                )));
            }
            match (pack.kind, pack.parent_version_code) {
                (PackKind::Patch, None) => {
                    return Err(FormatError::Parent(format!(
                        "patch {} has no parent_version_code",
                        pack.id
                    )))
                }
                (PackKind::Patch, Some(parent)) if parent >= pack.version_code => {
                    return Err(FormatError::Parent(format!(
                        "patch {} parent_version_code {parent} is not below {}",
                        pack.id, pack.version_code
                    )))
                }
                (kind, Some(_)) if kind != PackKind::Patch => {
                    return Err(FormatError::Parent(format!(
                        "{kind:?} pack {} must not carry parent_version_code",
                        pack.id
                    )))
                }
                _ => {}
            }
            if let (Some(min), Some(max)) = (&pack.min_shell, &pack.max_shell) {
                if min > max {
                    return Err(FormatError::Spec(format!(
                        "pack {} min_shell {min} is above max_shell {max}",
                        pack.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Whether this manifest may replace one already seen at `last_watermark`.
    ///
    /// Equal watermarks are accepted. Rejecting them would let a single
    /// same-minute double publish silently and permanently invalidate a
    /// manifest for every client that saw the other one — and the real
    /// downgrade defence is the per-pack `version_code` check, not this.
    pub fn is_fresh_enough(&self, last_watermark: u64) -> bool {
        self.watermark >= last_watermark
    }
}

/// Whether `c` is a formatting character that can hide or reorder text.
///
/// `char::is_control` only covers category Cc, so the Cf characters survive it —
/// including U+202E RIGHT-TO-LEFT OVERRIDE, which makes the text after it render
/// reversed and lets CDN-controlled text read as something other than what it
/// stores. The rule here is *drop what can hide or reorder text*, not *drop
/// everything invisible*: an invisible character that only shapes the glyphs
/// around it is content, and deleting it corrupts real notes.
///
/// Dropped, by what they do rather than by block:
/// - reorder — the bidi embeddings and overrides U+202A..=U+202E, the isolates
///   and the deprecated shaping/digit controls U+2066..=U+206F, the directional
///   marks U+200E/U+200F, and U+061C
/// - hide — U+200B, which splits a word the reader sees as one, the BOM U+FEFF,
///   the soft hyphen U+00AD, U+180E, the interlinear annotations U+FFF9..=U+FFFB
///   (they wrap text the renderer may not show), the musical formatting controls
///   U+1D173..=U+1D17A, and the tag block U+E0000..=U+E007F, the usual carrier
///   for text hidden inside a visible string
///
/// Deliberately kept:
/// - U+200C ZWNJ and U+200D ZWJ shape real content — `می\u{200C}رود` needs the
///   first, emoji sequences and Indic conjuncts need the second. Neither can
///   reorder or conceal anything, so dropping them would corrupt notes to
///   defend against nothing.
/// - U+2060..=U+2064, the word joiner and the invisible math operators: they
///   affect line breaking and mathematical semantics, never order or visibility.
/// - The script-specific marks U+0600..=U+0605, U+06DD, U+070F, U+08E2,
///   U+110BD, U+110CD and U+1BCA0..=U+1BCA3, which prefix real content in
///   Arabic, Kaithi and shorthand.
/// - RTL *letters*, for the same reason: they are content, not formatting.
///
/// U+200E/U+200F are the close call. Mixed-direction text does use them
/// legitimately, but they carry no glyph and only nudge the order neutrals
/// render in — exactly the deception being defended against — so for text this
/// side never authored they go, and the host's own bidi algorithm orders what is
/// left.
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{061C}'
        | '\u{180E}'
        | '\u{200B}'
        | '\u{200E}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0000}'..='\u{E007F}')
}

/// Remove tag-like spans, invisible formatting and control characters from
/// CDN-controlled text.
///
/// `notes` is documented as diagnostic-only, but it still reaches the host's UI
/// through `check`, so markup must not survive the parser. Deliberately
/// conservative and lossy rather than a sanitiser: everything from a `<` to the
/// next `>` is dropped, an unterminated `<` drops the rest of the note, and a
/// stray `>` is dropped too, so no angle bracket can reach a DOM. Character
/// references are left encoded on purpose — decoding them could reintroduce the
/// markup just removed. Formatting characters that can hide or reorder text
/// ([`is_invisible_format`]) are dropped outright rather than replaced, so they
/// cannot split a word the reader sees as one. Runs of control characters (NUL
/// and newlines included) collapse to one space so a note cannot forge log
/// lines, and the result is trimmed.
fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ if is_invisible_format(c) => {}
            _ if c.is_control() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

fn truncate_chars(s: &mut String, max: usize) {
    if s.chars().count() > max {
        let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
        s.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "aa00000000000000000000000000000000000000000000000000000000000001";

    fn pack(kind: &str, code: u64, parent: Option<u64>) -> serde_json::Value {
        let mut p = serde_json::json!({
            "id": "core",
            "kind": kind,
            "version": "1.0.0",
            "version_code": code,
            "url": "https://cdn.example.com/tpk/core/x.tpk",
            "size": 1234,
            "sha256": HASH,
        });
        if let Some(parent) = parent {
            p["parent_version_code"] = serde_json::json!(parent);
        }
        p
    }

    fn manifest(packs: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "spec": "tpk-channel/1",
            "channel": "stable",
            "published_at": "2026-09-11T15:00:00Z",
            "watermark": 202609111500u64,
            "packs": packs,
        })
    }

    fn parse(v: &serde_json::Value) -> Result<ChannelManifest> {
        ChannelManifest::parse(serde_json::to_string(v).unwrap().as_bytes())
    }

    #[test]
    fn parses_a_minimal_channel() {
        let m = parse(&manifest(vec![pack("base", 10000, None)])).unwrap();
        assert_eq!(m.channel, "stable");
        assert_eq!(m.key_epoch, 1, "defaults to the first generation");
        assert_eq!(m.packs[0].rollout, 100, "defaults to full rollout");
        assert!(!m.packs[0].optional);
        assert!(m.force_shell.is_none());
    }

    #[test]
    fn rejects_unknown_spec() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["spec"] = serde_json::json!("tpk-channel/2");
        assert!(matches!(
            parse(&v).unwrap_err(),
            FormatError::SpecTag { .. }
        ));
    }

    #[test]
    fn rejects_zero_key_epoch() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["key_epoch"] = serde_json::json!(0);
        assert!(parse(&v).is_err());
    }

    #[test]
    fn patch_requires_a_lower_parent_version_code() {
        assert!(parse(&manifest(vec![pack("patch", 10003, None)])).is_err());
        assert!(parse(&manifest(vec![pack("patch", 10003, Some(10003))])).is_err());
        assert!(parse(&manifest(vec![pack("patch", 10003, Some(20000))])).is_err());
        assert!(parse(&manifest(vec![pack("patch", 10003, Some(10000))])).is_ok());
    }

    #[test]
    fn non_patch_must_not_carry_parent_version_code() {
        assert!(parse(&manifest(vec![pack("base", 10000, Some(9000))])).is_err());
        #[cfg(not(app_store))]
        assert!(parse(&manifest(vec![pack("dlc", 20000, Some(9000))])).is_err());
    }

    #[test]
    #[cfg(app_store)]
    fn a_dlc_entry_is_dropped_instead_of_failing_the_channel() {
        let v = manifest(vec![
            pack("base", 10000, None),
            pack("dlc", 20000, None),
            pack("mod", 30000, None),
        ]);
        let m = parse(&v).unwrap();
        assert_eq!(m.packs.len(), 1, "only the base survives");
        assert_eq!(m.packs[0].version_code, 10000);
    }

    #[test]
    #[cfg(app_store)]
    fn an_unknown_kind_still_fails_the_channel() {
        // Skipping is only for kinds this build deliberately removed, not a
        // general tolerance for entries the client does not understand.
        assert!(parse(&manifest(vec![pack("plugin", 10000, None)])).is_err());
    }

    #[test]
    fn rejects_duplicate_id_and_version_code() {
        let v = manifest(vec![pack("base", 10000, None), pack("base", 10000, None)]);
        assert!(parse(&v).is_err());
    }

    #[test]
    fn base_and_patch_for_one_id_coexist() {
        let v = manifest(vec![
            pack("base", 10000, None),
            pack("patch", 10003, Some(10000)),
        ]);
        assert_eq!(parse(&v).unwrap().packs.len(), 2);
    }

    #[test]
    fn rollout_must_be_in_range() {
        for bad in [0u8, 101, 255] {
            let mut v = manifest(vec![pack("base", 10000, None)]);
            v["packs"][0]["rollout"] = serde_json::json!(bad);
            assert!(parse(&v).is_err(), "should reject rollout {bad}");
        }
        for ok in [1u8, 50, 100] {
            let mut v = manifest(vec![pack("base", 10000, None)]);
            v["packs"][0]["rollout"] = serde_json::json!(ok);
            assert_eq!(parse(&v).unwrap().packs[0].rollout, ok);
        }
    }

    #[test]
    fn notes_are_truncated_not_rejected() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("x".repeat(500));
        let m = parse(&v).unwrap();
        assert_eq!(m.notes.unwrap().chars().count(), MAX_NOTES_LEN);
    }

    #[test]
    fn notes_have_markup_stripped_before_truncation() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("<script>alert(1)</script> <b>ok</b> 3 < 4");
        let notes = parse(&v).unwrap().notes.unwrap();
        assert!(
            !notes.contains('<') && !notes.contains('>'),
            "angle brackets survived: {notes}"
        );
        // The `<` with no `>` takes the rest of the note with it.
        assert_eq!(notes, "alert(1) ok 3");

        // Control characters collapse, so a note cannot forge a log line.
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("<b>a</b>\n\n\u{0}b");
        assert_eq!(parse(&v).unwrap().notes.unwrap(), "a b");

        // The 200-character budget counts visible text, not markup.
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!(format!("<b>{}</b>", "x".repeat(300)));
        assert_eq!(parse(&v).unwrap().notes.unwrap(), "x".repeat(MAX_NOTES_LEN));

        // Nothing but markup is no note at all, not an empty one.
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("<div><br/></div> <b></b>");
        assert_eq!(parse(&v).unwrap().notes, None);
    }

    #[test]
    fn notes_drop_bidi_overrides() {
        // The override goes, the words around it stay — and it is dropped, not
        // replaced, so "in\u{202E}voice" does not become two words.
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("in\u{202E}voice \u{200B}ready\u{FEFF}");
        assert_eq!(parse(&v).unwrap().notes.unwrap(), "invoice ready");

        // Nothing but invisibles is no note at all, not an empty one.
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("\u{202E}\u{202D}\u{061C}\u{2066}\u{E0041}");
        assert_eq!(parse(&v).unwrap().notes, None);

        // RTL letters are content, not formatting: they survive unchanged.
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("تحديث \u{202E}עברית");
        assert_eq!(parse(&v).unwrap().notes.unwrap(), "تحديث עברית");
    }

    #[test]
    fn notes_keep_shaping_joiners() {
        // ZWNJ and ZWJ shape real content — Persian needs the first, emoji
        // sequences the second — and neither can hide or reorder anything.
        let persian = "می\u{200C}رود";
        let family = "👨\u{200D}👩\u{200D}👧";
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!(format!("{persian} {family}"));
        assert_eq!(
            parse(&v).unwrap().notes.unwrap(),
            format!("{persian} {family}")
        );
    }

    #[test]
    fn notes_truncation_respects_char_boundaries() {
        let mut v = manifest(vec![pack("base", 10000, None)]);
        v["notes"] = serde_json::json!("修复登录页样式".repeat(100));
        let m = parse(&v).unwrap();
        assert_eq!(m.notes.unwrap().chars().count(), MAX_NOTES_LEN);
    }

    #[test]
    fn equal_watermark_is_accepted() {
        let m = parse(&manifest(vec![pack("base", 10000, None)])).unwrap();
        assert!(m.is_fresh_enough(202609111500), "equal must be accepted");
        assert!(m.is_fresh_enough(202609111400), "newer must be accepted");
        assert!(
            !m.is_fresh_enough(202609111600),
            "strictly older must be rejected"
        );
    }

    #[test]
    fn a_channel_entry_with_min_shell_above_max_shell_is_rejected() {
        let mut p = pack("base", 10000, None);
        p["min_shell"] = serde_json::json!("3.0.0");
        p["max_shell"] = serde_json::json!("2.0.0");
        assert!(matches!(
            parse(&manifest(vec![p.clone()])).unwrap_err(),
            FormatError::Spec(_)
        ));

        // An equal pair is a single supported shell, not an empty range.
        p["min_shell"] = serde_json::json!("2.0.0");
        assert!(parse(&manifest(vec![p])).is_ok());
    }

    #[test]
    fn empty_pack_list_is_valid() {
        // A channel with nothing on offer is how you stop a rollout.
        assert!(parse(&manifest(vec![])).unwrap().packs.is_empty());
    }
}
