// Document parsing into content blocks with attached markers (SPEC.md §5).
// Port of impl/js/src/parse.js (`parseDocument`, blank-line mode).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::hash::normalize_newlines;
use crate::markers::{find_markers, strip_markers, Marker};
use crate::segment::segment_blank_line;
use crate::text::ascii_trim;

/// A content block with its attached markers (SPEC.md §5).
#[derive(Clone, Debug)]
pub struct Block {
    /// Block body with markers removed and ASCII-trimmed.
    pub content: String,
    /// Markers attached to this block, in document order.
    pub markers: Vec<Marker>,
    /// 1-based line number where the block's chunk starts.
    pub line: usize,
    /// 0-based content-block index; `-1` marks an orphan marker chunk.
    pub index: i64,
}

/// Parse into content blocks with their attached markers, blank-line mode
/// (SPEC.md §5 baseline). A chunk that is only markers attaches to the previous
/// content block; a marker-only chunk with no preceding content block is an
/// orphan (`index == -1`).
///
/// CommonMark mode (§5.2) is deferred from the parser-free core; the mode is not
/// a parameter here so an unknown mode is unrepresentable (rather than a runtime
/// error, as in the JS/Python `mode=` string surface).
pub fn parse_document(md: &str) -> Vec<Block> {
    let text = normalize_newlines(md);
    let chunks = segment_blank_line(&text);

    let mut blocks: Vec<Block> = Vec::new();
    let mut cidx: i64 = 0;
    for (start, chunk) in chunks {
        let markers = find_markers(&chunk, start - 1);
        let stripped = strip_markers(&chunk);
        let content = ascii_trim(&stripped).to_string();
        if content.is_empty() {
            // marker-only chunk: attach to the previous content block if any
            let attach = matches!(blocks.last(), Some(b) if b.index >= 0);
            if attach {
                blocks.last_mut().unwrap().markers.extend(markers);
            } else {
                blocks.push(Block {
                    content: String::new(),
                    markers,
                    line: start,
                    index: -1,
                });
            }
        } else {
            blocks.push(Block {
                content,
                markers,
                line: start,
                index: cidx,
            });
            cidx += 1;
        }
    }
    blocks
}
