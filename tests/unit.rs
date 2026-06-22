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
    findings.iter().filter(|f| f.code == code).filter_map(|f| f.id.clone()).collect()
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
    let res = resolve(&build_anchors(REORDER_BEFORE), after, DEFAULT_THRESHOLD, DEFAULT_MARGIN);
    assert_eq!(find(&res, "ing").method, "marker");
    assert_eq!(find(&res, "dlq").method, "marker");
}

#[test]
fn hash_tier_stripped_reordered_verbatim_recovers_by_hash() {
    let after = "Invalid payloads route to a dead-letter queue for replay.\n\n\
         The order pipeline ingests and normalizes partner messages.\n";
    let res = resolve(&build_anchors(REORDER_BEFORE), after, DEFAULT_THRESHOLD, DEFAULT_MARGIN);
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
    let sel = Selector { quote: "the quick brown fox jumps".to_string(), ..Default::default() };
    let bm = best_match(&sel, &cands);
    assert_eq!(bm.index, 0);
    assert_eq!(bm.score, 1.0);
}

#[test]
fn quote_matcher_no_good_match_scores_below_threshold() {
    let cands =
        ["the quick brown fox jumps".to_string(), "a totally different sentence here".to_string()];
    let sel = Selector { quote: "completely unrelated text xyz".to_string(), ..Default::default() };
    assert!(best_match(&sel, &cands).score < 0.5);
}

// --- write path: ported from impl/js/test/stamp.test.js (SPEC.md §3/§4/§6/§7/§8)
//
// The strong invariants checked here are: stamping never changes block bodies,
// the result lints clean, and every write op is idempotent.

use markstay::{
    find_markers, format_attr_value, format_marker, is_id_charset, mint_id, repair_duplicates,
    restamp, stamp, FormatError, Renamed, RestampOptions, StampOptions, Syntax, DEFAULT_ALPHABET,
};

/// Deterministic id factory `id00, id01, ...` for reproducible assertions.
/// Collision-avoidance in the write helpers wraps this, so plain sequential ids
/// are fine.
fn counter(prefix: &'static str) -> impl FnMut() -> String {
    let mut n = 0u32;
    move || {
        let s = format!("{}{:02}", prefix, n);
        n += 1;
        s
    }
}

/// A factory yielding a fixed list of proposals in order (for collision tests).
fn seq(ids: Vec<String>) -> impl FnMut() -> String {
    let mut i = 0usize;
    move || {
        let s = ids[i].clone();
        i += 1;
        s
    }
}

fn bodies(md: &str) -> Vec<String> {
    parse_document(md).into_iter().filter(|b| b.index >= 0).map(|b| b.content).collect()
}

fn all_codes(md: &str) -> Vec<&'static str> {
    let (_, findings) = lint_document(md);
    findings.iter().map(|f| f.code).collect()
}

fn error_codes(md: &str) -> Vec<&'static str> {
    let (_, findings) = lint_document(md);
    findings.iter().filter(|f| f.level.as_str() == "error").map(|f| f.code).collect()
}

const DOC: &str = "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n\n- a\n- b\n";

// --- mint_id (§6) ---

#[test]
fn mint_id_default_ids_match_charset_and_length() {
    let mut urandom = |k: usize| {
        // A spread of bytes (including some >= the rejection limit) so the
        // rejection loop is exercised across draws.
        (0..k).map(|i| ((i as u32 * 37 + 5) % 256) as u8).collect::<Vec<u8>>()
    };
    for _ in 0..200 {
        let id = mint_id(8, DEFAULT_ALPHABET, &mut urandom);
        assert_eq!(id.chars().count(), 8);
        assert!(is_id_charset(&id), "{} not in charset", id);
    }
}

#[test]
fn mint_id_injectable_byte_source_is_deterministic() {
    let zeros = |k: usize| vec![0u8; k]; // every byte 0 -> alphabet[0] = 'A'
    assert_eq!(mint_id(8, DEFAULT_ALPHABET, zeros), "AAAAAAAA");
    assert_eq!(mint_id(3, DEFAULT_ALPHABET, zeros), "AAA");
}

#[test]
#[should_panic]
fn mint_id_rejects_zero_length() {
    mint_id(0, DEFAULT_ALPHABET, |k| vec![0u8; k]);
}

#[test]
#[should_panic]
fn mint_id_rejects_degenerate_alphabet() {
    mint_id(8, "x", |k| vec![0u8; k]);
}

// --- format_attr_value / format_marker (§3 / §4) ---

