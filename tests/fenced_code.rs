// SPEC.md §3.3 (v1.5): text inside a fenced code block is content, not markup.
//
// A port of impl/js/test/fenced-code.test.js, which ports
// impl/py/tests/test_fenced_code.py. The §5.2 CommonMark cases and the §5.5
// child-block cases are Python-only, since neither segmenter is in this core;
// they are noted where they would sit. The four symptoms below were all observed
// on this project's own SPEC.md, which is the document the rule exists for: a
// tool's own specification is exactly where marker examples live.

use std::collections::BTreeSet;

use markstay::{
    body_hash, code_lines, lint_document, normalize_body, parse_document, repair_duplicates,
    restamp, stamp, RestampOptions, StampOptions,
};

/// Deterministic id factory `n0, n1, ...` for reproducible assertions.
fn counter() -> impl FnMut() -> String {
    let mut n = 0u32;
    move || {
        let s = format!("n{n}");
        n += 1;
        s
    }
}

fn lines(ns: &[usize]) -> BTreeSet<usize> {
    ns.iter().copied().collect()
}

fn marker_ids(md: &str) -> Vec<String> {
    parse_document(md).iter().flat_map(|b| b.markers.iter().filter_map(|m| m.id.clone())).collect()
}

// --- fence recognition (§3.3, the line rule) ------------------------------

#[test]
fn code_lines_recognises_the_fences_the_line_rule_can_see() {
    let cases: Vec<(&str, BTreeSet<usize>)> = vec![
        ("a\n```\ncode\n```\nb\n", lines(&[2, 3, 4])),
        ("a\n~~~\ncode\n~~~\nb\n", lines(&[2, 3, 4])),
        // Up to three leading spaces opens and closes.
        ("a\n   ```\ncode\n   ```\nb\n", lines(&[2, 3, 4])),
        // Four does not: that is an indented code block, which §3.3 leaves alone.
        ("a\n    ```\ncode\n    ```\nb\n", lines(&[])),
        // A tab is not one of the three spaces: CommonMark expands it against a
        // column model this rule deliberately does not have.
        ("a\n\t```\ncode\n\t```\nb\n", lines(&[])),
        // A longer opener contains a shorter run.
        ("````\n```\ninner\n```\n````\n", lines(&[1, 2, 3, 4, 5])),
        // A shorter run cannot close a longer one, so this never closes.
        ("````\ncode\n```\nstill code\n", lines(&[1, 2, 3, 4, 5])),
        // Different character cannot close.
        ("```\ncode\n~~~\nstill code\n", lines(&[1, 2, 3, 4, 5])),
        // An unclosed fence runs to the end of the document.
        ("a\n```\ncode\n", lines(&[2, 3, 4])),
        // A closing fence takes space or tab and nothing else after the run.
        ("```\ncode\n``` \nafter\n", lines(&[1, 2, 3])),
        ("```\ncode\n```x\nstill code\n", lines(&[1, 2, 3, 4, 5])),
        // A backtick fence's info string may not contain a backtick.
        ("a\n```md `x`\nnot a fence\n", lines(&[])),
        // A tilde fence's info string may.
        ("a\n~~~md `x`\ncode\n~~~\n", lines(&[2, 3, 4])),
    ];
    for (md, expected) in cases {
        assert_eq!(code_lines(md), expected, "{md:?}");
    }
}

#[test]
fn crlf_and_lf_twins_give_the_same_mask() {
    let lf = "a\n```\ncode\n```\nb\n";
    assert_eq!(code_lines(&lf.replace('\n', "\r\n")), code_lines(lf));
}

#[test]
fn a_fence_carrying_a_blockquote_marker_is_not_covered() {
    // §3.3 names this as the rule's real limit rather than hiding it: seeing this
    // fence means knowing the container, which is the parser §5.2 exists to avoid.
    assert_eq!(code_lines("> ```\n> code\n> ```\n"), lines(&[]));
}

// --- reading (§3.3 reader rules, §8) --------------------------------------

#[test]
fn a_marker_in_a_fence_identifies_no_block() {
    let md = "Intro.\n\n```md\nThe paragraph.\n<!-- stay:demo hash=sha256:7a9c -->\n```\n";
    let blocks = parse_document(md);
    assert_eq!(blocks.len(), 2);
    assert!(blocks.iter().all(|b| b.markers.is_empty()));
}

#[test]
fn a_marker_in_a_fence_is_hashed_with_the_body() {
    let fence = "```md\nThe paragraph.\n<!-- stay:demo hash=sha256:7a9c -->\n```";
    let blocks = parse_document(&format!("{fence}\n"));
    assert!(blocks[0].content.contains("stay:demo"));
    assert_eq!(blocks[0].content, normalize_body(fence));
}

