// The write path (SPEC.md §3 / §4 / §6 / §7 / §8): mint ids, serialize markers,
// stamp an unmarked corpus, refresh drifted hashes, and repair duplicate ids.
// Port of impl/js/src/stamp.js (and the Python reference
// impl/py/src/markstay/stamp.py); the three are gated by the shared conformance
// corpus.
//
// String-level and parser-free like the rest of the core (no Markdown parser), so
// it stays dependency-free and `no_std` + `alloc` (the marker surgery below reuses
// the hand-rolled scanner in markers.rs rather than a regex). Every operation is
// idempotent in the obvious sense: stamping an already-stamped document is a
// no-op, restamping an undrifted document is a no-op, and repairing a document
// with no duplicates is a no-op.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::hash::{body_hash, normalize_newlines};
use crate::id::is_id_charset;
use crate::markers::{
    find_markers, find_sub, is_id_byte, is_ws_byte, rewrite_markers, strip_markers, Marker, Syntax,
};
use crate::parse::parse_document;
use crate::segment::segment_blank_line;
use crate::text::ascii_trim;

/// Default truncation for a freshly written hash (§8 permits any prefix). 12 hex =
/// 48 bits, enough to make an accidental same-prefix collision within one document
/// negligible, while staying lighter than the full 64-char digest.
pub const DEFAULT_HASH_LENGTH: usize = 12;

/// Closing delimiter per syntax: a written value must never contain it, or it
/// would terminate the marker early.
fn terminator(syntax: Syntax) -> &'static str {
    match syntax {
        Syntax::Html => "-->",
        Syntax::Mdx => "*/}",
    }
}

/// A marker serialization error (SPEC.md §3 / §4). Mirrors the JS `throw` /
/// Python `raise`: a malformed id, a non-hex hash, a malformed attribute key, a
/// value outside the §4 qchar set, or a value that would close the marker early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// `id` does not match the §6 charset `[A-Za-z0-9_-]+`.
    InvalidId(String),
    /// `hash` is not a non-empty hex string.
    NonHexHash(String),
    /// An attribute key does not match the §4 grammar `[A-Za-z][A-Za-z0-9_-]*`.
    InvalidKey(String),
    /// A value contains a character outside the §4 qchar set (printable ASCII).
    NonQchar(String),
    /// A serialized value contains the syntax's closing delimiter.
    Terminator(Syntax),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minted {
    pub id: String,
    /// 1-based line of the inserted marker.
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renamed {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug)]
pub struct StampResult {
    pub text: String,
    pub minted: Vec<Minted>,
}

#[derive(Clone, Debug)]
pub struct RestampResult {
    pub text: String,
    pub refreshed: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RepairResult {
    pub text: String,
    pub renamed: Vec<Renamed>,
}

/// Options for [`stamp`]. `Default` is `{ html, hash: true, hash_length: 12 }`.
#[derive(Clone, Debug)]
pub struct StampOptions {
    pub syntax: Syntax,
    pub hash: bool,
    pub hash_length: usize,
}

impl Default for StampOptions {
    fn default() -> Self {
        StampOptions {
            syntax: Syntax::Html,
            hash: true,
            hash_length: DEFAULT_HASH_LENGTH,
        }
    }
}

/// Options for [`restamp`]. `hash_length = None` preserves each marker's stored
/// precision; `add_missing` injects a hash into hashless markers.
#[derive(Clone, Debug, Default)]
pub struct RestampOptions {
    pub hash_length: Option<usize>,
    pub add_missing: bool,
}

/// §4 attribute key grammar `^[A-Za-z][A-Za-z0-9_-]*$`.
fn is_valid_key(k: &str) -> bool {
    let mut bytes = k.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() => {}
        _ => return false,
    }
    bytes.all(is_id_byte)
}

