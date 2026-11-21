// Fenced code blocks (SPEC.md §3.3, v1.5) and closed inline code spans (§3.5,
// v1.12): text inside either is content, not markup. Port of impl/js/src/code.js
// (`fenceState`, `codeLines`, `stripMarkersOutsideCode`), which ports
// `fence_state` / `code_lines` / `strip_markers_outside_code` from
// impl/py/src/markstay/lint.py, the reference spelling.
//
// The rule is document-level, computed once and threaded, on the
// `blank_frontmatter` precedent (§5.3). It cannot live in segmentation, because
// the blank-line segmenter has no concept of a fence (a maximal run of non-blank
// lines is exactly why §5.2 exists), and it cannot live in `find_markers`, which
// is handed *chunks*: a chunk that begins inside a fence carries no opener, so a
// chunk-local fence scan answers confidently and wrongly. Unlike frontmatter
// blanking this is a mask rather than a removal, because a fence's content must
// still be hashed (§8).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::hash::normalize_newlines;
use crate::markers::{scan_marker_records, strip_markers, Marker, Syntax};

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

/// The code mask for one document (SPEC.md §3.3 and §3.5).
///
/// `fence` holds the 1-based line numbers inside a fenced code block, which is
/// the question every "is line L code" test asks, so [`CodeMask::contains`]
/// answers that half alone. `spans` adds the closed inline code spans §3.5 makes
/// content: 1-based line number to the `[start, end)` columns of each span's
/// content. Only the marker test, [`marker_outside_code`], reads it.
///
/// Columns are **byte** offsets into the LF-normalized line, the unit
/// [`Marker::column`] uses. The Python reference counts code points and the
/// JavaScript one UTF-16 code units, so on a line holding non-ASCII text the
/// three give different numbers for the same span.
///
/// Equality compares both halves. A caller holding only fence lines builds a mask
/// with `CodeMask { fence, ..Default::default() }`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeMask {
    /// 1-based line numbers inside a fenced code block (§3.3).
    pub fence: BTreeSet<usize>,
    /// 1-based line number to the `[start, end)` byte columns of each closed
    /// inline code span's content on that line (§3.5).
    pub spans: BTreeMap<usize, Vec<(usize, usize)>>,
}

impl CodeMask {
    /// Build the mask from fence lines already computed over `text`, so a caller
    /// that needs [`fence_state`]'s writer half does not scan twice.
    pub(crate) fn from_fence(text: &str, fence: BTreeSet<usize>) -> Self {
        let lf = normalize_newlines(text);
        let spans = inline_code_spans(&lf, &fence);
        CodeMask { fence, spans }
    }

    /// Whether 1-based `line` lies inside a fenced code block. The span half is
    /// deliberately not consulted: a span covers columns, never a whole line.
    pub fn contains(&self, line: &usize) -> bool {
        self.fence.contains(line)
    }

    /// Whether the mask hides nothing at all. Counts both halves, because a
    /// document with no fence can still have a span to honour.
    pub fn is_empty(&self) -> bool {
        self.fence.is_empty() && self.spans.is_empty()
    }
}

/// The code mask of `text` (SPEC.md §3.3 and §3.5): the 1-based line numbers
/// inside a fenced code block, and the closed inline code spans. Text in either
/// is content: a marker-shaped string there identifies no block, is hashed with
/// the body (§8), and does not make its block stamped.
pub fn code_lines(text: &str) -> CodeMask {
    let lf = normalize_newlines(text);
    let fence = fence_state(&lf).inside;
    let spans = inline_code_spans(&lf, &fence);
    CodeMask { fence, spans }
}

/// Whether `marker` is a marker rather than content (SPEC.md §3.3, §3.5). A
/// marker is judged by where it *opens*: the line, for a fence, and the opener's
/// column, for a span. The grammar spans newlines, so one match can start
/// outside a fence and run into it. Every path that separates active markers
/// from marker-shaped content asks here, so the reading is one decision rather
/// than a copy of the same expression at each seam.
pub fn marker_outside_code(marker: &Marker, code: &CodeMask) -> bool {
    if code.fence.contains(&marker.line) {
        return false;
    }
    code.spans.get(&marker.line).is_none_or(|spans| {
        !spans.iter().any(|&(start, end)| start <= marker.column && marker.column < end)
    })
}

