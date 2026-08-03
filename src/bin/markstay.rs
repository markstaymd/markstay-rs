//! `markstay` command-line interface (thin wrapper over the library; mirrors the
//! Python CLI in impl/py/src/markstay/cli.py and the npm `markstay` CLI). A single
//! static binary that needs no interpreter, suitable as a pre-commit / CI gate.
//!
//!     markstay preserve                     print the §11 instruction for an agent
//!     markstay preserve --wrap DOC.md       that instruction + the doc, as a prompt
//!     markstay lint    FILE...              well-formedness + intra-doc checks
//!     markstay lint    --before OLD.md NEW  regeneration diff (SPEC.md §11)
//!     markstay check-staged [FILE...]       check the staged commit (§11)
//!     markstay check-worktree [FILE...]     check files on disk against HEAD (§11)
//!     markstay stamp   FILE... [-w]         mint ids for unmarked blocks (§6)
//!     markstay restamp FILE... [-w]         refresh drifted hashes (§8)
//!     markstay repair  FILE... [-w]         mint fresh ids for duplicate ids (§7)
//!
//! `preserve` is listed first because measurement puts it first: an instructed
//! rewrite keeps ~96-100% of markers against ~5% for a naive one, so the
//! instruction prevents loss and every check below only catches it.
//!
//! `lint` exits non-zero when any error-level finding is reported, so it gates a
//! commit hook or an agent's post-edit step. The write verbs print the result to
//! stdout by default and a one-line note to stderr; `-w`/`--write` edits files in
//! place (required for more than one file). CommonMark mode (SPEC.md §5.2) is
//! deferred from v1.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use markstay::{
    check_entries, has_errors, is_markdown, lint_diff, lint_document, mint_id, preserve_wrap,
    repair_duplicates, restamp, sort_findings, stamp, CommitEntry, Finding, RestampOptions,
    StampOptions, Syntax, DEFAULT_ALPHABET, DEFAULT_HASH_LENGTH, DEFAULT_ID_LENGTH,
    PRESERVE_INSTRUCTION,
};

fn usage() -> &'static str {
    "usage: markstay <command> [options] FILE...\n\
     \n\
     commands:\n\
     \x20 preserve                         print the §11 instruction for an agent\n\
     \x20 preserve --wrap DOC.md           that instruction + the doc, as a prompt\n\
     \x20 lint     FILE...                 well-formedness + intra-doc checks\n\
     \x20 lint     --before OLD.md NEW.md  regeneration diff\n\
     \x20 check-staged [FILE...]           check the staged commit against HEAD\n\
     \x20 check-worktree [FILE...]         check files on disk against HEAD\n\
     \x20 stamp    FILE... [-w]            mint ids for unmarked blocks\n\
     \x20 restamp  FILE... [-w]            refresh drifted hashes\n\
     \x20 repair   FILE... [-w]            mint fresh ids for duplicate ids\n\
     \n\
     common options: --json / --show-drift (lint/check), -w/--write, --mdx,\n\
     \x20               --no-hash, --hash-length N (stamp/restamp), --add-missing (restamp),\n\
     \x20               --wrap FILE / --task TEXT (preserve)\n\
     \n\
     preserve is listed first because measurement puts it first: an instructed rewrite\n\
     keeps ~96-100% of markers against ~5% for a naive one, so the instruction prevents\n\
     loss and every check below only catches it."
}

fn arg_err(msg: &str) -> ExitCode {
    eprintln!("error: {}\n{}", msg, usage());
    ExitCode::from(2)
}

/// OS byte source for the CLI mint path. The library core takes an injected byte
/// source and never calls the OS, so this lives in the binary (which owns `std`)
/// and stays dependency-free: Unix reads `/dev/urandom`; Windows calls
/// `ProcessPrng` (the user-mode CSPRNG `getrandom` and `std` themselves use); any
/// other target has no zero-dep system RNG, so the write verbs report a clean
/// error rather than minting from nothing.
#[cfg(unix)]
fn os_random(n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = fs::File::open("/dev/urandom")?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(windows)]
fn os_random(n: usize) -> std::io::Result<Vec<u8>> {
    // ProcessPrng (bcryptprimitives.dll) is the modern user-mode CSPRNG: it needs
    // no initialization and cannot fail (it returns nonzero unconditionally).
    #[link(name = "bcryptprimitives")]
    extern "system" {
        fn ProcessPrng(pb_data: *mut u8, cb_data: usize) -> i32;
    }
    let mut buf = vec![0u8; n];
    unsafe {
        ProcessPrng(buf.as_mut_ptr(), buf.len());
    }
    Ok(buf)
}

