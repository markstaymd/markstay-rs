// Behavioral unit tests, ported from impl/js/test/unit.test.js (which ports
// linter/test_lint.py and eval/attachment/test_attach.py). These assert parity
// beyond the shared corpus: the lint codes, the regeneration-diff codes
// (DROPPED_ID / DUPLICATED_ID / RELOCATED_ID), the resolver ladder
// (marker -> hash -> quote -> detached) including the "surface, don't guess"
// margin guard, and the SHA-256 FIPS vectors backing the vendored primitive.
// CommonMark-mode cases (SPEC.md §5.2) are deferred from the parser-free core.

use markstay::{
    best_match, body_hash, build_anchors, has_errors, lint_diff, lint_document, parse_document,
    resolve, Finding, Selector, DEFAULT_MARGIN, DEFAULT_THRESHOLD,
};

fn codes_sorted(findings: &[Finding]) -> Vec<&'static str> {
    let mut c: Vec<&'static str> = findings.iter().map(|f| f.code).collect();
    c.sort_unstable();
    c
}

fn ids_for(findings: &[Finding], code: &str) -> Vec<String> {
    findings
        .iter()
        .filter(|f| f.code == code)
        .filter_map(|f| f.id.clone())
        .collect()
}

// --- vendored SHA-256: FIPS-180 vectors (backs hash.json) -------------------

#[test]
fn sha256_fips_vectors() {
    // The two canonical FIPS-180-4 examples, full digests.
    assert_eq!(
        body_hash("", None),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        body_hash("abc", None),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// --- linter: well-formedness + intra-doc (ported from test_lint.py) ---------

#[test]
fn clean_doc_with_correct_hash_has_no_findings() {
    let body = "The order pipeline ingests messages and normalizes them.";
    let h = body_hash(body, Some(4));
    let md = format!(
        "{body}\n<!-- stay:8f24 hash=sha256:{h} -->\n\n\
         A second paragraph that is also identified.\n<!-- stay:a1b2 -->\n"
    );
    let (_, findings) = lint_document(&md);
    assert!(codes_sorted(&findings).is_empty());
    assert!(!has_errors(&findings));
}

#[test]
fn uppercase_hex_hash_does_not_report_drift() {
    let body = "Users authenticate with an API key in the Authorization header.";
    let h = body_hash(body, Some(4)).to_uppercase();
    let md = format!("{body}\n<!-- stay:8f24 hash=sha256:{h} -->\n");
    let (_, findings) = lint_document(&md);
    assert!(codes_sorted(&findings).is_empty());
    assert!(!has_errors(&findings));
}

#[test]
fn marker_with_no_blank_line_attaches_to_block_above() {
    let blocks = parse_document("Just one paragraph.\n<!-- stay:p1 -->\n");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].content, "Just one paragraph.");
    let ids: Vec<&str> = blocks[0].markers.iter().filter_map(|m| m.id.as_deref()).collect();
    assert_eq!(ids, ["p1"]);
}

#[test]
fn marker_only_chunk_attaches_to_previous_content_block() {
    let blocks = parse_document("Some content.\n\n<!-- stay:x -->\n");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].content, "Some content.");
    let ids: Vec<&str> = blocks[0].markers.iter().filter_map(|m| m.id.as_deref()).collect();
    assert_eq!(ids, ["x"]);
}

#[test]
fn duplicate_id_is_an_error() {
    let md = "Block one.\n<!-- stay:dup -->\n\nBlock two.\n<!-- stay:dup -->\n";
    let (_, findings) = lint_document(md);
    assert!(codes_sorted(&findings).contains(&"DUPLICATE_ID"));
    assert!(has_errors(&findings));
}

#[test]
fn malformed_marker_no_id_is_reported() {
    let (_, findings) = lint_document("A paragraph.\n<!-- stay:note=hello -->\n");
    assert!(codes_sorted(&findings).contains(&"MALFORMED_MARKER"));
}

