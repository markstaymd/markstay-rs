// Well-formedness, intra-document checks, and the regeneration diff
// (SPEC.md §7 / §8 / §10 / §11). Port of impl/js/src/lint.js (`lintBlocks`,
// `lintDocument`, `lintDiff`, `lintDiffBlocks`, `sortFindings`, `hasErrors`).
//
// Finding emission order is detection order; `sort_findings` produces the
// canonical (level, line, code) order. Where JS uses an insertion-ordered `Map`
// for the id index, this port keeps an insertion-ordered `Vec` so same-(level,
// code,line) findings retain document order after the stable sort, matching the
// other runners on the `diff` corpus.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::adoption::adopted_ids;
use crate::hash::body_hash;
use crate::parse::{parse_document, Block};
use crate::quote::{body_score_normalized, normalize};

/// SPEC.md §11 (v1.11): a kept id has moved off its block when its current
/// block scores under `RELOCATED_BELOW` against its old text and exactly one
/// block in the run of free blocks directly above it scores `RELOCATED_AT` or
/// more against that text. A block is free when it carries no block stay id
/// present in both documents.
pub const RELOCATED_BELOW: f64 = 0.5;
/// See [`RELOCATED_BELOW`].
pub const RELOCATED_AT: f64 = 0.8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
        }
    }
    fn rank(&self) -> u8 {
        match self {
            Level::Error => 0,
            Level::Warn => 1,
            Level::Info => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub level: Level,
    pub code: &'static str,
    pub message: String,
    pub id: Option<String>,
    pub line: Option<usize>,
}

fn finding(
    level: Level,
    code: &'static str,
    message: String,
    id: Option<String>,
    line: Option<usize>,
) -> Finding {
    Finding { level, code, message, id, line }
}

/// Well-formedness and intra-document invariants over a pre-segmented block list
/// (SPEC.md §7 / §8 / §10). The single source of the finding logic: the
/// blank-line front end (`lint_document`) and any tree front end feed their
/// blocks here and get identical findings on segmentations that agree. Returns
/// findings in detection order (use [`sort_findings`] for canonical ordering).
pub fn lint_blocks(blocks: &[Block]) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    // id -> first line seen (insertion order is irrelevant: lookup only).
    let mut seen: Vec<(String, usize)> = Vec::new();

    for b in blocks {
        let orphan = b.index == -1;
        for mk in &b.markers {
            if mk.malformed {
                findings.push(finding(
                    Level::Error,
                    "MALFORMED_MARKER",
                    format!("marker has no parseable id: {:?}", mk.raw),
                    None,
                    Some(mk.line),
                ));
                continue;
            }
            let id = mk.id.clone().unwrap_or_default();
            // Duplicate ids remain a document-global lexical invariant even
            // when §16 excludes a child marker from container attribution.
            if let Some((_, first)) = seen.iter().find(|(k, _)| *k == id) {
                findings.push(finding(
                    Level::Error,
                    "DUPLICATE_ID",
                    format!("id {} appears more than once (first at line {})", id, first),
                    Some(id.clone()),
                    Some(mk.line),
                ));
            } else {
                seen.push((id.clone(), mk.line));
            }
            if orphan {
                findings.push(finding(
                    Level::Error,
                    "ORPHAN_MARKER",
                    format!("marker {} has no preceding block to attach to", id),
                    Some(id.clone()),
                    Some(mk.line),
                ));
                continue;
            }
            if !mk.is_block_stay() {
                continue;
            }
            if let Some(h) = &mk.hash {
                if !b.content.is_empty() {
                    let now = body_hash(&b.content, Some(h.len()));
                    if now != *h {
                        findings.push(finding(
                            Level::Warn,
                            "HASH_DRIFT",
                            format!(
                                "id {}: stored sha256:{} != current sha256:{} \
                                 (content edited since the hash was written)",
                                id, h, now
                            ),
                            Some(id.clone()),
                            Some(mk.line),
                        ));
                    }
                }
            }
        }
    }
    findings
}

/// Well-formedness and intra-document invariants for a single document. Returns
/// `(blocks, findings)` with findings in detection order.
pub fn lint_document(md: &str) -> (Vec<Block>, Vec<Finding>) {
    let blocks = parse_document(md);
    let findings = lint_blocks(&blocks);
    (blocks, findings)
}

