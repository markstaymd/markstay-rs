// Behavioral unit tests, ported from impl/js/test/unit.test.js (which ports
// linter/test_lint.py and eval/attachment/test_attach.py). These assert parity
// beyond the shared corpus: the lint codes, the regeneration-diff codes
// (DROPPED_ID / DUPLICATED_ID / RELOCATED_ID), the resolver ladder
// (marker -> hash -> quote -> detached) including the "surface, don't guess"
// margin guard, and the SHA-256 FIPS vectors backing the vendored primitive.
// CommonMark-mode cases (SPEC.md §5.2) are deferred from the parser-free core.

use markstay::{
    best_match, body_hash, build_anchors, check_entries, has_errors, lint_diff, lint_document,
    parse_document, resolve, CommitEntry, Finding, Selector, DEFAULT_MARGIN, DEFAULT_THRESHOLD,
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
fn orphan_attribution_precedes_subhash_exclusion_and_hash_drift() {
    for marker in [
        "<!-- stay:digest subhash=sha256:dead hash=sha256:dead -->",
        "<!-- stay:bare subhash=bogus hash=sha256:dead -->",
        "<!-- stay:quoted subhash=\"sha256:dead\" hash=sha256:dead -->",
        "<!-- stay:extension x-subhash=bogus hash=sha256:dead -->",
    ] {
        let md = format!("{marker}\n\nReal content below.\n");
        let (_, findings) = lint_document(&md);
        assert_eq!(codes_sorted(&findings), ["ORPHAN_MARKER"], "{marker}");
        assert!(!findings.iter().any(|f| f.code == "HASH_DRIFT"), "{marker}");
    }
}

#[test]
fn hash_drift_is_a_warning_not_an_error() {
    let (_, findings) = lint_document("Edited content.\n<!-- stay:z9 hash=sha256:dead -->\n");
    assert_eq!(codes_sorted(&findings), ["HASH_DRIFT"]);
    assert!(!has_errors(&findings));
}

#[test]
fn subhash_markers_stay_lexical_for_duplicates_but_never_bind_to_containers() {
    let md = "<!-- stay:child subhash=bogus hash=sha256:dead -->\n\n\
              Body.\n<!-- stay:child subhash=bogus hash=sha256:dead -->\n";
    let (_, findings) = lint_document(md);
    assert_eq!(codes_sorted(&findings), ["DUPLICATE_ID", "ORPHAN_MARKER"]);
}

#[test]
fn x_subhash_is_an_ordinary_block_attribute() {
    let (_, findings) =
        lint_document("Body.\n<!-- stay:block x-subhash=sha256:abcd hash=sha256:dead -->\n");
    assert_eq!(codes_sorted(&findings), ["HASH_DRIFT"]);
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

#[test]
fn diff_ignores_child_ids_as_container_identity() {
    let before = "Body.\n<!-- stay:child subhash=bogus -->\n";
    assert!(lint_diff(before, "Body.\n").is_empty());
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
fn anchors_and_marker_lookup_exclude_exact_key_subhash_markers() {
    let before = "Body.\n\
                  <!-- stay:child subhash=bogus -->\n\
                  <!-- stay:custom x-subhash=bogus -->\n\
                  <!-- stay:parent -->\n";
    assert_eq!(
        build_anchors(before).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["custom", "parent"]
    );

    let ordinary = "Body.\n<!-- stay:c -->\n";
    let child = "Body.\n<!-- stay:c subhash=bogus -->\n";
    let result = resolve(&build_anchors(ordinary), child, DEFAULT_THRESHOLD, DEFAULT_MARGIN);
    assert_eq!(find(&result, "c").method, "hash");
}

#[test]
fn staged_pairing_ignores_overlap_supplied_only_by_child_ids() {
    let entries = [
        CommitEntry {
            status: 'D',
            source: "old.md",
            destination: "old.md",
            before: Some("Old.\n<!-- stay:c subhash=bogus -->\n"),
            after: None,
        },
        CommitEntry {
            status: 'A',
            source: "new.md",
            destination: "new.md",
            before: None,
            after: Some("New.\n<!-- stay:c subhash=bogus -->\n"),
        },
    ];
    let result = check_entries(&entries, &[]);
    assert_eq!(result.pairings.len(), 1);
    assert_eq!(result.pairings[0].path, "new.md");
    assert_eq!(result.pairings[0].baseline, None);
    assert!(result.notes.is_empty());
    assert!(result.reports.is_empty());
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
fn format_attr_value_rejects_outside_one_line_writer_set() {
    // The §3.3 writer set is printable ASCII only. Normalized LF is valid reader
    // qchar inside a quoted value (§4), but writer output must remain one line.
    assert!(matches!(format_attr_value("tab\there"), Err(FormatError::NonQchar(_))));
    assert!(matches!(format_attr_value("line\nbreak"), Err(FormatError::NonQchar(_))));
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
    assert!(format_marker("ok", None, &[("x-k", "a--!>b")], Syntax::Html).is_err());
    assert!(format_marker("ok", None, &[("x-k", "a*/b")], Syntax::Mdx).is_err());
    // Normalized LF is reader syntax only; the §3.3 writer contract rejects it.
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

#[test]
fn restamp_add_missing_leaves_a_subhash_marker_alone() {
    // SPEC.md §5.5: a child marker addresses the list item, so the list's digest
    // must not be written in beside it. This build has no child support at all,
    // which is exactly the tool the rule is aimed at.
    let md = "- Alpha <!-- stay:k1 subhash=sha256:8655 -->\n- Beta\n";
    let res = restamp(md, &RestampOptions { add_missing: true, ..Default::default() });
    assert_eq!(res.text, md);
    assert!(res.refreshed.is_empty());
}

#[test]
fn restamp_arbitrary_subhash_and_stale_container_hash_remain_untouched() {
    let md = "- Alpha <!-- stay:k1 subhash=bogus hash=sha256:dead -->\n- Beta\n";
    let opts = RestampOptions { add_missing: true, ..Default::default() };
    let result = restamp(md, &opts);
    assert_eq!(result.text, md);
    assert!(result.refreshed.is_empty());
}

#[test]
fn restamp_edits_the_parsed_hash_not_hash_text_inside_a_quoted_value() {
    let md = "Body.\n<!-- stay:x x-note=\"hash=sha256:beef\" hash=sha256:dead -->\n";
    let result = restamp(md, &RestampOptions::default());
    assert!(result.text.contains("x-note=\"hash=sha256:beef\""));
    assert!(!result.text.contains("hash=sha256:dead"));
}

#[test]
fn stamp_subhash_marker_does_not_make_its_block_stamped() {
    // SPEC.md §16, the other half of the write-path shim: a tool with no child
    // support must not read child markers as evidence the list is done, or the
    // container never gets a stay and every child resolves on tier-4 evidence.
    let md = "- Ship the linter <!-- stay:c1 subhash=sha256:9d2f -->\n\
              - Ship the hook <!-- stay:c2 subhash=sha256:41ac -->\n";
    let res = stamp(md, &StampOptions::default(), || "cont1".to_string());
    assert_eq!(res.minted.len(), 1);
    assert_eq!(res.minted[0].id, "cont1");
    assert!(res.text.contains("<!-- stay:cont1 hash=sha256:"));
    assert!(res.text.contains("stay:c1 subhash=sha256:9d2f"));
    assert!(res.text.contains("stay:c2 subhash=sha256:41ac"));
}

#[test]
fn stamp_arbitrary_subhash_does_not_make_its_block_stamped() {
    let md = "A paragraph.\n<!-- stay:c1 subhash=bogus -->\n";
    let opts = StampOptions { hash: false, ..Default::default() };
    let result = stamp(md, &opts, || "parent".to_string());
    assert_eq!(result.minted.len(), 1);
    assert_eq!(result.minted[0].id, "parent");
    assert!(result.text.contains("stay:c1 subhash=bogus"));
    assert!(result.text.contains("stay:parent"));
}

#[test]
fn stamp_custom_key_ending_in_subhash_is_not_the_reserved_key() {
    // SPEC.md §4: the boundary is whitespace, not a word boundary. A hyphen is not a
    // word byte, so a word boundary would read `x-subhash` as the reserved key and
    // mint a second stay onto a block that already has one.
    let md = "A paragraph.\n<!-- stay:x1 x-subhash=sha256:abcd -->\n";
    let res = stamp(md, &StampOptions::default(), || "spurious".to_string());
    assert!(res.minted.is_empty());
    assert_eq!(res.text, md);
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

// --- leading YAML frontmatter is metadata, not a block (SPEC.md §5) ---------

const FM_DOC: &str = "---\nstatus: active\nowner: tim\n---\n\n# Heading\n\nBody para.\n";

fn contents(md: &str) -> Vec<String> {
    parse_document(md).into_iter().map(|b| b.content).collect()
}

#[test]
fn frontmatter_is_not_a_block() {
    assert_eq!(contents(FM_DOC), ["# Heading", "Body para."]);
}

#[test]
fn frontmatter_does_not_shift_line_numbers() {
    // blanking is line-for-line, so reported lines stay true to the source
    let got: Vec<(i64, usize)> = parse_document(FM_DOC).iter().map(|b| (b.index, b.line)).collect();
    assert_eq!(got, [(0, 6), (1, 8)]);
}

#[test]
fn frontmatter_metadata_edit_does_not_drift_a_content_hash() {
    let before: Vec<String> =
        parse_document(FM_DOC).iter().map(|b| body_hash(&b.content, None)).collect();
    let edited = FM_DOC.replace("status: active", "status: complete");
    let after: Vec<String> =
        parse_document(&edited).iter().map(|b| body_hash(&b.content, None)).collect();
    assert_eq!(after, before);
}

#[test]
fn leading_thematic_break_with_no_closing_fence_is_a_block() {
    assert_eq!(contents("---\n\n# Heading\n\nBody para.\n"), ["---", "# Heading", "Body para."]);
}

#[test]
fn frontmatter_does_not_swallow_a_paragraph_between_two_thematic_breaks() {
    // Regression, found by external review of the reference: the naive
    // first-closing-fence rule silently ate `Intro paragraph.`
    assert_eq!(
        contents("---\n\nIntro paragraph.\n\n---\n\nBody.\n"),
        ["---", "Intro paragraph.", "---", "Body."]
    );
}

#[test]
fn frontmatter_does_not_swallow_a_setext_heading() {
    // `---` / `Title` / `---` is a thematic break plus a setext H2; the payload has
    // to look like YAML before the span is treated as metadata.
    assert!(contents("---\nTitle\n---\n\nBody.\n").join("\n").contains("Title"));
}

#[test]
fn frontmatter_does_not_swallow_an_atx_heading() {
    // Regression, found by external review: a YAML comment and an ATX heading are
    // byte-identical, so `#` cannot be the evidence that a span is frontmatter.
    assert!(contents("---\n# Heading\n---\nBody.\n").join("\n").contains("# Heading"));
    assert!(contents("---\n# just a comment\n---\n\nBody.\n")
        .join("\n")
        .contains("# just a comment"));
}

#[test]
fn a_payload_with_a_blank_line_is_not_frontmatter() {
    // Fails towards ordinary Markdown: not skipping is a stray drift warning, while
    // over-skipping silently destroys content.
    assert!(contents("---\nstatus: active\n\nowner: tim\n---\n\nBody.\n")
        .iter()
        .any(|c| c.contains("status: active")));
}

#[test]
fn an_empty_payload_is_not_frontmatter() {
    assert!(contents("---\n---\n\nBody.\n").iter().any(|c| c.contains("---")));
}

#[test]
fn yamlish_payload_forms_are_recognized() {
    for payload in ["status: active", "- one\n- two", "empty:", "nested:\n  a: 1"] {
        let md = format!("---\n{payload}\n---\n\nBody.\n");
        assert_eq!(contents(&md), ["Body."], "payload: {payload:?}");
    }
}

#[test]
fn the_closing_fence_tolerates_trailing_whitespace_and_accepts_dots() {
    assert_eq!(contents("---\nkey: v\n---   \n\nBody.\n"), ["Body."]);
    assert_eq!(contents("---\ntitle: t\n...\n\n# Heading\n\nBody.\n"), ["# Heading", "Body."]);
}

#[test]
fn crlf_is_normalized_before_frontmatter_detection() {
    assert_eq!(contents("---\r\nkey: v\r\n---\r\n\r\n# H\r\n\r\nBody.\r\n"), ["# H", "Body."]);
}

#[test]
fn frontmatter_is_only_recognized_at_document_start() {
    let md = "# Heading\n\n---\ntitle: not frontmatter\n---\n\nBody.\n";
    assert!(contents(md).join("\n").contains("title: not frontmatter"));
}

#[test]
fn no_blank_line_after_the_closing_fence_still_splits_correctly() {
    // A filter-the-chunks-afterwards implementation gets this one wrong.
    assert_eq!(contents("---\ntitle: t\n---\n# Heading\n\nBody.\n"), ["# Heading", "Body."]);
}

#[test]
fn the_yamlish_test_is_ascii_pinned_not_runtime_whitespace() {
    // Cross-language agreement, found by external review of the port: `\S` means
    // three different things in Python, ECMAScript and Rust, so the rule spells the
    // ASCII set out. An ASCII control character is not a key start (the span stays
    // ordinary Markdown); an exotic non-ASCII space is, exactly as for hashing (§8),
    // where NBSP is content rather than whitespace.
    assert!(
        contents("---\n\u{1c}key: v\n---\n\nBody.\n").iter().any(|c| c.contains("key: v")),
        "an ASCII control is not a key start"
    );
    for ch in ['\u{a0}', '\u{85}', '\u{feff}'] {
        let md = format!("---\n{ch}key: v\n---\n\nBody.\n");
        assert_eq!(contents(&md), ["Body."], "key start {ch:?}");
        let md_item = format!("---\n- {ch}\n---\n\nBody.\n");
        assert_eq!(contents(&md_item), ["Body."], "list item {ch:?}");
    }
}

#[test]
fn a_marker_after_the_closing_fence_is_an_orphan() {
    // The visible consequence for a document stamped before this change.
    assert!(all_codes("---\nkey: v\n---\n<!-- stay:x -->\n\nBody.\n").contains(&"ORPHAN_MARKER"));
}

#[test]
fn a_marker_inside_the_frontmatter_payload_is_dropped() {
    // Pins actual behaviour: the marker is blanked with the rest of the metadata
    // and raises nothing. No tool writes one there (the stamper writes after the
    // block), so this is documented rather than defended.
    assert!(all_codes("---\nkey: v\n<!-- stay:x -->\n---\n\nBody.\n").is_empty());
}

#[test]
fn stamp_leaves_frontmatter_unmarked_and_lints_clean() {
    // The write path segments through the same frontmatter skip as the read path. A
    // stamper that missed it would mint an id for the metadata, and the linter would
    // then (correctly) report ORPHAN_MARKER on the marker it had just written.
    let doc = "---\nstatus: active\nowner: tim\n---\n\n# Title\n\nBody paragraph.\n";
    let res = stamp(doc, &StampOptions::default(), counter("id"));
    assert_eq!(res.minted.len(), 2); // the heading and the paragraph, not the metadata
    assert!(res.text.starts_with("---\nstatus: active\nowner: tim\n---\n"));
    assert!(error_codes(&res.text).is_empty());

    // blanking is line-for-line, so the insertion point still indexes the source
    let lines: Vec<&str> = res.text.split('\n').collect();
    assert_eq!(lines[5], "# Title");
    assert!(lines[6].starts_with("<!-- stay:id00"));

    // and a metadata-only edit of the stamped document stays clean
    let edited = res.text.replace("status: active", "status: complete");
    assert!(all_codes(&edited).is_empty());
}

#[test]
fn stamp_still_identifies_a_leading_thematic_break() {
    let res = stamp("---\n\nBody.\n", &StampOptions::default(), counter("id"));
    assert_eq!(res.minted.len(), 2);
    assert!(error_codes(&res.text).is_empty());
}

#[test]
fn restamp_finds_no_hash_to_refresh_in_frontmatter() {
    let doc = "---\nstatus: active\nowner: tim\n---\n\n# Title\n\nBody paragraph.\n";
    let stamped = stamp(doc, &StampOptions::default(), counter("id")).text;
    let edited = stamped.replace("owner: tim", "owner: someone");
    let res = restamp(&edited, &RestampOptions::default());
    assert!(res.refreshed.is_empty());
    assert_eq!(res.text, edited);
}
