// The attachment resolver: the §9.1 evidence ladder (MARKER -> HASH -> QUOTE ->
// DETACHED), assigning blocks exclusively (v1.11). Anchor construction ports
// impl/js/src/resolve.js (`buildAnchors`, `buildAnchorsFromBlocks`), which ports
// eval/attachment/resolver.py; the exclusive ladder and the merge exemption are
// written from §9.1's text and the conformance vectors.

use alloc::string::String;
use alloc::vec::Vec;

use crate::hash::body_hash;
use crate::parse::{parse_document, Block};
use crate::quote::{normalize, rank_matches, window_prefix, window_suffix, Selector};

/// Default thresholds for the QUOTE tier (SPEC.md §9 commit rule). A recovery is
/// committed only when the best candidate clears `threshold` AND beats the
/// runner-up by `margin`.
pub const DEFAULT_THRESHOLD: f64 = 0.5;
pub const DEFAULT_MARGIN: f64 = 0.05;

/// SPEC.md §9.1's merge exemption fails outright when more than this many stays
/// select one block at the QUOTE tier, before any other check.
pub const MERGE_CONTENDER_CAP: usize = 16;

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
    let blocks: Vec<Block> =
        parse_document(before_md).into_iter().filter(|b| b.index >= 0).collect();
    build_anchors_from_blocks(&blocks)
}