/// Insertion-ordered id index: id -> content blocks carrying that id, in
/// document order. Only content blocks (`index >= 0`) with well-formed markers.
fn id_index<'a>(blocks: &'a [Block]) -> Vec<(String, Vec<&'a Block>)> {
    let mut out: Vec<(String, Vec<&'a Block>)> = Vec::new();
    for b in blocks {
        if b.index < 0 {
            continue;
        }
        for mk in &b.markers {
            if !mk.is_block_stay() {
                continue;
            }
            if let Some(id) = &mk.id {
                if let Some(entry) = out.iter_mut().find(|(k, _)| k == id) {
                    entry.1.push(b);
                } else {
                    out.push((id.clone(), alloc::vec![b]));
                }
            }
        }
    }
    out
}

/// Every well-formed marker on a content block as `(id, has_subhash)`, in
/// document order. Lexical rather than attached-stay semantics: it answers
/// "does any marker still carry this id", which is the §16 (v1.9) drop check for
/// a child stay and needs no §5.5 or §5.6 segmentation. Orphan chunks are
/// included: an orphan marker still carries its id, and the single-document
/// lint reports the position as ORPHAN_MARKER.
pub(crate) fn stay_marker_ids(blocks: &[Block]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for b in blocks {
        for mk in &b.markers {
            if mk.malformed {
                continue;
            }
            if let Some(id) = &mk.id {
                out.push((id.clone(), mk.has_subhash));
            }
        }
    }
    out
}

/// The after-edit content blocks, each marked free or owned. A block carrying
/// a kept id is that id's, so it is never another id's surviving text, and it
/// ends the window above any block below it. Every block stay id on an
/// after-edit content block is in the after index, so "present in both"
/// reduces to "present in the baseline". Bodies are hashed and normalized on
/// first use.
struct Window<'a> {
    blocks: Vec<&'a Block>,
    at: BTreeMap<usize, usize>, // block address -> position in `blocks`
    owned: Vec<bool>,
    before_hashes: BTreeMap<String, usize>, // baseline content blocks per body hash
    hashes: Vec<Option<String>>,
    norms: Vec<Option<String>>,
}

impl<'a> Window<'a> {
    fn new(
        before_idx: &[(String, Vec<&Block>)],
        before_blocks: &[Block],
        after_blocks: &'a [Block],
    ) -> Self {
        let baseline: BTreeSet<&str> = before_idx.iter().map(|(k, _)| k.as_str()).collect();
        let blocks: Vec<&Block> = after_blocks.iter().filter(|b| b.index >= 0).collect();
        let at = blocks.iter().enumerate().map(|(i, b)| (*b as *const Block as usize, i)).collect();
        let owned = blocks
            .iter()
            .map(|b| {
                b.markers.iter().any(|mk| {
                    mk.is_block_stay() && mk.id.as_deref().is_some_and(|id| baseline.contains(id))
                })
            })
            .collect();
        let mut before_hashes: BTreeMap<String, usize> = BTreeMap::new();
        for b in before_blocks.iter().filter(|b| b.index >= 0 && !b.content.is_empty()) {
            *before_hashes.entry(body_hash(&b.content, None)).or_insert(0) += 1;
        }
        let hashes = alloc::vec![None; blocks.len()];
        let norms = alloc::vec![None; blocks.len()];
        Window { blocks, at, owned, before_hashes, hashes, norms }
    }

    /// Counts the candidates directly above `block` whose normalized body
    /// satisfies `hit`, stopping once the count passes one. A candidate is a
    /// free block between `block` and the nearest block above carrying a kept
    /// id, unless it is unchanged from a baseline block other than the one
    /// `old` names: that block was there before the edit, so the edit did not
    /// strand it.
    fn hits_above(&mut self, block: &Block, old: &str, hit: impl Fn(&str) -> bool) -> usize {
        let own = body_hash(old, None);
        let mut i = self.at[&(block as *const Block as usize)];
        let mut hits = 0;
        while i > 0 && !self.owned[i - 1] {
            i -= 1;
            if self.blocks[i].content.is_empty() {
                continue;
            }
            let h = self.hashes[i].get_or_insert_with(|| body_hash(&self.blocks[i].content, None));
            let seen = self.before_hashes.get(h.as_str()).copied().unwrap_or(0);
            if seen > usize::from(*h == own) {
                continue;
            }
            let body = self.norms[i].get_or_insert_with(|| normalize(&self.blocks[i].content));
            if hit(body) {
                hits += 1;
                if hits > 1 {
                    break;
                }
            }
        }
        hits
    }
}