#[test]
fn orphan_marker_at_top_is_reported() {
    let (_, findings) = lint_document("<!-- stay:loose -->\n\nReal content below.\n");
    assert!(codes_sorted(&findings).contains(&"ORPHAN_MARKER"));
}

#[test]
fn hash_drift_is_a_warning_not_an_error() {
    let (_, findings) = lint_document("Edited content.\n<!-- stay:z9 hash=sha256:dead -->\n");
    assert_eq!(codes_sorted(&findings), ["HASH_DRIFT"]);
    assert!(!has_errors(&findings));
}

#[test]
fn mdx_marker_is_parsed_with_mdx_syntax() {
    let blocks = parse_document("An MDX block.\n{/* stay:mdx1 hash=sha256:abcd */}\n");
    assert_eq!(blocks[0].markers[0].id.as_deref(), Some("mdx1"));
    assert_eq!(blocks[0].markers[0].syntax.as_str(), "mdx");
}

// --- regeneration diff (ported from test_lint.py) ---------------------------

#[test]
fn diff_reports_a_dropped_id() {
    let before = "A.\n<!-- stay:a -->\n\nB.\n<!-- stay:b -->\n";
    let after = "A.\n<!-- stay:a -->\n\nB rewritten without its marker.\n";
    let findings = lint_diff(before, after);
    assert_eq!(ids_for(&findings, "DROPPED_ID"), ["b"]);
    assert!(has_errors(&findings));
}

#[test]
fn diff_reports_a_duplicated_id() {
    let before = "A.\n<!-- stay:a -->\n";
    let after = "A.\n<!-- stay:a -->\n\nCopy of A.\n<!-- stay:a -->\n";
    assert!(codes_sorted(&lint_diff(before, after)).contains(&"DUPLICATED_ID"));
}

#[test]
fn diff_reports_a_new_id_as_info_not_error() {
    let before = "A.\n<!-- stay:a -->\n";
    let after = "A.\n<!-- stay:a -->\n\nBrand new block.\n<!-- stay:c -->\n";
    let findings = lint_diff(before, after);
    assert_eq!(ids_for(&findings, "NEW_ID"), ["c"]);
    assert!(!has_errors(&findings));
}

#[test]
fn diff_reports_an_exact_content_relocation_swap() {
    let before = "Alpha content.\n<!-- stay:aaa -->\n\nBeta content.\n<!-- stay:bbb -->\n";
    let after = "Beta content.\n<!-- stay:aaa -->\n\nAlpha content.\n<!-- stay:bbb -->\n";
    let findings = lint_diff(before, after);
    let mut relocated = ids_for(&findings, "RELOCATED_ID");
    relocated.sort();
    assert_eq!(relocated, ["aaa", "bbb"]);
    assert!(has_errors(&findings));
}

#[test]
fn diff_treats_an_in_place_edit_as_drift_not_relocation() {
    let before = "Alpha content.\n<!-- stay:aaa -->\n";
    let after = "Alpha content, now revised.\n<!-- stay:aaa -->\n";
    assert_eq!(codes_sorted(&lint_diff(before, after)), ["HASH_DRIFT"]);
}

// --- resolver ladder (ported / adapted from test_attach.py) -----------------

const REORDER_BEFORE: &str = "The order pipeline ingests and normalizes partner messages.\n\
     <!-- stay:ing -->\n\n\
     Invalid payloads route to a dead-letter queue for replay.\n<!-- stay:dlq -->\n";

fn find<'a>(res: &'a [markstay::Resolution], id: &str) -> &'a markstay::Resolution {
    res.iter().find(|r| r.id == id).expect("id resolved")
}

#[test]
fn marker_tier_kept_markers_resolve_by_marker() {
    let after = "Invalid payloads route to a dead-letter queue for replay.\n\
         <!-- stay:dlq -->\n\n\
         The order pipeline ingests and normalizes partner messages.\n<!-- stay:ing -->\n";
    let res = resolve(
        &build_anchors(REORDER_BEFORE),
        after,
        DEFAULT_THRESHOLD,
        DEFAULT_MARGIN,
    );
    assert_eq!(find(&res, "ing").method, "marker");
    assert_eq!(find(&res, "dlq").method, "marker");
}

