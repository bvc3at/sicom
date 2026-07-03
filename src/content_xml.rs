//! XML-aware handling of SIGame `content.xml` media references.
//!
//! SIGame packs reference media in three ways (per the reference implementation
//! in VladimirKhil/SI, `SIPackages`):
//!
//! - format v4 and older: `<atom type="image">@filename.png</atom>` — the `@`
//!   prefix marks an internal reference; without it the text is an external URL
//!   (`Atom.IsLink`, Atom.cs);
//! - format v5: `<item type="image" isRef="True">filename.png</item>` — `isRef`
//!   is parsed case-insensitively (ContentItem.cs uses `bool.TryParse`);
//! - the package logo: `<package logo="@filename.png">` (`@`-prefixed in every
//!   known pack, both v4 and v5).
//!
//! At load time the game maps `Uri.UnescapeDataString(zip entry name)` to the
//! entry (ZipSIPackageContainer.cs) and resolves a reference by looking up the
//! raw link, then `Uri.EscapeUriString(link)` (SIDocument.cs). The invariant
//! this module preserves: the URL-decoded entry name must keep matching the
//! reference text. Because compression only ever swaps the file extension, a
//! rewrite swaps the extension inside the raw reference bytes in place — it
//! never re-encodes, re-escapes, or otherwise touches surrounding XML.

use anyhow::{Context, Result, anyhow, bail};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use std::ops::Range;

const BOM: &str = "\u{feff}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Images,
    Audio,
    Video,
}

impl Category {
    pub fn dir_prefix(self) -> &'static str {
        match self {
            Category::Images => "Images/",
            Category::Audio => "Audio/",
            Category::Video => "Video/",
        }
    }

    /// Category referenced by a v4 `<atom type="...">` / v5 `<item type="...">`.
    fn from_media_type(media_type: &str) -> Option<Self> {
        match media_type {
            "image" => Some(Category::Images),
            "voice" | "audio" => Some(Category::Audio),
            "video" => Some(Category::Video),
            _ => None,
        }
    }
}

/// One internal media reference found in content.xml.
#[derive(Debug)]
pub struct MediaRef {
    /// Byte range of the raw (still XML-escaped) link text inside the
    /// BOM-stripped document, excluding any `@` prefix.
    span: Range<usize>,
    /// The link as SIGame sees it: XML-unescaped, `@` prefix stripped.
    pub link: String,
    pub category: Category,
}

fn split_bom(xml: &str) -> (&str, &str) {
    xml.strip_prefix(BOM).map_or(("", xml), |rest| (BOM, rest))
}

fn attr_value(e: &BytesStart, name: &[u8]) -> Result<Option<String>> {
    for attr in e.attributes().with_checks(false) {
        let attr = attr.context("malformed attribute")?;
        if attr.key.as_ref() == name {
            return Ok(Some(
                attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .context("malformed attribute value")?
                    .into_owned(),
            ));
        }
    }
    Ok(None)
}

/// Element context whose text content is a media reference.
#[derive(Clone, Copy)]
struct RefContext {
    category: Category,
    /// v4 atoms are internal references only when the text starts with '@'.
    needs_at: bool,
}

/// Turns an accumulated text run into a `MediaRef`, if it is an internal link.
fn finalize_run(
    doc: &str,
    context: &mut Option<RefContext>,
    run: &mut Option<Range<usize>>,
    refs: &mut Vec<MediaRef>,
) -> Result<()> {
    let (Some(ctx), Some(span)) = (context.take(), run.take()) else {
        *context = None;
        *run = None;
        return Ok(());
    };
    let raw = &doc[span.clone()];
    let link = quick_xml::escape::unescape(raw)
        .with_context(|| format!("unsupported entity in media reference: {raw}"))?;
    if ctx.needs_at {
        // The C# writer emits the '@' literally, so require it in the raw
        // bytes as well as the unescaped text; anything else is an external
        // URL and must not be touched.
        if let (Some(link), Some(_)) = (link.strip_prefix('@'), raw.strip_prefix('@')) {
            if !link.is_empty() {
                refs.push(MediaRef {
                    span: span.start + 1..span.end,
                    link: link.to_string(),
                    category: ctx.category,
                });
            }
        }
    } else if !link.is_empty() {
        refs.push(MediaRef {
            span,
            link: link.into_owned(),
            category: ctx.category,
        });
    }
    Ok(())
}

