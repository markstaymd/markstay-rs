// Well-formedness, intra-document checks, and the regeneration diff
// (SPEC.md §7 / §8 / §10 / §11). Port of impl/js/src/lint.js (`lintBlocks`,
// `lintDocument`, `lintDiff`, `lintDiffBlocks`, `sortFindings`, `hasErrors`).
//
// Finding emission order is detection order; `sort_findings` produces the
// canonical (level, line, code) order. Where JS uses an insertion-ordered `Map`
// for the id index, this port keeps an insertion-ordered `Vec` so same-(level,
// code,line) findings retain document order after the stable sort, matching the
// other runners on the `diff` corpus.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::hash::body_hash;
use crate::parse::{parse_document, Block};

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
    Finding {
        level,
        code,
        message,
        id,
        line,
    }
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
            if orphan {
                findings.push(finding(
                    Level::Error,
                    "ORPHAN_MARKER",
                    format!("marker {} has no preceding block to attach to", id),
                    Some(id.clone()),
                    Some(mk.line),
                ));
            }
            if let Some((_, first)) = seen.iter().find(|(k, _)| *k == id) {
                findings.push(finding(
                    Level::Error,
                    "DUPLICATE_ID",
                    format!(
                        "id {} appears more than once (first at line {})",
                        id, first
                    ),
                    Some(id.clone()),
                    Some(mk.line),
                ));
            } else {
                seen.push((id.clone(), mk.line));
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
            if mk.malformed {
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

/// Regeneration diff (SPEC.md §11): what an edit did to the ids. Catches the
/// AI-rewrite failure mode (dropped markers) plus duplication and exact-content
/// relocation. Returns findings in detection order.
pub fn lint_diff(before_md: &str, after_md: &str) -> Vec<Finding> {
    lint_diff_blocks(&parse_document(before_md), &parse_document(after_md))
}

/// Regeneration diff over two pre-segmented block lists (the segmentation-neutral
/// core of [`lint_diff`]).
pub fn lint_diff_blocks(before_blocks: &[Block], after_blocks: &[Block]) -> Vec<Finding> {
    let before_idx = id_index(before_blocks);
    // before: ids with exactly one block, in first-seen order.
    let before: Vec<(String, &Block)> = before_idx
        .iter()
        .filter(|(_, v)| v.len() == 1)
        .map(|(k, v)| (k.clone(), v[0]))
        .collect();
    let after = id_index(after_blocks);
    let mut findings: Vec<Finding> = Vec::new();

    let before_has = |id: &str| before.iter().any(|(k, _)| k == id);
    let before_get = |id: &str| before.iter().find(|(k, _)| k == id).map(|(_, b)| *b);
    let after_get = |id: &str| after.iter().find(|(k, _)| k == id).map(|(_, v)| v);

    // DROPPED: in baseline, gone after.
    for (mid, _) in &before {
        if after_get(mid).is_none() {
            findings.push(finding(
                Level::Error,
                "DROPPED_ID",
                format!(
                    "id {} was in the baseline but is gone after the edit (silent loss)",
                    mid
                ),
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
        let moved_from = before_by_content
            .iter()
            .find(|(h, _)| *h == ah)
            .map(|(_, id)| id.clone());
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
