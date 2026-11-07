use markstay::{find_markers, lint_document, parse_document, rewrite_markers, strip_markers};

#[test]
fn strict_marker_grammar_normalizes_lf_but_preserves_raw_and_lines() {
    let raw = "<!-- stay:x x-note=\"a\r\nb\" -->";
    let markers = find_markers(&format!("one\rtwo\r{raw}"), 0);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].id.as_deref(), Some("x"));
    assert_eq!(markers[0].raw, raw);
    assert_eq!(markers[0].line, 3);
}

#[test]
fn complete_body_validation_rejects_bad_attrs_escapes_and_lf_separators() {
    for text in [
        "<!-- stay:x broken -->",
        "<!-- stay:x x-note=\"a\\q\" -->",
        "<!-- stay:x\nhash=sha256:aa -->",
    ] {
        assert!(find_markers(text, 0).is_empty(), "{text:?}");
    }
    let marker = &find_markers("<!-- stay:x hash=sha256:deadZ -->", 0)[0];
    assert_eq!(marker.hash, None);
}

#[test]
fn html_first_host_closer_rejects_and_resumes() {
    for bad in ["<!-- stay:bad x-note=\"a-->b\" -->", "<!-- stay:bad x-note=\"a--!>b\" -->"] {
        let text = format!("{bad}\n<!-- stay:good -->");
        let ids: Vec<String> = find_markers(&text, 0).into_iter().filter_map(|m| m.id).collect();
        assert_eq!(ids, ["good"]);
        assert_eq!(strip_markers(&text), format!("{bad}\n"));
    }
}

#[test]
fn mdx_first_host_closer_rejects_and_resumes() {
    let bad = "{/* stay:bad x-note=\"a*/b\" */}";
    let text = format!("{bad}\n{{/* stay:good */}}");
    let ids: Vec<String> = find_markers(&text, 0).into_iter().filter_map(|m| m.id).collect();
    assert_eq!(ids, ["good"]);
    assert_eq!(strip_markers(&text), format!("{bad}\n"));
}

#[test]
fn every_opener_is_independent_including_one_nested_in_a_valid_marker() {
    let accepted = "<!-- stay:outer x-note=\"{/* stay:inner */}\" -->";
    let ids: Vec<String> = find_markers(accepted, 0).into_iter().filter_map(|m| m.id).collect();
    assert_eq!(ids, ["outer", "inner"]);
    assert_eq!(strip_markers(accepted), "");
    assert_eq!(rewrite_markers(accepted, |_| Some("changed".to_string()), None), accepted);

    let rejected = "<!-- stay:bad broken {/* stay:inner */} -->";
    let ids: Vec<String> = find_markers(rejected, 0).into_iter().filter_map(|m| m.id).collect();
    assert_eq!(ids, ["inner"]);
}

#[test]
fn malformed_no_id_diagnostic_stays_body_text_and_is_not_rewritten() {
    let malformed = "<!-- stay:note=hello -->";
    assert_eq!(strip_markers(malformed), malformed);
    let mut calls = 0;
    assert_eq!(
        rewrite_markers(
            malformed,
            |_| {
                calls += 1;
                Some("changed".to_string())
            },
            None,
        ),
        malformed
    );
    assert_eq!(calls, 0);

    let md = format!("Body.\n{malformed}");
    let blocks = parse_document(&md);
    assert_eq!(blocks[0].content, md);
    assert!(blocks[0].markers[0].malformed);
    let (_, findings) = lint_document(&md);
    assert_eq!(findings.iter().map(|f| f.code).collect::<Vec<_>>(), ["MALFORMED_MARKER"]);
}

#[test]
fn parse_document_keeps_original_crlf_marker_serialization() {
    let raw = "<!-- stay:x x-note=\"a\r\nb\" -->";
    let blocks = parse_document(&format!("Body.\r\n{raw}\r\n"));
    assert_eq!(blocks[0].markers[0].raw, raw);
    assert_eq!(blocks[0].content, "Body.");
}

