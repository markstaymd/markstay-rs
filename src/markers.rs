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

fn next_opener(text: &[u8], from: usize) -> Option<(usize, Syntax, usize)> {
    let html = find_sub(&text[from..], b"<!--").map(|n| from + n);
    let mdx = find_sub(&text[from..], b"{/*").map(|n| from + n);
    match (html, mdx) {
        (None, None) => None,
        (Some(start), None) => Some((start, Syntax::Html, 4)),
        (None, Some(start)) => Some((start, Syntax::Mdx, 3)),
        (Some(h), Some(m)) if h <= m => Some((h, Syntax::Html, 4)),
        (Some(_), Some(m)) => Some((m, Syntax::Mdx, 3)),
    }
}

fn host_close(text: &[u8], syntax: Syntax, from: usize) -> Option<(usize, usize, bool)> {
    match syntax {
        Syntax::Mdx => {
            let start = from + find_sub(&text[from..], b"*/")?;
            let valid = text.get(start + 2) == Some(&b'}');
            Some((start, start + if valid { 3 } else { 2 }, valid))
        }
        Syntax::Html => {
            let ordinary = find_sub(&text[from..], b"-->").map(|n| from + n);
            let parse_error = find_sub(&text[from..], b"--!>").map(|n| from + n);
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
    let mut cursor = 0usize;
    while cursor < normalized.len() {
        let Some((open_start, syntax, open_len)) = next_opener(normalized, cursor) else { break };
        let mut body_start = open_start + open_len;
        while normalized.get(body_start).is_some_and(|b| is_ws_byte(*b)) {
            body_start += 1;
        }
        if !normalized[body_start..].starts_with(b"stay:") {
            cursor = open_start + 1;
            continue;
        }
        let Some((close_start, close_end, close_valid)) =
            host_close(normalized, syntax, body_start + 5)
        else {
            cursor = open_start + 1;
            continue;
        };
        let body = &view.text[body_start..close_start];
        let parsed = if close_valid { parse_body(body) } else { None };
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
        let line =
            line_offset + normalized[..open_start].iter().filter(|&&b| b == b'\n').count() + 1;
        let marker = Marker {
            id: parsed.as_ref().map(|p| p.id.clone()),
            hash: block_hash,
            subhash: child_hash,
            has_subhash,
            raw: text[raw_start..raw_end].to_string(),
            syntax,
            line,
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