#[test]
fn hash_tier_stripped_reordered_verbatim_recovers_by_hash() {
    let after = "Invalid payloads route to a dead-letter queue for replay.\n\n\
         The order pipeline ingests and normalizes partner messages.\n";
    let res = resolve(
        &build_anchors(REORDER_BEFORE),
        after,
        DEFAULT_THRESHOLD,
        DEFAULT_MARGIN,
    );
    assert_eq!(find(&res, "ing").method, "hash");
    assert_eq!(find(&res, "dlq").method, "hash");
    assert_eq!(find(&res, "ing").target, Some(1));
    assert_eq!(find(&res, "dlq").target, Some(0));
}

#[test]
fn quote_tier_paraphrased_block_recovers_via_quote_selector() {
    let before = "The quick brown fox jumps over the lazy dog.\n<!-- stay:a -->\n\n\
         An entirely unrelated sentence about relational databases.\n<!-- stay:b -->\n";
    let after = "The quick brown fox leaps over the lazy dog.\n\n\
         An entirely unrelated sentence about relational databases.\n";
    let res = resolve(&build_anchors(before), after, DEFAULT_THRESHOLD, DEFAULT_MARGIN);
    assert_eq!(find(&res, "b").method, "hash"); // verbatim survivor
    assert_eq!(find(&res, "a").method, "quote"); // paraphrased, recovered by quote
    assert_eq!(find(&res, "a").target, Some(0));
}

#[test]
fn deleted_block_resolves_to_detached() {
    let before = "Only block here.\n<!-- stay:solo -->\n";
    let res = resolve(&build_anchors(before), "", DEFAULT_THRESHOLD, DEFAULT_MARGIN);
    assert_eq!(find(&res, "solo").method, "detached");
    assert_eq!(find(&res, "solo").target, None);
}

#[test]
fn clone_refuses_to_guess_identical_twins_detach() {
    let before = "Same body.\n<!-- stay:a -->\n\nSame body.\n<!-- stay:b -->\n";
    let after = "Same body.\n\nSame body.\n";
    let res = resolve(&build_anchors(before), after, DEFAULT_THRESHOLD, DEFAULT_MARGIN);
    assert_eq!(find(&res, "a").method, "detached");
    assert_eq!(find(&res, "b").method, "detached");
}

#[test]
fn margin_guard_lowering_threshold_exposes_a_near_dup_false_attach() {
    let before = "Same body.\n<!-- stay:a -->\n\nSame body.\n<!-- stay:b -->\n";
    let after = "Same body.\n\nSame body.\n";
    let anchors = build_anchors(before);
    let guarded = resolve(&anchors, after, 0.5, 0.05);
    let unguarded = resolve(&anchors, after, 0.3, 0.0);
    assert!(find(&guarded, "a").method == "detached" && find(&guarded, "b").method == "detached");
    assert!(find(&unguarded, "a").method == "quote" || find(&unguarded, "b").method == "quote");
}

// --- quote matcher units (ported from test_attach.py) -----------------------

#[test]
fn quote_matcher_exact_quote_wins_with_score_one() {
    let cands = [
        "the quick brown fox jumps".to_string(),
        "a totally different sentence here".to_string(),
        "the quick brown fox leaps high".to_string(),
    ];
    let sel = Selector {
        quote: "the quick brown fox jumps".to_string(),
        ..Default::default()
    };
    let bm = best_match(&sel, &cands);
    assert_eq!(bm.index, 0);
    assert_eq!(bm.score, 1.0);
}

#[test]
fn quote_matcher_no_good_match_scores_below_threshold() {
    let cands = [
        "the quick brown fox jumps".to_string(),
        "a totally different sentence here".to_string(),
    ];
    let sel = Selector {
        quote: "completely unrelated text xyz".to_string(),
        ..Default::default()
    };
    assert!(best_match(&sel, &cands).score < 0.5);
}
