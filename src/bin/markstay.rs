//! `markstay` command-line linter (thin wrapper over the library; mirrors the
//! Python CLI in impl/py/src/markstay/cli.py). A single static binary that needs
//! no interpreter, suitable as a pre-commit / CI gate.
//!
//!     markstay FILE [FILE ...]            # well-formedness + intra-doc checks
//!     markstay --before OLD.md NEW.md     # regeneration diff
//!     markstay --json ...                 # machine-readable findings
//!
//! Exit status is non-zero when any error-level finding is reported, so it can
//! gate a commit hook or an agent's post-edit step. CommonMark mode (SPEC.md
//! §5.2) is deferred from v1.

use std::fs;
use std::process::ExitCode;

use markstay::{has_errors, lint_diff, lint_document, sort_findings, Finding};

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

fn render_text(label: &str, findings: &[Finding]) -> String {
    if findings.is_empty() {
        return format!("{}: clean (no findings)", label);
    }
    let mut out = vec![format!("{}:", label)];
    for f in sort_findings(findings) {
        let where_ = match f.line {
            Some(n) => format!("L{}", n),
            None => "-".to_string(),
        };
        out.push(format!(
            "  [{:5}] {:16} {:>5}  {}",
            f.level.as_str(),
            f.code,
            where_,
            f.message
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

fn usage() -> String {
    "usage: markstay [--json] FILE [FILE ...]\n       markstay [--json] --before OLD.md NEW.md"
        .to_string()
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let mut json = false;
    let mut before: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--json" => json = true,
            "--before" => {
                i += 1;
                if i >= argv.len() {
                    eprintln!("error: --before needs a file argument\n{}", usage());
                    return ExitCode::from(2);
                }
                before = Some(argv[i].clone());
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown option {}\n{}", other, usage());
                return ExitCode::from(2);
            }
            other => files.push(other.to_string()),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("error: no input file\n{}", usage());
        return ExitCode::from(2);
    }

    let read = |path: &str| -> Result<String, ()> {
        fs::read_to_string(path).map_err(|e| {
            eprintln!("error: cannot read {}: {}", path, e);
        })
    };

    let mut results: Vec<(String, Vec<Finding>)> = Vec::new();
    if let Some(old) = &before {
        if files.len() != 1 {
            eprintln!("error: --before takes exactly one NEW file\n{}", usage());
            return ExitCode::from(2);
        }
        let before_md = match read(old) {
            Ok(s) => s,
            Err(()) => return ExitCode::from(2),
        };
        let after_md = match read(&files[0]) {
            Ok(s) => s,
            Err(()) => return ExitCode::from(2),
        };
        let label = format!("{} -> {}", old, files[0]);
        results.push((label, lint_diff(&before_md, &after_md)));
    } else {
        for f in &files {
            let md = match read(f) {
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
        let rendered: Vec<String> = results
            .iter()
            .map(|(label, fs)| render_text(label, fs))
            .collect();
        println!("{}", rendered.join("\n"));
    }

    let any_err = results.iter().any(|(_, fs)| has_errors(fs));
    if any_err {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