/// Build anchors from an already-segmented list of content blocks (`index >= 0`,
/// in document order). The segmentation-neutral core of [`build_anchors`].
pub fn build_anchors_from_blocks(blocks: &[Block]) -> Vec<Anchor> {
    let mut anchors: Vec<Anchor> = Vec::new();
    for i in 0..blocks.len() {
        let b = &blocks[i];
        let prev = if i > 0 { blocks[i - 1].content.clone() } else { String::new() };
        let next = if i + 1 < blocks.len() { blocks[i + 1].content.clone() } else { String::new() };
        // SPEC.md §9: the stored prefix/suffix carry up to 48 characters of the
        // neighbour on each side. Storing whole blocks caps the achievable ratio
        // near 2*48/(len+48), because the candidate side is windowed at match time.
        let selector = Selector {
            quote: b.content.clone(),
            prefix: window_prefix(&prev),
            suffix: window_suffix(&next),
        };
        for mk in &b.markers {
            if !mk.is_block_stay() {
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
/// Returns one resolution per distinct anchor id, in the order the ids first
/// appear; `target` is the after-doc content-block index or `None`.
pub fn resolve(anchors: &[Anchor], after_md: &str, threshold: f64, margin: f64) -> Vec<Resolution> {
    let after_blocks: Vec<Block> =
        parse_document(after_md).into_iter().filter(|b| b.index >= 0).collect();
    resolve_over_blocks(anchors, &after_blocks, threshold, margin)
}

/// SPEC.md §9.1's merge exemption: can every quote be placed in `body` at its
/// own occurrence, no two overlapping? Quotes and body are compared after §9's
/// quote normalization; an occurrence is a half-open span, so two may touch.
/// Each quote is a separate contender, so two equal quotes need two
/// occurrences. Fails with more than [`MERGE_CONTENDER_CAP`] quotes, then on
/// any empty quote, then when the quotes' total length exceeds the body's; with
/// no quotes it holds.
///
/// The decision is the subset recurrence: `end(S)` is the smallest offset by
/// which every quote in `S` can be placed disjointly, `end(empty) = 0`, and
/// `end(S)` is the least `find(q, end(S - q)) + len(q)` over the `q` in `S`
/// where both exist. §9.1 states offsets in code points. The search here runs on
/// UTF-8 byte offsets with `str::find`, which decides the same thing: a `&str`
/// can only occur at a char boundary of another, and the map from code-point
/// offsets to byte offsets is strictly increasing, so it preserves every
/// comparison the recurrence makes. The length check counts code points, as the
/// text says; it is a shortcut either way and never decides.
pub fn disjoint_in<S: AsRef<str>>(quotes: &[S], body: &str) -> bool {
    let k = quotes.len();
    if k > MERGE_CONTENDER_CAP {
        return false;
    }
    if k == 0 {
        return true;
    }
    let qs: Vec<String> = quotes.iter().map(|q| normalize(q.as_ref())).collect();
    if qs.iter().any(|q| q.is_empty()) {
        return false;
    }
    let b = normalize(body);
    let total: usize = qs.iter().map(|q| q.chars().count()).sum();
    if total > b.chars().count() {
        return false;
    }
    let full: usize = (1usize << k) - 1;
    // end[S], or None where undefined.
    let mut end: Vec<Option<usize>> = alloc::vec![None; full + 1];
    end[0] = Some(0);
    for set in 1..=full {
        let mut best: Option<usize> = None;
        for (i, q) in qs.iter().enumerate() {
            let bit = 1usize << i;
            if set & bit == 0 {
                continue;
            }
            let Some(from) = end[set ^ bit] else { continue };
            let Some(at) = b[from..].find(q.as_str()) else { continue };
            let e = from + at + q.len();
            if best.is_none() || Some(e) < best {
                best = Some(e);
            }
        }
        end[set] = best;
    }
    end[full].is_some()
}

/// SPEC.md §9's commit rule over a target and runner-up taken from a ranked
/// list (scores unclamped, runner-up 0 when there is none). Both are clamped to
/// 1.0 before the threshold and margin apply, as `best_match` reports them.
/// Returns the reported score and whether it commits.
fn commit_rule(target: f64, runner_up: Option<f64>, threshold: f64, margin: f64) -> (f64, bool) {
    let score = target.min(1.0);
    let second = runner_up.map_or(0.0, |r| r.min(1.0));
    (score, score >= threshold && (score - second) >= margin)
}

/// Resolve anchors against an already-segmented list of after-doc content blocks
/// (`index >= 0`, in document order). The segmentation-neutral core of
/// [`resolve`].
///
/// SPEC.md §9.1 (v1.11) applies the tiers strongest-first and assigns
/// exclusively: every stay still unresolved is evaluated at a tier before any
/// commit at that tier takes effect, and a block committed at a tier is claimed
/// for the weaker ones. When the anchor list repeats an id (a §7 error in the
/// baseline), the last anchor read for it is the one resolved, reported at the
/// id's first position.
pub fn resolve_over_blocks(
    anchors: &[Anchor],
    after_blocks: &[Block],
    threshold: f64,
    margin: f64,
) -> Vec<Resolution> {
    let bodies: Vec<String> = after_blocks.iter().map(|b| b.content.clone()).collect();
    let hashes: Vec<String> = bodies.iter().map(|b| body_hash(b, None)).collect();

    let mut stays: Vec<&Anchor> = Vec::new();
    for a in anchors {
        match stays.iter().position(|s| s.id == a.id) {
            Some(p) => stays[p] = a,
            None => stays.push(a),
        }
    }

    // The blocks each id's block-stay marker sits on, distinct, in document order.
    let mut carriers: Vec<(String, Vec<usize>)> = Vec::new();
    for (idx, b) in after_blocks.iter().enumerate() {
        for mk in &b.markers {
            if !mk.is_block_stay() {
                continue;
            }
            let Some(id) = &mk.id else { continue };
            match carriers.iter_mut().find(|(k, _)| k == id) {
                Some((_, v)) => {
                    if v.last() != Some(&idx) {
                        v.push(idx);
                    }
                }
                None => carriers.push((id.clone(), alloc::vec![idx])),
            }
        }
    }

    let n = stays.len();
    let mut out: Vec<Option<Resolution>> = alloc::vec![None; n];
    let mut claimed: Vec<bool> = alloc::vec![false; bodies.len()];
    let settle = |out: &mut Vec<Option<Resolution>>,
                  s: usize,
                  method: &'static str,
                  target: Option<usize>,
                  score: f64| {
        out[s] = Some(Resolution { id: stays[s].id.clone(), method, target, score });
    };

    // Tier 1: MARKER. One carrier commits. Several resolve among themselves
    // only: the one carrier whose hash is the stored hash, else §9's commit rule
    // with the carriers as the only candidates, else DETACHED. Never the weaker
    // tiers. Commits at a tier are claimed once the whole tier is evaluated.
    let mut commits: Vec<(usize, &'static str, usize, f64)> = Vec::new();
    for (s, a) in stays.iter().enumerate() {
        let Some((_, cs)) = carriers.iter().find(|(k, _)| *k == a.id) else { continue };
        if cs.len() == 1 {
            commits.push((s, "marker", cs[0], 1.0));
            continue;
        }
        let exact: Vec<usize> = cs.iter().copied().filter(|&i| hashes[i] == a.hash).collect();
        if exact.len() == 1 {
            commits.push((s, "marker", exact[0], 1.0));
            continue;
        }
        let ranked: Vec<(f64, usize)> = rank_matches(&a.selector, &bodies)
            .into_iter()
            .filter(|(_, i)| cs.contains(i))
            .collect();
        let (score, ok) = commit_rule(ranked[0].0, ranked.get(1).map(|r| r.0), threshold, margin);
        if ok {
            commits.push((s, "marker", ranked[0].1, score));
        } else {
            settle(&mut out, s, "detached", None, score);
        }
    }
    for (s, method, target, score) in commits.drain(..) {
        settle(&mut out, s, method, Some(target), score);
        claimed[target] = true;
    }

    // Tier 2: HASH. The stored hash was recorded by one stay and exactly one
    // unclaimed block has it.
    for (s, a) in stays.iter().enumerate() {
        if out[s].is_some() {
            continue;
        }
        if stays.iter().filter(|o| o.hash == a.hash).count() != 1 {
            continue;
        }
        let hits: Vec<usize> =
            (0..hashes.len()).filter(|&i| hashes[i] == a.hash && !claimed[i]).collect();
        if hits.len() == 1 {
            commits.push((s, "hash", hits[0], 1.0));
        }
    }
    for (s, method, target, score) in commits.drain(..) {
        settle(&mut out, s, method, Some(target), score);
        claimed[target] = true;
    }

    // Tier 3: QUOTE. Every block is ranked, claimed or not. The target is the
    // best unclaimed block, the runner-up the best other block. Stays that select
    // the same target commit together only if their quotes fit it disjointly.
    let mut proposals: Vec<(usize, Vec<(usize, f64)>)> = Vec::new();
    for (s, a) in stays.iter().enumerate() {
        if out[s].is_some() {
            continue;
        }
        let ranked = rank_matches(&a.selector, &bodies);
        let Some(t) = ranked.iter().position(|(_, i)| !claimed[*i]) else {
            settle(&mut out, s, "detached", None, 0.0);
            continue;
        };
        let runner_up = if t == 0 { ranked.get(1) } else { ranked.first() };
        let (score, ok) = commit_rule(ranked[t].0, runner_up.map(|r| r.0), threshold, margin);
        if !ok {
            settle(&mut out, s, "detached", None, score);
            continue;
        }
        let target = ranked[t].1;
        match proposals.iter_mut().find(|(k, _)| *k == target) {
            Some((_, group)) => group.push((s, score)),
            None => proposals.push((target, alloc::vec![(s, score)])),
        }
    }
    for (target, group) in &proposals {
        let holds = group.len() == 1 || {
            let quotes: Vec<&str> =
                group.iter().map(|(s, _)| stays[*s].selector.quote.as_str()).collect();
            disjoint_in(&quotes, &bodies[*target])
        };
        for &(s, score) in group {
            if holds {
                settle(&mut out, s, "quote", Some(*target), score);
            } else {
                settle(&mut out, s, "detached", None, score);
            }
        }
    }

    out.into_iter().map(|r| r.expect("every stay settles at some tier")).collect()
}
