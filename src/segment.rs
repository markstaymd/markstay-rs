// Blank-line block segmentation (SPEC.md §5 baseline). Port of
// impl/js/src/segment.js (`segmentBlankLine`).
//
// CommonMark-tree segmentation (§5.2) is deferred from the parser-free core (it
// needs a Markdown parser); only the dependency-free blank-line path is here.

use alloc::string::String;
use alloc::vec::Vec;

use crate::text::is_ascii_blank_line;

/// Split `text` into blocks: a block is a maximal run of non-blank lines bounded
/// by blank lines or the document edges. Returns `(start_line_1based, chunk)`
/// spans in document order. A blank line is empty or only ASCII whitespace
/// (SPEC.md §5).
pub fn segment_blank_line(text: &str) -> Vec<(usize, String)> {
    let mut chunks: Vec<(usize, String)> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut start = 0usize;
    for (idx, ln) in text.split('\n').enumerate() {
        if is_ascii_blank_line(ln) {
            if !cur.is_empty() {
                chunks.push((start, cur.join("\n")));
                cur.clear();
                start = 0;
            }
        } else {
            if cur.is_empty() {
                start = idx + 1;
            }
            cur.push(ln);
        }
    }
    if !cur.is_empty() {
        chunks.push((start, cur.join("\n")));
    }
    chunks
}