/// Serialize one attribute value (SPEC.md §4): a bare token when it is non-empty,
/// has no whitespace or double quote, and is all printable ASCII; otherwise a
/// double-quoted string with `\` and `"` escaped.
///
/// Returns [`FormatError::NonQchar`] if the value contains a character outside the
/// §4 qchar set (printable ASCII 0x20-0x7E): a newline or other control character
/// has no representation and would corrupt the marker.
pub fn format_attr_value(value: &str) -> Result<String, FormatError> {
    // §4 qchar: printable ASCII only.
    if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(FormatError::NonQchar(value.to_string()));
    }
    // Bare token iff non-empty, all printable-non-space ASCII, and no `"`.
    if !value.is_empty()
        && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        && !value.contains('"')
    {
        return Ok(value.to_string());
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// Serialize a marker (SPEC.md §3 / §4).
///
/// * `id`     required, matches the §6 charset `[A-Za-z0-9_-]+`
/// * `hash`   optional hex; emitted as `hash=sha256:<hex>` (folded lowercase)
/// * `attrs`  extra `(key, value)` attributes; keys must satisfy the §4 key
///   grammar (callers namespace extensions with `x-` themselves)
/// * `syntax` `Html` (`<!-- ... -->`) or `Mdx` (`{/* ... */}`)
///
/// Errors if the id/hash/keys are malformed, or if a serialized value would
/// contain the syntax's closing delimiter (which would break the marker).
pub fn format_marker(
    id: &str,
    hash: Option<&str>,
    attrs: &[(&str, &str)],
    syntax: Syntax,
) -> Result<String, FormatError> {
    if !is_id_charset(id) {
        return Err(FormatError::InvalidId(id.to_string()));
    }
    let mut body = String::from("stay:");
    body.push_str(id);
    if let Some(h) = hash {
        if h.is_empty() || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FormatError::NonHexHash(h.to_string()));
        }
        body.push_str(" hash=sha256:");
        body.push_str(&h.to_ascii_lowercase());
    }
    for &(k, v) in attrs {
        if !is_valid_key(k) {
            return Err(FormatError::InvalidKey(k.to_string()));
        }
        body.push(' ');
        body.push_str(k);
        body.push('=');
        body.push_str(&format_attr_value(v)?);
    }
    if body.contains(terminator(syntax)) {
        return Err(FormatError::Terminator(syntax));
    }
    Ok(match syntax {
        Syntax::Mdx => {
            let mut s = String::from("{/* ");
            s.push_str(&body);
            s.push_str(" */}");
            s
        }
        Syntax::Html => {
            let mut s = String::from("<!-- ");
            s.push_str(&body);
            s.push_str(" -->");
            s
        }
    })
}

/// Mint an id from `new_id` that is not already present in `used`; record it.
fn mint_unique(used: &mut BTreeSet<String>, new_id: &mut dyn FnMut() -> String) -> String {
    loop {
        let id = new_id();
        if !used.contains(&id) {
            used.insert(id.clone());
            return id;
        }
    }
}

struct PendingBlock {
    last_line0: usize,
    content: String,
    has_id: bool,
}

