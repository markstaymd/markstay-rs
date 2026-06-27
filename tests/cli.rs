//! CLI exit-code contract for the write-verb argument parser. Runs the built
//! binary as a subprocess and asserts the exit codes the JS/Python CLIs share:
//! an argument error is 2, `--help` is 0. These pin the shared `parse_write_args`
//! driver so a future refactor cannot quietly change a verb's exit behaviour.

use std::process::Command;

fn exit_code(args: &[&str]) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_markstay"))
        .args(args)
        .output()
        .expect("spawn markstay binary")
        .status
        .code()
        .expect("process exited via a code, not a signal")
}

#[test]
fn hash_length_zero_is_arg_error() {
    // parse_positive rejects 0; both verbs that take --hash-length must exit 2.
    assert_eq!(exit_code(&["stamp", "--hash-length", "0", "x.md"]), 2);
    assert_eq!(exit_code(&["restamp", "--hash-length", "0", "x.md"]), 2);
}

#[test]
fn hash_length_missing_value_is_arg_error() {
    assert_eq!(exit_code(&["stamp", "--hash-length"]), 2);
    assert_eq!(exit_code(&["restamp", "--hash-length"]), 2);
}

#[test]
fn unknown_flag_is_arg_error() {
    assert_eq!(exit_code(&["stamp", "--bogus"]), 2);
    assert_eq!(exit_code(&["restamp", "--bogus"]), 2);
    assert_eq!(exit_code(&["repair", "--bogus"]), 2);
}

#[test]
fn multiple_files_without_write_is_arg_error() {
    // The run_write guard fires before any file is read, so missing files are fine.
    assert_eq!(exit_code(&["stamp", "a.md", "b.md"]), 2);
    assert_eq!(exit_code(&["restamp", "a.md", "b.md"]), 2);
    assert_eq!(exit_code(&["repair", "a.md", "b.md"]), 2);
}

#[test]
fn help_exits_zero() {
    assert_eq!(exit_code(&["stamp", "--help"]), 0);
    assert_eq!(exit_code(&["restamp", "-h"]), 0);
    assert_eq!(exit_code(&["repair", "--help"]), 0);
}

// --- HASH_DRIFT channel (ported from linter/test_lint.py) --------------------
//
// HASH_DRIFT is load-bearing in the structured channel (the RAG chunker treats it
// as fatal; the Plate contrast counts it) but noise in the default human render
// (it never blocks, only ever says "you edited things"). The `lint` text render
// hides it by default behind --show-drift; the finding, its warn level, and --json
// are untouched.

use std::path::PathBuf;

const DRIFT_DOC: &str = "Edited.\n<!-- stay:z9 hash=sha256:dead -->\n";
const MIXED_DOC: &str =
    "Edited.\n<!-- stay:z9 hash=sha256:dead -->\n\nA para.\n<!-- stay:note=hello -->\n";

fn stdout(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_markstay"))
        .args(args)
        .output()
        .expect("spawn markstay binary");
    String::from_utf8(out.stdout).expect("stdout is utf-8")
}

fn tmp_md(name: &str, body: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("markstay-cli-{}-{}.md", std::process::id(), name));
    std::fs::write(&p, body).expect("write temp md");
    p
}

#[test]
fn lint_hides_drift_by_default_lists_with_flag() {
    let p = tmp_md("drift", DRIFT_DOC);
    let path = p.to_str().unwrap();
    let hidden = stdout(&["lint", path]);
    let shown = stdout(&["lint", "--show-drift", path]);
    std::fs::remove_file(&p).ok();

    assert!(!hidden.contains("HASH_DRIFT"), "drift line dropped by default");
    assert!(
        hidden.contains("hash-drift") && hidden.contains("--show-drift"),
        "collapsed receipt present"
    );
    assert!(shown.contains("HASH_DRIFT"), "drift listed on request");
    assert!(!shown.contains("hidden (--show-drift"), "no collapsed line when shown");
    // The summary counts the real totals either way (a hidden drift still happened).
    assert!(hidden.contains("0 error, 1 warn, 0 info"));
    assert!(shown.contains("0 error, 1 warn, 0 info"));
}

#[test]
fn lint_keeps_real_findings_and_counts_with_mixed_set() {
    let p = tmp_md("mixed", MIXED_DOC);
    let path = p.to_str().unwrap();
    let hidden = stdout(&["lint", path]);
    let shown = stdout(&["lint", "--show-drift", path]);
    std::fs::remove_file(&p).ok();

    for r in [&hidden, &shown] {
        assert!(r.contains("1 error, 1 warn, 0 info"), "counts unchanged");
        assert!(r.contains("MALFORMED_MARKER"), "the actionable line stays");
    }
    assert!(!hidden.contains("HASH_DRIFT"));
    assert!(hidden.contains("1 hash-drift finding hidden"), "singular, collapsed");
}

#[test]
fn json_is_byte_identical_with_and_without_show_drift() {
    let p = tmp_md("json", DRIFT_DOC);
    let path = p.to_str().unwrap();
    let a = stdout(&["lint", "--json", path]);
    let b = stdout(&["lint", "--json", "--show-drift", path]);
    std::fs::remove_file(&p).ok();

    assert_eq!(a, b, "structured channel untouched by the flag");
    assert!(a.contains("HASH_DRIFT"), "drift still carried in --json");
}

#[test]
fn before_diff_text_path_hides_drift_by_default() {
    let before = tmp_md("before", "Alpha content.\n<!-- stay:aaa -->\n");
    let after = tmp_md("after", "Alpha content, now revised.\n<!-- stay:aaa -->\n");
    let bp = before.to_str().unwrap();
    let ap = after.to_str().unwrap();
    let hidden = stdout(&["lint", "--before", bp, ap]);
    let shown = stdout(&["lint", "--show-drift", "--before", bp, ap]);
    std::fs::remove_file(&before).ok();
    std::fs::remove_file(&after).ok();

    assert!(!hidden.contains("HASH_DRIFT"));
    assert!(hidden.contains("hash-drift"), "collapsed line on the diff path");
    assert!(shown.contains("HASH_DRIFT"));
}

#[test]
fn guardrail_hash_drift_stays_warn_in_return_tuples() {
    // The invariant the whole change hinges on: the structured channel must keep
    // HASH_DRIFT at warn, because the RAG chunker's fatal check and the Plate
    // contrast both read these return values, not the printed text.
    let (_, doc) = markstay::lint_document(DRIFT_DOC);
    let drift: Vec<_> = doc.iter().filter(|f| f.code == "HASH_DRIFT").collect();
    assert!(!drift.is_empty() && drift.iter().all(|f| f.level.as_str() == "warn"));

    let diff =
        markstay::lint_diff("Alpha.\n<!-- stay:a -->\n", "Alpha, revised.\n<!-- stay:a -->\n");
    let diff_drift: Vec<_> = diff.iter().filter(|f| f.code == "HASH_DRIFT").collect();
    assert!(!diff_drift.is_empty() && diff_drift.iter().all(|f| f.level.as_str() == "warn"));
}
