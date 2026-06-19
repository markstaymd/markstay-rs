// The attachment resolver: the §9.1 evidence ladder (MARKER -> HASH -> QUOTE ->
// DETACHED). Port of impl/js/src/resolve.js (`buildAnchors`,
// `buildAnchorsFromBlocks`, `resolve`, `resolveOverBlocks`), which ports
// eval/attachment/resolver.py.

use alloc::string::String;
use alloc::vec::Vec;

use crate::hash::body_hash;
use crate::parse::{parse_document, Block};
use crate::quote::{best_match, Selector};

/// Default thresholds for the QUOTE tier (SPEC.md §9 commit rule). A recovery is
/// committed only when the best candidate clears `threshold` AND beats the
/// runner-up by `margin`.
pub const DEFAULT_THRESHOLD: f64 = 0.5;
pub const DEFAULT_MARGIN: f64 = 0.05;

/// An anchor extracted from an annotated baseline block.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub id: String,
    pub hash: String,
    pub selector: Selector,
}

/// One id's resolution against the edited document.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub id: String,
    /// `"marker"` | `"hash"` | `"quote"` | `"detached"`.
    pub method: &'static str,
    /// After-doc content-block index, or `None` when detached.
    pub target: Option<usize>,
    pub score: f64,
}

/// Extract anchors from an annotated baseline document: each non-orphan block
/// with a well-formed marker contributes one anchor carrying the block's full
/// body hash and a quote selector built from the block and its neighbours.
pub fn build_anchors(before_md: &str) -> Vec<Anchor> {
    let blocks: Vec<Block> = parse_document(before_md)
        .into_iter()
        .filter(|b| b.index >= 0)
        .collect();
    build_anchors_from_blocks(&blocks)
}

/// Build anchors from an already-segmented list of content blocks (`index >= 0`,
/// in document order). The segmentation-neutral core of [`build_anchors`].
pub fn build_anchors_from_blocks(blocks: &[Block]) -> Vec<Anchor> {
    let mut anchors: Vec<Anchor> = Vec::new();
    for i in 0..blocks.len() {
        let b = &blocks[i];
        let prev = if i > 0 {
            blocks[i - 1].content.clone()
        } else {
            String::new()
        };
        let next = if i + 1 < blocks.len() {
            blocks[i + 1].content.clone()
        } else {
            String::new()
        };
        let selector = Selector {
            quote: b.content.clone(),
            prefix: prev,
            suffix: next,
        };
        for mk in &b.markers {
            if mk.malformed {
                continue;
            }
            if let Some(id) = &mk.id {
                anchors.push(Anchor {
                    id: id.clone(),
                    hash: body_hash(&b.content, None),
                    selector: selector.clone(),
                });
            }
        }
    }
    anchors
}

/// Resolve every anchor id against the edited document via the evidence ladder.
/// Returns resolutions in anchor order; `target` is the after-doc content-block
/// index or `None`.
pub fn resolve(anchors: &[Anchor], after_md: &str, threshold: f64, margin: f64) -> Vec<Resolution> {
    let after_blocks: Vec<Block> = parse_document(after_md)
        .into_iter()
        .filter(|b| b.index >= 0)
        .collect();
    resolve_over_blocks(anchors, &after_blocks, threshold, margin)
}

/// Resolve anchors against an already-segmented list of after-doc content blocks
/// (`index >= 0`, in document order). The segmentation-neutral core of
/// [`resolve`].
pub fn resolve_over_blocks(
    anchors: &[Anchor],
    after_blocks: &[Block],
    threshold: f64,
    margin: f64,
) -> Vec<Resolution> {
    let bodies: Vec<String> = after_blocks.iter().map(|b| b.content.clone()).collect();

    // Tier 1 lookup: ids whose marker is still attached, mapped to block index.
    let mut surviving: Vec<(String, usize)> = Vec::new();
    for (idx, b) in after_blocks.iter().enumerate() {
        for mk in &b.markers {
            if mk.malformed {
                continue;
            }
            if let Some(id) = &mk.id {
                if !surviving.iter().any(|(k, _)| k == id) {
                    surviving.push((id.clone(), idx));
                }
            }
        }
    }

    // Tier 2 lookup: full-body hash -> block indices (list, to detect ambiguity).
    let mut hash_to_idx: Vec<(String, Vec<usize>)> = Vec::new();
    for (idx, body) in bodies.iter().enumerate() {
        let h = body_hash(body, None);
        if let Some(entry) = hash_to_idx.iter_mut().find(|(k, _)| *k == h) {
            entry.1.push(idx);
        } else {
            hash_to_idx.push((h, alloc::vec![idx]));
        }
    }

    let mut out: Vec<Resolution> = Vec::with_capacity(anchors.len());
    for a in anchors {
        // Tier 1: marker survived.
        if let Some((_, idx)) = surviving.iter().find(|(k, _)| *k == a.id) {
            out.push(Resolution {
                id: a.id.clone(),
                method: "marker",
                target: Some(*idx),
                score: 1.0,
            });
            continue;
        }
        // Tier 2: body hash uniquely identifies a surviving block.
        let hits: &[usize] = hash_to_idx
            .iter()
            .find(|(k, _)| *k == a.hash)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[]);
        if hits.len() == 1 {
            out.push(Resolution {
                id: a.id.clone(),
                method: "hash",
                target: Some(hits[0]),
                score: 1.0,
            });
            continue;
        }
        // Tier 3: quote recovery, committed only on a clear winner.
        let bm = best_match(&a.selector, &bodies);
        if bm.index >= 0 && bm.score >= threshold && (bm.score - bm.runner_up) >= margin {
            out.push(Resolution {
                id: a.id.clone(),
                method: "quote",
                target: Some(bm.index as usize),
                score: bm.score,
            });
        } else {
            out.push(Resolution {
                id: a.id.clone(),
                method: "detached",
                target: None,
                score: bm.score,
            });
        }
    }
    out
}
