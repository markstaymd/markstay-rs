//! Strict host-first marker discovery (SPEC.md §3 / §4).
//!
//! Recognition uses an LF-normalized view, while every returned span and `raw`
//! string addresses the caller's original bytes. One scanner feeds discovery,
//! stripping, fenced-code masking, and rewriting.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Syntax {
    Html,
    Mdx,
}

impl Syntax {
    pub fn as_str(&self) -> &'static str {
        match self {
            Syntax::Html => "html",
            Syntax::Mdx => "mdx",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Marker {
    /// The positional id, or `None` for the required no-id diagnostic shape.
    pub id: Option<String>,
    /// A valid bare `hash=sha256:<hex>`, folded lowercase.
    pub hash: Option<String>,
    /// A valid bare `subhash=sha256:<hex>`, folded lowercase.
    pub subhash: Option<String>,
    /// Exact parsed-key presence, independent of the subhash value's validity.
    pub has_subhash: bool,
    /// Exact original marker serialization, delimiters included.
    pub raw: String,
    pub syntax: Syntax,
    /// 1-based line number after CRLF/lone-CR normalization.
    pub line: usize,
    pub malformed: bool,
    // Raw-relative spans used by write surgery. Keeping these on the parsed
    // marker prevents the writer from growing a second attribute grammar.
    pub(crate) stay_span: Option<(usize, usize)>,
    pub(crate) hash_span: Option<(usize, usize, usize)>,
}

impl Marker {
    /// Whether this lexical token may identify its containing §5 block.
    pub fn is_block_stay(&self) -> bool {
        !self.malformed && !self.has_subhash
    }
}

#[inline]
pub(crate) fn is_ws_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t')
}

#[inline]
pub(crate) fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

#[inline]
fn is_key_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

pub(crate) fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

struct NormalizedView {
    text: String,
    /// Normalized byte boundary -> original byte boundary.
    raw_boundaries: Vec<usize>,
}

fn normalized_view(text: &str) -> NormalizedView {
    let bytes = text.as_bytes();
    let mut normalized = String::with_capacity(text.len());
    let mut raw_boundaries = Vec::with_capacity(text.len() + 1);
    raw_boundaries.push(0);
    let mut raw = 0usize;
    while raw < bytes.len() {
        if bytes[raw] == b'\r' {
            raw += if bytes.get(raw + 1) == Some(&b'\n') { 2 } else { 1 };
            normalized.push('\n');
            raw_boundaries.push(raw);
        } else {
            let width =
                text[raw..].chars().next().expect("raw offset is a char boundary").len_utf8();
            normalized.push_str(&text[raw..raw + width]);
            for delta in 1..=width {
                raw_boundaries.push(raw + delta);
            }
            raw += width;
        }
    }
    NormalizedView { text: normalized, raw_boundaries }
}

#[derive(Clone)]
struct Attribute {
    key: String,
    value: String,
    quoted: bool,
    key_start: usize,
    value_start: usize,
    value_end: usize,
}

struct ParsedBody {
    id: String,
    id_end: usize,
    attributes: Vec<Attribute>,
}

fn parse_body(body: &str) -> Option<ParsedBody> {
    let bytes = body.as_bytes();
    if !bytes.starts_with(b"stay:") {
        return None;
    }
    let mut pos = 5usize;
    let id_start = pos;
    while bytes.get(pos).is_some_and(|b| is_id_byte(*b)) {
        pos += 1;
    }
    if pos == id_start || (pos < bytes.len() && !is_ws_byte(bytes[pos])) {
        return None;
    }
    let id = body[id_start..pos].to_string();
    let id_end = pos;
    let mut attributes = Vec::new();

    while pos < bytes.len() {
        let separator_start = pos;
        while bytes.get(pos).is_some_and(|b| is_ws_byte(*b)) {
            pos += 1;
        }
        if pos == bytes.len() {
            break;
        }
        if pos == separator_start || !is_key_start(bytes[pos]) {
            return None;
        }
        let key_start = pos;
        pos += 1;
        while bytes.get(pos).is_some_and(|b| is_id_byte(*b)) {
            pos += 1;
        }
        let key = body[key_start..pos].to_string();
        if bytes.get(pos) != Some(&b'=') {
            return None;
        }
        pos += 1;
        if pos == bytes.len() {
            return None;
        }

        let quoted = bytes[pos] == b'"';
        let value_start;
        let value_end;
        if quoted {
            pos += 1;
            value_start = pos;
            while pos < bytes.len() && bytes[pos] != b'"' {
                match bytes[pos] {
                    b'\\' => {
                        if !matches!(bytes.get(pos + 1), Some(b'\\' | b'"')) {
                            return None;
                        }
                        pos += 2;
                    }
                    b'\n' | 0x20..=0x21 | 0x23..=0x5b | 0x5d..=0x7e => pos += 1,
                    _ => return None,
                }
            }
            if pos == bytes.len() {
                return None;
            }
            value_end = pos;
            pos += 1;
        } else {
            value_start = pos;
            while pos < bytes.len() && !is_ws_byte(bytes[pos]) {
                if bytes[pos] == b'"' || !(0x21..=0x7e).contains(&bytes[pos]) {
                    return None;
                }
                pos += 1;
            }
            if pos == value_start {
                return None;
            }
            value_end = pos;
        }
        attributes.push(Attribute {
            key,
            value: body[value_start..value_end].to_string(),
            quoted,
            key_start,
            value_start,
            value_end,
        });
    }
    Some(ParsedBody { id, id_end, attributes })
}

#[derive(Clone, Copy)]
enum BodyState {
    Start,
    Id,
    Separator,
    Key,
    Value,
    Bare,
    Quoted,
    Escape,
    AfterQuote,
}

impl BodyState {
    fn next(self, byte: u8) -> Option<Self> {
        use BodyState::*;
        match self {
            Start if is_id_byte(byte) => Some(Id),
            Id if is_id_byte(byte) => Some(Id),
            Id | Bare | AfterQuote if is_ws_byte(byte) => Some(Separator),
            Separator if is_ws_byte(byte) => Some(Separator),
            Separator if is_key_start(byte) => Some(Key),
            Key if is_id_byte(byte) => Some(Key),
            Key if byte == b'=' => Some(Value),
            Value if byte == b'"' => Some(Quoted),
            Value | Bare if byte != b'"' && (0x21..=0x7e).contains(&byte) => Some(Bare),
            Quoted if byte == b'"' => Some(AfterQuote),
            Quoted if byte == b'\\' => Some(Escape),
            Quoted if byte == b'\n' || (0x20..=0x7e).contains(&byte) => Some(Quoted),
            Escape if matches!(byte, b'\\' | b'"') => Some(Quoted),
            _ => None,
        }
    }