#[test]
fn parse_document_discovers_a_quoted_marker_spanning_a_blank_line() {
    let raw = "<!-- stay:x x-note=\"a\r\n\r\nb\" -->";
    let blocks = parse_document(&format!("Body.\r\n{raw}\r\n"));
    assert_eq!(blocks[0].content, "Body.");
    assert_eq!(blocks[0].markers[0].raw, raw);
}

#[test]
fn required_key_first_diagnostic_survives_an_invalid_marker_closer() {
    let text = "<!-- stay:note=hello --!>\n{/* stay:note=hello */x";
    let markers = find_markers(text, 0);
    assert_eq!(markers.len(), 2);
    assert!(markers.iter().all(|marker| marker.malformed && marker.id.is_none()));
    assert_eq!(
        markers.iter().map(|marker| marker.raw.as_str()).collect::<Vec<_>>(),
        ["<!-- stay:note=hello --!>", "{/* stay:note=hello */"]
    );
    let (_, findings) = lint_document(text);
    assert_eq!(findings.iter().filter(|finding| finding.code == "MALFORMED_MARKER").count(), 2);
}

#[test]
fn exact_subhash_key_presence_is_independent_of_digest_validity() {
    let text = "<!-- stay:a subhash=bogus -->\n\
                <!-- stay:b subhash=\"sha256:abcd\" -->\n\
                <!-- stay:c x-subhash=sha256:abcd -->\n\
                <!-- stay:d x-note=\"a subhash=bogus\" -->";
    let markers = find_markers(text, 0);
    assert_eq!(
        markers.iter().map(|m| m.has_subhash).collect::<Vec<_>>(),
        [true, true, false, false]
    );
    assert!(markers.iter().all(|m| m.subhash.is_none()));
}

#[test]
fn line_count_runs_across_skipped_openers_crlf_and_offset() {
    // Skipped: a non-marker comment, an unclosed MDX opener, a `--!>` close.
    let text = "a\r\n<!-- note -->\r\n<!-- stay:x -->\r\n{/* stay:bad\r\n\r\
                <!-- stay:y --!>\n<!-- stay:z -->";
    let lines: Vec<(Option<String>, usize)> =
        find_markers(text, 10).into_iter().map(|m| (m.id, m.line)).collect();
    assert_eq!(lines, [(Some("x".into()), 13), (Some("z".into()), 17)]);
    // The offset is added per emitted marker only, so no marker means no sum.
    assert!(find_markers("no markers\n", usize::MAX).is_empty());
}

#[test]
fn overlapping_unterminated_attributes_resume_after_each_host_closer() {
    let mut text = "<!--stay:x a=".repeat(4000);
    text.push_str("\r\n-->\r\n");
    text.push_str(&"{/*stay:x a=".repeat(4000));
    text.push_str("\r*/}\r<!-- stay:good hash=sha256:AbCd -->");
    let markers = find_markers(&text, 7);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].id.as_deref(), Some("good"));
    assert_eq!(markers[0].hash.as_deref(), Some("abcd"));
    assert_eq!(markers[0].line, 12);
}

#[test]
fn overlapping_quoted_candidates_keep_multiline_escapes_and_host_boundaries() {
    let outer = "<!-- stay:outer note=\"a\\\"\r\n{/* stay:inner note=bare */}\\\\\" -->";
    let text = format!(
        "<!--stay:bad a=<!--stay:bad a=\r\n--!>\r\n\
         {{/* stay:bad a=<!-- stay:html note=\"x\r\ny\" -->\n*/}}\r{outer}"
    );
    let markers = find_markers(&text, 20);
    assert_eq!(
        markers.iter().map(|marker| marker.id.as_deref()).collect::<Vec<_>>(),
        [Some("html"), Some("outer"), Some("inner")]
    );
    assert_eq!(markers.iter().map(|marker| marker.line).collect::<Vec<_>>(), [23, 26, 27]);
    assert_eq!(markers[1].raw, outer);
}
