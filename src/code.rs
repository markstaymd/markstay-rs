// Fenced code blocks (SPEC.md §3.3, v1.5): text inside a fence is content, not
// markup. Port of impl/js/src/code.js (`fenceState`, `codeLines`,
// `stripMarkersOutsideCode`), which ports `fence_state` / `code_lines` /
// `strip_markers_outside_code` from impl/py/src/markstay/lint.py, the reference
// spelling.
//
// The rule is document-level, computed once and threaded, on the
// `blank_frontmatter` precedent (§5.3). It cannot live in segmentation, because
// the blank-line segmenter has no concept of a fence (a maximal run of non-blank
// lines is exactly why §5.2 exists), and it cannot live in `find_markers`, which
// is handed *chunks*: a chunk that begins inside a fence carries no opener, so a
// chunk-local fence scan answers confidently and wrongly. Unlike frontmatter
// blanking this is a mask rather than a removal, because a fence's content must
// still be hashed (§8).

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use crate::hash::normalize_newlines;
use crate::markers::{scan_marker_records, strip_markers};

/// Fence geometry for one document (SPEC.md §3.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FenceState {
    /// 1-based line numbers that lie inside a fenced code block.
    pub inside: BTreeSet<usize>,
    /// 1-based line numbers a fence is still open *after*.
    pub open_after: BTreeSet<usize>,
}

/// A fence run at the start of a line: `^ {0,3}(`{3,}|~{3,})`, returning the run
/// character, its length, and the rest of the line (the info string for an
/// opener, which must be empty-or-whitespace for a closer).
fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let rest = line
        .strip_prefix("   ")
        .or_else(|| line.strip_prefix("  "))
        .or_else(|| line.strip_prefix(' '))
        .unwrap_or(line);
    let bytes = rest.as_bytes();
    let ch = *bytes.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let run = bytes.iter().take_while(|&&b| b == ch).count();
    if run < 3 {
        return None;
    }
    Some((ch, run, &rest[run..]))
}

/// Fence geometry for one document (SPEC.md §3.3, v1.5): the 1-based line numbers
/// that lie inside a fenced code block, and the 1-based line numbers a fence is
/// still open *after*.
///
/// The second set is what the write path needs and it is not derivable from the
/// first: a marker appended after line L lands on a new line inside the fence
/// exactly when a fence is open at the end of L, and an unclosed fence runs to the
/// end of the document, where there is no later line to test.
///
/// Recognition is line-based and deliberately narrow, so both segmenters (§5) and
/// every tool agree on it without a block parser:
///
/// * the scan runs on LF-split lines, so a CRLF document and its LF twin give the
///   same answer (§8);
/// * an opening fence has at most three leading **spaces** and then three or more
///   backticks or tildes. A tab is not one of the three: CommonMark expands it to
///   the next four-column stop, which needs a column model this rule deliberately
///   does not have. A backtick fence's info string may not contain a backtick;
/// * it closes at the first later line with at most three leading spaces that is a
///   run of the **same** character, **at least as long** as the opener, followed by
///   nothing but spaces and tabs. A longer opener is what lets a fence contain a
///   shorter one, and the whitespace set is named rather than left to "whitespace"
///   because three implementations picking three sets is the way this rule fails
///   quietly;
/// * an unclosed fence runs to the end of the document.
///
/// The fence lines themselves are inside the block, deliberately rather than as an
/// edge case: a marker-shaped string can sit in an opening fence's info string,
/// where before §3.3 it was read as a marker and bound to whatever block preceded
/// it.
pub fn fence_state(text: &str) -> FenceState {
    let lf = normalize_newlines(text);
    let mut state = FenceState::default();
    let mut fence: Option<(u8, usize)> = None;
    for (idx, line) in lf.split('\n').enumerate() {
        let num = idx + 1;
        match fence {
            None => {
                let Some((ch, run, info)) = fence_run(line) else { continue };
                if ch == b'`' && info.contains('`') {
                    continue;
                }
                fence = Some((ch, run));
                state.inside.insert(num);
                state.open_after.insert(num);
            }
            Some((open_ch, open_run)) => {
                state.inside.insert(num);
                let closes = match fence_run(line) {
                    Some((ch, run, rest)) => {
                        ch == open_ch
                            && run >= open_run
                            && rest.chars().all(|c| c == ' ' || c == '\t')
                    }
                    None => false,
                };
                if closes {
                    fence = None;
                } else {
                    state.open_after.insert(num);
                }
            }
        }
    }
    state
}

/// The 1-based line numbers inside a fenced code block (SPEC.md §3.3). Text there
/// is content: a marker-shaped string on one of these lines identifies no block,
/// is hashed with the body (§8), and does not make its block stamped.
pub fn code_lines(text: &str) -> BTreeSet<usize> {
    fence_state(text).inside
}

/// Remove markers from `text`, leaving marker-shaped strings inside a fenced code
/// block in place (SPEC.md §3.3: they are content, and §8 hashes them with the
/// body). `code` is the document-level mask and `line_offset` the 0-based line
/// index at which `text` begins in the document it was computed over.
///
/// A marker is judged by the line it *opens* on, which is the only line a reader
/// can see it start on; the grammar spans newlines, so one match can cross lines.
pub fn strip_markers_outside_code(
    text: &str,
    code: &BTreeSet<usize>,
    line_offset: usize,
) -> String {
    if code.is_empty() {
        return strip_markers(text);
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for record in scan_marker_records(text, line_offset) {
        if record.marker.malformed || code.contains(&record.marker.line) {
            continue;
        }
        if let Some(previous) = ranges.last_mut() {
            if record.start < previous.1 {
                previous.1 = previous.1.max(record.end);
                continue;
            }
        }
        ranges.push((record.start, record.end));
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for (start, end) in ranges {
        out.push_str(&text[last..start]);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}
