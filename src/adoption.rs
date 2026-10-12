// SPEC.md §16 (v1.10): a block stay rewritten in place as a child stay keeps
// its id. Port of impl/js/src/adoption.js (`adoptedIds`), blank-line mode.
//
// The test is lexical, so it needs no §5.5 segmentation. The line shapes are
// hand-rolled over ASCII bytes, the same reading the Python and JS regexes give.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::code::code_lines;
use crate::hash::{normalize_body, normalize_newlines};
use crate::markers::MarkerRecord;
use crate::parse::{document_marker_records, line_starts, strip_record_ranges};
use crate::segment::{blank_frontmatter, segment_blank_line};

fn is_space_tab(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn skip_space_tab(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_space_tab(b[i]) {
        i += 1;
    }
    i
}

/// `^ {0,3}`: the index after up to three leading spaces, or `None` past three.
fn indent_up_to_3(b: &[u8]) -> Option<usize> {
    let n = b.iter().take_while(|&&c| c == b' ').count();
    (n <= 3).then_some(n)
}

/// A list marker at `i`: `-`, `+`, `*`, or one to nine digits then `.` or `)`.
fn list_marker_end(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i) {
        Some(b'-' | b'+' | b'*') => Some(i + 1),
        Some(c) if c.is_ascii_digit() => {
            let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            (n <= 9 && matches!(b.get(i + n), Some(b'.' | b')'))).then_some(i + n + 1)
        }
        _ => None,
    }
}

fn ends_or_space_tab(b: &[u8], i: usize) -> bool {
    b.get(i).is_none_or(|&c| is_space_tab(c))
}

/// One to six `#` at `i`, then a space, a tab or the end of the line.
fn hashes_at(b: &[u8], i: usize) -> bool {
    let n = b[i.min(b.len())..].iter().take_while(|&&c| c == b'#').count();
    (1..=6).contains(&n) && ends_or_space_tab(b, i + n)
}

fn is_atx(line: &str) -> bool {
    let b = line.as_bytes();
    indent_up_to_3(b).is_some_and(|i| hashes_at(b, i))
}

fn is_opener(line: &str) -> bool {
    let b = line.as_bytes();
    indent_up_to_3(b).and_then(|i| list_marker_end(b, i)).is_some_and(|j| ends_or_space_tab(b, j))
}

fn is_thematic(line: &str) -> bool {
    let b = line.as_bytes();
    let Some(i) = indent_up_to_3(b) else { return false };
    let Some(&c) = b.get(i) else { return false };
    if !matches!(c, b'-' | b'*' | b'_') {
        return false;
    }
    b[i..].iter().all(|&x| x == c || is_space_tab(x))
        && b[i..].iter().filter(|&&x| x == c).count() >= 3
}

fn is_lead(line: &str) -> bool {
    let rest = &line.as_bytes()[skip_space_tab(line.as_bytes(), 0)..];
    matches!(rest.first(), Some(b'>' | b'<' | b'|'))
        || rest.starts_with(b"```")
        || rest.starts_with(b"~~~")
}

fn is_empty_item(line: &str) -> bool {
    let b = line.as_bytes();
    list_marker_end(b, skip_space_tab(b, 0)).is_some_and(|j| skip_space_tab(b, j) == b.len())
}

/// After any spaces, tabs and list markers, one to six `#` then a space, a tab
/// or the end of the line.
fn is_heading_led(line: &str) -> bool {
    let b = line.as_bytes();
    let mut i = skip_space_tab(b, 0);
    while let Some(j) = list_marker_end(b, i) {
        if j < b.len() && is_space_tab(b[j]) {
            i = skip_space_tab(b, j);
        } else {
            break;
        }
    }
    i < b.len() && hashes_at(b, i)
}

