// Hash normalization and body hashing (SPEC.md §8). Port of impl/js/src/hash.js
// (`normalizeBody`, `bodyHash`), which ports `normalize_body` / `body_hash` from
// the Python reference.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sha256::sha256_hex;
use crate::text::rstrip_line_ws;

/// Replace CRLF and lone CR with LF (SPEC.md §8 step 1). Mirrors JS
/// `.replace(/\r\n/g,"\n").replace(/\r/g,"\n")`.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Normalize a block body for hashing (SPEC.md §8), in order:
///
/// 1. line endings CRLF / lone CR -> LF
/// 2. strip per-line trailing ASCII whitespace
/// 3. drop leading and trailing blank lines
///
/// Markers are removed upstream before this runs. The trailing-whitespace set is
/// ASCII (matching Python `rstrip(" \t\f\v")`), so the SHA-256 agrees across
/// implementations without a Unicode whitespace table.
pub fn normalize_body(text: &str) -> String {
    let lf = normalize_newlines(text);
    let lines: Vec<&str> = lf.split('\n').map(rstrip_line_ws).collect();
    let mut start = 0;
    while start < lines.len() && lines[start].is_empty() {
        start += 1;
    }
    let mut end = lines.len();
    while end > start && lines[end - 1].is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

/// SHA-256 of the UTF-8 encoding of the normalized body, lowercase hex.
/// Optionally truncated to `length` hex chars (prefix), matching SPEC.md §8
/// truncation. `None` or `Some(0)` returns the full 64-char digest.
pub fn body_hash(text: &str, length: Option<usize>) -> String {
    let h = sha256_hex(normalize_body(text).as_bytes());
    match length {
        Some(n) if n > 0 => h.chars().take(n).collect(),
        _ => h,
    }
}
