// Marker grammar and discovery (SPEC.md §3 / §4). Port of impl/js/src/markers.js
// (`findMarkers`, `stripMarkers`), which ports the marker regexes and
// `find_markers` / `_strip_markers` from the Python reference.
//
// The reference does a raw-text scan: the body is captured lazily up to the
// closing delimiter, then id / hash are pulled out of it. The other impls use a
// regex (`re` / `RegExp`); here the scanner is hand-rolled to stay zero-dep (the
// grammar is tiny and fixed). The two regexes it reproduces are:
//
//   HTML: <!--\s*(stay:.*?)\s*-->        (DOTALL)
//   MDX:  \{/\*\s*(stay:.*?)\s*\*/\}     (DOTALL)
//
// The capture group always begins with `stay:`, so a marker-shaped comment whose
// first token is not `stay:` is not a marker. A marker-shaped comment inside a
// code fence IS treated as a real marker (current reference behaviour; pinned by
// the corpus). `\s` is taken as ASCII whitespace, which agrees with Python/JS on
// the corpus (marker whitespace is kept ASCII).
//
// Scanning is byte-based and safe: the delimiters and whitespace are all ASCII,
// so every slice boundary (open start, `stay:` start, close start) lands on a
// UTF-8 char boundary even when the body contains multibyte text, and an ASCII
// byte never occurs inside a multibyte sequence (so a byte search for `-->` etc.
// cannot false-match mid-character).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::text::rstrip_line_ws;

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
    /// The positional id (first token after `stay:`), or `None` if malformed.
    pub id: Option<String>,
    /// The block hash, canonically lowercase hex, or `None` if absent.
    pub hash: Option<String>,
    /// The full marker text, delimiters included.
    pub raw: String,
    pub syntax: Syntax,
    /// 1-based line number of the marker start in the document.
    pub line: usize,
    /// True when no parseable id was found (`id` is `None`).
    pub malformed: bool,
}

#[inline]
fn is_ws_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)
}

#[inline]
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// First index of `needle` in `haystack`, or `None`.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// One raw marker match: byte offset of the open delimiter, the full match text,
/// and the trimmed group body (starting with `stay:`).
struct RawMatch {
    start: usize,
    raw: String,
    body: String,
}

/// Scan `text` for every `open ... close` marker (matching the regex semantics
/// in the module header) in document order.
fn scan(text: &str, open: &[u8], close: &[u8]) -> Vec<RawMatch> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = find_sub(&bytes[pos..], open) {
        let open_pos = pos + rel;
        // \s* after the open delimiter
        let mut i = open_pos + open.len();
        while i < bytes.len() && is_ws_byte(bytes[i]) {
            i += 1;
        }
        // The group must begin with the literal `stay:`.
        if !bytes[i..].starts_with(b"stay:") {
            pos = open_pos + 1;
            continue;
        }
        let group_start = i;
        // Lazy `.*?` then `\s*close`: the body extends to the FIRST close
        // delimiter (search past the fixed `stay:`), with trailing ASCII
        // whitespace stripped (the greedy `\s*` eats it).
        let search_from = group_start + 5;
        match find_sub(&bytes[search_from..], close) {
            None => {
                // No closing delimiter for this open: not a match here.
                pos = open_pos + 1;
            }
            Some(crel) => {
                let q = search_from + crel;
                let raw = text[open_pos..q + close.len()].to_string();
                let body = rstrip_line_ws(&text[group_start..q]).to_string();
                out.push(RawMatch {
                    start: open_pos,
                    raw,
                    body,
                });
                pos = q + close.len();
            }
        }
    }
    out
}

/// Parse the positional id from a marker body (`^stay:\s*([A-Za-z0-9_-]+)(?=\s|$)`).
/// Returns `None` when no id token is present or the id is followed by a non-ws,
/// non-end character (e.g. `stay:note=hello`), which makes the marker malformed.
fn parse_id(body: &str) -> Option<String> {
    let rest = body.strip_prefix("stay:")?;
    let rest = rest.trim_start_matches(crate::text::is_ws);
    let run: String = rest.chars().take_while(|&c| is_id_char(c)).collect();
    if run.is_empty() {
        return None;
    }
    // Lookahead: the char after the id run must be ASCII whitespace or end.
    // (Shrinking the run only ever exposes another id char, never whitespace, so
    // the lookahead can succeed only at the maximal run boundary.)
    let after = &rest[run.len()..]; // run is ASCII => byte len == char count
    match after.chars().next() {
        None => Some(run),
        Some(c) if crate::text::is_ws(c) => Some(run),
        _ => None,
    }
}

/// Parse the block hash from a marker body
/// (`\bhash\s*=\s*sha256:([0-9a-fA-F]+)`), returned lowercase. First match wins.
fn parse_hash(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = find_sub(&bytes[from..], b"hash") {
        let at = from + rel;
        // \b before `hash`: previous char must be non-word (or start of string).
        let boundary = at == 0 || !is_word_byte(bytes[at - 1]);
        if boundary {
            let mut j = at + 4;
            while j < bytes.len() && is_ws_byte(bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                j += 1;
                while j < bytes.len() && is_ws_byte(bytes[j]) {
                    j += 1;
                }
                if bytes[j..].starts_with(b"sha256:") {
                    let hexstart = j + 7;
                    let mut k = hexstart;
                    while k < bytes.len() && bytes[k].is_ascii_hexdigit() {
                        k += 1;
                    }
                    if k > hexstart {
                        return Some(body[hexstart..k].to_ascii_lowercase());
                    }
                }
            }
        }
        from = at + 1;
    }
    None
}

/// All markstay markers in `text`, ordered by position. `line_offset` is the
/// 0-based line index where `text` begins in the full document.
pub fn find_markers(text: &str, line_offset: usize) -> Vec<Marker> {
    let bytes = text.as_bytes();
    let mut raws: Vec<(usize, String, String, Syntax)> = Vec::new();
    for &(open, close, syn) in &[
        (b"<!--".as_slice(), b"-->".as_slice(), Syntax::Html),
        (b"{/*".as_slice(), b"*/}".as_slice(), Syntax::Mdx),
    ] {
        for m in scan(text, open, close) {
            raws.push((m.start, m.raw, m.body, syn));
        }
    }
    raws.sort_by_key(|t| t.0);

    let mut out = Vec::with_capacity(raws.len());
    for (start, raw, body, syn) in raws {
        let nl = bytes[..start].iter().filter(|&&b| b == b'\n').count();
        let line = line_offset + nl + 1;
        let id = parse_id(&body);
        let hash = parse_hash(&body);
        let malformed = id.is_none();
        out.push(Marker {
            id,
            hash,
            raw,
            syntax: syn,
            line,
            malformed,
        });
    }
    out
}

/// Remove every marker from `text` (HTML first, then MDX, as the reference).
pub fn strip_markers(text: &str) -> String {
    let stage1 = remove_matches(text, b"<!--", b"-->");
    remove_matches(&stage1, b"{/*", b"*/}")
}

fn remove_matches(text: &str, open: &[u8], close: &[u8]) -> String {
    let ms = scan(text, open, close);
    if ms.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for m in &ms {
        let end = m.start + m.raw.len();
        out.push_str(&text[last..m.start]);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}
