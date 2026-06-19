// Quote / selector recovery scoring (SPEC.md §9). Port of impl/js/src/quote.js
// (`normalize`, `quoteRatio`, `bodyScore`, `contextBonus`, `bestMatch`), which
// ports eval/attachment/quote.py.

use alloc::string::String;
use alloc::vec::Vec;

use crate::ratio::ratio as raw_ratio;
use crate::text::{ascii_collapse, ascii_lower, ascii_trim};

/// How much neighbour context to keep on each side (SPEC.md §9). Code points,
/// not bytes.
pub const CONTEXT_CHARS: usize = 48;

/// A stored quote selector: the block body plus its neighbours' bodies.
#[derive(Clone, Debug, Default)]
pub struct Selector {
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
}

/// Result of [`best_match`].
#[derive(Clone, Copy, Debug)]
pub struct BestMatch {
    /// Index of the winning candidate, or `-1` for an empty candidate list.
    pub index: i64,
    pub score: f64,
    pub runner_up: f64,
}

/// §9 matching normalization: trim, collapse ASCII whitespace runs to a single
/// space, then lowercase ASCII letters. Capitalization and reflowed line breaks
/// (common after an LLM edit) must not register as differences. Pinned to ASCII
/// (SPEC.md §9 / SPEC_DECISIONS.md): non-ASCII passes through unchanged and
/// identical across implementations.
pub fn normalize(text: &str) -> String {
    ascii_lower(&ascii_collapse(ascii_trim(text)))
}

/// markstay ratio wrapper: empty input floors to 0.0 (raw ratio returns 1.0).
pub fn quote_ratio(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    raw_ratio(a, b)
}

fn cp_len(s: &str) -> usize {
    s.chars().count()
}

/// First `n` code points of `s` (Python `s[:n]`).
fn cp_take_start(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Last `n` code points of `s` (Python `s[-n:]`).
fn cp_take_end(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

#[inline]
fn clamp_one(x: f64) -> f64 {
    if x > 1.0 {
        1.0
    } else {
        x
    }
}

/// Similarity of a stored selector's quote to a candidate block body, in [0, 1].
/// Exact containment floors the score at the length ratio of shorter to longer,
/// so a surviving half of a split paragraph cannot score arbitrarily low.
pub fn body_score(sel: &Selector, candidate: &str) -> f64 {
    let q = normalize(&sel.quote);
    let c = normalize(candidate);
    if q.is_empty() || c.is_empty() {
        return 0.0;
    }
    if q == c {
        return 1.0;
    }
    let mut base = quote_ratio(&q, &c);
    let lq = cp_len(&q);
    let lc = cp_len(&c);
    let (short, long, ls, ll) = if lq <= lc {
        (&q, &c, lq, lc)
    } else {
        (&c, &q, lc, lq)
    };
    if !short.is_empty() && long.contains(short.as_str()) {
        let containment = ls as f64 / ll as f64;
        if containment > base {
            base = containment;
        }
    }
    base
}

/// Small additive bonus in [0, ~0.1] when the candidate's neighbours match the
/// stored prefix/suffix. Used only to break near-ties; not a primary key.
pub fn context_bonus(sel: &Selector, prev_text: &str, next_text: &str) -> f64 {
    let mut bonus = 0.0;
    if !sel.prefix.is_empty() {
        let prev_ctx = cp_take_end(prev_text, CONTEXT_CHARS);
        bonus += 0.05 * quote_ratio(&normalize(&sel.prefix), &normalize(&prev_ctx));
    }
    if !sel.suffix.is_empty() {
        let next_ctx = cp_take_start(next_text, CONTEXT_CHARS);
        bonus += 0.05 * quote_ratio(&normalize(&sel.suffix), &normalize(&next_ctx));
    }
    bonus
}

/// Rank candidate block bodies against a selector. On an exact score tie the
/// later candidate wins (sort `(score, index)` descending), and the score
/// ceiling is 1.0. An empty candidate list returns `{ index: -1, .. }`.
pub fn best_match(sel: &Selector, candidates: &[String]) -> BestMatch {
    let n = candidates.len();
    let mut scored: Vec<(f64, usize)> = Vec::with_capacity(n);
    for i in 0..n {
        let s = body_score(sel, &candidates[i]);
        let prev = if i > 0 { candidates[i - 1].as_str() } else { "" };
        let next = if i + 1 < n {
            candidates[i + 1].as_str()
        } else {
            ""
        };
        scored.push((s + context_bonus(sel, prev, next), i));
    }
    if scored.is_empty() {
        return BestMatch {
            index: -1,
            score: 0.0,
            runner_up: 0.0,
        };
    }
    // Sort (score, index) descending: an exact tie picks the later candidate.
    scored.sort_by(|x, y| {
        y.0.partial_cmp(&x.0)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then(y.1.cmp(&x.1))
    });
    let (best_score, best_index) = scored[0];
    let runner_up = if scored.len() > 1 { scored[1].0 } else { 0.0 };
    BestMatch {
        index: best_index as i64,
        score: clamp_one(best_score),
        runner_up: clamp_one(runner_up),
    }
}