/// SPEC.md §11's relocation test for a kept id whose block changed: (1) its
/// current block scores under [`RELOCATED_BELOW`] against the old text, by §9's
/// body score without context; (2) exactly one candidate in the run of free
/// blocks directly above the current block scores [`RELOCATED_AT`] or more
/// against the old text, where a block unchanged from another baseline block is
/// not a candidate; (3) the current block's normalized text is not inside the
/// old text, so typing a blank line into a paragraph (the marker stays on a
/// short tail) is drift, not relocation. The window is the shape the rule names, a block
/// inserted between another block and its trailing marker. Each free block sits
/// in the run above at most one block, so the scan costs at most the free
/// blocks times the most kept ids any one block carries: linear while ids sit
/// one to a block, and quadratic when an edit piles every marker onto one block.
/// `window` yields the window, built on first use.
fn moved_off<'w, 'a: 'w>(
    old: &str,
    block: &Block,
    window: impl FnOnce() -> &'w mut Window<'a>,
) -> bool {
    let q = normalize(old);
    let c = normalize(&block.content);
    if body_score_normalized(&q, &c) >= RELOCATED_BELOW || q.contains(c.as_str()) {
        return false;
    }
    window().hits_above(block, old, |f| body_score_normalized(&q, f) >= RELOCATED_AT) == 1
}

/// Regeneration diff (SPEC.md §11): what an edit did to the ids. Catches the
/// AI-rewrite failure mode (dropped markers) plus duplication and relocation:
/// an exact swap, or an id moved off its block (v1.11). Returns findings in
/// detection order.
pub fn lint_diff(before_md: &str, after_md: &str) -> Vec<Finding> {
    diff_blocks(&parse_document(before_md), &parse_document(after_md), &|gone| {
        adopted_ids(before_md, after_md, gone)
    })
}

/// Regeneration diff over two pre-segmented block lists (the segmentation-neutral
/// core of [`lint_diff`]). Blocks carry no source, so SPEC.md §16's (v1.10)
/// in-place adoption test, which reads it, is not applied: every id it would
/// keep is reported dropped.
pub fn lint_diff_blocks(before_blocks: &[Block], after_blocks: &[Block]) -> Vec<Finding> {
    diff_blocks(before_blocks, after_blocks, &|_| BTreeSet::new())
}