    fn accepts_end(self) -> bool {
        matches!(self, Self::Id | Self::Separator | Self::Bare | Self::AfterQuote)
    }
}

/// Candidates of one host syntax share a first closer until their starts pass
/// it. Memoize grammar validity for that interval before allocating attributes:
/// each (byte position, grammar state) is visited at most once, even when many
/// overlapping candidates would otherwise reparse the same failing suffix.
#[derive(Default)]
struct BodyValidity {
    close: Option<usize>,
    start: usize,
    cache: Vec<[u8; 9]>,
}

impl BodyValidity {
    fn accepts(&mut self, text: &[u8], start: usize, close: usize) -> bool {
        if self.close != Some(close) {
            self.close = Some(close);
            self.start = start;
            self.cache.clear();
            self.cache.resize(close - start + 1, [0; 9]);
        }
        let mut pos = start;
        let mut state = BodyState::Start;
        let mut path = Vec::new();
        let accepted = loop {
            let index = pos - self.start;
            let cached = self.cache[index][state as usize];
            if cached != 0 {
                break cached == 2;
            }
            #[cfg(test)]
            body_validity_tests::VISITS.with(|visits| visits.set(visits.get() + 1));
            path.push((index, state));
            if pos == close {
                break state.accepts_end();
            }
            let Some(next) = state.next(text[pos]) else { break false };
            state = next;
            pos += 1;
        };
        for (index, state) in path {
            self.cache[index][state as usize] = if accepted { 2 } else { 1 };
        }
        accepted
    }
}

fn malformed_key_first(body: &str) -> bool {
    let bytes = body.as_bytes();
    if !bytes.starts_with(b"stay:") || !bytes.get(5).is_some_and(|b| is_key_start(*b)) {
        return false;
    }
    let mut pos = 6usize;
    while bytes.get(pos).is_some_and(|b| is_id_byte(*b)) {
        pos += 1;
    }
    bytes.get(pos) == Some(&b'=')
}

fn digest(value: &str, quoted: bool) -> Option<String> {
    if quoted {
        return None;
    }
    let hex = value.strip_prefix("sha256:")?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex.to_ascii_lowercase())
}

#[derive(Clone)]
pub(crate) struct MarkerRecord {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) marker: Marker,
}

/// `find_sub` from a position over one text, reusing the last answer while it
/// still holds. Every marker searches forward for each opener and closer, and a
/// needle that is far away or absent would otherwise be rescanned to the end per
/// marker.
struct ForwardFind<'a> {
    text: &'a [u8],
    needle: &'static [u8],
    from: usize,
    at: Option<usize>,
}