#[cfg(not(any(unix, windows)))]
fn os_random(_n: usize) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no zero-dependency system RNG on this target; the CLI write verbs \
         (stamp/repair) are supported on Unix and Windows only",
    ))
}

/// Verify the OS RNG is usable before a write verb that mints ids, so an
/// unsupported target or an unreadable `/dev/urandom` fails with a clear message
/// instead of panicking mid-run.
fn rng_preflight() -> Result<(), ExitCode> {
    match os_random(1) {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("error: system RNG unavailable: {}", e);
            Err(ExitCode::from(2))
        }
    }
}

fn read_file(path: &str) -> Result<String, ()> {
    fs::read_to_string(path).map_err(|e| {
        eprintln!("error: cannot read {}: {}", path, e);
    })
}

/// The §11 instruction, on its own or wrapped around a document. No parsing, no
/// git, no file writes: this verb only ever composes text, which is why it costs
/// the same in all three ecosystems and lands the half of §11 that measurement
/// says prevents loss rather than catches it.
fn cmd_preserve(args: &[String]) -> ExitCode {
    let mut wrap: Option<String> = None;
    let mut task: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--wrap" | "--task" => {
                let flag = args[i].clone();
                i += 1;
                let Some(val) = args.get(i) else {
                    return arg_err(&format!("{} needs a value", flag));
                };
                if flag == "--wrap" {
                    wrap = Some(val.clone());
                } else {
                    task = Some(val.clone());
                }
            }
            other => {
                return arg_err(&format!(
                    "preserve takes no FILE arguments (use --wrap {:?})",
                    other
                ))
            }
        }
        i += 1;
    }

    if task.is_some() && wrap.is_none() {
        return arg_err("--task only applies with --wrap");
    }
    let Some(path) = wrap else {
        println!("{}", PRESERVE_INSTRUCTION);
        return ExitCode::SUCCESS;
    };
    let doc = if path == "-" {
        let mut buf = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
            eprintln!("error: cannot read stdin: {}", e);
            return ExitCode::from(2);
        }
        buf
    } else {
        match read_file(&path) {
            Ok(s) => s,
            Err(()) => return ExitCode::from(2),
        }
    };
    println!("{}", preserve_wrap(&doc, task.as_deref()));
    ExitCode::SUCCESS
}

fn parse_positive(s: &str) -> Result<usize, ()> {
    match s.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(()),
    }
}

// --- lint --------------------------------------------------------------------

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// Human render. HASH_DRIFT is the dominant, non-actionable line in normal use (it
// never blocks; it only ever says "you edited things"), so it is hidden by default
// and collapsed to one discoverable line. `show_drift=true` lists it. `--json` and
// the return tuples always carry drift, so the structured channel is unaffected.
// The error/warn/info summary counts the real totals either way.
fn render_text(label: &str, findings: &[Finding], show_drift: bool) -> String {
    if findings.is_empty() {
        return format!("{}: clean (no findings)", label);
    }
    let mut out = vec![format!("{}:", label)];
    let mut n_drift_hidden = 0usize;
    for f in sort_findings(findings) {
        if !show_drift && f.code == "HASH_DRIFT" {
            n_drift_hidden += 1;
            continue;
        }
        let where_ = match f.line {
            Some(n) => format!("L{}", n),
            None => "-".to_string(),
        };
        out.push(format!("  [{:5}] {:16} {:>5}  {}", f.level.as_str(), f.code, where_, f.message));
    }
    if n_drift_hidden > 0 {
        let noun = if n_drift_hidden == 1 { "finding" } else { "findings" };
        out.push(format!(
            "  -> {} hash-drift {} hidden (--show-drift to list)",
            n_drift_hidden, noun
        ));
    }
    let n_err = findings.iter().filter(|f| f.level.as_str() == "error").count();
    let n_warn = findings.iter().filter(|f| f.level.as_str() == "warn").count();
    let n_info = findings.iter().filter(|f| f.level.as_str() == "info").count();
    out.push(format!("  -> {} error, {} warn, {} info", n_err, n_warn, n_info));
    out.join("\n")
}