/// `adopted` answers the §16 (v1.10) in-place adoption test for the ids that
/// would otherwise be dropped.
fn diff_blocks(
    before_blocks: &[Block],
    after_blocks: &[Block],
    adopted: &dyn Fn(&[String]) -> BTreeSet<String>,
) -> Vec<Finding> {
    let before_idx = id_index(before_blocks);
    // before: ids with exactly one block, in first-seen order.
    let before: Vec<(String, &Block)> =
        before_idx.iter().filter(|(_, v)| v.len() == 1).map(|(k, v)| (k.clone(), v[0])).collect();
    let after = id_index(after_blocks);
    let mut findings: Vec<Finding> = Vec::new();

    let before_has = |id: &str| before.iter().any(|(k, _)| k == id);
    let before_get = |id: &str| before.iter().find(|(k, _)| k == id).map(|(_, b)| *b);
    let after_get = |id: &str| after.iter().find(|(k, _)| k == id).map(|(_, v)| v);

    // DROPPED: in baseline, gone after. SPEC.md §16 (v1.10): a block stay
    // rewritten in place as its list item's child stay is kept, and said so:
    // the test is lexical, so a shape it misjudges is printed rather than
    // passed in silence.
    let gone: Vec<String> = before
        .iter()
        .filter(|(mid, _)| after_get(mid).is_none())
        .map(|(mid, _)| mid.clone())
        .collect();
    let kept = if gone.is_empty() { BTreeSet::new() } else { adopted(&gone) };
    for mid in gone {
        if kept.contains(&mid) {
            findings.push(finding(
                Level::Warn,
                "ADOPTED_ID",
                format!(
                    "id {} was rewritten in place as a child stay and is kept; \
                     check it sits on the list item it names",
                    mid
                ),
                Some(mid.clone()),
                None,
            ));
        } else {
            findings.push(finding(
                Level::Error,
                "DROPPED_ID",
                format!("id {} was in the baseline but is gone after the edit (silent loss)", mid),
                Some(mid.clone()),
                None,
            ));
        }
    }

    // DUPLICATED: appears more than once after.
    for (mid, blks) in &after {
        if blks.len() > 1 {
            findings.push(finding(
                Level::Error,
                "DUPLICATED_ID",
                format!(
                    "id {} appears {} times after the edit \
                     (copy without re-mint, or a regeneration collision)",
                    mid,
                    blks.len()
                ),
                Some(mid.clone()),
                None,
            ));
        }
    }

    // NEW: not in baseline.
    for (mid, _) in &after {
        if !before_has(mid) {
            findings.push(finding(
                Level::Info,
                "NEW_ID",
                format!("id {} is new (not in the baseline)", mid),
                Some(mid.clone()),
                None,
            ));
        }
    }

    // content-keyed before index (first id per content hash), for exact-swap
    // relocation detection.
    let mut before_by_content: Vec<(String, String)> = Vec::new();
    for (mid, b) in &before {
        if !b.content.is_empty() {
            let h = body_hash(&b.content, None);
            if !before_by_content.iter().any(|(hh, _)| *hh == h) {
                before_by_content.push((h, mid.clone()));
            }
        }
    }

    let mut window: Option<Window> = None; // built on the first drifted id that needs it

    for (mid, blks) in &after {
        if !before_has(mid) || blks.len() != 1 {
            continue;
        }
        let a = blks[0];
        let b0 = before_get(mid).unwrap();
        if a.content.is_empty() || b0.content.is_empty() {
            continue;
        }
        let ah = body_hash(&a.content, None);
        if ah == body_hash(&b0.content, None) {
            continue; // unchanged
        }
        let moved_from = before_by_content.iter().find(|(h, _)| *h == ah).map(|(_, id)| id.clone());
        match moved_from {
            Some(src) if src != *mid => {
                findings.push(finding(
                    Level::Error,
                    "RELOCATED_ID",
                    format!(
                        "id {} now sits on content that previously carried id {} \
                         (markers look swapped or relocated)",
                        mid, src
                    ),
                    Some(mid.clone()),
                    None,
                ));
            }
            _ if moved_off(&b0.content, a, || {
                window.get_or_insert_with(|| Window::new(&before_idx, before_blocks, after_blocks))
            }) =>
            {
                findings.push(finding(
                    Level::Error,
                    "RELOCATED_ID",
                    format!(
                        "id {} now sits on different content, and its old text survives on \
                         another block that carries no kept id (a block inserted above its marker?)",
                        mid
                    ),
                    Some(mid.clone()),
                    None,
                ));
            }
            _ => {
                findings.push(finding(
                    Level::Warn,
                    "HASH_DRIFT",
                    format!("id {}: content changed between versions (edited in place)", mid),
                    Some(mid.clone()),
                    None,
                ));
            }
        }
    }

    // SPEC.md §16 (v1.9): a child stay is a stay, so its loss is reported too,
    // under its own code so the reader rule holds (a `subhash` marker is never
    // reported as its container's stay). Lexical: the id sat on exactly one
    // marker in the baseline, that marker carried `subhash`, and no marker of
    // any kind carries it after the edit.
    let before_markers = stay_marker_ids(before_blocks);
    let after_markers = stay_marker_ids(after_blocks);
    for (mid, child) in &before_markers {
        if !*child {
            continue;
        }
        let count = before_markers.iter().filter(|(k, _)| k == mid).count();
        if count == 1 && !after_markers.iter().any(|(k, _)| k == mid) {
            findings.push(finding(
                Level::Error,
                "DROPPED_CHILD_ID",
                format!(
                    "child id {} was in the baseline but no marker carries it after the edit \
                     (silent loss)",
                    mid
                ),
                Some(mid.clone()),
                None,
            ));
        }
    }
    findings
}

/// Canonical finding order: (level rank, line, code). Stable.
pub fn sort_findings(findings: &[Finding]) -> Vec<Finding> {
    let mut v: Vec<Finding> = findings.to_vec();
    v.sort_by(|x, y| {
        x.level
            .rank()
            .cmp(&y.level.rank())
            .then(x.line.unwrap_or(0).cmp(&y.line.unwrap_or(0)))
            .then(x.code.cmp(y.code))
    });
    v
}

/// True when any finding is error level.
pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| matches!(f.level, Level::Error))
}
