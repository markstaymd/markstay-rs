// Document parsing into content blocks with lexical marker tokens (SPEC.md §5).
// Port of impl/js/src/parse.js (`parseDocument`, blank-line mode).

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::code::code_lines;
use crate::hash::normalize_newlines;
use crate::markers::{scan_marker_records, Marker, MarkerRecord};
use crate::segment::{blank_frontmatter, segment_blank_line};
use crate::text::ascii_trim;

/// A content block with its lexical marker tokens (SPEC.md §5 / §16).
#[derive(Clone, Debug)]
pub struct Block {
    /// Block body with markers removed and ASCII-trimmed.
    pub content: String,
    /// Lexical markers in this block. `subhash` markers do not bind to this block.
    pub markers: Vec<Marker>,
    /// 1-based line number where the block's chunk starts.
    pub line: usize,
    /// 0-based content-block index; `-1` marks an orphan marker chunk.
    pub index: i64,
}

fn same_marker_facts(left: &Marker, right: &Marker) -> bool {
    left.line == right.line
        && left.syntax == right.syntax
        && left.id == right.id
        && left.hash == right.hash
        && left.subhash == right.subhash
        && left.has_subhash == right.has_subhash
        && left.malformed == right.malformed
}

fn document_marker_records(md: &str, normalized: &str, text: &str) -> Vec<MarkerRecord> {
    let frontmatter_lines: BTreeSet<usize> = normalized
        .split('\n')
        .zip(text.split('\n'))
        .enumerate()
        .filter_map(|(index, (source, blanked))| (source != blanked).then_some(index + 1))
        .collect();
    let raw_records: Vec<MarkerRecord> = scan_marker_records(md, 0)
        .into_iter()
        .filter(|record| !frontmatter_lines.contains(&record.marker.line))
        .collect();
    let mut normalized_records = scan_marker_records(text, 0);
    let same = raw_records.len() == normalized_records.len()
        && raw_records
            .iter()
            .zip(&normalized_records)
            .all(|(left, right)| same_marker_facts(&left.marker, &right.marker));
    if same {
        for (normalized_record, raw_record) in normalized_records.iter_mut().zip(raw_records) {
            normalized_record.marker.raw = raw_record.marker.raw;
        }
    }
    normalized_records
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    starts.push(0);
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn strip_record_ranges(text: &str, records: &[MarkerRecord], source_start: usize) -> String {
    let source_end = source_start + text.len();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for record in records {
        let left = source_start.max(record.start);
        let right = source_end.min(record.end);
        if left >= right {
            continue;
        }
        if let Some(previous) = spans.last_mut() {
            if left < previous.1 {
                previous.1 = previous.1.max(right);
                continue;
            }
        }
        spans.push((left, right));
    }
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut previous = source_start;
    for (start, end) in spans {
        out.push_str(&text[previous - source_start..start - source_start]);
        previous = end;
    }
    out.push_str(&text[previous - source_start..]);
    out
}

/// Parse into content blocks with their lexical marker tokens, blank-line mode
/// (SPEC.md §5 baseline). A leading YAML frontmatter block is metadata rather than
/// content and is skipped before segmentation (see `segment::blank_frontmatter`).
/// A chunk that is only markers attaches to the previous content block; a
/// marker-only chunk with no preceding content block is an orphan (`index == -1`).
///
/// CommonMark mode (§5.2) is deferred from the parser-free core; the mode is not
/// a parameter here so an unknown mode is unrepresentable (rather than a runtime
/// error, as in the JS/Python `mode=` string surface).
pub fn parse_document(md: &str) -> Vec<Block> {
    let normalized = normalize_newlines(md);
    let text = blank_frontmatter(&normalized).into_owned();
    // SPEC.md §3.3: text inside a fenced code block is content. The rule is
    // computed once over the whole document and threaded, on the
    // `blank_frontmatter` precedent, because neither segmenter has a concept of a
    // fence and `find_markers` is handed chunks. Blanking preserves line numbers,
    // so this mask indexes the caller's text too.
    let code = code_lines(&text);
    let chunks = segment_blank_line(&text);
    let document_records = document_marker_records(md, &normalized, &text);
    let active_records: Vec<MarkerRecord> =
        document_records.into_iter().filter(|record| !code.contains(&record.marker.line)).collect();
    let removable_records: Vec<MarkerRecord> =
        active_records.iter().filter(|record| !record.marker.malformed).cloned().collect();
    let starts = line_starts(&text);

    let mut blocks: Vec<Block> = Vec::new();
    let mut cidx: i64 = 0;
    for (start, chunk) in chunks {
        let chunk_start = starts[start - 1];
        let chunk_end = chunk_start + chunk.len();
        // §3.3: a marker-shaped string in a fence is content, not a marker.
        let markers: Vec<Marker> = active_records
            .iter()
            .filter(|record| chunk_start <= record.start && record.start < chunk_end)
            .map(|record| record.marker.clone())
            .collect();
        let stripped = strip_record_ranges(&chunk, &removable_records, chunk_start);
        let content = ascii_trim(&stripped).to_string();
        if content.is_empty() {
            // marker-only chunk: attach to the previous content block if any
            let attach = matches!(blocks.last(), Some(b) if b.index >= 0);
            if attach {
                blocks.last_mut().unwrap().markers.extend(markers);
            } else {
                blocks.push(Block { content: String::new(), markers, line: start, index: -1 });
            }
        } else {
            blocks.push(Block { content, markers, line: start, index: cidx });
            cidx += 1;
        }
    }
    blocks
}