fn finding_json(f: &Finding) -> String {
    let id = match &f.id {
        Some(s) => format!("\"{}\"", json_escape(s)),
        None => "null".to_string(),
    };
    let line = match f.line {
        Some(n) => n.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"level\": \"{}\", \"code\": \"{}\", \"id\": {}, \"line\": {}, \"message\": \"{}\"}}",
        f.level.as_str(),
        f.code,
        id,
        line,
        json_escape(&f.message)
    )
}

fn cmd_lint(args: &[String]) -> ExitCode {
    let mut json = false;
    let mut show_drift = false;
    let mut before: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--show-drift" => show_drift = true,
            "--before" => {
                i += 1;
                if i >= args.len() {
                    return arg_err("--before needs a file argument");
                }
                before = Some(args[i].clone());
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                return arg_err(&format!("unknown option {}", other))
            }
            other => files.push(other.to_string()),
        }
        i += 1;
    }

    if files.is_empty() {
        return arg_err("no input file");
    }

    let mut results: Vec<(String, Vec<Finding>)> = Vec::new();
    if let Some(old) = &before {
        if files.len() != 1 {
            return arg_err("--before takes exactly one NEW file");
        }
        let before_md = match read_file(old) {
            Ok(s) => s,
            Err(()) => return ExitCode::from(2),
        };
        let after_md = match read_file(&files[0]) {
            Ok(s) => s,
            Err(()) => return ExitCode::from(2),
        };
        results.push((format!("{} -> {}", old, files[0]), lint_diff(&before_md, &after_md)));
    } else {
        for f in &files {
            let md = match read_file(f) {
                Ok(s) => s,
                Err(()) => return ExitCode::from(2),
            };
            let (_, findings) = lint_document(&md);
            results.push((f.clone(), findings));
        }
    }

    if json {
        let mut blocks: Vec<String> = Vec::new();
        for (label, fs) in &results {
            let items: Vec<String> = sort_findings(fs).iter().map(finding_json).collect();
            blocks.push(format!(
                "  \"{}\": [\n    {}\n  ]",
                json_escape(label),
                items.join(",\n    ")
            ));
        }
        println!("{{\n{}\n}}", blocks.join(",\n"));
    } else {
        let rendered: Vec<String> =
            results.iter().map(|(label, fs)| render_text(label, fs, show_drift)).collect();
        println!("{}", rendered.join("\n"));
    }

    if results.iter().any(|(_, fs)| has_errors(fs)) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// --- check-staged / check-worktree -----------------------------------------

#[derive(Clone, Debug)]
struct GitChange {
    status: char,
    source: String,
    destination: String,
}

#[derive(Clone, Debug)]
struct OwnedCommitEntry {
    status: char,
    source: String,
    destination: String,
    before: Option<String>,
    after: Option<String>,
}

fn git(args: &[String], allow_fail: bool) -> Result<Option<String>, String> {
    let output =
        Command::new("git").args(args).output().map_err(|e| format!("cannot run git: {}", e))?;
    if !output.status.success() {
        if allow_fail {
            return Ok(None);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), stderr.trim()));
    }
    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|_| format!("git {} returned text that is not valid UTF-8", args.join(" ")))
}

fn git_text(args: &[&str], allow_fail: bool) -> Result<Option<String>, String> {
    git(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(), allow_fail)
}

fn parse_name_status(raw: &str) -> Vec<GitChange> {
    let fields: Vec<&str> = raw.split('\0').collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < fields.len() && !fields[i].is_empty() {
        let Some(status) = fields[i].chars().next() else {
            break;
        };
        if matches!(status, 'R' | 'C') && i + 2 < fields.len() {
            out.push(GitChange {
                status,
                source: fields[i + 1].to_string(),
                destination: fields[i + 2].to_string(),
            });
            i += 3;
        } else if i + 1 < fields.len() {
            out.push(GitChange {
                status,
                source: fields[i + 1].to_string(),
                destination: fields[i + 1].to_string(),
            });
            i += 2;
        } else {
            break;
        }
    }
    out
}