/// Stamp every unmarked content block (SPEC.md §5/§6): for each block with no
/// well-formed id, mint one (via `new_id`, deduped against existing ids) and
/// append its marker on a new line directly after the block (the §3.1 trailing
/// form, no blank line, so it binds to that block). Blocks that already carry a
/// well-formed id are left untouched, and block bodies are never modified.
///
/// Returns [`StampResult`] with LF-normalized `text` and `minted` `[{id, line}]`.
pub fn stamp(md: &str, opts: &StampOptions, mut new_id: impl FnMut() -> String) -> StampResult {
    let norm = normalize_newlines(md);

    // Existing ids across the whole document, so a minted id can't collide.
    let mut used: BTreeSet<String> = BTreeSet::new();
    for mk in find_markers(&norm, 0) {
        if !mk.malformed {
            if let Some(id) = mk.id {
                used.insert(id);
            }
        }
    }

    // Walk blank-line chunks, mirroring parse.rs attachment, but keep each content
    // block's last source line so a marker can be inserted right after it.
    let mut needs_stamp: Vec<PendingBlock> = Vec::new();
    let mut current: Option<usize> = None;
    for (start, chunk) in segment_blank_line(&norm) {
        let content = ascii_trim(&strip_markers(&chunk)).to_string();
        let has_id = find_markers(&chunk, 0)
            .iter()
            .any(|mk| mk.id.is_some() && !mk.malformed);
        if !content.is_empty() {
            let n_lines = chunk.split('\n').count();
            needs_stamp.push(PendingBlock {
                last_line0: start + n_lines - 2,
                content,
                has_id,
            });
            current = Some(needs_stamp.len() - 1);
        } else if let Some(idx) = current {
            // marker-only chunk: its id (if any) identifies the preceding block
            if has_id {
                needs_stamp[idx].has_id = true;
            }
        }
    }

    let mut insert_after: BTreeMap<usize, String> = BTreeMap::new();
    let mut minted: Vec<Minted> = Vec::new();
    for blk in &needs_stamp {
        if blk.has_id {
            continue;
        }
        let id = mint_unique(&mut used, &mut new_id);
        let hex = if opts.hash {
            Some(body_hash(&blk.content, Some(opts.hash_length)))
        } else {
            None
        };
        let marker = format_marker(&id, hex.as_deref(), &[], opts.syntax)
            .expect("format_marker: a minted id and computed hash are well-formed");
        insert_after.insert(blk.last_line0, marker);
        minted.push(Minted {
            id,
            line: blk.last_line0 + 1,
        });
    }

    if insert_after.is_empty() {
        return StampResult {
            text: norm,
            minted: Vec::new(),
        };
    }

    let mut out = String::with_capacity(norm.len());
    for (i, line) in norm.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
        if let Some(marker) = insert_after.get(&i) {
            out.push('\n');
            out.push_str(marker);
        }
    }
    StampResult { text: out, minted }
}

/// Refresh hashes that no longer match their block (SPEC.md §8): the deliberate
/// "I edited this block on purpose, accept the new content" operation. For each
/// well-formed marker whose stored `hash` differs from the current body hash (at
/// the stored precision, or `opts.hash_length` if given), rewrite it to the
/// current value. With `opts.add_missing`, markers carrying no hash gain one.
///
/// Returns [`RestampResult`] with LF-normalized `text` and the `refreshed` ids.
pub fn restamp(md: &str, opts: &RestampOptions) -> RestampResult {
    let norm = normalize_newlines(md);

    // id -> the block body it identifies (first occurrence wins; a duplicate id is
    // a separate lint error and is left for repair_duplicates).
    let mut content_by_id: BTreeMap<String, String> = BTreeMap::new();
    for b in parse_document(&norm) {
        if b.index < 0 {
            continue;
        }
        for mk in &b.markers {
            if mk.malformed {
                continue;
            }
            if let Some(id) = &mk.id {
                content_by_id
                    .entry(id.clone())
                    .or_insert_with(|| b.content.clone());
            }
        }
    }

    let mut refreshed: Vec<String> = Vec::new();
    let text = rewrite_markers(&norm, |mk: &Marker| {
        let id = mk.id.as_ref()?;
        let content = content_by_id.get(id)?;
        if let Some(stored) = &mk.hash {
            let len = opts.hash_length.unwrap_or(stored.len());
            let now = body_hash(content, Some(len));
            if &now == stored {
                return None; // unchanged at this precision
            }
            refreshed.push(id.clone());
            Some(replace_first_hash(&mk.raw, &now))
        } else if opts.add_missing {
            let now = body_hash(content, Some(opts.hash_length.unwrap_or(DEFAULT_HASH_LENGTH)));
            refreshed.push(id.clone());
            Some(insert_hash_after_stay(&mk.raw, &now))
        } else {
            None
        }
    });
    RestampResult { text, refreshed }
}