#[test]
fn two_fences_sharing_an_example_id_are_not_a_duplicate() {
    // Symptom 3: SPEC.md reported a DUPLICATE_ID under its own linter that no
    // restamp could clear, because restamp resolves an id to the first block
    // carrying it and hands the second that block's digest.
    let md = "```md\n<!-- stay:8f24 hash=sha256:7a9c -->\n```\n\n\
              ```mdx\n{/* stay:8f24 hash=sha256:7a9c */}\n```\n";
    assert!(lint_document(md).1.is_empty());
}

#[test]
fn a_marker_in_an_opening_fence_info_string_is_not_a_marker() {
    // The fence lines are part of the block deliberately: before §3.3 this bound
    // to whatever block preceded it.
    let md = "Intro.\n\n~~~md <!-- stay:demo -->\ncode\n~~~\n";
    let blocks = parse_document(md);
    assert_eq!(blocks.len(), 2);
    assert!(blocks.iter().all(|b| b.markers.is_empty()));
}

#[test]
fn an_inline_code_span_still_carries_a_marker() {
    // §3.3 declines inline spans on purpose: that is the shape pandoc's native
    // markdown writer produces when it mangles a trailing marker, and a mangled
    // marker a tool can still see beats one that has silently stopped existing.
    assert_eq!(marker_ids("A line showing `<!-- stay:demo -->` inline.\n"), ["demo"]);
}

// --- writing (§3.3 writer rules) ------------------------------------------

#[test]
fn restamp_leaves_an_illustrative_hash_alone() {
    // Symptom 1, and the one that shipped: a restamp of SPEC.md rewrote §3.1's
    // and §3.2's example `hash=` values to the digest of the fence around them.
    let md = "```md\nThe paragraph being identified.\n<!-- stay:8f24 hash=sha256:7a9c -->\n```\n";
    let result = restamp(md, &RestampOptions::default());
    assert_eq!(result.text, md);
    assert!(result.refreshed.is_empty());
}

#[test]
fn restamp_leaves_an_example_alone_even_when_the_id_is_live() {
    // The mask is load-bearing rather than belt-and-braces: with a real block
    // carrying the same id, the example is reachable through `content_by_id`.
    let body = "Live content.";
    let md = format!(
        "{body}\n<!-- stay:demo hash=sha256:{} -->\n\n```md\n<!-- stay:demo hash=sha256:7a9c -->\n```\n",
        body_hash(body, Some(4))
    );
    assert_eq!(restamp(&md, &RestampOptions::default()).text, md);
}

#[test]
fn a_fence_showing_a_marker_now_gets_a_stay_of_its_own() {
    // Symptom 2: the example counted as the fence's stay, so the block a tutorial
    // most wants addressable was the one block that was not. (Python asserts this
    // under §5.2; here the fence has no internal blank line, so the baseline
    // segmenter reads it as one block and reaches the same answer.)
    let md = "```md\nThe paragraph.\n<!-- stay:demo hash=sha256:7a9c -->\n```\n";
    let result = stamp(md, &StampOptions::default(), counter());
    let fence = md.trim_end_matches('\n');
    assert_eq!(
        result.text,
        format!("{fence}\n<!-- stay:n0 hash=sha256:{} -->\n", body_hash(fence, Some(12)))
    );
    assert_eq!(result.minted.len(), 1);
    assert_eq!(result.minted[0].id, "n0");
    assert_eq!(result.minted[0].line, 4);
}

#[test]
fn the_stamper_refuses_to_write_into_a_listing() {
    // Symptom 4, and the worst one: under the baseline segmenter a fence with an
    // internal blank line splits into ordinary blocks, and a stamping run put a
    // real marker inside this specification's own §4 ABNF grammar.
    let md = "```text\nfirst = a\n\nsecond = b\n```\n";
    let result = stamp(md, &StampOptions::default(), counter());
    assert_eq!(result.text, md);
    assert!(result.minted.is_empty());
}

#[test]
fn repair_does_not_rename_an_example_id() {
    let body = "Live content.";
    let md = format!(
        "{body}\n<!-- stay:demo hash=sha256:{} -->\n\n```md\n<!-- stay:demo -->\n```\n",
        body_hash(body, Some(4))
    );
    let result = repair_duplicates(&md, counter());
    assert_eq!(result.text, md);
    assert!(result.renamed.is_empty());
}

// --- migration (§3.3, the two shapes) -------------------------------------

