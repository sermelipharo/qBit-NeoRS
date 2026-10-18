//! Feed parser: a port of qBittorrent's `RSS::Private::Parser`
//! (`rss_parser.cpp`) on top of `quick-xml`, plus RSS 1.0 (RDF) support.
//!
//! Field extraction follows the original exactly (element names compared by
//! local name, GUID/ID fallback chain, enclosure/magnet/link rules, the
//! "unchanged lastBuildDate" short-circuit, articles before an XML error are
//! kept). Feed content is hostile input: DTD entities are never expanded
//! (only the XML and XHTML named entities and character references are),
//! the document size, nesting depth, event count, article count and every
//! string length are capped, and nothing here can panic on malformed input.

use std::borrow::Cow;
use std::collections::HashSet;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::date::{Rfc822, parse_iso8601, parse_rfc822};
use super::entities;

/// Largest feed document accepted (also the HTTP download cap).
pub(crate) const MAX_FEED_SIZE: usize = 32 * 1024 * 1024;
/// Articles kept from a single document (in document order).
pub(crate) const MAX_PARSED_ARTICLES: usize = 10_000;
const MAX_DEPTH: usize = 512;
const MAX_EVENTS: usize = 8_000_000;
const MAX_ATTRIBUTES: usize = 64;
/// Text collected for one element (description, extra fields).
const MAX_TEXT: usize = 1024 * 1024;
const MAX_SHORT_TEXT: usize = 64 * 1024;
/// Atom `xml:base` longer than this is ignored.
const MAX_BASE_URL: usize = 2048;
const MAX_FEED_TITLE: usize = 1024;
const MAX_EXTRA_FIELDS: usize = 32;
const MAX_EXTRA_KEY: usize = 128;

/// One parsed article (qBittorrent's article `QVariantHash`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ParsedArticle {
    pub id: String,
    /// Unix ms; `None` = invalid/missing (the feed assigns a dummy date).
    pub date: Option<i64>,
    pub title: String,
    pub author: String,
    pub description: String,
    pub torrent_url: String,
    pub link: String,
    /// Other child elements by local name (`category`, `size`, ...).
    pub extra: Vec<(String, String)>,
}

impl ParsedArticle {
    fn set_extra(&mut self, key: &str, value: String) {
        if key.is_empty() || key.len() > MAX_EXTRA_KEY {
            return;
        }
        let value = truncate(value, MAX_SHORT_TEXT);
        if let Some(slot) = self.extra.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else if self.extra.len() < MAX_EXTRA_FIELDS {
            self.extra.push((key.to_owned(), value));
        }
    }

