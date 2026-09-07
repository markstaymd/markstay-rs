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
