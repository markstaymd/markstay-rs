// Shared ASCII whitespace + ASCII case-fold helpers. Port of impl/js/src/text.js.
//
// SPEC.md pins hash normalization (§8), block segmentation (§5), and §9 matching
// normalization to ASCII whitespace and ASCII-only case folding, so a second
// implementation reproduces hashing and recovery exactly without a Unicode
// whitespace set or case-fold table (SPEC_DECISIONS.md). One definition here,
// used by hash/segment/parse/quote, mirrors the Python reference's
// `rstrip(" \t\f\v")`, `strip(" \t\f\v")`, and the §9 `normalize` ASCII rules.

use alloc::string::{String, ToString};

/// ASCII whitespace *within a line* (no newlines): space, tab, form feed,
/// vertical tab. Mirrors Python `rstrip(" \t\f\v")` / JS `[ \t\f\v]`.
#[inline]
pub fn is_line_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{0c}' | '\u{0b}')
}

/// ASCII whitespace including newlines: the §9 trim/collapse set
/// (`[ \t\n\r\f\v]`), matching Python `strip(" \t\n\r\f\v")` / JS.
#[inline]
pub fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0c}' | '\u{0b}')
}

/// A line that is empty or only ASCII line-whitespace (SPEC.md §5 blank line;
/// matches Python `ln.strip(" \t\f\v") == ""`).
#[inline]
pub fn is_ascii_blank_line(ln: &str) -> bool {
    ln.chars().all(is_line_ws)
}

/// Strip trailing ASCII line-whitespace from a single line (Python
/// `ln.rstrip(" \t\f\v")`). NBSP and other non-ASCII whitespace are preserved.
#[inline]
pub fn rstrip_line_ws(s: &str) -> &str {
    s.trim_end_matches(is_line_ws)
}

/// Strip leading and trailing ASCII whitespace (incl. newlines), mirroring
/// JS `asciiTrim` / Python `strip(" \t\n\r\f\v")`. Borrows the input.
#[inline]
pub fn ascii_trim(s: &str) -> &str {
    s.trim_matches(is_ws)
}

/// Collapse runs of ASCII whitespace (incl. newlines) to a single space
/// (JS `asciiCollapse`; Python `re.sub(r"[ \t\n\r\f\v]+", " ", s)`).
pub fn ascii_collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if is_ws(c) {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// Lowercase ASCII letters only, leaving non-ASCII unchanged (JS `asciiLower`;
/// Python ASCII fold). `str::to_ascii_lowercase` is exactly this.
#[inline]
pub fn ascii_lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Owned-string variant of [`ascii_trim`], for the public API surface that
/// mirrors JS `asciiTrim` (which returns a string).
#[inline]
pub fn ascii_trim_owned(s: &str) -> String {
    ascii_trim(s).to_string()
}
