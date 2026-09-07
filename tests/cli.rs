//! CLI exit-code contract for the write-verb argument parser. Runs the built
//! binary as a subprocess and asserts the exit codes the JS/Python CLIs share:
//! an argument error is 2, `--help` is 0. These pin the shared `parse_write_args`
//! driver so a future refactor cannot quietly change a verb's exit behaviour.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

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
fn readme_keeps_the_preservation_instruction_ahead_of_the_check_backstop() {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("read crate README");
    let instruction =
        readme.find("## Keeping stays alive through an agent's edit").expect("instruction heading");
    let cli = readme.find("## CLI").expect("CLI heading");
    let preserve = readme[cli..].find("markstay preserve").expect("preserve example") + cli;
    let check = readme[cli..].find("markstay check-staged").expect("check example") + cli;
    assert!(instruction < cli, "instruction section must precede CLI checks");
    assert!(preserve < check, "preserve must precede check-staged in CLI examples");
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

// --- preserve (SPEC.md §11) -------------------------------------------------
//
// The verb the eval says matters most: an instructed rewrite keeps ~96-100% of
// markers against ~5% for a naive one. Its CLI contract is deliberately dull, no
// parsing and no git, so these pin the shape rather than the content (the text
// itself is held byte-identical to the Python and JS copies by the conformance
// corpus, in tests/conformance.rs).

#[test]
fn preserve_prints_the_instruction_verbatim() {
    let out = stdout(&["preserve"]);
    assert_eq!(out, format!("{}\n", markstay::PRESERVE_INSTRUCTION));
    assert_eq!(exit_code(&["preserve"]), 0);
}

#[test]
fn preserve_wrap_composes_the_measured_prompt_shape() {
    let doc_body = "# Title\n\nA paragraph.\n";
    let p = tmp_md("preserve", doc_body);
    let path = p.to_str().unwrap();
    let out = stdout(&["preserve", "--wrap", path, "--task", "Tighten it."]);
    std::fs::remove_file(&p).ok();

    let want = format!("{}\n", markstay::preserve_wrap(doc_body, Some("Tighten it.")));
    assert_eq!(out, want);
    // task first, then the instruction, then the document behind the rule
    let t = out.find("Tighten it.").expect("task present");
    let i = out.find(markstay::PRESERVE_INSTRUCTION).expect("instruction present");
    let d = out.find("A paragraph.").expect("document present");
    assert!(t < i && i < d);
}

#[test]
fn preserve_rejects_a_bare_file_and_a_task_without_wrap() {
    // A bare FILE is the plausible mistake (every other verb takes one), so it has
    // to fail loudly rather than print the instruction and ignore the doc.
    assert_eq!(exit_code(&["preserve", "doc.md"]), 2);
    assert_eq!(exit_code(&["preserve", "--task", "Tighten it."]), 2);
    assert_eq!(exit_code(&["preserve", "--wrap"]), 2);
    assert_eq!(exit_code(&["preserve", "--wrap", "no-such-file.md"]), 2);
}

#[test]
fn preserve_rejects_input_that_is_not_utf8() {
    // Rust's read_to_string already rejects; JS and Python were made to match,
    // because Node substitutes U+FFFD and Python surrogate-escapes, which is
    // precisely how three implementations stop emitting the same bytes.
    let p = std::env::temp_dir().join(format!("markstay-cli-{}-badutf8.md", std::process::id()));
    std::fs::write(&p, b"Body \xff byte.\n").expect("write temp file");
    let code = exit_code(&["preserve", "--wrap", p.to_str().unwrap()]);
    std::fs::remove_file(&p).ok();
    assert_eq!(code, 2);
}

// --- check-staged / check-worktree -----------------------------------------

static REPO_SEQ: AtomicUsize = AtomicUsize::new(0);

struct TempRepo {
    path: PathBuf,
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).ok();
    }
}

fn git(repo: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn temp_repo() -> TempRepo {
    let n = REPO_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("markstay-rs-check-{}-{}", std::process::id(), n));
    fs::create_dir_all(&path).expect("create temp repo");
    git(&path, &["init", "-q"]);
    git(&path, &["checkout", "-q", "-b", "main"]);
    TempRepo { path }
}

