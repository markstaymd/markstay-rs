// SPEC.md §3.5 (v1.12): a closed inline code span is content, not markup.
//
// A port of impl/py/tests/test_code_spans.py. The case that opened it: a review
// quoted one example marker twice on a line, and every tool reading the file
// reported a duplicate id that no restamp could clear. The rule is a line scan
// and is allowed to read MORE markers than a renderer does, never fewer, so most
// cases below pin where it stops: each one is a document where CommonMark could
// read the marker as a live comment.

use markstay::{
    body_hash, code_lines, find_markers, lint_diff, lint_document, marker_outside_code,
    parse_document, repair_duplicates, restamp, stamp, RestampOptions, StampOptions,
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

fn block_ids(md: &str) -> Vec<Vec<String>> {
    parse_document(md)
        .iter()
        .map(|b| b.markers.iter().filter_map(|m| m.id.clone()).collect())
        .collect()
}

fn ids(groups: &[&[&str]]) -> Vec<Vec<String>> {
    groups.iter().map(|g| g.iter().map(|s| s.to_string()).collect()).collect()
}

// --- what the span mask hides ----------------------------------------------

#[test]
fn a_marker_in_a_closed_span_is_content() {
    let cases = [
        "Shows `<!-- stay:demo -->` inline.\n",
        "Shows `{/* stay:demo */}` inline.\n",
        "Shows ``a ` <!-- stay:demo --> b`` with a longer run.\n",
        "Shows `<!-- stay:demo -->` and `<!-- stay:demo -->` twice.\n",
        "- an item quoting `<!-- stay:demo -->` here\n",
        "> a quote showing `<!-- stay:demo -->` here\n",
        "Shows `<!-- stay:demo -->` inline.\r\n",
        // An ordinary link, inline or by reference, is passed over, and so is a
        // task-list checkbox: none of them can take a backtick.
        "See [doc](u) and `<!-- stay:demo -->` here.\n",
        "See [a][b] and `<!-- stay:demo -->` here.\n",
        "[a][b] `<!-- stay:q -->`\n",
        "- [ ] an item quoting `<!-- stay:demo -->` here\n",
    ];
    for md in cases {
        assert_eq!(block_ids(md), ids(&[&[]]), "{md:?}");
    }
}

#[test]
fn two_identical_quotes_are_not_a_duplicate() {
    let md = "Quote `<!-- stay:Nested01 -->` and `<!-- stay:Nested01 -->` here.\n";
    assert!(lint_document(md).1.is_empty());
}

#[test]
fn a_quoted_marker_is_hashed_with_the_body() {
    let body = "Shows `<!-- stay:demo -->` inline.";
    let good = format!("{body}\n<!-- stay:live hash=sha256:{} -->\n", body_hash(body, Some(12)));
    assert!(lint_document(&good).1.is_empty());
    let stripped = "Shows `` inline.";
    let old = format!("{body}\n<!-- stay:live hash=sha256:{} -->\n", body_hash(stripped, Some(12)));
    let codes: Vec<&str> = lint_document(&old).1.iter().map(|f| f.code).collect();
    assert_eq!(codes, ["HASH_DRIFT"]);
}

#[test]
fn a_real_marker_beside_a_span_still_counts() {
    assert_eq!(block_ids("Shows `code` beside it. <!-- stay:live -->\n"), ids(&[&["live"]]));
    let md = "Shows `<!-- stay:demo -->` then a real one. <!-- stay:live -->\n";
    assert_eq!(block_ids(md), ids(&[&["live"]]));
}

#[test]
fn the_mask_is_by_column_not_by_line() {
    let md = "Shows `<!-- stay:demo -->` then <!-- stay:live -->\n";
    let code = code_lines(md);
    assert!(code.fence.is_empty() && !code.is_empty());
    let markers = find_markers(md, 0);
    let outside: Vec<bool> = markers.iter().map(|mk| marker_outside_code(mk, &code)).collect();
    assert_eq!(outside, [false, true]);
}

#[test]
fn columns_are_bytes_and_agree_with_the_marker_on_a_non_ascii_line() {
    // Rust's unit is the byte, where Python counts code points and JavaScript
    // UTF-16 code units. The span and the marker column use the same unit, so the
    // verdict is the same.
    let md = "Ünïcödé `<!-- stay:u1 -->` 漢字 `<!-- stay:u2 -->` <!-- stay:live -->\n";
    let code = code_lines(md);
    let line = md.trim_end();
    let quoted: Vec<usize> = line.match_indices("<!--").map(|(at, _)| at).collect();
    assert_eq!(code.spans[&1], [(quoted[0], quoted[0] + 16), (quoted[1], quoted[1] + 16)]);
    let markers = find_markers(md, 0);
    let columns: Vec<usize> = markers.iter().map(|mk| mk.column).collect();
    assert_eq!(columns, quoted);
    assert_eq!(block_ids(md), ids(&[&["live"]]));
}

// --- where the scan stops, so the marker stays live -------------------------

#[test]
fn the_scan_leaves_a_possibly_live_marker_alone() {
    let cases: [(&str, &[&[&str]]); 34] = [
        // pandoc's native writer: raw-inline HTML, not code.
        ("Shows `<!-- stay:demo -->`{=html} raw.\n", &[&["demo"]]),
        // GFM splits a table row's cells before any inline scan.
        ("Shows `a | <!-- stay:demo -->` with a pipe.\n", &[&["demo"]]),
        // ... so a span holding a `|` stops the scan rather than letting the runs
        // after it pair across a cell boundary.
        ("a ` | x ` ` <!-- stay:live --> `\n|---|---|\n", &[&["live"]]),
        // An escaped backtick opens nothing for CommonMark.
        ("Shows \\`<!-- stay:demo -->` escaped.\n", &[&["demo"]]),
        // A run left open on one line may close on the next and re-pair it.
        ("A lone ` here\nthen `<!-- stay:demo -->` there.\n", &[&["demo"]]),
        // An HTML tag or autolink opening first outranks a code span.
        ("Text <a title=\"`\"> `<!-- stay:demo -->`\n", &[&["demo"]]),
        ("Text <http://x.y/`> `<!-- stay:demo -->`\n", &[&["demo"]]),
        // A backtick inside a marker-shaped string stops the scan, as an opener
        // or as a closer: markdown-it-py does not always close a span inside a
        // comment.
        ("Shows `<!-- stay:demo quote=\"`\" --> after.\n", &[&["demo"]]),
        // ... or as a run between the opener and the closer.
        ("Shows ``a <!-- stay:demo quote=\"`\" --> b`` here.\n", &[&["demo"]]),
        // A link destination or title takes a backtick before any span is
        // looked for, and so does a destination left unclosed before the run.
        ("[x](`) <!-- stay:live --> `\n", &[&["live"]]),
        ("[x](u \"`\") <!-- stay:live --> `\n", &[&["live"]]),
        ("See [x](a\\b) `<!-- stay:demo -->`\n", &[&["demo"]]),
        ("See [x](u `<!-- stay:demo -->` )\n", &[&["demo"]]),
        // A label still open, and a `]:` that may open a reference definition.
        ("[ `<!-- stay:live -->`\n", &[&["live"]]),
        ("[a]: /u `<!-- stay:live -->`\n", &[&["live"]]),
        // An MDX expression, comments included, holds no delimiters for MDX.
        ("{/* ` */} {/* stay:live */} {/* ` */}\n", &[&["live"]]),
        ("Use {x} `<!-- stay:demo -->`\n", &[&["demo"]]),
        // A nested or escaped bracket closes no label, so the label stays open.
        ("[[]`<!-- stay:live -->`x`\n", &[&["live"]]),
        ("[a\\]`<!-- stay:live -->`x`\n", &[&["live"]]),
        // A brace in a destination is no ordinary link: MDX reads an expression.
        ("[x]({/* foo) ` */} {/* stay:live */} `\n", &[&["live"]]),
        // A label's closing `]` is examined again, and here opens a destination.
        ("[a][x](`) <!-- stay:live --> `\n", &[&["live"]]),
        // An MDX-form marker is read like text: a tag or a bracket in its value
        // stops the scan, and so does its own `{`.
        ("{/* stay:x a=\"<a b='\" */}`'> <!-- stay:live --> `\n", &[&["x", "live"]]),
        ("{/* stay:x a=[ */}`<!-- stay:live -->`x`\n", &[&["x", "live"]]),
        ("Text {/* stay:x */} `<!-- stay:live -->`\n", &[&["x", "live"]]),
        // An HTML-form marker whose body could open a construct is read too.
        ("A <!-- stay:x a=\"[\" --> `<!-- stay:live -->`\n", &[&["x", "live"]]),
        // ... and so is one holding `--`, whose opener `<` then stops the scan.
        ("A <!-- stay:x a=b--c --> then `<!-- stay:live -->`\n", &[&["x", "live"]]),
        // A raw-text tag name followed by a no-break space still opens a block.
        ("<pre\u{a0}>\n\n`<!-- stay:live -->`\n", &[&[], &["live"]]),
        // markdown-it-py folds a raw-text name by Unicode: `ſ` is `s`, `İ` and `ı`
        // are `i`, so each of these opens a block that runs across the blank line.
        ("<\u{17f}cript>\n\n`<!-- stay:live -->`\n", &[&[], &["live"]]),
        ("<scr\u{130}pt>\n\n`<!-- stay:live -->`\n", &[&[], &["live"]]),
        ("<scr\u{131}pt>\n\n`<!-- stay:live -->`\n", &[&[], &["live"]]),
        ("<\u{17f}tyle>\n\n`<!-- stay:live -->`\n", &[&[], &["live"]]),
        // A block opened after its container ends runs to its own end condition.
        ("- <div>\n<pre>\n\n`<!-- stay:live -->`\n", &[&[], &["live"]]),
        // A backtick inside a marker-shaped string stops the scan.
        ("A <!-- stay:live quote=\"`\" --> then `<!-- stay:demo -->`\n", &[&["live", "demo"]]),
        // A line starting with a comment is an HTML block: no inline content.
        ("Para.\n<!-- stay:live --> `<!-- stay:demo -->`\n", &[&["live", "demo"]]),
    ];
    for (md, expected) in cases {
        assert_eq!(block_ids(md), ids(expected), "{md:?}");
    }
}

#[test]
fn a_backtick_inside_a_marker_stops_rather_than_pairs() {
    // Not in the Python file: there the run inside the quote, paired instead of
    // stopping the scan, closes on nothing that matters. Here it pairs with the
    // run after `x`, which leaves the next two runs free to hide `demo`.
    let md = "A <!-- stay:live quote=\"`\" --> x` then `<!-- stay:demo -->` y`\n";
    assert_eq!(block_ids(md), ids(&[&["live", "demo"]]));
}

#[test]
fn an_html_block_resumes_once_every_tracked_end_is_met() {
    // `<pre>` opens inside the `<div>` block and closes before the blank line, so
    // the scan resumes after it and the quote below is content.
    let md = "<div>\n<pre>\n</pre>\n\n`<!-- stay:demo -->`\n";
    assert_eq!(block_ids(md), ids(&[&[], &[]]));
    // Unclosed, it carries the block across the blank line and the comment is live.
    let md = "<div>\n<pre>\n\n`<!-- stay:live -->`\n";
    assert_eq!(block_ids(md), ids(&[&[], &["live"]]));
}

#[test]
fn an_html_marker_with_a_plain_body_is_passed_over() {
    // Its body holds nothing a renderer could read as a construct, so the scan
    // passes over it and the quote after it is content.
    let md = "A <!-- stay:x a=b --> then `<!-- stay:demo -->`\n";
    assert_eq!(block_ids(md), ids(&[&["x"]]));
}

#[test]
fn a_raw_text_name_continued_by_a_name_byte_is_no_raw_text_block() {
    // `<pre-x>` is a type 7 block, which ends at the blank line, so the quote
    // after it is content; `<pre\u{a0}>` is a `<pre>` block that runs on.
    let md = "<pre-x>\n\n`<!-- stay:demo -->`\n";
    assert_eq!(block_ids(md), ids(&[&[], &[]]));
}

#[test]
fn a_blank_line_restarts_the_scan() {
    let md = "A lone ` here\n\nthen `<!-- stay:demo -->` there.\n";
    assert_eq!(block_ids(md), ids(&[&[], &[]]));
}

#[test]
fn spans_before_a_stop_on_the_same_line_stand() {
    let md = "Shows `<!-- stay:demo -->` then <b> `<!-- stay:demo -->`\n";
    assert_eq!(block_ids(md), ids(&[&["demo"]]));
}

#[test]
fn a_run_on_html_block_runs_across_blank_lines() {
    let md = "Intro.\n\n<pre>\n\n`<!-- stay:demo -->`\n\n</pre>\n\nAfter `<!-- stay:x -->`.\n";
    assert_eq!(block_ids(md), ids(&[&[], &[], &["demo"], &[], &[]]));
}

#[test]
fn fence_lines_neither_pair_nor_get_scanned() {
    let md = "Intro `x\n```\ncode\n```\nthen `<!-- stay:demo -->`.\n";
    assert_eq!(block_ids(md), ids(&[&[]]));
}

// --- writing ----------------------------------------------------------------

#[test]
fn stamp_gives_a_paragraph_quoting_a_marker_a_stay_of_its_own() {
    let md = "Shows `<!-- stay:demo hash=sha256:7a9c -->` inline.\n";
    let out = stamp(md, &StampOptions::default(), counter());
    let body = "Shows `<!-- stay:demo hash=sha256:7a9c -->` inline.";
    assert_eq!(
        out.text,
        format!("{md}<!-- stay:n0 hash=sha256:{} -->\n", body_hash(body, Some(12)))
    );
}

#[test]
fn restamp_leaves_a_quoted_example_alone() {
    let md = "Shows `<!-- stay:demo hash=sha256:7a9c -->` inline.\n\
              <!-- stay:live hash=sha256:0000 -->\n";
    let out = restamp(md, &RestampOptions::default());
    assert!(out.text.contains("`<!-- stay:demo hash=sha256:7a9c -->`"));
    assert_eq!(out.text.matches("hash=sha256:7a9c").count(), 1);
    assert!(lint_document(&out.text).1.is_empty());
}

#[test]
fn repair_leaves_two_identical_quotes_alone() {
    let md = "Quote `<!-- stay:demo -->` and `<!-- stay:demo -->` here.\n";
    assert_eq!(repair_duplicates(md, counter()).text, md);
}

// The two below are not in the Python file. Its write tests quote an id no live
// marker carries, so the transform declines it whether or not the mask reached
// the writer; these quote a live id, as the fence tests do.

#[test]
fn restamp_leaves_a_quoted_example_alone_even_when_the_id_is_live() {
    let body = "Live content.";
    let md = format!(
        "{body}\n<!-- stay:demo hash=sha256:{} -->\n\nQuote `<!-- stay:demo hash=sha256:7a9c -->` here.\n",
        body_hash(body, Some(4))
    );
    let out = restamp(&md, &RestampOptions::default());
    assert_eq!(out.text, md);
    assert!(out.refreshed.is_empty());
}

#[test]
fn repair_renames_a_live_duplicate_and_leaves_its_quote_alone() {
    let md = "Live.\n<!-- stay:demo -->\n\nOther.\n<!-- stay:demo -->\n\nQuote `<!-- stay:demo -->` here.\n";
    let out = repair_duplicates(md, counter());
    assert_eq!(
        out.text,
        "Live.\n<!-- stay:demo -->\n\nOther.\n<!-- stay:n0 -->\n\nQuote `<!-- stay:demo -->` here.\n"
    );
    assert_eq!(out.renamed.len(), 1);
}

#[test]
fn a_quoted_copy_of_an_id_does_not_block_its_adoption() {
    // §16's adoption test counts one marker per id on each side. A quote of the
    // id is content, so it is not a second marker that would refuse the adoption.
    let before =
        "- [ ] alpha\n  **Waits**: none <!-- stay:i1 -->\n- [ ] beta\n<!-- stay:list -->\n\n\
                  Quote `<!-- stay:i1 -->` here.\n";
    let after =
        before.replace("none <!-- stay:i1 -->", "none <!-- stay:i1 subhash=sha256:1122 -->");
    let found: Vec<(&str, Option<String>)> =
        lint_diff(before, &after).into_iter().map(|f| (f.code, f.id)).collect();
    assert_eq!(found, [("ADOPTED_ID", Some("i1".to_string()))]);
}