fn staged_changes() -> Result<Vec<GitChange>, String> {
    let raw = git_text(&["diff", "--cached", "--name-status", "-z", "--find-renames"], false)?
        .unwrap_or_default();
    Ok(parse_name_status(&raw))
}

fn worktree_changes() -> Result<Vec<GitChange>, String> {
    let raw = match git_text(&["diff", "HEAD", "--name-status", "-z", "--find-renames"], true)? {
        Some(raw) => raw,
        None => git_text(&["diff", "--cached", "--name-status", "-z", "--find-renames"], true)?
            .unwrap_or_default(),
    };
    let mut out = parse_name_status(&raw);
    let untracked =
        git_text(&["ls-files", "--others", "--exclude-standard", "-z"], false)?.unwrap_or_default();
    for path in untracked.split('\0').filter(|path| !path.is_empty()) {
        out.push(GitChange {
            status: 'A',
            source: path.to_string(),
            destination: path.to_string(),
        });
    }
    Ok(out)
}

fn git_show(revision: &str, path: &str) -> Result<Option<String>, String> {
    git(&["show".to_string(), format!("{}:{}", revision, path)], true)
}

fn materialize_changes(
    changes: &[GitChange],
    root: &Path,
    worktree: bool,
) -> Result<Vec<OwnedCommitEntry>, String> {
    let mut out = Vec::new();
    for change in changes {
        let tracked = match change.status {
            'D' => is_markdown(&change.source),
            'R' => is_markdown(&change.source) || is_markdown(&change.destination),
            _ => is_markdown(&change.destination),
        };
        if !tracked {
            continue;
        }

        let before_path = if matches!(change.status, 'R' | 'C' | 'D') {
            &change.source
        } else {
            &change.destination
        };
        let before = git_show("HEAD", before_path)?;
        let after = if change.status == 'D' {
            None
        } else if worktree {
            // Match the Python and JS commands: an unreadable or invalid UTF-8
            // worktree file is treated as empty and linted from there.
            Some(fs::read_to_string(root.join(&change.destination)).unwrap_or_default())
        } else {
            git_show("", &change.destination)?.or_else(|| Some(String::new()))
        };
        out.push(OwnedCommitEntry {
            status: change.status,
            source: change.source.clone(),
            destination: change.destination.clone(),
            before,
            after,
        });
    }
    Ok(out)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

fn repo_relative(path: &str, root: &Path) -> Result<String, String> {
    let input = Path::new(path);
    let absolute = if input.is_absolute() {
        input.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| root.to_path_buf()).join(input)
    };
    let normalized = normalize_lexical(&absolute);
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("cannot resolve git work tree {}: {}", root.display(), error))?;
    let canonical = fs::canonicalize(&normalized).unwrap_or(normalized);
    let relative = canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| format!("scoped path is outside the git work tree: {}", path))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn print_check_json(result: &markstay::StagedCheck) {
    let reports: Vec<String> = result
        .reports
        .iter()
        .map(|report| {
            let findings: Vec<String> =
                sort_findings(&report.findings).iter().map(finding_json).collect();
            format!(
                "    \"{}\": [\n      {}\n    ]",
                json_escape(&report.label),
                findings.join(",\n      ")
            )
        })
        .collect();
    let findings = if reports.is_empty() {
        "{}".to_string()
    } else {
        format!("{{\n{}\n  }}", reports.join(",\n"))
    };
    let notes = if result.notes.is_empty() {
        "[]".to_string()
    } else {
        let items: Vec<String> =
            result.notes.iter().map(|note| format!("    \"{}\"", json_escape(note))).collect();
        format!("[\n{}\n  ]", items.join(",\n"))
    };
    println!("{{\n  \"findings\": {},\n  \"notes\": {}\n}}", findings, notes);
}