/// The start of an HTML block that runs on across blank lines (CommonMark
/// types 1 to 5); tag names fold ASCII case only.
fn opens_run_on_html(line: &str) -> bool {
    let rest = &line.as_bytes()[skip_space_tab(line.as_bytes(), 0)..];
    if rest.starts_with(b"<!--") || rest.starts_with(b"<?") || rest.starts_with(b"<![CDATA[") {
        return true;
    }
    if rest.starts_with(b"<!") && rest.get(2).is_some_and(|c| c.is_ascii_alphabetic()) {
        return true;
    }
    if rest.first() != Some(&b'<') {
        return false;
    }
    ["script", "pre", "style", "textarea"].iter().any(|tag| {
        let n = tag.len();
        rest.get(1..=n).is_some_and(|name| name.eq_ignore_ascii_case(tag.as_bytes()))
            && rest.get(n + 1).is_none_or(|&c| is_space_tab(c) || c == b'>')
    })
}

struct Carrier {
    subhash: bool,
    /// Index into the view's blocks.
    block: usize,
    /// The marker's position in its block's marker-stripped source.
    offset: usize,
    eligible: bool,
}

/// A block holding a carrier: its normalized body and marker-stripped source.
struct CarrierBlock {
    body: String,
    stripped: String,
    /// No other content block has the same body, settled once the view is built.
    unique: bool,
}

struct View {
    counts: BTreeMap<String, usize>,
    carriers: BTreeMap<String, Vec<Carrier>>,
    blocks: Vec<CarrierBlock>,
}

/// One document's half of the test: how many markers carry each id, the carrier
/// facts for each marker of `ids` inside a content block, and the blocks those
/// carriers sit in, each with whether no other content block shares its body.
/// Carriers are built for `ids` alone and share their block's stripped source,
/// so the cost stays linear in the document however many markers it holds or
/// adopts.
fn adoption_view(md: &str, ids: &BTreeSet<&str>) -> View {
    let normalized = normalize_newlines(md);
    let text = blank_frontmatter(&normalized).into_owned();
    let code = code_lines(&text);
    let records: Vec<MarkerRecord> = document_marker_records(md, &normalized, &text)
        .into_iter()
        .filter(|r| !code.contains(&r.marker.line) && !r.marker.malformed)
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in &records {
        if let Some(id) = &r.marker.id {
            *counts.entry(id.clone()).or_insert(0) += 1;
        }
    }

    let starts = line_starts(&text);
    // Each line's records, so a strip reads the few markers near it rather than
    // every marker in the document. The line holding an offset is the number of
    // line starts at or before it.
    let line_of = |offset: usize| starts.partition_point(|&s| s <= offset);
    let mut by_line: BTreeMap<usize, Vec<usize>> = BTreeMap::new(); // record indices
    for (k, r) in records.iter().enumerate() {
        for n in line_of(r.start)..=line_of(r.start.max(r.end.saturating_sub(1))) {
            by_line.entry(n).or_default().push(k);
        }
    }
    let empty: Vec<usize> = Vec::new();
    let indices = |n: usize| by_line.get(&n).unwrap_or(&empty);
    let on = |n: usize| indices(n).iter().map(|&k| &records[k]);
    // first line opening a run-on HTML block
    let html_first = text.split('\n').enumerate().find_map(|(i, line)| {
        let n = i + 1;
        (!code.contains(&n) && opens_run_on_html(&strip_record_ranges(line, on(n), starts[i])))
            .then_some(n)
    });

    let mut carriers: BTreeMap<String, Vec<Carrier>> = BTreeMap::new();
    let mut bodies: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocks: Vec<CarrierBlock> = Vec::new();
    for (start, chunk) in segment_blank_line(&text) {
        let chunk_start = starts[start - 1];
        let chunk_end = chunk_start + chunk.len();
        let lines: Vec<&str> = chunk.split('\n').collect();
        let near_idx: BTreeSet<usize> =
            (start..start + lines.len()).flat_map(|n| indices(n).iter().copied()).collect();
        let near: Vec<&MarkerRecord> = near_idx.iter().map(|&k| &records[k]).collect();
        let stripped = strip_record_ranges(&chunk, near.iter().copied(), chunk_start);
        if stripped.bytes().all(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)) {
            continue; // a marker-only chunk is not a block
        }
        let body = normalize_body(&stripped);
        *bodies.entry(body.clone()).or_insert(0) += 1;
        let inside: Vec<&MarkerRecord> = near
            .iter()
            .copied()
            .filter(|r| chunk_start <= r.start && r.start < chunk_end)
            .collect();
        let wanted: Vec<&MarkerRecord> = inside
            .iter()
            .copied()
            .filter(|r| r.marker.id.as_deref().is_some_and(|id| ids.contains(id)))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let clean: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| strip_record_ranges(line, on(start + i), starts[start - 1 + i]))
            .collect();
        let blank = |s: &str| s.bytes().all(is_space_tab);
        // §5's ASCII whitespace, for the lines skipped after a leading heading.
        let ascii_blank = |s: &str| s.bytes().all(|c| matches!(c, b' ' | b'\t' | 0x0c | 0x0b));
        let single = !inside.iter().any(|r| text[r.start..r.end].contains('\n'));
        let mut opening = 0;
        if is_atx(lines[0]) {
            opening = 1;
            while opening < lines.len() && ascii_blank(&clean[opening]) {
                opening += 1;
            }
        }
        let opens =
            opening < lines.len() && is_opener(lines[opening]) && !is_thematic(lines[opening]);
        // The first row past the opening line each line test refuses: a marker
        // is eligible only above both.
        let led = (opening + 1..lines.len())
            .find(|&i| is_thematic(lines[i]) || is_lead(lines[i]))
            .unwrap_or(lines.len());
        let bare = (opening..lines.len())
            .find(|&i| is_empty_item(&clean[i]) || is_heading_led(&clean[i]))
            .unwrap_or(lines.len());
        // Where each marker lands in `stripped`: its offset in the chunk less the
        // marker bytes removed before it.
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for r in &near {
            let (left, right) = (chunk_start.max(r.start), chunk_end.min(r.end));
            if left >= right {
                continue;
            }
            match spans.last_mut() {
                Some(last) if left < last.1 => last.1 = last.1.max(right),
                _ => spans.push((left, right)),
            }
        }
        let mut removed = Vec::with_capacity(spans.len() + 1);
        removed.push(0usize);
        for (left, right) in &spans {
            removed.push(removed[removed.len() - 1] + right - left);
        }
        blocks.push(CarrierBlock { body, stripped, unique: false });
        for r in &wanted {
            let Some(mid) = &r.marker.id else { continue };
            let row = line_of(r.start) - start; // 0-based in chunk
            let k = spans.partition_point(|&(left, _)| left < r.start);
            let gone = removed[k] - if k > 0 { spans[k - 1].1.saturating_sub(r.start) } else { 0 };
            let eligible = single
                && opens
                && opening <= row
                && row < led.min(bare)
                && !blank(&clean[row])
                && html_first.is_none_or(|n| n >= start + row);
            carriers.entry(mid.clone()).or_default().push(Carrier {
                subhash: r.marker.has_subhash,
                block: blocks.len() - 1,
                offset: r.start - chunk_start - gone,
                eligible,
            });
        }
    }
    for block in &mut blocks {
        block.unique = bodies.get(&block.body) == Some(&1);
    }
    View { counts, carriers, blocks }
}