fn write_repo(repo: &Path, name: &str, text: &str) {
    let path = repo.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, text).expect("write repo file");
}

fn stayed_doc(n: usize, prefix: &str) -> String {
    let mut text = "# Doc\n\n".to_string();
    for i in 0..n {
        text.push_str(&format!(
            "## Section {}\n\nBody text for section {}, long enough to hash.\n\
             <!-- stay:{}{} -->\n{}",
            i,
            i,
            prefix,
            i,
            if i + 1 == n { "" } else { "\n" }
        ));
    }
    text
}

fn run_at(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_markstay"))
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn markstay binary")
}

fn all_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn commit_all(repo: &Path) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "snapshot"]);
}

#[test]
fn check_staged_pairs_a_delete_create_rewrite_and_blocks_loss() {
    let repo = temp_repo();
    write_repo(&repo.path, "STATUS.md", &stayed_doc(9, "s"));
    commit_all(&repo.path);

    git(&repo.path, &["mv", "STATUS.md", "PHASE1.md"]);
    write_repo(&repo.path, "PHASE1.md", "# Doc\n\n## Phase 1\n\nAll done.\n<!-- stay:s0 -->\n");
    git(&repo.path, &["add", "-A"]);

    let output = run_at(&repo.path, &["check-staged"]);
    let text = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{}", text);
    assert_eq!(text.matches("DROPPED_ID").count(), 8, "{}", text);
    assert!(text.contains("baseline STATUS.md"), "{}", text);
}

#[test]
fn check_staged_is_quiet_for_a_preserving_edit_and_a_pure_rename() {
    let repo = temp_repo();
    write_repo(&repo.path, "a.md", &stayed_doc(2, "s"));
    commit_all(&repo.path);

    let edited = stayed_doc(2, "s").replace("Body text for section 0", "Reworded section 0");
    write_repo(&repo.path, "a.md", &edited);
    git(&repo.path, &["add", "-A"]);
    let edit = run_at(&repo.path, &["check-staged"]);
    assert!(edit.status.success(), "{}", all_output(&edit));
    assert!(all_output(&edit).trim().is_empty(), "{}", all_output(&edit));

    git(&repo.path, &["commit", "-qm", "edit"]);
    git(&repo.path, &["mv", "a.md", "renamed.md"]);
    git(&repo.path, &["add", "-A"]);
    let rename = run_at(&repo.path, &["check-staged"]);
    assert!(rename.status.success(), "{}", all_output(&rename));
    assert!(all_output(&rename).trim().is_empty(), "{}", all_output(&rename));
}

#[test]
fn check_staged_notes_a_rename_out_of_markdown() {
    let repo = temp_repo();
    write_repo(&repo.path, "notes.md", &stayed_doc(2, "s"));
    commit_all(&repo.path);
    git(&repo.path, &["mv", "notes.md", "notes.txt"]);

    let output = run_at(&repo.path, &["check-staged"]);
    let text = all_output(&output);
    assert!(output.status.success(), "{}", text);
    assert!(text.contains("renamed to notes.txt, leaving Markdown tracking"), "{}", text);
}

#[test]
fn check_staged_reports_a_cross_document_move_as_nonblocking() {
    let repo = temp_repo();
    let moved = "## Section 2\n\nBody text for section 2, long enough to hash.\n<!-- stay:s2 -->\n";
    write_repo(&repo.path, "a.md", &stayed_doc(3, "s"));
    write_repo(&repo.path, "b.md", &stayed_doc(2, "t"));
    commit_all(&repo.path);

    write_repo(&repo.path, "a.md", &stayed_doc(3, "s").replace(moved, ""));
    write_repo(&repo.path, "b.md", &(stayed_doc(2, "t") + "\n" + moved));
    git(&repo.path, &["add", "-A"]);
    let output = run_at(&repo.path, &["check-staged"]);
    let text = all_output(&output);
    assert!(output.status.success(), "{}", text);
    assert!(!text.contains("DROPPED_ID"), "{}", text);
    assert!(text.contains("s2: moved out of a.md into b.md"), "{}", text);
}

