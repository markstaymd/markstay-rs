// Blank-line block segmentation (SPEC.md §5 baseline). Port of
// impl/js/src/segment.js (`segmentBlankLine`).
//
// CommonMark-tree segmentation (§5.2) is deferred from the parser-free core (it
// needs a Markdown parser); only the dependency-free blank-line path is here.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use crate::text::is_ascii_blank_line;

/// Space or tab only, the trailing set the frontmatter fences tolerate. Narrower
/// than [`crate::text::is_line_ws`] on purpose: the reference's fence patterns are
/// `^---[ \t]*$` and `^(?:---|\.\.\.)[ \t]*$`, so a form feed after the fence must
/// NOT make it a fence here either.
#[inline]
fn is_space_or_tab(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// `^---[ \t]*$`: the opening fence.
fn is_frontmatter_open(ln: &str) -> bool {
    ln.strip_prefix("---").is_some_and(|rest| rest.chars().all(is_space_or_tab))
}

/// `^(?:---|\.\.\.)[ \t]*$`: the closing fence. `...` is legal YAML and Jekyll
/// accepts it.
fn is_frontmatter_close(ln: &str) -> bool {
    ln.strip_prefix("---")
        .or_else(|| ln.strip_prefix("..."))
        .is_some_and(|rest| rest.chars().all(is_space_or_tab))
}

/// Not an ASCII control character and not a space: the reference's
/// `[^\x00-\x20\x7f]`, which is where the other implementations write `\S`.
///
/// Whitespace is ASCII-pinned here exactly as it is for hashing (§8) and matching
/// (§9), and `char::is_whitespace` must NOT be used: Python, ECMAScript and Rust
/// each define Unicode whitespace differently (U+001C is whitespace to Python
/// only, U+0085 to Python and Rust only, U+00A0 to Python and ECMAScript only,
/// U+FEFF to ECMAScript only). This rule DELETES a span from the document, so a
/// definition that varies by runtime is the one kind of divergence that loses data.
#[inline]
fn is_yaml_token_char(c: char) -> bool {
    c > '\u{20}' && c != '\u{7f}'
}

/// `^[ \t]*(?:-[ \t]+[^\x00-\x20\x7f]|[^\x00-\x20\x7f:#][^:]*:(?:[ \t]|$))`: one
/// payload line that could only be YAML, never Markdown prose , a mapping key or a
/// list item.
///
/// A YAML comment (`# ...`) is deliberately NOT accepted: it is byte-identical to
/// an ATX heading, so accepting it lets `---` / `# Heading` / `---` be read as
/// frontmatter and silently destroys the heading. Comment-only frontmatter
/// therefore is not skipped, which is the safe direction to be wrong in.
fn is_yamlish_line(ln: &str) -> bool {
    let rest = ln.trim_start_matches(is_space_or_tab);
    let mut chars = rest.chars();
    let Some(first) = chars.next() else { return false };

    // `- item`: a dash, at least one space or tab, then a token character.
    if first == '-' {
        let after = chars.as_str();
        let trimmed = after.trim_start_matches(is_space_or_tab);
        if trimmed.len() < after.len() {
            if let Some(c) = trimmed.chars().next() {
                if is_yaml_token_char(c) {
                    return true;
                }
            }
        }
    }

    // `key:` followed by a space, a tab, or end of line. `[^:]*` cannot cross a
    // colon, so only the FIRST colon on the line can close the key.
    if !is_yaml_token_char(first) || first == ':' || first == '#' {
        return false;
    }
    match rest.find(':') {
        Some(i) => rest[i + 1..].chars().next().is_none_or(is_space_or_tab),
        None => false,
    }
}

/// Blank a leading YAML frontmatter block so the segmenter never sees it as
/// content (SPEC.md §5). Port of `_blank_frontmatter` from
/// linter/markstay_lint.py.
///
/// Frontmatter is document metadata, not a block: it carries no prose to identify,
/// and hashing it makes a metadata edit (`status: draft` -> `status: done`) drift a
/// content hash. It is removed *before* segmentation rather than filtered after,
/// because the two segmenters disagree about what it is , the baseline reads the
/// whole fenced span as one block, while CommonMark reads the opening `---` as a
/// thematic break and the closing one as a setext underline, turning the metadata
/// into an H2. Leaving it in puts a document that is inside SPEC.md §5.4's
/// agreement subset outside the set the two segmenters actually agree on.
///
/// Recognition is deliberately conservative, because `---` is also a thematic break
/// and a setext underline, so a loose rule silently eats real content. All four
/// must hold:
///
/// 1. line 1 is exactly `---`;
/// 2. a later line is exactly `---` or `...` (the closing fence). Without one the
///    opener is an ordinary thematic break;
/// 3. the payload between the fences is non-empty and contains no blank line. This
///    is what stops `---` / blank / `Intro.` / blank / `---` (two thematic breaks
///    around a paragraph) from being read as frontmatter that swallows the
///    paragraph;
/// 4. at least one payload line is unambiguously YAML (see [`is_yamlish_line`]).
///
/// Conditions 3 and 4 confine the ambiguity rather than removing it. Any blank-free
/// payload that reads as YAML is *also* ordinary Markdown: `---` / `- Keep this` /
/// `---` is a list between two thematic breaks, `---` / `title: v` / `---` is a
/// setext heading under one. Both satisfy all four conditions and their content *is*
/// excluded. Frontmatter wins, the same call every mainstream site generator makes.
/// A document that fails any of the four conditions falls through to ordinary
/// Markdown, where the worst case is that frontmatter is not skipped (a stray
/// hash-drift warning) rather than content being silently discarded.
///
/// Lines are replaced one-for-one with empty lines, so every line number the caller
/// reports is unchanged. Crate-internal: not part of the public API.
pub(crate) fn blank_frontmatter(text: &str) -> Cow<'_, str> {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.first().is_none_or(|ln| !is_frontmatter_open(ln)) {
        return Cow::Borrowed(text);
    }
    for i in 1..lines.len() {
        if !is_frontmatter_close(lines[i]) {
            continue;
        }
        let payload = &lines[1..i];
        if payload.is_empty() || payload.iter().any(|ln| is_ascii_blank_line(ln)) {
            return Cow::Borrowed(text);
        }
        if !payload.iter().any(|ln| is_yamlish_line(ln)) {
            return Cow::Borrowed(text);
        }
        // (i + 1) empty lines, then the rest of the document verbatim: the same
        // string `"\n".join([""] * (i + 1) + lines[i + 1:])` builds in Python.
        let mut out = String::with_capacity(text.len());
        for _ in 0..i {
            out.push('\n');
        }
        for ln in &lines[i + 1..] {
            out.push('\n');
            out.push_str(ln);
        }
        return Cow::Owned(out);
    }
    Cow::Borrowed(text)
}

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