/// The ids among `ids` whose block stay was rewritten in place as a child stay
/// (SPEC.md §16, v1.10): one plain marker before and one `subhash` marker after,
/// in the same place in an unchanged list block that no other block duplicates.
pub fn adopted_ids(before_md: &str, after_md: &str, ids: &[String]) -> BTreeSet<String> {
    let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let b = adoption_view(before_md, &wanted);
    let a = adoption_view(after_md, &wanted);
    let mut same: BTreeMap<(usize, usize), bool> = BTreeMap::new(); // compared once each
    let mut out = BTreeSet::new();
    for mid in ids {
        if b.counts.get(mid) != Some(&1) || a.counts.get(mid) != Some(&1) {
            continue;
        }
        let (Some([x]), Some([y])) =
            (b.carriers.get(mid).map(Vec::as_slice), a.carriers.get(mid).map(Vec::as_slice))
        else {
            continue;
        };
        let unchanged = *same
            .entry((x.block, y.block))
            .or_insert_with(|| b.blocks[x.block].stripped == a.blocks[y.block].stripped);
        if !x.subhash
            && y.subhash
            && x.eligible
            && y.eligible
            && unchanged
            && x.offset == y.offset
            && b.blocks[x.block].unique
            && a.blocks[y.block].unique
        {
            out.insert(mid.clone());
        }
    }
    out
}