impl<'a> ForwardFind<'a> {
    fn new(text: &'a [u8], needle: &'static [u8]) -> Self {
        ForwardFind { text, needle, from: usize::MAX, at: None }
    }

    fn find(&mut self, query: usize) -> Option<usize> {
        if query < self.from || self.at.is_some_and(|at| query > at) {
            self.from = query;
            self.at = find_sub(&self.text[query..], self.needle).map(|n| query + n);
        }
        self.at
    }
}

struct Finders<'a> {
    html: ForwardFind<'a>,
    mdx: ForwardFind<'a>,
    html_close: ForwardFind<'a>,
    html_parse_error_close: ForwardFind<'a>,
    mdx_close: ForwardFind<'a>,
}

fn next_opener(find: &mut Finders, from: usize) -> Option<(usize, Syntax, usize)> {
    let html = find.html.find(from);
    let mdx = find.mdx.find(from);
    match (html, mdx) {
        (None, None) => None,
        (Some(start), None) => Some((start, Syntax::Html, 4)),
        (None, Some(start)) => Some((start, Syntax::Mdx, 3)),
        (Some(h), Some(m)) if h <= m => Some((h, Syntax::Html, 4)),
        (Some(_), Some(m)) => Some((m, Syntax::Mdx, 3)),
    }
}

fn host_close(
    text: &[u8],
    find: &mut Finders,
    syntax: Syntax,
    from: usize,
) -> Option<(usize, usize, bool)> {
    match syntax {
        Syntax::Mdx => {
            let start = find.mdx_close.find(from)?;
            let valid = text.get(start + 2) == Some(&b'}');
            Some((start, start + if valid { 3 } else { 2 }, valid))
        }
        Syntax::Html => {
            let ordinary = find.html_close.find(from);
            let parse_error = find.html_parse_error_close.find(from);
            match (ordinary, parse_error) {
                (None, None) => None,
                (Some(start), None) => Some((start, start + 3, true)),
                (None, Some(start)) => Some((start, start + 4, false)),
                (Some(o), Some(p)) if o <= p => Some((o, o + 3, true)),
                (Some(_), Some(p)) => Some((p, p + 4, false)),
            }
        }
    }
}

pub(crate) fn scan_marker_records(text: &str, line_offset: usize) -> Vec<MarkerRecord> {
    let view = normalized_view(text);
    let normalized = view.text.as_bytes();
    let mut records = Vec::new();
    // Openers arrive in source order, so the line count advances with them.
    let mut newlines = 0usize;
    let mut counted_to = 0usize;
    let mut find = Finders {
        html: ForwardFind::new(normalized, b"<!--"),
        mdx: ForwardFind::new(normalized, b"{/*"),
        html_close: ForwardFind::new(normalized, b"-->"),
        html_parse_error_close: ForwardFind::new(normalized, b"--!>"),
        mdx_close: ForwardFind::new(normalized, b"*/"),
    };
    let mut html_validity = BodyValidity::default();
    let mut mdx_validity = BodyValidity::default();
    let mut cursor = 0usize;
    while cursor < normalized.len() {
        let Some((open_start, syntax, open_len)) = next_opener(&mut find, cursor) else { break };
        let mut body_start = open_start + open_len;
        while normalized.get(body_start).is_some_and(|b| is_ws_byte(*b)) {
            body_start += 1;
        }
        if !normalized[body_start..].starts_with(b"stay:") {
            cursor = open_start + 1;
            continue;
        }
        let Some((close_start, close_end, close_valid)) =
            host_close(normalized, &mut find, syntax, body_start + 5)
        else {
            cursor = open_start + 1;
            continue;
        };
        let body = &view.text[body_start..close_start];
        let validity = match syntax {
            Syntax::Html => &mut html_validity,
            Syntax::Mdx => &mut mdx_validity,
        };
        let parsed = if close_valid && validity.accepts(normalized, body_start + 5, close_start) {
            parse_body(body)
        } else {
            None
        };
        let malformed = parsed.is_none() && malformed_key_first(body);
        if parsed.is_none() && !malformed {
            cursor = open_start + 1;
            continue;
        }

        let raw_start = view.raw_boundaries[open_start];
        let raw_end = view.raw_boundaries[close_end];
        let raw_body_start = view.raw_boundaries[body_start];
        let mut block_hash = None;
        let mut child_hash = None;
        let mut has_subhash = false;
        let mut stay_span = None;
        let mut hash_span = None;
        if let Some(parsed) = &parsed {
            stay_span = Some((
                raw_body_start - raw_start,
                view.raw_boundaries[body_start + parsed.id_end] - raw_start,
            ));
            for attr in &parsed.attributes {
                let value_digest = digest(&attr.value, attr.quoted);
                if attr.key == "hash" && block_hash.is_none() {
                    if let Some(value) = value_digest.clone() {
                        block_hash = Some(value);
                        let key_start =
                            view.raw_boundaries[body_start + attr.key_start] - raw_start;
                        let value_start =
                            view.raw_boundaries[body_start + attr.value_start] - raw_start;
                        let value_end =
                            view.raw_boundaries[body_start + attr.value_end] - raw_start;
                        hash_span = Some((key_start, value_start + 7, value_end));
                    }
                }
                if attr.key == "subhash" {
                    has_subhash = true;
                    if child_hash.is_none() {
                        child_hash = value_digest;
                    }
                }
            }
        }
        newlines += normalized[counted_to..open_start].iter().filter(|&&b| b == b'\n').count();
        counted_to = open_start;
        let marker = Marker {
            id: parsed.as_ref().map(|p| p.id.clone()),
            hash: block_hash,
            subhash: child_hash,
            has_subhash,
            raw: text[raw_start..raw_end].to_string(),
            syntax,
            line: line_offset + (newlines + 1),
            malformed,
            stay_span,
            hash_span,
        };
        records.push(MarkerRecord { start: raw_start, end: raw_end, marker });
        // Every opener is an independent candidate. A valid outer marker does
        // not hide a nested opener from §5.6's overlap refusal.
        cursor = open_start + 1;
    }
    records
}