/// Repair duplicate ids (SPEC.md §7: a copy mints a new stay). The first block to
/// carry a duplicated id keeps it; every later marker carrying that id is given a
/// fresh, collision-free id (via `new_id`). A copied block's content is unchanged,
/// so its hash stays valid and is left as-is.
///
/// Returns [`RepairResult`] with LF-normalized `text` and `renamed` `[{from, to}]`.
pub fn repair_duplicates(md: &str, mut new_id: impl FnMut() -> String) -> RepairResult {
    let norm = normalize_newlines(md);
    let blocks = parse_document(&norm);

    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for b in &blocks {
        if b.index < 0 {
            continue;
        }
        for mk in &b.markers {
            if mk.malformed {
                continue;
            }
            if let Some(id) = &mk.id {
                used.insert(id.clone());
                *count.entry(id.clone()).or_insert(0) += 1;
            }
        }
    }
    // A duplicate is any id on more than one marker, so two markers sharing an id
    // on the *same* block (which lint_document also flags) are repaired, not just
    // the copy-across-blocks case.
    let dup: BTreeSet<String> = count
        .into_iter()
        .filter(|(_, c)| *c > 1)
        .map(|(id, _)| id)
        .collect();
    if dup.is_empty() {
        return RepairResult {
            text: norm,
            renamed: Vec::new(),
        };
    }

    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut renamed: Vec<Renamed> = Vec::new();
    let text = rewrite_markers(&norm, |mk: &Marker| {
        let id = mk.id.as_ref()?;
        if !dup.contains(id) {
            return None;
        }
        let c = seen.entry(id.clone()).or_insert(0);
        *c += 1;
        if *c == 1 {
            return None; // first occurrence keeps the id
        }
        let fresh = mint_unique(&mut used, &mut new_id);
        renamed.push(Renamed {
            from: id.clone(),
            to: fresh.clone(),
        });
        Some(replace_stay_id(&mk.raw, &fresh))
    });
    RepairResult { text, renamed }
}

// --- raw-marker string surgery (mirrors the JS `mk.raw.replace(/.../, ...)`) ---
//
// These reproduce the three single-shot regex substitutions the JS/Python write
// helpers run over a marker's raw text. The Rust core has no regex, so each walks
// the bytes using the shared scanner primitives from markers.rs.

/// Byte span `(stay_start, id_end)` of the first `stay:\s*<id>` token in `raw`.
fn find_stay_span(raw: &str) -> Option<(usize, usize)> {
    let bytes = raw.as_bytes();
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

/// Replace the first `hash\s*=\s*sha256:<hex>` run in `raw` with
/// `hash=sha256:{now}` (mirrors `re.sub(..., count=1)`).
fn replace_first_hash(raw: &str, now: &str) -> String {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while let Some(rel) = find_sub(&bytes[i..], b"hash") {
        let at = i + rel;
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
                    let mut out = String::with_capacity(raw.len() + now.len());
                    out.push_str(&raw[..at]);
                    out.push_str("hash=sha256:");
                    out.push_str(now);
                    out.push_str(&raw[k..]);
                    return out;
                }
            }
        }
        i = at + 1;
    }
    raw.to_string()
}

/// Insert ` hash=sha256:{now}` immediately after the first `stay:<id>` token.
fn insert_hash_after_stay(raw: &str, now: &str) -> String {
    match find_stay_span(raw) {
        Some((_, end)) => {
            let mut out = String::with_capacity(raw.len() + now.len() + 16);
            out.push_str(&raw[..end]);
            out.push_str(" hash=sha256:");
            out.push_str(now);
            out.push_str(&raw[end..]);
            out
        }
        None => raw.to_string(),
    }
}

/// Replace the first `stay:\s*<id>` token with `stay:{fresh}` (whitespace between
/// `stay:` and the id is collapsed, matching the JS/Python substitution).
fn replace_stay_id(raw: &str, fresh: &str) -> String {
    match find_stay_span(raw) {
        Some((start, end)) => {
            let mut out = String::with_capacity(raw.len() + fresh.len());
            out.push_str(&raw[..start]);
            out.push_str("stay:");
            out.push_str(fresh);
            out.push_str(&raw[end..]);
            out
        }
        None => raw.to_string(),
    }
}