    fn extra(&self, key: &str) -> Option<&str> {
        self.extra.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Stores a child element that has no dedicated handling. Names that are
    /// article keys overwrite those keys, like in the original hash.
    fn set_other(&mut self, key: &str, value: String) {
        match key {
            "id" => self.id = truncate(value, MAX_SHORT_TEXT),
            "title" => self.title = truncate(value, MAX_SHORT_TEXT),
            "author" => self.author = truncate(value, MAX_SHORT_TEXT),
            "description" => self.description = truncate(value, MAX_TEXT),
            "torrentURL" => self.torrent_url = url_value(value),
            "link" => self.link = url_value(value),
            // `dc:date` and friends: QVariant(QString) converts with Qt::ISODate.
            "date" => self.date = parse_iso8601(value.trim()),
            // A feed must not be able to pre-mark its own items as read.
            "isRead" => {}
            _ => self.set_extra(key, value),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ParseResult {
    pub title: String,
    pub last_build_date: String,
    /// Newest first in qBittorrent's sense: reverse document order.
    pub articles: Vec<ParsedArticle>,
    /// Empty when the document parsed cleanly.
    pub error: String,
}

/// Parses a downloaded feed. `last_build_date` is the value remembered from
/// the previous parse of the same feed; `now_ms` is the fallback date.
pub(crate) fn parse(data: &[u8], last_build_date: &str, now_ms: i64) -> ParseResult {
    let mut result = ParseResult { last_build_date: last_build_date.to_owned(), ..Default::default() };
    if data.len() > MAX_FEED_SIZE {
        result.error = format!("Feed is too large ({} bytes).", data.len());
        return result;
    }
    let text = decode_document(data);
    let mut p = Parser {
        cur: Cursor::new(&text),
        result,
        ids: HashSet::new(),
        fallback: now_ms,
        base_url: String::new(),
        rdf: false,
    };
    p.run();
    let Parser { cur, mut result, .. } = p;
    if let Some(err) = cur.error {
        result.error = err;
    }
    result.articles.reverse();
    result
}

struct Parser<'a> {
    cur: Cursor<'a>,
    result: ParseResult,
    ids: HashSet<String>,
    fallback: i64,
    base_url: String,
    rdf: bool,
}

impl Parser<'_> {
    fn run(&mut self) {
        let mut found_channel = false;
        while let Some(root) = self.cur.read_next_start_element() {
            match root.local.as_str() {
                "rss" => {
                    while let Some(child) = self.cur.read_next_start_element() {
                        if child.local == "channel" {
                            self.parse_rss_channel();
                            found_channel = true;
                            break;
                        }
                        self.cur.skip_current_element();
                    }
                    break;
                }
                "feed" => {
                    // The base is prepended to every entry link: an oversized
                    // one would be copied into thousands of articles.
                    let base = root.attr("xml:base").unwrap_or_default();
                    self.base_url = if base.len() <= MAX_BASE_URL { base.to_owned() } else { String::new() };
                    self.parse_atom_channel();
                    found_channel = true;
                    break;
                }
                // RSS 1.0: items are siblings of <channel> inside <rdf:RDF>.
                "RDF" => {
                    self.rdf = true;
                    self.parse_rss_channel();
                    found_channel = true;
                    break;
                }
                _ => self.cur.skip_current_element(),
            }
        }
        if self.cur.error.is_none() && !found_channel {
            self.cur.error = Some("Invalid RSS feed.".to_owned());
        }
    }

    fn parse_rss_channel(&mut self) {
        loop {
            match self.cur.next() {
                Tok::Eof => return,
                Tok::Start(el) => match el.local.as_str() {
                    "title" => {
                        self.result.title = truncate(self.cur.read_element_text(false), MAX_FEED_TITLE);
                    }
                    "lastBuildDate" => {
                        let value = truncate(self.cur.read_element_text(false), MAX_FEED_TITLE);
                        if !value.is_empty() {
                            if self.result.last_build_date == value {
                                // The feed has not changed since last time.
                                return;
                            }
                            self.result.last_build_date = value;
                        }
                    }
                    "item" => self.parse_rss_article(),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn parse_rss_article(&mut self) {
        let mut article = ParsedArticle::default();
        let mut alt_torrent_url = String::new();
        loop {
            match self.cur.next() {
                Tok::Eof => break,
                Tok::End(name) if name == "item" => break,
                Tok::Start(el) => match el.local.as_str() {
                    "title" => article.title = truncate(self.cur.read_element_text(false).trim().to_owned(), MAX_SHORT_TEXT),
                    "enclosure" => {
                        let ty = el.attr("type").unwrap_or_default();
                        if ty == "application/x-bittorrent" {
                            article.torrent_url = url_value(el.attr("url").unwrap_or_default().to_owned());
                        } else if ty.is_empty() {
                            alt_torrent_url = url_value(el.attr("url").unwrap_or_default().to_owned());
                        }
                    }
                    "link" => {
                        let text = self.cur.read_element_text(false).trim().to_owned();
                        if starts_with_ci(&text, "magnet:") {
                            article.torrent_url = url_value(text);
                        } else {
                            article.link = url_value(text);
                        }
                    }
                    "description" => article.description = truncate(self.cur.read_element_text(true), MAX_TEXT),
                    "pubDate" => {
                        let text = self.cur.read_element_text(false);
                        article.date = match parse_rfc822(&text) {
                            Rfc822::Valid(ms) => Some(ms),
                            Rfc822::Fallback => Some(self.fallback),
                            Rfc822::Invalid => None,
                        };
                    }
                    "author" => article.author = truncate(self.cur.read_element_text(false).trim().to_owned(), MAX_SHORT_TEXT),
                    "guid" => article.id = truncate(self.cur.read_element_text(false).trim().to_owned(), MAX_SHORT_TEXT),
                    other => {
                        let other = other.to_owned();
                        let text = self.cur.read_element_text(true);
                        if self.rdf && other == "creator" && article.author.is_empty() {
                            article.author = truncate(text.trim().to_owned(), MAX_SHORT_TEXT);
                        }
                        article.set_other(&other, text);
                    }
                },
                _ => {}
            }
        }
        if article.torrent_url.is_empty() {
            article.torrent_url = alt_torrent_url;
        }
        // `torrent:magnetURI` (ezRSS namespace) when nothing better exists.
        if article.torrent_url.is_empty()
            && let Some(magnet) = article.extra("magnetURI").map(str::trim)
            && starts_with_ci(magnet, "magnet:")
        {
            article.torrent_url = url_value(magnet.to_owned());
        }
        self.add_article(article);
    }

    fn parse_atom_channel(&mut self) {
        loop {
            match self.cur.next() {
                Tok::Eof => return,
                Tok::Start(el) => match el.local.as_str() {
                    "title" => {
                        self.result.title = truncate(self.cur.read_element_text(false), MAX_FEED_TITLE);
                    }
                    "updated" => {
                        let value = truncate(self.cur.read_element_text(false), MAX_FEED_TITLE);
                        if !value.is_empty() {
                            if self.result.last_build_date == value {
                                return;
                            }
                            self.result.last_build_date = value;
                        }
                    }
                    "entry" => self.parse_atom_article(),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn parse_atom_article(&mut self) {
        let mut article = ParsedArticle::default();
        let mut double_content = false;
        loop {
            match self.cur.next() {
                Tok::Eof => break,
                Tok::End(name) if name == "entry" => break,
                Tok::Start(el) => match el.local.as_str() {
                    "title" => article.title = truncate(self.cur.read_element_text(false).trim().to_owned(), MAX_SHORT_TEXT),
                    "link" => {
                        let link = if el.attrs.is_empty() {
                            self.cur.read_element_text(false).trim().to_owned()
                        } else {
                            el.attr("href").unwrap_or_default().to_owned()
                        };
                        if starts_with_ci(&link, "magnet:") {
                            article.torrent_url = url_value(link);
                        } else if self.base_url.is_empty() || self.base_url.len() + link.len() > MAX_SHORT_TEXT {
                            article.link = url_value(link);
                        } else {
                            article.link = url_value(format!("{}{}", self.base_url, link));
                        }
                    }
                    "summary" | "content" => {
                        if double_content {
                            self.cur.skip_current_element();
                            continue;
                        }
                        let text = self.cur.read_element_text(true).trim().to_owned();
                        if !text.is_empty() {
                            article.description = truncate(text, MAX_TEXT);
                            double_content = true;
                        }
                    }
                    "updated" => {
                        let text = self.cur.read_element_text(false);
                        article.date = Some(parse_iso8601(text.trim()).unwrap_or(self.fallback));
                    }
                    "author" => {
                        while let Some(child) = self.cur.read_next_start_element() {
                            if child.local == "name" {
                                article.author =
                                    truncate(self.cur.read_element_text(false).trim().to_owned(), MAX_SHORT_TEXT);
                            } else {
                                self.cur.skip_current_element();
                            }
                        }
                    }
                    "id" => article.id = truncate(self.cur.read_element_text(false).trim().to_owned(), MAX_SHORT_TEXT),
                    other => {
                        let other = other.to_owned();
                        let text = self.cur.read_element_text(true);
                        article.set_other(&other, text);
                    }
                },
                _ => {}
            }
        }
        self.add_article(article);
    }

    fn add_article(&mut self, mut article: ParsedArticle) {
        if article.torrent_url.is_empty() {
            article.torrent_url = article.link.clone();
        }
        if article.id.is_empty() {
            article.id = if !article.torrent_url.is_empty() { article.torrent_url.clone() } else { article.title.clone() };
            if article.id.is_empty() {
                // Cannot be identified: ignored.
                return;
            }
        }
        if self.result.articles.len() >= MAX_PARSED_ARTICLES || self.ids.contains(&article.id) {
            return;
        }
        self.ids.insert(article.id.clone());
        self.result.articles.push(article);
    }
}

// ---- XML cursor (a small QXmlStreamReader look-alike) -----------------------

struct Element {
    local: String,
    /// Attributes by qualified name (`xml:base`, `href`, ...).
    attrs: Vec<(String, String)>,
}

impl Element {
    fn attr(&self, qname: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == qname).map(|(_, v)| v.as_str())
    }
}

enum Tok {
    Start(Element),
    End(String),
    Text(String),
    Eof,
}

struct Cursor<'a> {
    reader: Reader<&'a [u8]>,
    src: &'a str,
    pending_end: Option<String>,
    depth: usize,
    events: usize,
    done: bool,
    error: Option<String>,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Self {
        let mut reader = Reader::from_str(src);
        let cfg = reader.config_mut();
        // Real-world feeds often contain bare '&' in URLs.
        cfg.allow_dangling_amp = true;
        cfg.check_end_names = true;
        cfg.expand_empty_elements = false;
        Self { reader, src, pending_end: None, depth: 0, events: 0, done: false, error: None }
    }

    fn fail(&mut self, message: String, offset: usize) {
        let offset = offset.min(self.src.len());
        let before = self.src.get(..offset).unwrap_or(self.src);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        let chars = before.chars().count();
        self.error = Some(format!("{message} (line: {line}, column: {column}, offset: {chars})."));
        self.done = true;
    }

    fn next(&mut self) -> Tok {
        if let Some(name) = self.pending_end.take() {
            self.depth = self.depth.saturating_sub(1);
            return Tok::End(name);
        }
        loop {
            if self.done {
                return Tok::Eof;
            }
            self.events += 1;
            if self.events > MAX_EVENTS {
                let pos = self.reader.buffer_position() as usize;
                self.fail("Too many XML nodes".to_owned(), pos);
                return Tok::Eof;
            }
            let event = match self.reader.read_event() {
                Ok(ev) => ev,
                Err(e) => {
                    let pos = self.reader.error_position() as usize;
                    self.fail(e.to_string(), pos);
                    return Tok::Eof;
                }
            };
            match event {
                Event::Start(e) => {
                    self.depth += 1;
                    if self.depth > MAX_DEPTH {
                        let pos = self.reader.buffer_position() as usize;
                        self.fail("Document is nested too deeply".to_owned(), pos);
                        return Tok::Eof;
                    }
                    return Tok::Start(element(&e));
                }
                Event::Empty(e) => {
                    self.depth += 1;
                    if self.depth > MAX_DEPTH {
                        let pos = self.reader.buffer_position() as usize;
                        self.fail("Document is nested too deeply".to_owned(), pos);
                        return Tok::Eof;
                    }
                    let el = element(&e);
                    self.pending_end = Some(el.local.clone());
                    return Tok::Start(el);
                }
                Event::End(e) => {
                    self.depth = self.depth.saturating_sub(1);
                    return Tok::End(String::from_utf8_lossy(e.local_name().as_ref()).into_owned());
                }
                Event::Text(t) => return Tok::Text(String::from_utf8_lossy(&t).into_owned()),
                Event::CData(t) => return Tok::Text(String::from_utf8_lossy(&t).into_owned()),
                Event::GeneralRef(r) => {
                    let name = String::from_utf8_lossy(&r);
                    return Tok::Text(match entities::resolve(&name) {
                        Some(c) => c.to_string(),
                        // Unknown (or DTD-declared) entity: kept verbatim, never expanded.
                        None => format!("&{name};"),
                    });
                }
                Event::Eof => {
                    if self.depth > 0 {
                        self.fail("Premature end of document.".to_owned(), self.src.len());
                    }
                    self.done = true;
                    return Tok::Eof;
                }
                // Declarations, DTDs, comments and processing instructions are ignored.
                _ => {}
            }
        }
    }

    /// Reads until the next start element inside the current element;
    /// `None` when the current element ends (its end tag is consumed).
    fn read_next_start_element(&mut self) -> Option<Element> {
        loop {
            match self.next() {
                Tok::Start(el) => return Some(el),
                Tok::End(_) | Tok::Eof => return None,
                Tok::Text(_) => {}
            }
        }
    }

    /// Skips the rest of the element whose start tag was just read.
    fn skip_current_element(&mut self) {
        let mut nested = 0usize;
        loop {
            match self.next() {
                Tok::Start(_) => nested += 1,
                Tok::End(_) => {
                    if nested == 0 {
                        return;
                    }
                    nested -= 1;
                }
                Tok::Eof => return,
                Tok::Text(_) => {}
            }
        }
    }

    /// Text content of the element whose start tag was just read, up to its
    /// end tag. With `include_children` the text of descendants is included;
    /// otherwise child elements are skipped.
    fn read_element_text(&mut self, include_children: bool) -> String {
        let mut out = String::new();
        let mut nested = 0usize;
        loop {
            match self.next() {
                Tok::Text(t) => {
                    if out.len() < MAX_TEXT {
                        out.push_str(&t);
                    }
                }
                Tok::Start(_) => {
                    if include_children {
                        nested += 1;
                    } else {
                        self.skip_current_element();
                    }
                }
                Tok::End(_) => {
                    if nested == 0 {
                        return truncate(out, MAX_TEXT);
                    }
                    nested -= 1;
                }
                Tok::Eof => return truncate(out, MAX_TEXT),
            }
        }
    }
}

fn element(e: &BytesStart<'_>) -> Element {
    let local = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for attr in e.attributes().flatten().take(MAX_ATTRIBUTES) {
        // Feed attributes are short (URLs, types, lengths): oversized values
        // are dropped rather than copied into every article.
        if attr.value.len() > MAX_SHORT_TEXT {
            continue;
        }
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let raw = String::from_utf8_lossy(&attr.value);
        attrs.push((key, unescape_attribute(&raw)));
    }
    Element { local, attrs }
}

/// Resolves references in an attribute value and applies XML attribute
/// whitespace normalization. Unknown references are kept verbatim.
fn unescape_attribute(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        // Only look a short distance ahead: searching the whole rest of the
        // value for every '&' made "&&&&…" attributes quadratic.
        let resolved = after
            .bytes()
            .take(33)
            .position(|b| b == b';')
            .filter(|end| *end > 0)
            .and_then(|end| entities::resolve(&after[..end]).map(|c| (c, end)));
        match resolved {
            Some((c, end)) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.chars().map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c }).collect()
}

/// Converts the raw document to UTF-8 the way an XML reader does: BOM,
/// then the `encoding` of the XML declaration, else UTF-8. Line ends are
/// normalized to `\n`.
pub(crate) fn decode_document(data: &[u8]) -> String {
    let text: Cow<'_, str> = if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(data) {
        let (cow, _) = enc.decode_without_bom_handling(&data[bom_len..]);
        cow
    } else if data.starts_with(&[0x3C, 0x00, 0x3F, 0x00]) || data.starts_with(&[0x3C, 0x00, 0x00, 0x00]) {
        encoding_rs::UTF_16LE.decode_without_bom_handling(data).0
    } else if data.starts_with(&[0x00, 0x3C, 0x00, 0x3F]) {
        encoding_rs::UTF_16BE.decode_without_bom_handling(data).0
    } else {
        let declared = declared_encoding(data)
            .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
            // A document that is not UTF-16 cannot meaningfully declare it.
            .filter(|enc| *enc != encoding_rs::UTF_16LE && *enc != encoding_rs::UTF_16BE);
        match declared {
            Some(enc) if enc != encoding_rs::UTF_8 => enc.decode_without_bom_handling(data).0,
            _ => String::from_utf8_lossy(data),
        }
    };
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    if text.contains('\r') { text.replace("\r\n", "\n").replace('\r', "\n") } else { text.to_owned() }
}

/// `encoding="..."` from an XML declaration at the very start of the data.
fn declared_encoding(data: &[u8]) -> Option<String> {
    let head = &data[..data.len().min(1024)];
    let head = std::str::from_utf8(head).ok().or_else(|| {
        // Cut at the last valid UTF-8 boundary.
        let valid = std::str::from_utf8(head).err()?.valid_up_to();
        std::str::from_utf8(&head[..valid]).ok()
    })?;
    let decl = head.trim_start().strip_prefix("<?xml")?;
    let decl = &decl[..decl.find("?>")?];
    let pos = decl.find("encoding")?;
    let rest = decl[pos + "encoding".len()..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = &rest[1..];
    let end = value.find(quote)?;
    Some(value[..end].trim().to_owned()).filter(|v| !v.is_empty() && v.len() <= 64)
}

pub(crate) fn starts_with_ci(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// URLs longer than the cap are dropped rather than truncated (a truncated
/// URL would point somewhere else).
fn url_value(s: String) -> String {
    if s.len() > MAX_SHORT_TEXT { String::new() } else { s }
}

/// Truncates at a character boundary.
pub(crate) fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn parse_str(s: &str) -> ParseResult {
        parse(s.as_bytes(), "", NOW)
    }

    #[test]
    fn oversized_xml_base_is_not_amplified() {
        // 60 KiB base + 10 000 tiny entries used to hold ~2.4 GB of links.
        let base = format!("https://e.org/{}/", "a".repeat(60 * 1024));
        let mut xml = format!(r#"<feed xmlns="http://www.w3.org/2005/Atom" xml:base="{base}">"#);
        for i in 0..10_000 {
            xml.push_str(&format!(r#"<entry><id>{i}</id><link href="{i}"/></entry>"#));
        }
        xml.push_str("</feed>");
        let r = parse_str(&xml);
        assert_eq!(r.articles.len(), 10_000);
        let total: usize = r.articles.iter().map(|a| a.link.len() + a.torrent_url.len() + a.id.len()).sum();
        assert!(total < 1024 * 1024, "{total}");
        // A normal base still applies.
        let r = parse_str(r#"<feed xml:base="https://e.org/x/"><entry><id>1</id><link href="a.torrent"/></entry></feed>"#);
        assert_eq!(r.articles[0].link, "https://e.org/x/a.torrent");
    }

    #[test]
    #[cfg(unix)]
    fn attribute_unescaping_is_linear() {
        // Compare the CPU time of two input sizes instead of a wall-clock budget,
        // so machine load and scheduling do not matter: 8x the input costs about
        // 8x when linear and about 64x when quadratic.
        fn thread_cpu() -> std::time::Duration {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            // SAFETY: `ts` is a valid out-pointer for the duration of the call.
            assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) }, 0);
            std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
        }
        fn cost(n: usize) -> std::time::Duration {
            let value = format!("{};", "&".repeat(n));
            let start = thread_cpu();
            assert_eq!(unescape_attribute(&value).len(), value.len());
            thread_cpu() - start
        }
        let (small, large) = (cost(64 * 1024), cost(512 * 1024));
        assert!(large < small.max(std::time::Duration::from_micros(200)) * 24, "{small:?} -> {large:?}");
        // Huge attributes are dropped by the parser before unescaping.
        let value = format!("{};", "&".repeat(4 * 1024 * 1024));
        let xml = format!(r#"<rss><channel><item><title>t</title><enclosure url="{value}"/></item></channel></rss>"#);
        assert!(parse_str(&xml).articles[0].torrent_url.is_empty());
        assert_eq!(unescape_attribute("a&amp;b&#233;&lt;&unknown;&"), "a&bé<&unknown;&");
        assert_eq!(unescape_attribute("&&amp;"), "&&");
    }

    #[test]
    fn rss2_with_enclosures_and_magnets() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torrent="http://xmlns.ezrss.it/0.1/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel>
  <title>Linux ISOs</title>
  <lastBuildDate>Sat, 03 Oct 2026 04:05:06 +0000</lastBuildDate>
  <item>
    <title>  Ubuntu 26.04 Desktop  </title>
    <link>https://example.org/ubuntu</link>
    <guid isPermaLink="false"> ubuntu-2604 </guid>
    <pubDate>Sat, 03 Oct 2026 04:05:06 +0000</pubDate>
    <enclosure url="https://example.org/ubuntu.torrent" length="1" type="application/x-bittorrent"/>
    <description><![CDATA[<b>Fresh</b> release]]> &amp; more</description>
    <author> canonical </author>
    <category>Linux</category>
    <dc:creator>someone</dc:creator>
  </item>
  <item>
    <title>Debian 13</title>
    <link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link>
    <pubDate>not a date</pubDate>
  </item>
  <item>
    <title>Fedora 44</title>
    <link>https://example.org/fedora</link>
    <enclosure url="https://example.org/fedora.torrent"/>
  </item>
  <item>
    <title>Arch</title>
    <link>https://example.org/arch</link>
    <torrent:magnetURI>magnet:?xt=urn:btih:ffffffffffffffffffffffffffffffffffffffff</torrent:magnetURI>
  </item>
  <item>
    <title>Only link</title>
    <link>https://example.org/only</link>
  </item>
  <item><description>no id at all</description></item>
</channel>
</rss>"#;
        let r = parse_str(xml);
        assert_eq!(r.error, "");
        assert_eq!(r.title, "Linux ISOs");
        assert_eq!(r.last_build_date, "Sat, 03 Oct 2026 04:05:06 +0000");
        // Reverse document order; the id-less article is dropped.
        let titles: Vec<_> = r.articles.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["Only link", "Arch", "Fedora 44", "Debian 13", "Ubuntu 26.04 Desktop"]);

        let ubuntu = &r.articles[4];
        assert_eq!(ubuntu.id, "ubuntu-2604");
        assert_eq!(ubuntu.torrent_url, "https://example.org/ubuntu.torrent");
        assert_eq!(ubuntu.link, "https://example.org/ubuntu");
        assert_eq!(ubuntu.date, Some(1791000306000));
        assert_eq!(ubuntu.description, "<b>Fresh</b> release & more");
        assert_eq!(ubuntu.author, "canonical");
        assert_eq!(ubuntu.extra("category"), Some("Linux"));
        assert_eq!(ubuntu.extra("creator"), Some("someone"));

        let debian = &r.articles[3];
        assert_eq!(debian.torrent_url, "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567");
        assert_eq!(debian.link, "");
        assert_eq!(debian.id, debian.torrent_url);
        assert_eq!(debian.date, Some(NOW), "unparseable pubDate uses the fallback date");

        let fedora = &r.articles[2];
        assert_eq!(fedora.torrent_url, "https://example.org/fedora.torrent", "type-less enclosure");
        let arch = &r.articles[1];
        assert_eq!(arch.torrent_url, "magnet:?xt=urn:btih:ffffffffffffffffffffffffffffffffffffffff");
        let only = &r.articles[0];
        assert_eq!(only.torrent_url, "https://example.org/only", "link fallback");
        assert_eq!(only.id, "https://example.org/only");
        assert_eq!(only.date, None, "no pubDate");
    }

    #[test]
    fn unchanged_last_build_date_short_circuits() {
        let xml = "<rss><channel><title>T</title><lastBuildDate>X</lastBuildDate>\
                   <item><title>a</title></item></channel></rss>";
        let r = parse(xml.as_bytes(), "X", NOW);
        assert!(r.articles.is_empty());
        assert_eq!(r.error, "");
        let r = parse(xml.as_bytes(), "Y", NOW);
        assert_eq!(r.articles.len(), 1);
        assert_eq!(r.last_build_date, "X");
    }

    #[test]
    fn atom_feed() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:base="https://example.org/">
  <title type="text">Atom &amp; Eve</title>
  <updated>2026-10-03T04:05:06Z</updated>
  <author><name>Feed author</name></author>
  <entry>
    <title type="html">Entry &eacute;one&nbsp;!</title>
    <link rel="alternate" href="posts/1"/>
    <id>urn:uuid:1</id>
    <updated>2026-10-03T04:05:06+02:00</updated>
    <summary>   </summary>
    <content type="html">&lt;p&gt;Body&lt;/p&gt;</content>
    <summary>ignored second content</summary>
    <author><email>x@y</email><name> Jane </name></author>
    <published>2026-10-01T00:00:00Z</published>
  </entry>
  <entry>
    <title>Magnet entry</title>
    <link href="magnet:?xt=urn:btih:abcdefabcdefabcdefabcdefabcdefabcdefabcd"/>
    <updated>garbage</updated>
  </entry>
  <entry>
    <title>Text link</title>
    <link>https://example.org/abs</link>
  </entry>
</feed>"#;
        let r = parse_str(xml);
        assert_eq!(r.error, "");
        assert_eq!(r.title, "Atom & Eve");
        assert_eq!(r.last_build_date, "2026-10-03T04:05:06Z");
        assert_eq!(r.articles.len(), 3);
        let first = &r.articles[2];
        assert_eq!(first.title, "Entry \u{e9}one\u{a0}!");
        assert_eq!(first.link, "https://example.org/posts/1");
        assert_eq!(first.torrent_url, "https://example.org/posts/1");
        assert_eq!(first.id, "urn:uuid:1");
        assert_eq!(first.date, Some(1790993106000));
        assert_eq!(first.description, "<p>Body</p>");
        assert_eq!(first.author, "Jane");
        assert_eq!(first.extra("published"), Some("2026-10-01T00:00:00Z"));
        let magnet = &r.articles[1];
        assert_eq!(magnet.torrent_url, "magnet:?xt=urn:btih:abcdefabcdefabcdefabcdefabcdefabcdefabcd");
        assert_eq!(magnet.date, Some(NOW));
        // Text link (no attributes) also gets the base prepended (string concatenation).
        assert_eq!(r.articles[0].link, "https://example.org/https://example.org/abs");
    }

    #[test]
    fn rdf_feed() {
        let xml = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/"
         xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel rdf:about="https://example.org/">
    <title>RDF Feed</title>
    <items><rdf:Seq><rdf:li resource="https://example.org/1"/></rdf:Seq></items>
  </channel>
  <item rdf:about="https://example.org/1">
    <title>Show S01E02 720p</title>
    <link>https://example.org/1.torrent</link>
    <dc:date>2026-10-03T04:05:06Z</dc:date>
    <dc:creator>Uploader</dc:creator>
  </item>
</rdf:RDF>"#;
        let r = parse_str(xml);
        assert_eq!(r.error, "");
        assert_eq!(r.title, "RDF Feed");
        assert_eq!(r.articles.len(), 1);
        let a = &r.articles[0];
        assert_eq!(a.title, "Show S01E02 720p");
        assert_eq!(a.torrent_url, "https://example.org/1.torrent");
        assert_eq!(a.date, Some(1791000306000));
        assert_eq!(a.author, "Uploader");
    }

    #[test]
    fn errors_keep_articles_parsed_so_far() {
        let xml = "<rss><channel><item><title>one</title></item><item><title>two</title></itm>";
        let r = parse_str(xml);
        assert!(r.error.contains("line: 1"), "{}", r.error);
        let titles: Vec<_> = r.articles.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(titles, ["two", "one"]);
        assert_eq!(parse_str("<html><body/></html>").error, "Invalid RSS feed.");
        assert_eq!(parse_str("").error, "Invalid RSS feed.");
    }

    #[test]
    fn rss_date_variants_and_duplicates() {
        let xml = "<rss><channel>\
            <item><guid>a</guid><title>first</title><pubDate>03 Oct 2026 04:05:06 +0099</pubDate></item>\
            <item><guid>a</guid><title>dup</title></item>\
            <item><guid>b</guid><dc:date>2026-10-03T04:05:06Z</dc:date></item>\
            </channel></rss>";
        let r = parse_str(xml);
        assert_eq!(r.articles.len(), 2);
        assert_eq!(r.articles[1].title, "first");
        assert_eq!(r.articles[1].date, None, "invalid offset gives an invalid date");
        assert_eq!(r.articles[0].date, Some(1791000306000));
    }

    #[test]
    fn entities_and_dtd_are_not_expanded() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE rss [
  <!ENTITY lol "lol">
  <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<rss><channel><item><title>&lol2; &xxe; &copy; &#169; &#xA9; AT&T</title><guid>1</guid></item></channel></rss>"#;
        let r = parse_str(xml);
        assert_eq!(r.articles.len(), 1);
        assert_eq!(r.articles[0].title, "&lol2; &xxe; \u{a9} \u{a9} \u{a9} AT&T");
    }

    #[test]
    fn encodings() {
        // windows-1251 declared, Cyrillic title.
        let mut data = b"<?xml version=\"1.0\" encoding=\"windows-1251\"?><rss><channel><item><title>".to_vec();
        data.extend_from_slice(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
        data.extend_from_slice(b"</title></item></channel></rss>");
        let r = parse(&data, "", NOW);
        assert_eq!(r.articles[0].title, "Привет");

        // UTF-16LE with BOM.
        let s = "<rss><channel><item><title>Ünïcödé</title></item></channel></rss>";
        let mut utf16 = vec![0xFF, 0xFE];
        for u in s.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(parse(&utf16, "", NOW).articles[0].title, "Ünïcödé");

        // UTF-8 BOM and CRLF.
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(b"<rss><channel><item><title>a</title><description>x\r\ny</description></item></channel></rss>");
        let r = parse(&data, "", NOW);
        assert_eq!(r.articles[0].description, "x\ny");

        // Invalid UTF-8 without a declaration is decoded lossily.
        let r = parse(b"<rss><channel><item><title>\xff\xfe ok</title></item></channel></rss>", "", NOW);
        assert_eq!(r.articles.len(), 1);
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        let deep = format!("<rss><channel>{}</channel></rss>", "<a>".repeat(10_000));
        let r = parse_str(&deep);
        assert!(r.error.contains("nested too deeply"), "{}", r.error);

        let many = format!(
            "<rss><channel>{}</channel></rss>",
            (0..(MAX_PARSED_ARTICLES + 50)).map(|i| format!("<item><guid>{i}</guid></item>")).collect::<String>()
        );
        assert_eq!(parse_str(&many).articles.len(), MAX_PARSED_ARTICLES);

        let huge_title = format!("<rss><channel><item><title>{}</title></item></channel></rss>", "x".repeat(3 * MAX_TEXT));
        let r = parse_str(&huge_title);
        assert!(r.articles[0].title.len() <= MAX_SHORT_TEXT);

        let huge_url = format!(
            "<rss><channel><item><title>t</title><link>https://e/{}</link></item></channel></rss>",
            "a".repeat(MAX_SHORT_TEXT)
        );
        let r = parse_str(&huge_url);
        assert_eq!(r.articles[0].link, "");

        let too_big = vec![b' '; MAX_FEED_SIZE + 1];
        assert!(parse(&too_big, "", NOW).error.contains("too large"));

        // Pseudo-random garbage, truncated documents and odd markup.
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let alphabet = b"<>/=\"'&;#x!?[]-:rsitemchanelfdgu 0123456789\n";
        for _ in 0..500 {
            let len = (seed % 300) as usize;
            let mut doc = b"<rss><channel><item>".to_vec();
            for _ in 0..len {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                doc.push(alphabet[(seed % alphabet.len() as u64) as usize]);
            }
            let _ = parse(&doc, "", NOW);
            let _ = parse(&doc[20..], "", NOW);
        }
        for doc in [
            "<",
            "<?xml",
            "<rss",
            "<rss><channel><item><title>",
            "<feed><entry><link href='x'",
            "<rss><channel><item><enclosure url=\"&#0;&#xFFFFFFFF;&bogus\" type='application/x-bittorrent'/></item></channel></rss>",
            "<![CDATA[",
            "<!DOCTYPE x [<!ENTITY a SYSTEM 'http://evil/'>]><rss>&a;</rss>",
            "\u{0}\u{1}\u{2}",
        ] {
            let _ = parse_str(doc);
        }
    }

    #[test]
    fn attribute_unescaping() {
        assert_eq!(unescape_attribute("a&amp;b&lt;&#65;&bogus;&\tc\nd"), "a&b<A&bogus;& c d");
    }
}