pub fn find_markers(text: &str, line_offset: usize) -> Vec<Marker> {
    scan_marker_records(text, line_offset).into_iter().map(|record| record.marker).collect()
}

fn valid_records(text: &str) -> Vec<MarkerRecord> {
    scan_marker_records(text, 0).into_iter().filter(|record| !record.marker.malformed).collect()
}

pub fn strip_markers(text: &str) -> String {
    let records = valid_records(text);
    if records.is_empty() {
        return text.to_string();
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for record in records {
        if let Some(previous) = ranges.last_mut() {
            if record.start < previous.1 {
                previous.1 = previous.1.max(record.end);
                continue;
            }
        }
        ranges.push((record.start, record.end));
    }
    let mut out = String::with_capacity(text.len());
    let mut previous = 0usize;
    for (start, end) in ranges {
        out.push_str(&text[previous..start]);
        previous = end;
    }
    out.push_str(&text[previous..]);
    out
}

/// Rewrite non-overlapping valid markers outside the optional fenced-code mask.
/// Ambiguous overlapping spans remain byte-for-byte unchanged.
pub fn rewrite_markers<F>(text: &str, mut transform: F, code: Option<&BTreeSet<usize>>) -> String
where
    F: FnMut(&Marker) -> Option<String>,
{
    let records: Vec<MarkerRecord> = valid_records(text)
        .into_iter()
        .filter(|record| !code.is_some_and(|lines| lines.contains(&record.marker.line)))
        .collect();
    let mut overlaps = alloc::vec![false; records.len()];
    for left in 0..records.len() {
        let mut right = left + 1;
        while right < records.len() && records[right].start < records[left].end {
            overlaps[left] = true;
            overlaps[right] = true;
            right += 1;
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut previous = 0usize;
    for (index, record) in records.into_iter().enumerate() {
        if overlaps[index] {
            continue;
        }
        out.push_str(&text[previous..record.start]);
        match transform(&record.marker) {
            Some(replacement) => out.push_str(&replacement),
            None => out.push_str(&record.marker.raw),
        }
        previous = record.end;
    }
    out.push_str(&text[previous..]);
    out
}

pub(crate) fn find_stay_span(s: &str) -> Option<(usize, usize)> {
    scan_marker_records(s, 0).into_iter().next()?.marker.stay_span
}

pub(crate) fn find_hash_hex_span(s: &str) -> Option<(usize, usize, usize)> {
    scan_marker_records(s, 0).into_iter().next()?.marker.hash_span
}

#[cfg(test)]
mod body_validity_tests {
    extern crate std;

    use core::cell::Cell;

    std::thread_local! {
        pub(super) static VISITS: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn overlapping_rejections_visit_each_state_position_at_most_once() {
        for (opener, closer) in [("<!--stay:", "-->"), ("{/*stay:", "*/}")] {
            let prefix = alloc::format!("{opener}x a=");
            let text = prefix.repeat(4000) + "\n" + closer;
            VISITS.with(|visits| visits.set(0));
            assert!(super::find_markers(&text, 0).is_empty());
            let visits = VISITS.with(Cell::get);
            // The lower bound confirms that instrumentation is live. Thread
            // local counts isolate this scan from Rust's parallel test runner.
            assert!(visits >= text.len() && visits <= 9 * text.len(), "{visits} state visits");
        }
    }
}