#[test]
fn migration_one_the_block_drifts_once_and_restamps_clean() {
    let fence = "```md\nExample.\n<!-- stay:demo -->\n```";
    let stale = body_hash("```md\nExample.\n\n```", Some(12)); // what v1.4 hashed
    let md = format!("{fence}\n<!-- stay:live hash=sha256:{stale} -->\n");
    let (_, findings) = lint_document(&md);
    assert_eq!(findings.iter().map(|f| f.code).collect::<Vec<_>>(), ["HASH_DRIFT"]);
    let fixed = restamp(&md, &RestampOptions::default()).text;
    assert!(lint_document(&fixed).1.is_empty());
}

#[test]
fn migration_two_the_block_is_silently_unstamped_and_needs_minting() {
    // A restamp does not fix this one: the block needs a stay minted, so it gets a
    // new id rather than a corrected hash, and no linter finding fires on the way
    // because an unstamped block is not an error.
    let md = "```md\nExample.\n<!-- stay:demo hash=sha256:7a9c -->\n```\n";
    assert!(lint_document(md).1.is_empty());
    assert_eq!(restamp(md, &RestampOptions::default()).text, md);
    let minted = stamp(md, &StampOptions::default(), counter()).minted;
    assert_eq!(minted.len(), 1);
    assert_eq!(minted[0].id, "n0");
    assert_eq!(minted[0].line, 4);
}

// --- the line rule's sharpest limit ---------------------------------------

#[test]
fn a_fence_opening_on_a_list_marker_line_swallows_the_rest_of_the_document() {
    // §3.3's limit, in the shape that costs the most. The opening fence shares a
    // line with the list marker, so the line scan never sees it open; the closing
    // "  ```" is the first fence-shaped line it does see and reads as an *opener*,
    // and with no later fence-shaped line it runs to EOF. Every marker after it
    // goes silent: no drift is reported because nothing is compared, and the block
    // simply looks unstamped. It fails closed rather than corrupting anything, and
    // it is pinned here so a later reader meets it as a decision rather than as a
    // surprise. The variant where a later fence-shaped line *does* close the
    // phantom is the test below.
    let md = "- ```txt\n  code\n  ```\n\nA later paragraph.\n<!-- stay:live hash=sha256:dead -->\n";
    assert_eq!(code_lines(md), lines(&[3, 4, 5, 6, 7]));
    assert!(marker_ids(md).is_empty());
    assert!(lint_document(md).1.is_empty());
    assert!(stamp(md, &StampOptions::default(), counter()).minted.is_empty());
}

#[test]
fn a_fence_on_its_own_line_inside_a_list_item_is_seen_correctly() {
    // The common shape is fine, which is why the case above is a limit rather than
    // a defect in the rule: a balanced pair at one to three spaces of indent opens
    // and closes exactly where CommonMark puts it.
    let md = "- item:\n\n  ```py\n  code\n  ```\n\nAfter.\n<!-- stay:live -->\n";
    assert_eq!(code_lines(md), lines(&[3, 4, 5]));
    assert_eq!(marker_ids(md), ["live"]);
}

#[test]
fn a_phantom_fence_that_later_closes_still_refuses_the_block_it_straddles() {
    // The phantom opener from the case above is closed by the next fence-shaped
    // line, so the mask ends and later lines are ordinary content again. What does
    // not come back is the block that *straddles* the close: it begins inside the
    // phantom fence and ends outside it, and `stamp` refuses it even though the
    // marker would land on an unmasked line.
    //
    // That refusal is the rule working rather than over-reaching, and the case
    // below shows why: the same shape occurs with a perfectly real fence, where the
    // straddling block's body is half a listing and §5.2 already says such a half
    // cannot reliably carry a stay. Refusing on "a fence was open before this block
    // started" is the fail-closed side of a question the line scan cannot answer,
    // not a proxy that happens to be wrong here.
    let md = "- ```txt\n  code\n  ```\n\nA paragraph.\n```\nAfter.\n";
    assert_eq!(code_lines(md), lines(&[3, 4, 5, 6]));
    assert!(stamp(md, &StampOptions::default(), counter()).minted.is_empty());
}

#[test]
fn a_real_blank_line_fence_refuses_the_half_that_straddles_its_close() {
    // No phantom anywhere: a genuine fence with an internal blank line, which the
    // baseline segmenter splits. The second half runs past the closing fence into
    // the prose after it, so its body would be `b`, the closing fence, and a line
    // of prose. §3.3: "under the baseline segmenter the halves of such a fence are
    // simply not stampable, which is where §5.2 already arrives." (Python pins the
    // §5.2 half of this case; that segmenter is deferred from this core.)
    let md = "```text\na\n\nb\n```\nprose\n";
    let contents: Vec<String> = parse_document(md).iter().map(|b| b.content.clone()).collect();
    assert_eq!(contents, ["```text\na", "b\n```\nprose"]);
    assert!(stamp(md, &StampOptions::default(), counter()).minted.is_empty());
}