/// Extracts every internal media reference from `content.xml`.
///
/// Fails when the document is not well-formed XML or uses constructs whose
/// references cannot be rewritten byte-exactly (CDATA, comments or custom
/// entities inside a reference element). Callers must treat an error as "do
/// not modify this pack at all".
pub fn find_media_refs(xml: &str) -> Result<Vec<MediaRef>> {
    let (_bom, doc) = split_bom(xml);
    let mut reader = Reader::from_str(doc);

    let mut refs = Vec::new();
    let mut context: Option<RefContext> = None;
    // Raw byte range of the accumulated text run inside `context`.
    let mut run: Option<Range<usize>> = None;
    let mut prev_pos = 0usize;

    loop {
        let event = reader
            .read_event()
            .context("content.xml is not well-formed XML")?;
        let cur_pos = usize::try_from(reader.buffer_position())
            .map_err(|_| anyhow!("content.xml too large"))?;

        match &event {
            Event::Start(e) => {
                finalize_run(doc, &mut context, &mut run, &mut refs)?;
                match e.local_name().as_ref() {
                    b"atom" => {
                        context = attr_value(e, b"type")?
                            .as_deref()
                            .and_then(Category::from_media_type)
                            .map(|category| RefContext { category, needs_at: true });
                    }
                    b"item" => {
                        let is_ref = attr_value(e, b"isRef")?
                            .is_some_and(|v| v.eq_ignore_ascii_case("true"));
                        if is_ref {
                            context = attr_value(e, b"type")?
                                .as_deref()
                                .and_then(Category::from_media_type)
                                .map(|category| RefContext { category, needs_at: false });
                        }
                    }
                    b"package" => {
                        if let Some(logo) = attr_value(e, b"logo")? {
                            if let Some(link) = logo.strip_prefix('@') {
                                if !link.is_empty() {
                                    let tag = &doc[prev_pos..cur_pos];
                                    let value_span = find_logo_value_span(tag).ok_or_else(
                                        || anyhow!("cannot locate logo attribute value"),
                                    )?;
                                    refs.push(MediaRef {
                                        // +1 skips the '@'
                                        span: prev_pos + value_span.start + 1
                                            ..prev_pos + value_span.end,
                                        link: link.to_string(),
                                        category: Category::Images,
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(_) | Event::GeneralRef(_) => {
                if context.is_some() {
                    run = Some(match run.take() {
                        Some(r) => r.start..cur_pos,
                        None => prev_pos..cur_pos,
                    });
                }
            }
            Event::CData(_) => {
                if context.is_some() {
                    bail!("CDATA inside a media reference element is not supported");
                }
            }
            Event::Comment(_) | Event::PI(_) => {
                if run.is_some() {
                    bail!("comment inside a media reference element is not supported");
                }
                // A comment between the start tag and the text is fine: the
                // run has not begun, keep the context.
            }
            Event::End(_) | Event::Empty(_) => finalize_run(doc, &mut context, &mut run, &mut refs)?,
            Event::Eof => {
                finalize_run(doc, &mut context, &mut run, &mut refs)?;
                break;
            }
            _ => finalize_run(doc, &mut context, &mut run, &mut refs)?,
        }
        prev_pos = cur_pos;
    }

    Ok(refs)
}

/// Finds the byte range of the raw `logo` attribute value (between the quotes)
/// inside the raw bytes of a `<package ...>` start tag.
fn find_logo_value_span(tag: &str) -> Option<Range<usize>> {
    let bytes = tag.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            // Skip a quoted attribute value so a value containing `logo=`
            // cannot fool us.
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                i += 1;
            }
            b'l' if tag[i..].starts_with("logo")
                && i > 0
                && bytes[i - 1].is_ascii_whitespace() =>
            {
                let mut j = i + 4;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'=' {
                    j += 1;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        let quote = bytes[j] as char;
                        let start = j + 1;
                        let end = tag[start..].find(quote)? + start;
                        return Some(start..end);
                    }
                }
                i += 4;
            }
            _ => i += 1,
        }
    }
    None
}

/// Mirrors .NET `Uri.UnescapeDataString`: decodes `%XX` sequences (invalid
/// sequences are kept literally), interpreting the result as UTF-8.
pub fn unescape_data_string(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Mirrors .NET `Uri.EscapeUriString` closely enough for name matching:
/// percent-encodes the UTF-8 bytes of every character outside the RFC 3986
/// unreserved and reserved sets.
pub fn escape_uri_string(s: &str) -> String {
    const KEEP: &[u8] = b"-._~:/?#[]@!$&'()*+,;=";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || KEEP.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// True when reference text `link` resolves to a zip entry whose URL-decoded
/// file name is `decoded_entry`, per the SIGame lookup (raw first, then
/// escaped).
pub fn link_matches(link: &str, decoded_entry: &str) -> bool {
    link == decoded_entry || escape_uri_string(link) == decoded_entry
}

/// Rewrites media references for converted files.
///
/// `conversions` maps, per category, the URL-decoded original file name
/// (without the directory prefix) to the URL-decoded new file name; the two
/// must differ only by extension. Returns the rewritten document and the
/// number of references updated.
pub fn rewrite_media_refs(
    xml: &str,
    conversions: &HashMap<Category, HashMap<String, String>>,
) -> Result<(String, usize)> {
    let (bom, doc) = split_bom(xml);
    let refs = find_media_refs(xml)?;

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for media_ref in &refs {
        let Some(category_conversions) = conversions.get(&media_ref.category) else {
            continue;
        };
        let Some(new_name) = category_conversions
            .iter()
            .find(|(old, _)| link_matches(&media_ref.link, old))
            .map(|(_, new)| new)
        else {
            continue;
        };
        let new_ext = std::path::Path::new(new_name)
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| anyhow!("converted name has no extension: {new_name}"))?;
        // Swap the extension inside the raw bytes so everything before the
        // last dot — escaping, percent-encoding, the '@' prefix — is
        // preserved verbatim. Extensions are plain ASCII in every form.
        let raw = &doc[media_ref.span.clone()];
        let new_raw = match raw.rfind('.') {
            Some(dot) => format!("{}.{}", &raw[..dot], new_ext),
            None => quick_xml::escape::escape(new_name.as_str()).into_owned(),
        };
        edits.push((media_ref.span.clone(), new_raw));
    }

    // Apply back-to-front so earlier spans stay valid.
    edits.sort_by_key(|(span, _)| span.start);
    let mut out = doc.to_string();
    let count = edits.len();
    for (span, new_raw) in edits.into_iter().rev() {
        out.replace_range(span, &new_raw);
    }

    Ok((format!("{bom}{out}"), count))
}

/// Simulates SIGame's reference resolution: returns every internal reference
/// in `xml` that does NOT resolve against `entry_names` (full zip entry
/// names). Used as a final guard before writing a pack.
pub fn unresolved_refs(xml: &str, entry_names: &[String]) -> Result<Vec<String>> {
    let refs = find_media_refs(xml)?;

    let mut decoded_by_category: HashMap<Category, Vec<String>> = HashMap::new();
    for name in entry_names {
        for category in [Category::Images, Category::Audio, Category::Video] {
            if let Some(file_name) = name.strip_prefix(category.dir_prefix()) {
                if !file_name.is_empty() {
                    decoded_by_category
                        .entry(category)
                        .or_default()
                        .push(unescape_data_string(file_name));
                }
            }
        }
    }

    let empty = Vec::new();
    Ok(refs
        .iter()
        .filter(|r| {
            let entries = decoded_by_category.get(&r.category).unwrap_or(&empty);
            !entries.iter().any(|e| link_matches(&r.link, e))
        })
        .map(|r| format!("{}{}", r.category.dir_prefix(), r.link))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const V5_XML: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?><package name=\"t\" version=\"5\" logo=\"@logo pic.png\"><rounds><round name=\"r\"><themes><theme name=\"t\"><questions><question price=\"100\"><scenario>\
<item type=\"image\" isRef=\"True\">1.png</item>\
<item type=\"image\" isRef=\"True\">11.png</item>\
<item>Look at 1.png carefully</item>\
<item type=\"image\" isRef=\"True\">a&amp;b.png</item>\
<item type=\"audio\" isRef=\"True\" placement=\"background\">song.mp3</item>\
<item type=\"image\">https://example.com/ext.png</item>\
</scenario></question></questions></theme></themes></round></rounds></package>";

    const V4_XML: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?><package name=\"t\" version=\"4\" logo=\"@logo.jpg\"><scenario>\
<atom type=\"image\">@pic one.jpg</atom>\
<atom type=\"image\">https://vladimirkhil.com/images/svoyak-soft.jpg</atom>\
<atom type=\"voice\">@song.mp3</atom>\
<atom>plain text with pic one.jpg inside</atom>\
</scenario></package>";

    #[test]
    fn finds_v5_refs() {
        let refs = find_media_refs(V5_XML).unwrap();
        let images: Vec<_> = refs
            .iter()
            .filter(|r| r.category == Category::Images)
            .map(|r| r.link.as_str())
            .collect();
        assert_eq!(images, vec!["logo pic.png", "1.png", "11.png", "a&b.png"]);
        let audio: Vec<_> = refs
            .iter()
            .filter(|r| r.category == Category::Audio)
            .map(|r| r.link.as_str())
            .collect();
        assert_eq!(audio, vec!["song.mp3"]);
    }

    #[test]
    fn finds_v4_refs_and_ignores_urls() {
        let refs = find_media_refs(V4_XML).unwrap();
        let links: Vec<_> = refs.iter().map(|r| r.link.as_str()).collect();
        assert_eq!(links, vec!["logo.jpg", "pic one.jpg", "song.mp3"]);
    }

    fn image_conversions(pairs: &[(&str, &str)]) -> HashMap<Category, HashMap<String, String>> {
        let mut inner = HashMap::new();
        for (old, new) in pairs {
            inner.insert((*old).to_string(), (*new).to_string());
        }
        HashMap::from([(Category::Images, inner)])
    }

    #[test]
    fn rewrite_does_not_touch_text_or_substrings() {
        let conversions = image_conversions(&[("1.png", "1.webp")]);
        let (out, count) = rewrite_media_refs(V5_XML, &conversions).unwrap();
        assert_eq!(count, 1);
        // The ref got rewritten…
        assert!(out.contains(">1.webp</item>"));
        // …but neither the question text nor the longer file name did.
        assert!(out.contains("Look at 1.png carefully"));
        assert!(out.contains(">11.png</item>"));
        assert!(out.starts_with('\u{feff}'));
    }

    #[test]
    fn rewrite_preserves_xml_escapes() {
        let conversions = image_conversions(&[("a&b.png", "a&b.webp")]);
        let (out, count) = rewrite_media_refs(V5_XML, &conversions).unwrap();
        assert_eq!(count, 1);
        assert!(out.contains(">a&amp;b.webp</item>"));
    }

    #[test]
    fn rewrite_updates_logo_and_atoms_preserving_at() {
        let conversions = image_conversions(&[("logo.jpg", "logo.webp"), ("pic one.jpg", "pic one.webp")]);
        let (out, count) = rewrite_media_refs(V4_XML, &conversions).unwrap();
        assert_eq!(count, 2);
        assert!(out.contains("logo=\"@logo.webp\""));
        assert!(out.contains(">@pic one.webp</atom>"));
        // External URL ending in .jpg is untouched.
        assert!(out.contains("https://vladimirkhil.com/images/svoyak-soft.jpg"));
        // Unrelated text mentioning the file name is untouched.
        assert!(out.contains("plain text with pic one.jpg inside"));
    }

    #[test]
    fn matches_percent_encoded_entries() {
        // Real-world convention: zip entry is Uri-escaped, XML holds the
        // literal name (e.g. test.siq in the official repo).
        assert!(link_matches(
            "videoplayback (8)_2 (1).mp4",
            unescape_data_string("videoplayback%20(8)_2%20(1).mp4").as_str()
        ));
        assert!(link_matches("my image.png", "my image.png"));
        assert!(!link_matches("my image.png", "other.png"));
    }

    #[test]
    fn validation_finds_dangling_refs() {
        let entries = vec![
            "Images/1.png".to_string(),
            "Images/11.png".to_string(),
            "Images/logo%20pic.png".to_string(),
            "Images/a&b.png".to_string(),
            "Audio/song.mp3".to_string(),
        ];
        assert!(unresolved_refs(V5_XML, &entries).unwrap().is_empty());

        // Rename 1.png -> 1.webp in the zip without updating the XML: the
        // validation must flag it.
        let entries = vec![
            "Images/1.webp".to_string(),
            "Images/11.png".to_string(),
            "Images/logo%20pic.png".to_string(),
            "Images/a&b.png".to_string(),
            "Audio/song.mp3".to_string(),
        ];
        let missing = unresolved_refs(V5_XML, &entries).unwrap();
        assert_eq!(missing, vec!["Images/1.png".to_string()]);
    }

    #[test]
    fn cdata_in_ref_element_is_rejected() {
        let xml = "<package><scenario><atom type=\"image\"><![CDATA[@x.png]]></atom></scenario></package>";
        assert!(find_media_refs(xml).is_err());
    }
}