fn cmd_check(verb: &str, args: &[String], worktree: bool) -> ExitCode {
    let mut json = false;
    let mut show_drift = false;
    let mut files = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--show-drift" => show_drift = true,
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                return arg_err(&format!("unknown option {}", other));
            }
            other => files.push(other.to_string()),
        }
    }

    let root = match git_text(&["rev-parse", "--show-toplevel"], true) {
        Ok(Some(path)) => PathBuf::from(path.trim()),
        Ok(None) => return arg_err(&format!("{} must run inside a git work tree", verb)),
        Err(error) => {
            eprintln!("markstay: {}", error);
            return ExitCode::from(2);
        }
    };
    let scope: Vec<String> =
        match files.iter().map(|path| repo_relative(path, &root)).collect::<Result<Vec<_>, _>>() {
            Ok(scope) => scope,
            Err(error) => {
                eprintln!("markstay: {}", error);
                return ExitCode::from(2);
            }
        };

    let changes = match if worktree { worktree_changes() } else { staged_changes() } {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("markstay: {}", error);
            return ExitCode::from(2);
        }
    };
    let owned = match materialize_changes(&changes, &root, worktree) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("markstay: {}", error);
            return ExitCode::from(2);
        }
    };
    let entries: Vec<CommitEntry<'_>> = owned
        .iter()
        .map(|entry| CommitEntry {
            status: entry.status,
            source: &entry.source,
            destination: &entry.destination,
            before: entry.before.as_deref(),
            after: entry.after.as_deref(),
        })
        .collect();
    let result = check_entries(&entries, &scope);

    if json {
        print_check_json(&result);
    } else {
        for report in &result.reports {
            let actionable = report.findings.iter().any(|finding| {
                finding.level.as_str() == "error"
                    || (finding.level.as_str() == "warn" && finding.code != "HASH_DRIFT")
            });
            if show_drift || actionable {
                eprintln!("{}", render_text(&report.label, &report.findings, show_drift));
            }
        }
        if !result.notes.is_empty() {
            eprintln!("markstay: stays that changed document (not blocking):");
            for note in &result.notes {
                eprintln!("  {}", note);
            }
        }
    }

    if result.has_errors() {
        eprintln!(
            "\nmarkstay: this commit breaks a stay (dropped / duplicated / relocated / \
             malformed). Fix it, or bypass once with `git commit --no-verify`."
        );
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// --- write verbs (stamp / restamp / repair) ----------------------------------

/// Shared driver: run `op(text) -> (text, note)` per file, then either emit to
/// stdout (with the note on stderr) or edit in place. More than one file requires
/// `-w`.
fn run_write(
    verb: &str,
    files: &[String],
    write: bool,
    mut op: impl FnMut(&str) -> (String, String),
) -> ExitCode {
    if files.is_empty() {
        return arg_err("no input file");
    }
    if files.len() > 1 && !write {
        return arg_err(&format!("{} on multiple files requires -w/--write", verb));
    }
    for f in files {
        let text = match read_file(f) {
            Ok(s) => s,
            Err(()) => return ExitCode::from(2),
        };
        let (out, note) = op(&text);
        if write {
            if let Err(e) = fs::write(f, &out) {
                eprintln!("error: cannot write {}: {}", f, e);
                return ExitCode::from(2);
            }
        } else {
            print!("{}", out);
        }
        eprintln!("{}: {}", f, note);
    }
    ExitCode::SUCCESS
}

/// Consume a `--hash-length N` value into `slot` (shared by `stamp`/`restamp`).
/// Advances `*i` past the value and keeps the original inline block's arg-error
/// contract: `Err` carries the exit code `arg_err` returned.
fn take_hash_length(
    i: &mut usize,
    args: &[String],
    slot: &mut Option<usize>,
) -> Result<bool, ExitCode> {
    *i += 1;
    match args.get(*i).map(|s| parse_positive(s)) {
        Some(Ok(n)) => {
            *slot = Some(n);
            Ok(true)
        }
        _ => Err(arg_err("--hash-length needs a positive integer")),
    }
}

struct CommonArgs {
    write: bool,
    files: Vec<String>,
    help: bool,
}

/// Parse the flags shared by every write verb (`-w`/`--write`, `-h`/`--help`,
/// the unknown-option error, and bare file arguments). Each token is offered to
/// `extra` first: it returns `Ok(true)` if it claimed the token (advancing `*i`
/// itself for a flag that takes a value), `Ok(false)` to defer to the common
/// handling, or `Err(code)` to abort. On `-h`/`--help` usage is printed and
/// `help` is set so the caller exits 0 before doing any work. Keeps the
/// JS/Python CLI's exit codes and `usage`/error strings byte-identical.
fn parse_write_args(
    args: &[String],
    mut extra: impl FnMut(&str, &mut usize, &[String]) -> Result<bool, ExitCode>,
) -> Result<CommonArgs, ExitCode> {
    let mut out = CommonArgs { write: false, files: Vec::new(), help: false };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if extra(arg, &mut i, args)? {
            i += 1;
            continue;
        }
        match arg {
            "-w" | "--write" => out.write = true,
            "-h" | "--help" => {
                println!("{}", usage());
                out.help = true;
                return Ok(out);
            }
            other if other.starts_with('-') => {
                return Err(arg_err(&format!("unknown option {}", other)));
            }
            other => out.files.push(other.to_string()),
        }
        i += 1;
    }
    Ok(out)
}