#[test]
fn check_staged_deletion_note_does_not_block() {
    let repo = temp_repo();
    write_repo(&repo.path, "doomed.md", &stayed_doc(4, "d"));
    commit_all(&repo.path);
    git(&repo.path, &["rm", "-q", "doomed.md"]);

    let output = run_at(&repo.path, &["check-staged"]);
    let text = all_output(&output);
    assert!(output.status.success(), "{}", text);
    assert!(text.contains("deleted with 4 stay(s)"), "{}", text);
}

#[test]
fn check_staged_scope_accepts_an_absolute_path() {
    let repo = temp_repo();
    write_repo(&repo.path, "a.md", &stayed_doc(3, "s"));
    write_repo(&repo.path, "other.md", &stayed_doc(1, "o"));
    commit_all(&repo.path);
    write_repo(&repo.path, "a.md", &stayed_doc(1, "s"));
    write_repo(
        &repo.path,
        "other.md",
        &stayed_doc(1, "o").replace("Body text", "Reworded body text"),
    );
    git(&repo.path, &["add", "-A"]);

    let absolute = repo.path.join("a.md");
    let output = run_at(&repo.path, &["check-staged", absolute.to_str().unwrap()]);
    let text = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{}", text);
    assert!(text.contains("DROPPED_ID"), "{}", text);
    assert!(!text.contains("other.md"), "{}", text);
}

#[cfg(unix)]
#[test]
fn check_staged_scope_resolves_a_symlinked_absolute_path() {
    use std::os::unix::fs::symlink;

    let repo = temp_repo();
    write_repo(&repo.path, "a.md", &stayed_doc(3, "s"));
    commit_all(&repo.path);
    write_repo(&repo.path, "a.md", &stayed_doc(1, "s"));
    git(&repo.path, &["add", "-A"]);

    let alias = repo.path.with_extension("alias");
    symlink(&repo.path, &alias).expect("create repository symlink");
    let absolute = alias.join("a.md");
    let output = run_at(&repo.path, &["check-staged", absolute.to_str().unwrap()]);
    fs::remove_file(&alias).ok();

    let text = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{}", text);
    assert!(text.contains("DROPPED_ID"), "{}", text);
}

#[test]
fn check_worktree_sees_loss_before_staging_and_pairs_an_untracked_rename() {
    let repo = temp_repo();
    write_repo(&repo.path, "STATUS.md", &stayed_doc(5, "s"));
    commit_all(&repo.path);

    fs::remove_file(repo.path.join("STATUS.md")).expect("remove old path");
    write_repo(&repo.path, "PHASE1.md", "# Doc\n\n## Phase 1\n\nCollapsed.\n<!-- stay:s0 -->\n");
    let staged = run_at(&repo.path, &["check-staged"]);
    assert!(staged.status.success(), "{}", all_output(&staged));
    assert!(all_output(&staged).trim().is_empty(), "{}", all_output(&staged));

    let worktree = run_at(&repo.path, &["check-worktree"]);
    let text = all_output(&worktree);
    assert_eq!(worktree.status.code(), Some(1), "{}", text);
    assert_eq!(text.matches("DROPPED_ID").count(), 4, "{}", text);
    assert!(text.contains("baseline STATUS.md"), "{}", text);
}

#[test]
fn check_worktree_sees_indexed_files_before_the_first_commit() {
    let repo = temp_repo();
    write_repo(&repo.path, "broken.md", "Body.\n<!-- stay:note=hello -->\n");
    git(&repo.path, &["add", "broken.md"]);

    let output = run_at(&repo.path, &["check-worktree"]);
    let text = all_output(&output);
    assert_eq!(output.status.code(), Some(1), "{}", text);
    assert!(text.contains("MALFORMED_MARKER"), "{}", text);
}

#[test]
fn check_json_uses_stdout_and_is_valid() {
    let repo = temp_repo();
    write_repo(&repo.path, "a.md", &stayed_doc(2, "s"));
    commit_all(&repo.path);
    write_repo(&repo.path, "a.md", &stayed_doc(1, "s"));
    git(&repo.path, &["add", "-A"]);

    let output = run_at(&repo.path, &["check-staged", "--json"]);
    assert_eq!(output.status.code(), Some(1), "{}", all_output(&output));
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("check JSON parses");
    assert!(payload["findings"]["a.md"].is_array());
    assert!(payload["notes"].is_array());
}