#[test]
fn format_attr_value_bare_vs_quoted_with_escaping() {
    assert_eq!(format_attr_value("sha256:7a9c").unwrap(), "sha256:7a9c");
    assert_eq!(format_attr_value("two words").unwrap(), "\"two words\"");
    assert_eq!(format_attr_value("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
}

#[test]
fn format_attr_value_rejects_outside_qchar_set() {
    // §4 qchar is printable ASCII only; tab/control/non-ASCII have no form.
    assert!(matches!(format_attr_value("tab\there"), Err(FormatError::NonQchar(_))));
    assert!(matches!(format_attr_value("café"), Err(FormatError::NonQchar(_))));
}

#[test]
fn format_marker_html_and_mdx_round_trip_through_find_markers() {
    let html = format_marker("8f24", Some("7a9c"), &[], Syntax::Html).unwrap();
    assert_eq!(html, "<!-- stay:8f24 hash=sha256:7a9c -->");
    let mdx = format_marker("8f24", Some("7a9c"), &[], Syntax::Mdx).unwrap();
    assert_eq!(mdx, "{/* stay:8f24 hash=sha256:7a9c */}");
    for raw in [&html, &mdx] {
        let mk = &find_markers(raw, 0)[0];
        assert_eq!(mk.id.as_deref(), Some("8f24"));
        assert_eq!(mk.hash.as_deref(), Some("7a9c"));
        assert!(!mk.malformed);
    }
}

#[test]
fn format_marker_extension_attrs_and_uppercase_hash_folds_lower() {
    let m =
        format_marker("x1", Some("ABCD"), &[("x-acme-note", "hi there")], Syntax::Html).unwrap();
    assert_eq!(m, "<!-- stay:x1 hash=sha256:abcd x-acme-note=\"hi there\" -->");
}

#[test]
fn format_marker_rejects_bad_id_non_hex_and_terminator_values() {
    assert!(format_marker("bad id", None, &[], Syntax::Html).is_err());
    assert!(format_marker("ok", Some("zz"), &[], Syntax::Html).is_err());
    assert!(format_marker("ok", None, &[("x-k", "a-->b")], Syntax::Html).is_err());
    assert!(format_marker("ok", None, &[("x-k", "a*/}b")], Syntax::Mdx).is_err());
    // A value bearing a newline is rejected by the qchar guard inside the marker.
    assert!(format_marker("x", None, &[("x-v", "line\nbreak")], Syntax::Html).is_err());
}

// --- stamp (§5 / §6 / §8) ---

#[test]
fn stamp_marks_every_unmarked_block_leaves_bodies_unchanged_lints_clean() {
    let before = bodies(DOC);
    let res = stamp(DOC, &StampOptions::default(), counter("id"));
    assert_eq!(res.minted.len(), before.len()); // one id per content block
    assert_eq!(bodies(&res.text), before); // bodies untouched
    assert!(error_codes(&res.text).is_empty()); // clean
    for b in parse_document(&res.text).iter().filter(|x| x.index >= 0) {
        let ids = b.markers.iter().filter(|m| m.id.is_some() && !m.malformed).count();
        assert_eq!(ids, 1);
    }
}

#[test]
fn stamp_canonical_trailing_shape_with_fresh_matching_hash() {
    let res = stamp("Hello world.", &StampOptions::default(), || "abc12345".to_string());
    let h = body_hash("Hello world.", Some(12));
    assert_eq!(res.text, format!("Hello world.\n<!-- stay:abc12345 hash=sha256:{h} -->"));
}

#[test]
fn stamp_idempotent_and_leaves_already_marked_blocks_alone() {
    let once = stamp(DOC, &StampOptions::default(), counter("a")).text;
    let twice = stamp(&once, &StampOptions::default(), counter("b"));
    assert_eq!(twice.minted.len(), 0);
    assert_eq!(twice.text, once);
}

#[test]
fn stamp_marker_only_chunk_after_a_block_already_identifies_it() {
    let md = "Para body.\n\n<!-- stay:keep hash=sha256:0000 -->\n\nOther.";
    let res = stamp(md, &StampOptions::default(), || "new0".to_string());
    assert_eq!(res.minted.len(), 1); // only "Other." is unmarked
    assert_eq!(res.minted[0].id, "new0");
    assert!(res.text.contains("stay:keep"));
}

#[test]
fn stamp_minted_ids_never_collide_with_existing_ids() {
    let md = "A.\n<!-- stay:id00 -->\n\nB.";
    // factory would re-propose id00; collision-avoidance must skip it
    let res =
        stamp(md, &StampOptions::default(), seq(vec!["id00".into(), "id00".into(), "id01".into()]));
    assert_eq!(res.minted.len(), 1);
    assert_eq!(res.minted[0].id, "id01");
}

#[test]
fn stamp_mdx_syntax_and_no_hash() {
    let opts = StampOptions { syntax: Syntax::Mdx, hash: false, ..Default::default() };
    let res = stamp("Body.", &opts, || "m1".to_string());
    assert_eq!(res.text, "Body.\n{/* stay:m1 */}");
}

#[test]
fn stamp_hash_length_controls_written_precision() {
    let opts = StampOptions { hash_length: 4, ..Default::default() };
    let res = stamp("Body.", &opts, || "h1".to_string());
    let mk = &find_markers(&res.text, 0)[0];
    let h = mk.hash.as_deref().unwrap();
    assert_eq!(h.len(), 4);
    assert_eq!(h, body_hash("Body.", Some(4)));
}

// --- restamp (§8) ---

#[test]
fn restamp_refreshes_a_drifted_hash_and_then_lints_clean() {
    let stamped = stamp("Original body.", &StampOptions::default(), || "r1".to_string()).text;
    let edited = stamped.replace("Original body.", "Edited body now.");
    assert_eq!(all_codes(&edited), ["HASH_DRIFT"]);
    let res = restamp(&edited, &RestampOptions::default());
    assert_eq!(res.refreshed, ["r1"]);
    assert!(all_codes(&res.text).is_empty());
}

#[test]
fn restamp_no_op_when_nothing_drifted() {
    let stamped = stamp(DOC, &StampOptions::default(), counter("id")).text;
    let res = restamp(&stamped, &RestampOptions::default());
    assert!(res.refreshed.is_empty());
    assert_eq!(res.text, stamped);
}

#[test]
fn restamp_preserves_each_markers_stored_hash_precision() {
    // stored 4-char hash, content changed -> refreshed value is still 4 chars
    let md = "New text here.\n<!-- stay:p1 hash=sha256:0000 -->";
    let res = restamp(md, &RestampOptions::default());
    let mk = &find_markers(&res.text, 0)[0];
    let h = mk.hash.as_deref().unwrap();
    assert_eq!(h.len(), 4);
    assert_eq!(h, body_hash("New text here.", Some(4)));
}

#[test]
fn restamp_add_missing_gives_a_hashless_marker_a_hash() {
    let md = "Body text.\n<!-- stay:n1 -->";
    let res = restamp(md, &RestampOptions { add_missing: true, ..Default::default() });
    assert_eq!(res.refreshed, ["n1"]);
    let mk = &find_markers(&res.text, 0)[0];
    assert_eq!(mk.hash.as_deref(), Some(body_hash("Body text.", Some(12)).as_str()));
}

// --- repair_duplicates (§7) ---

#[test]
fn repair_first_occurrence_kept_later_reminted_lints_clean() {
    let md = "Para one.\n<!-- stay:dup hash=sha256:0000 -->\n\n\
              Para two.\n<!-- stay:dup hash=sha256:1111 -->";
    assert!(error_codes(md).contains(&"DUPLICATE_ID"));
    let res = repair_duplicates(md, || "fresh1".to_string());
    assert_eq!(res.renamed, vec![Renamed { from: "dup".into(), to: "fresh1".into() }]);
    assert!(res.text.contains("stay:dup")); // first kept
    assert!(res.text.contains("stay:fresh1")); // second re-minted
    assert!(error_codes(&res.text).is_empty());
}

#[test]
fn repair_two_same_id_markers_on_one_block() {
    let md = "A.\n<!-- stay:dup -->\n<!-- stay:dup -->";
    assert!(error_codes(md).contains(&"DUPLICATE_ID"));
    let res = repair_duplicates(md, || "fresh1".to_string());
    assert_eq!(res.renamed, vec![Renamed { from: "dup".into(), to: "fresh1".into() }]);
    assert!(error_codes(&res.text).is_empty());
}

#[test]
fn repair_no_op_when_there_are_no_duplicates() {
    let md = stamp(DOC, &StampOptions::default(), counter("id")).text;
    let res = repair_duplicates(&md, || "unused".to_string());
    assert!(res.renamed.is_empty());
    assert_eq!(res.text, md);
}

#[test]
fn repair_reminted_id_never_collides_with_existing_id() {
    let md = "One.\n<!-- stay:dup -->\n\nTwo.\n<!-- stay:dup -->\n\nThree.\n<!-- stay:taken -->";
    // first proposal clashes with an existing id, must be skipped
    let res = repair_duplicates(md, seq(vec!["taken".into(), "ok1".into()]));
    assert_eq!(res.renamed, vec![Renamed { from: "dup".into(), to: "ok1".into() }]);
}
