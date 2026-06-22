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
pub(crate) fn is_ws_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)
}

/// `\w` byte (`[A-Za-z0-9_]`): the predicate behind the `\b` word boundary that the
/// read and write hash scanners both require before the `hash` attribute key.
#[inline]
pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[inline]
fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Byte form of [`is_id_char`]: a §6 id character `[A-Za-z0-9_-]`. Shared with
/// the write path (id minting / marker serialization), which scans bytes.
#[inline]
pub(crate) fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// First index of `needle` in `haystack`, or `None`.
pub(crate) fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
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
                out.push(RawMatch { start: open_pos, raw, body });
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

/// Byte span `(stay_start, id_end)` of the first `stay:\s*<id>` token in `s`,
/// the write-path counterpart to `parse_id`. Deliberately asymmetric with the
/// read parser: it has no `^stay:` anchor and no `(?=\s|$)` lookahead, so in
/// isolation it would accept an id `parse_id` rejects (a later `stay:`, or
/// `stay:id=x`). That divergence is unreachable: restamp/repair only reach the
/// marker-surgery helpers once `find_markers` has accepted a well-formed id, so
/// the two grammars are kept separate rather than forced into one (a shared
/// finder would rescue inputs the read path must reject). Used by stamp.rs
/// (replace_stay_id / insert_hash_after_stay).
pub(crate) fn find_stay_span(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = find_sub(&bytes[i..], b"stay:") {
        let at = i + rel;
        let mut j = at + 5;
        while j < bytes.len() && is_ws_byte(bytes[j]) {
            j += 1;
        }
        let id_start = j;
        while j < bytes.len() && is_id_byte(bytes[j]) {
            j += 1;
        }
        if j > id_start {
            return Some((at, j));
        }
        i = at + 1;
    }
    None
}

/// Byte span of the first well-formed `\bhash\s*=\s*sha256:<hex>` run in `s`, as
/// `(key_start, hex_start, hex_end)`: `key_start` is the `h` of `hash`, and
/// `hex_start..hex_end` is the hex value as written (mixed case). `\b` is
/// enforced (the byte before `hash` is non-word or the string start), so a
/// `hash` embedded in a longer §4 key such as `rehash` is skipped. Canonical
/// home of the HASH grammar: the read path (`parse_hash`) lowercases
/// `hex_start..hex_end`; the write path (`replace_first_hash` in stamp.rs)
/// splices over `key_start..hex_end`. Both enforce the same `\b`, so they share
/// this with no boundary parameter.
pub(crate) fn find_hash_hex_span(s: &str) -> Option<(usize, usize, usize)> {
    let bytes = s.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = find_sub(&bytes[from..], b"hash") {
        let at = from + rel;
        // \b before `hash`: previous byte must be non-word (or the string start).
        if at != 0 && is_word_byte(bytes[at - 1]) {
            from = at + 1;
            continue;
        }
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
                let hex_start = j + 7;
                let mut k = hex_start;
                while k < bytes.len() && bytes[k].is_ascii_hexdigit() {
                    k += 1;
                }
                if k > hex_start {
                    return Some((at, hex_start, k));
                }
            }
        }
        from = at + 1;
    }
    None
}

/// Parse the block hash from a marker body
/// (`\bhash\s*=\s*sha256:([0-9a-fA-F]+)`), returned lowercase. First match wins.
fn parse_hash(body: &str) -> Option<String> {
    let (_, hex_start, hex_end) = find_hash_hex_span(body)?;
    Some(body[hex_start..hex_end].to_ascii_lowercase())
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
        out.push(Marker { id, hash, raw, syntax: syn, line, malformed });
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

/// Rewrite markers in place, in document order, without disturbing surrounding
/// text. Port of impl/js/src/markers.js `rewriteMarkers`. `transform(marker)`
/// receives a [`Marker`] (`line` is 0 here; position is not tracked) and returns
/// `Some(replacement)`, or `None` to leave the marker unchanged. The write
/// helpers (restamp, repair_duplicates) build on this so marker edits reuse the
/// one canonical grammar instead of re-deriving it.
///
/// The JS reference uses a single combined `HTML|MDX` regex, so matching is one
/// left-to-right pass that consumes each match (a marker delimiter inside an
/// already-matched marker is never a separate match). This reproduces that by
/// merging the two per-syntax scans and only ever taking the next match at or
/// after the previous match's end (HTML wins a start-position tie, which the
/// distinct open delimiters make impossible in practice).
pub fn rewrite_markers<F>(text: &str, mut transform: F) -> String
where
    F: FnMut(&Marker) -> Option<String>,
{
    let html = scan(text, b"<!--", b"-->");
    let mdx = scan(text, b"{/*", b"*/}");
    let mut hi = 0usize;
    let mut mi = 0usize;
    let mut cursor = 0usize;
    let mut last = 0usize;
    let mut out = String::with_capacity(text.len());
    loop {
        while hi < html.len() && html[hi].start < cursor {
            hi += 1;
        }
        while mi < mdx.len() && mdx[mi].start < cursor {
            mi += 1;
        }
        let use_html = match (html.get(hi).map(|m| m.start), mdx.get(mi).map(|m| m.start)) {
            (None, None) => break,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(hs), Some(ms)) => hs <= ms,
        };
        let (chosen, syntax) =
            if use_html { (&html[hi], Syntax::Html) } else { (&mdx[mi], Syntax::Mdx) };
        let start = chosen.start;
        let end = start + chosen.raw.len();
        let id = parse_id(&chosen.body);
        let marker = Marker {
            id: id.clone(),
            hash: parse_hash(&chosen.body),
            raw: chosen.raw.clone(),
            syntax,
            line: 0,
            malformed: id.is_none(),
        };
        out.push_str(&text[last..start]);
        match transform(&marker) {
            Some(repl) => out.push_str(&repl),
            None => out.push_str(&marker.raw),
        }
        last = end;
        cursor = end;
    }
    out.push_str(&text[last..]);
    out
}