fn cmd_stamp(args: &[String]) -> ExitCode {
    let mut mdx = false;
    let mut no_hash = false;
    let mut hash_length: Option<usize> = None;
    let common = match parse_write_args(args, |flag, i, args| match flag {
        "--mdx" => {
            mdx = true;
            Ok(true)
        }
        "--no-hash" => {
            no_hash = true;
            Ok(true)
        }
        "--hash-length" => take_hash_length(i, args, &mut hash_length),
        _ => Ok(false),
    }) {
        Ok(c) => c,
        Err(code) => return code,
    };
    if common.help {
        return ExitCode::SUCCESS;
    }

    if let Err(code) = rng_preflight() {
        return code;
    }
    let opts = StampOptions {
        syntax: if mdx { Syntax::Mdx } else { Syntax::Html },
        hash: !no_hash,
        hash_length: hash_length.unwrap_or(DEFAULT_HASH_LENGTH),
    };
    run_write("stamp", &common.files, common.write, |md| {
        let res = stamp(md, &opts, || {
            mint_id(DEFAULT_ID_LENGTH, DEFAULT_ALPHABET, |n| {
                os_random(n).expect("system RNG verified by rng_preflight")
            })
        });
        (res.text, format!("{} id(s) minted", res.minted.len()))
    })
}

fn cmd_restamp(args: &[String]) -> ExitCode {
    let mut add_missing = false;
    let mut hash_length: Option<usize> = None;
    let common = match parse_write_args(args, |flag, i, args| match flag {
        "--add-missing" => {
            add_missing = true;
            Ok(true)
        }
        "--hash-length" => take_hash_length(i, args, &mut hash_length),
        _ => Ok(false),
    }) {
        Ok(c) => c,
        Err(code) => return code,
    };
    if common.help {
        return ExitCode::SUCCESS;
    }

    let opts = RestampOptions { hash_length, add_missing };
    run_write("restamp", &common.files, common.write, |md| {
        let res = restamp(md, &opts);
        (res.text, format!("{} hash(es) refreshed", res.refreshed.len()))
    })
}

fn cmd_repair(args: &[String]) -> ExitCode {
    let common = match parse_write_args(args, |_flag, _i, _args| Ok(false)) {
        Ok(c) => c,
        Err(code) => return code,
    };
    if common.help {
        return ExitCode::SUCCESS;
    }

    if let Err(code) = rng_preflight() {
        return code;
    }
    run_write("repair", &common.files, common.write, |md| {
        let res = repair_duplicates(md, || {
            mint_id(DEFAULT_ID_LENGTH, DEFAULT_ALPHABET, |n| {
                os_random(n).expect("system RNG verified by rng_preflight")
            })
        });
        (res.text, format!("{} duplicate id(s) re-minted", res.renamed.len()))
    })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Match the JS/Python CLI: bare invocation prints usage and exits 2; an
    // explicit help request exits 0.
    let Some(cmd) = argv.first() else {
        println!("{}", usage());
        return ExitCode::from(2);
    };
    match cmd.as_str() {
        "help" | "-h" | "--help" => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        "preserve" => cmd_preserve(&argv[1..]),
        "lint" => cmd_lint(&argv[1..]),
        "check-staged" => cmd_check("check-staged", &argv[1..], false),
        "check-worktree" => cmd_check("check-worktree", &argv[1..], true),
        "stamp" => cmd_stamp(&argv[1..]),
        "restamp" => cmd_restamp(&argv[1..]),
        "repair" => cmd_repair(&argv[1..]),
        other => arg_err(&format!("unknown command {:?}", other)),
    }
}