/// Remove markers from `text`, leaving marker-shaped strings that are content in
/// place: inside a fenced code block (§3.3) or a closed inline code span (§3.5),
/// where §8 hashes them with the body. `code` is the document-level mask and
/// `line_offset` the 0-based line index at which `text` begins in the document it
/// was computed over; `text` begins at a line start, so columns agree too.
pub fn strip_markers_outside_code(text: &str, code: &CodeMask, line_offset: usize) -> String {
    if code.is_empty() {
        return strip_markers(text);
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for record in scan_marker_records(text, line_offset) {
        if record.marker.malformed || !marker_outside_code(&record.marker, code) {
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

// --- closed inline code spans (SPEC.md §3.5, v1.12) -------------------------

/// `(start, end)` ranges in source order, merged where they overlap.
fn merged_ranges(ranges: impl Iterator<Item = (usize, usize)>) -> Vec<(usize, usize)> {
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(previous) if start < previous.1 => previous.1 = previous.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// The merged range holding offset `at`, if any.
fn range_at(ranges: &[(usize, usize)], at: usize) -> Option<(usize, usize)> {
    let index = ranges.partition_point(|&(start, _)| start <= at);
    (index > 0 && at < ranges[index - 1].1).then(|| ranges[index - 1])
}

const RAW_HTML_ATTRIBUTE: &[u8] = b"{=html}";
/// What a link destination, title or reference label must not hold for §3.5 to
/// pass over it: anything that can open a title, nest, escape or reach a span or
/// a tag.
const LINK_UNSAFE: &[u8] = b"`<{([\\\"'";
const RAW_TEXT_TAGS: [&[u8]; 4] = [b"script", b"pre", b"style", b"textarea"];

/// The byte length of raw-text tag name `tag` at the start of `bytes`, matched
/// case-insensitively in ASCII, or `None`. markdown-it-py also folds case by
/// Unicode, under which U+017F (`ſ`, bytes C5 BF) matches `s` and U+0130 (`İ`,
/// C4 B0) and U+0131 (`ı`, C4 B1) match `i`; those are spelled out, so every
/// implementation matches the same strings without a Unicode case table. Only
/// the opener folds this way; the closing tag stays ASCII.
fn folded_tag_name(bytes: &[u8], tag: &[u8]) -> Option<usize> {
    let mut at = 0usize;
    for &want in tag {
        let rest = bytes.get(at..)?;
        let first = *rest.first()?;
        if first.eq_ignore_ascii_case(&want) {
            at += 1;
        } else if (want == b's' && rest.starts_with(&[0xC5, 0xBF]))
            || (want == b'i'
                && (rest.starts_with(&[0xC4, 0xB0]) || rest.starts_with(&[0xC4, 0xB1])))
        {
            at += 2;
        } else {
            return None;
        }
    }
    Some(at)
}

#[inline]
fn is_space_or_tab(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn count_while(bytes: &[u8], pred: impl Fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|&&b| pred(b)).count()
}

/// The end condition of a CommonMark HTML block of types 1 to 5, the ones that
/// run on across blank lines. Every other block runs to a blank line and has no
/// end condition of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HtmlEnd {
    /// Type 1: `</script>`, `</pre>`, `</style>` or `</textarea>`, any of them,
    /// case-insensitive ASCII.
    RawText,
    /// Type 2: `-->`.
    Comment,
    /// Type 3: `?>`.
    Instruction,
    /// Type 4: `>`.
    Declaration,
    /// Type 5: `]]>`.
    Cdata,
}

impl HtmlEnd {
    /// The type 1 to 5 block a line's content (prefixes and leading spaces and
    /// tabs already set aside) opens, or `None` for any other content, a type 6
    /// or 7 opener included.
    fn opened_by(content: &[u8]) -> Option<Self> {
        if content.first() != Some(&b'<') {
            return None;
        }
        let raw_text = RAW_TEXT_TAGS.iter().any(|tag| {
            // Not continued by a tag-name byte. CommonMark wants a space, tab,
            // `>` or line end, and markdown-it-py accepts any Unicode whitespace
            // (a no-break space is the bytes C2 A0), so recognising more than
            // either is the safe side.
            folded_tag_name(&content[1..], tag).is_some_and(|end| {
                !content.get(1 + end).is_some_and(|&b| b.is_ascii_alphanumeric() || b == b'-')
            })
        });
        if raw_text {
            Some(HtmlEnd::RawText)
        } else if content.starts_with(b"<!--") {
            Some(HtmlEnd::Comment)
        } else if content.starts_with(b"<?") {
            Some(HtmlEnd::Instruction)
        } else if content.starts_with(b"<!") && content.get(2).is_some_and(u8::is_ascii_alphabetic)
        {
            Some(HtmlEnd::Declaration)
        } else if content.starts_with(b"<![CDATA[") {
            Some(HtmlEnd::Cdata)
        } else {
            None
        }
    }

    /// Whether `line` holds this block's end condition, searched over the whole
    /// line.
    fn found_in(self, line: &[u8]) -> bool {
        let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
        match self {
            HtmlEnd::Comment => has(b"-->"),
            HtmlEnd::Instruction => has(b"?>"),
            HtmlEnd::Declaration => line.contains(&b'>'),
            HtmlEnd::Cdata => has(b"]]>"),
            HtmlEnd::RawText => (0..line.len()).any(|at| {
                let rest = &line[at..];
                rest.starts_with(b"</")
                    && RAW_TEXT_TAGS.iter().any(|tag| {
                        rest.len() > tag.len() + 2
                            && rest[2..2 + tag.len()].eq_ignore_ascii_case(tag)
                            && rest[2 + tag.len()] == b'>'
                    })
            }),
        }
    }
}

/// A line's content once its container prefixes are set aside: any run of
/// blockquote `>` and list markers (`-`, `+`, `*`, or one to nine digits and `.`
/// or `)`, each followed by a space or tab), then any spaces and tabs. So
/// `> - <div>` reads as starting with `<`.
fn container_content(mut line: &[u8]) -> &[u8] {
    loop {
        let rest = &line[count_while(line, is_space_or_tab)..];
        let taken = match rest.first() {
            Some(b'>') => 1 + usize::from(rest.get(1).is_some_and(|&b| is_space_or_tab(b))),
            Some(b'-' | b'+' | b'*') => match count_while(&rest[1..], is_space_or_tab) {
                0 => break,
                gap => 1 + gap,
            },
            Some(b'0'..=b'9') => {
                let digits = count_while(rest, |b| b.is_ascii_digit());
                if digits > 9 || !matches!(rest.get(digits), Some(b'.' | b')')) {
                    break;
                }
                match count_while(&rest[digits + 1..], is_space_or_tab) {
                    0 => break,
                    gap => digits + 1 + gap,
                }
            }
            _ => break,
        };
        line = &rest[taken..];
    }
    &line[count_while(line, is_space_or_tab)..]
}

/// The closed inline code spans whose content SPEC.md §3.5 makes content, over
/// LF-normalized `lf` with its §3.3 fence lines `fence`.
///
/// A left-to-right scan of each line, over stretches of lines that are neither
/// blank nor inside a fence, since a code span crosses neither. A backtick run
/// opens a span that the next run of the same length on the same line closes,
/// and the scan resumes after the closer. Wherever the scan meets something
/// CommonMark could read before a backtick, it stops for the rest of the
/// stretch, and the spans it already closed stand:
///
/// * a run with no closer on its line, which CommonMark may close from a later
///   line, re-pairing everything after it;
/// * a `<` outside a span and outside an HTML-form marker-shaped string whose
///   body holds none of `<`, `[`, `]`, `{` or `--` (every other marker-shaped
///   string, every MDX-form one included, is read like text), which can open an
///   HTML tag or an autolink, and those outrank a code span by opening first;
/// * a `{` there, which opens an MDX expression, an MDX comment included, whose
///   backticks are no delimiters to an MDX parser;
/// * a `]` there followed by `(`, `[` or `:`, where a link or image destination,
///   a title, a reference label or a link reference definition consumes raw
///   text, backticks included, before any span is looked for, unless the part up
///   to the first `)` or `]` closes before the next run and holds none of
///   [`LINK_UNSAFE`], so an ordinary link is passed over;
/// * a `[` there with no `]` after it before the next run, or whose first `]`
///   follows a nested `[` or a backslash, a link label still open, which markdown-it-py scans ahead in a way that can leave a span
///   unclosed where CommonMark closes it;
/// * a backtick run inside a marker-shaped string (malformed ones included), as
///   an opener, a closer or anything between them, which is a delimiter or not
///   depending on whether a renderer accepts that string as a comment, and on
///   which renderer;
/// * a backtick run directly behind a backslash, which CommonMark escapes;
/// * a line whose content, after any `>` or list-marker prefix, starts with `<`:
///   an HTML block, which has no inline content. Types 1 to 5 run to their end
///   condition, across blank lines, and the rest to a blank line. The scan
///   resumes only after a blank line, even past an end condition, because a block
///   inside a list item ends with the item, and the line holding the end
///   condition is then paragraph text that runs on into the next. For the same
///   reason every type 1 to 5 opener met while a block is open is tracked as
///   well: the container may have ended and a new block begun.
///
/// A span whose content holds a `|` stops the scan as well, because GFM splits a
/// table row's cells before any inline scan, so neither it nor any pairing after
/// it is one a renderer makes. A span followed by `{=html}` is left live, since
/// pandoc's raw-inline syntax declares its content HTML.
///
/// Every syntax character here is ASCII, so the scan runs on bytes; a UTF-8
/// continuation byte never equals one of them.
fn inline_code_spans(lf: &str, fence: &BTreeSet<usize>) -> BTreeMap<usize, Vec<(usize, usize)>> {
    let mut spans: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();
    if !lf.contains('`') {
        return spans;
    }
    // Every marker-shaped string, malformed ones included, merged where they
    // overlap. Offsets are normalized-text bytes because `lf` is normalized.
    let records = scan_marker_records(lf, 0);
    let shaped = merged_ranges(records.iter().map(|r| (r.start, r.end)));
    let shaped_at = |at: usize| range_at(&shaped, at);
    // What `opens_before` may pass over unread: an HTML-form string whose body
    // holds nothing that could open a construct if a renderer declined it as a
    // comment. An MDX-form string is ordinary text to CommonMark, so a `<` or
    // `[` in its quoted value is live there; it is read like any other text, and
    // its own `{` stops the scan. The body is the bytes between the four-byte
    // opener and the last three, malformed records included.
    let skippable = merged_ranges(
        records
            .iter()
            .filter(|r| {
                let body = lf.as_bytes().get(r.start + 4..r.end.saturating_sub(3)).unwrap_or(&[]);
                r.marker.syntax == Syntax::Html
                    && !body.iter().any(|b| b"<[]{".contains(b))
                    && !body.windows(2).any(|w| w == b"--")
            })
            .map(|r| (r.start, r.end)),
    );
    let skippable_at = |at: usize| range_at(&skippable, at);
    // Whether line[left..right] holds, outside every HTML-form marker string it
    // may pass over (see `skippable`), a byte that can start a construct CommonMark or MDX reads before a backtick:
    // `<` (a tag or an autolink), `{` (an MDX expression), a `]` followed by `(`,
    // `[` or `:` (a link or image destination, a reference label, a link
    // reference definition), or a `[` whose label is still open. The byte after
    // a `]` is read from the whole line, past `right` if need be.
    let opens_before = |line: &[u8], start: usize, left: usize, right: usize| -> bool {
        let mut at = left;
        while at < right {
            if let Some((_, end)) = skippable_at(start + at) {
                at = end - start;
                continue;
            }
            let byte = line[at];
            let next = line.get(at + 1).copied();
            if byte == b']' && matches!(next, Some(b'(' | b'[')) {
                // A destination or label holding none of LINK_UNSAFE, closed
                // before the next run, consumes nothing a span could need, so an
                // ordinary link is passed over rather than stopping the scan.
                let closer = if next == Some(b'(') { b')' } else { b']' };
                let from = at + 2;
                let close = (from < right)
                    .then(|| line[from..right].iter().position(|&b| b == closer))
                    .flatten()
                    .map(|n| from + n);
                if let Some(close) = close {
                    if !line[from..close].iter().any(|b| LINK_UNSAFE.contains(b)) {
                        // Past a destination's `)`, but back ON a label's closing
                        // `]`, which may itself open a destination: `[a][b](`)`.
                        at = if closer == b')' { close + 1 } else { close };
                        continue;
                    }
                }
            }
            if byte == b'<'
                || byte == b'{'
                || (byte == b']' && matches!(next, Some(b'(' | b'[' | b':')))
            {
                return true;
            }
            if byte == b'[' {
                // A link label left open past the next run. markdown-it-py scans
                // such a label ahead and can leave a span unclosed that
                // CommonMark closes, so a comment after it stays live there. A
                // label counts as closed only by a `]` with no `[` or backslash
                // before it, since a nested or escaped bracket closes nothing.
                let label = &line[at + 1..right];
                match label.iter().position(|&b| b == b']') {
                    Some(close) if !label[..close].iter().any(|&b| b == b'[' || b == b'\\') => {}
                    _ => return true,
                }
            }
            at += 1;
        }
        false
    };

    let mut scanning = true;
    let mut html_open = false; // inside what may be an HTML block
    let mut pending: Vec<HtmlEnd> = Vec::new(); // type 1 to 5 end conditions still unmet
    let mut offset = 0usize;
    for (index, line) in lf.split('\n').enumerate() {
        let number = index + 1;
        let start = offset;
        offset += line.len() + 1;
        let line = line.as_bytes();
        let blank = line.iter().all(|&b| is_space_or_tab(b));
        // A byte-order mark opening the document is no text to a renderer that
        // strips it, as pandoc does, so it must not hide an HTML block opener.
        let unmarked = if number == 1 {
            line.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(line)
        } else {
            line
        };
        let content = container_content(unmarked);
        let opener = HtmlEnd::opened_by(content);
        if html_open {
            // A container can end under an open block and a new block start
            // after it, so every opener met here is tracked too. The scan
            // resumes only once every end condition seen has been met and a
            // blank line follows.
            if let Some(end) = opener.filter(|end| !end.found_in(line)) {
                pending.push(end);
            }
            pending.retain(|end| !end.found_in(line));
            if blank && pending.is_empty() {
                html_open = false;
                scanning = true;
            }
            continue;
        }
        if blank || fence.contains(&number) {
            scanning = true;
            continue;
        }
        if content.first() == Some(&b'<') {
            html_open = true;
            pending.clear();
            pending.extend(opener.filter(|end| !end.found_in(line)));
            continue;
        }
        if !scanning {
            continue;
        }

        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut at = 0usize;
        while at < line.len() {
            if line[at] == b'`' {
                let width = count_while(&line[at..], |b| b == b'`');
                runs.push((at, at + width));
                at += width;
            } else {
                at += 1;
            }
        }
        let mut found: Vec<(usize, usize)> = Vec::new();
        let mut pos = 0usize;
        let mut i = 0usize;
        let mut stopped = false;
        while i < runs.len() {
            let (left, right) = runs[i];
            if opens_before(line, start, pos, left)
                || shaped_at(start + left).is_some()
                || (left > 0 && line[left - 1] == b'\\')
            {
                stopped = true;
                break;
            }
            let width = right - left;
            let closer = (i + 1..runs.len()).find(|&j| runs[j].1 - runs[j].0 == width);
            // No closer on this line, or a run up to and including the closer
            // sits inside a marker-shaped string. CommonMark pairs the latter,
            // but markdown-it-py does not always: scanning a link label it skips
            // the comment and will not close a span inside it later.
            let Some(j) =
                closer.filter(|&j| (i + 1..=j).all(|k| shaped_at(start + runs[k].0).is_none()))
            else {
                stopped = true;
                break;
            };
            let (close_left, close_right) = runs[j];
            if line[right..close_left].contains(&b'|') {
                // GFM splits a table row's cells before any inline scan, so a
                // pair of runs in different cells is no span, and pairing the
                // runs after it would follow a split this scan cannot see.
                stopped = true;
                break;
            }
            if !line[close_right..].starts_with(RAW_HTML_ATTRIBUTE) {
                found.push((right, close_left));
            }
            pos = close_right;
            i = j + 1;
        }
        if stopped || opens_before(line, start, pos, line.len()) {
            scanning = false;
        }
        if !found.is_empty() {
            spans.insert(number, found);
        }
    }
    spans
}
