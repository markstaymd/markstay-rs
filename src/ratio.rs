// Ratcliff/Obershelp similarity ratio, a faithful port of CPython
// `difflib.SequenceMatcher(None, a, b, autojunk=False).ratio()` (SPEC.md §9), via
// impl/js/src/ratio.js. This is the conformance-critical module: markstay quote
// recovery scores candidate blocks with this ratio, so every implementation must
// agree on it bit-for-bit (the `seqmatch.json` corpus is the gate).
//
// Three things a naive ratio would get wrong, reproduced here:
//
//  1. **Code points, not bytes / UTF-16 units.** Python indexes Unicode scalar
//     values; a Rust `char` *is* a scalar value, so both inputs are converted to
//     `Vec<char>` and all indices/lengths are measured on those. Never index the
//     raw bytes (the non-BMP `seqmatch` vectors are the tripwire).
//  2. **`autojunk=false` / `isjunk=None`**: no junk and no popularity heuristic,
//     every element of `b` stays in `b2j`. The autojunk purge (n >= 200) is never
//     implemented.
//  3. **Tie-break earliest-in-a, then earliest-in-b.** `find_longest_match` only
//     adopts a new best on a *strictly* longer run, so the first maximal match in
//     iteration order wins, keeping `matching_blocks` identical, not just `ratio`.
//
// `no_std`: the index maps are `alloc::collections::BTreeMap` (the WASM handover
// assumes the core stays `alloc`-only; `std::collections::HashMap` is forbidden).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Build the `b2j` index: element -> ascending list of indices in `b`.
fn build_b2j(b: &[char]) -> BTreeMap<char, Vec<usize>> {
    let mut b2j: BTreeMap<char, Vec<usize>> = BTreeMap::new();
    for (i, &elt) in b.iter().enumerate() {
        b2j.entry(elt).or_default().push(i);
    }
    b2j
}

/// Longest matching block of `a[alo..ahi]` against `b[blo..bhi]`. Returns
/// `(besti, bestj, bestsize)`. Direct port of CPython `find_longest_match` with
/// junk handling removed (autojunk=False, isjunk=None).
fn find_longest_match(
    a: &[char],
    b2j: &BTreeMap<char, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let mut besti = alo;
    let mut bestj = blo;
    let mut bestsize = 0usize;
    let mut j2len: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, &elt) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut newj2len: BTreeMap<usize, usize> = BTreeMap::new();
        if let Some(idxs) = b2j.get(&elt) {
            for &j in idxs {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                // k = j2len[j-1] + 1 (j2len.get on j-1; j==0 -> wrap -> absent -> 0)
                let prev = if j == 0 { 0 } else { j2len.get(&(j - 1)).copied().unwrap_or(0) };
                let k = prev + 1;
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    // With no junk the DP already yields a maximal run, so CPython's junk and
    // non-junk extension passes are provable no-ops and are omitted.
    (besti, bestj, bestsize)
}

/// Matching blocks for two code-point slices, port of `get_matching_blocks()`:
/// recursive longest-match over a queue, sort, adjacent-block merge, terminated
/// by the `(la, lb, 0)` sentinel.
fn matching_blocks_of(a: &[char], b: &[char]) -> Vec<(usize, usize, usize)> {
    let b2j = build_b2j(b);
    let la = a.len();
    let lb = b.len();
    let mut queue: Vec<(usize, usize, usize, usize)> = Vec::new();
    queue.push((0, la, 0, lb));
    let mut blocks: Vec<(usize, usize, usize)> = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = find_longest_match(a, &b2j, alo, ahi, blo, bhi);
        if k != 0 {
            blocks.push((i, j, k));
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    // Tuple `sort` is lexicographic on (i, j, k), matching Python list.sort().
    blocks.sort();

    let mut i1 = 0usize;
    let mut j1 = 0usize;
    let mut k1 = 0usize;
    let mut non_adjacent: Vec<(usize, usize, usize)> = Vec::new();
    for (i2, j2, k2) in blocks {
        if i1 + k1 == i2 && j1 + k1 == j2 {
            k1 += k2;
        } else {
            if k1 != 0 {
                non_adjacent.push((i1, j1, k1));
            }
            i1 = i2;
            j1 = j2;
            k1 = k2;
        }
    }
    if k1 != 0 {
        non_adjacent.push((i1, j1, k1));
    }
    non_adjacent.push((la, lb, 0));
    non_adjacent
}

/// The matching blocks of `a` against `b` as `(a_index, b_index, size)` triples
/// over Unicode code points (the sentinel block is included, as in CPython).
pub fn matching_blocks(a: &str, b: &str) -> Vec<(usize, usize, usize)> {
    let aa: Vec<char> = a.chars().collect();
    let bb: Vec<char> = b.chars().collect();
    matching_blocks_of(&aa, &bb)
}

/// Raw Ratcliff/Obershelp ratio in [0, 1], equal to
/// `difflib.SequenceMatcher(None, a, b, autojunk=False).ratio()`.
/// Empty/empty returns 1.0 (the length-0 path), matching the raw matcher; the
/// markstay wrapper in `quote.rs` floors empty input to 0.0 instead.
pub fn ratio(a: &str, b: &str) -> f64 {
    let aa: Vec<char> = a.chars().collect();
    let bb: Vec<char> = b.chars().collect();
    let blocks = matching_blocks_of(&aa, &bb);
    let matches: usize = blocks.iter().map(|t| t.2).sum();
    let length = aa.len() + bb.len();
    if length != 0 {
        (2.0 * matches as f64) / (length as f64)
    } else {
        1.0
    }
}
