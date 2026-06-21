//! `markstay` command-line interface (thin wrapper over the library; mirrors the
//! Python CLI in impl/py/src/markstay/cli.py and the npm `markstay` CLI). A single
//! static binary that needs no interpreter, suitable as a pre-commit / CI gate.
//!
//!     markstay lint    FILE...              well-formedness + intra-doc checks
//!     markstay lint    --before OLD.md NEW  regeneration diff (SPEC.md §11)
//!     markstay stamp   FILE... [-w]         mint ids for unmarked blocks (§6)
//!     markstay restamp FILE... [-w]         refresh drifted hashes (§8)
//!     markstay repair  FILE... [-w]         mint fresh ids for duplicate ids (§7)
//!
//! `lint` exits non-zero when any error-level finding is reported, so it gates a
//! commit hook or an agent's post-edit step. The write verbs print the result to
//! stdout by default and a one-line note to stderr; `-w`/`--write` edits files in
//! place (required for more than one file). CommonMark mode (SPEC.md §5.2) is
//! deferred from v1.

use std::fs;
use std::process::ExitCode;

use markstay::{
    has_errors, lint_diff, lint_document, mint_id, repair_duplicates, restamp, sort_findings, stamp,
    Finding, RestampOptions, StampOptions, Syntax, DEFAULT_ALPHABET, DEFAULT_HASH_LENGTH,
    DEFAULT_ID_LENGTH,
};

fn usage() -> &'static str {
    "usage: markstay <command> [options] FILE...\n\
     \n\
     commands:\n\
     \x20 lint     FILE...                 well-formedness + intra-doc checks\n\
     \x20 lint     --before OLD.md NEW.md  regeneration diff\n\
     \x20 stamp    FILE... [-w]            mint ids for unmarked blocks\n\
     \x20 restamp  FILE... [-w]            refresh drifted hashes\n\
     \x20 repair   FILE... [-w]            mint fresh ids for duplicate ids\n\
     \n\
     common options: --json (lint), -w/--write, --mdx, --no-hash,\n\
     \x20               --hash-length N (stamp/restamp), --add-missing (restamp)"
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

fn cmd_lint(args: &[String]) -> ExitCode {
    let mut json = false;
    let mut before: Option<String> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
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
            other if other.starts_with('-') => return arg_err(&format!("unknown option {}", other)),
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
        let rendered: Vec<String> = results
            .iter()
            .map(|(label, fs)| render_text(label, fs))
            .collect();
        println!("{}", rendered.join("\n"));
    }

    if results.iter().any(|(_, fs)| has_errors(fs)) {
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

fn cmd_stamp(args: &[String]) -> ExitCode {
    let mut write = false;
    let mut mdx = false;
    let mut no_hash = false;
    let mut hash_length: Option<usize> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--write" => write = true,
            "--mdx" => mdx = true,
            "--no-hash" => no_hash = true,
            "--hash-length" => {
                i += 1;
                match args.get(i).map(|s| parse_positive(s)) {
                    Some(Ok(n)) => hash_length = Some(n),
                    _ => return arg_err("--hash-length needs a positive integer"),
                }
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => return arg_err(&format!("unknown option {}", other)),
            other => files.push(other.to_string()),
        }
        i += 1;
    }

    if let Err(code) = rng_preflight() {
        return code;
    }
    let opts = StampOptions {
        syntax: if mdx { Syntax::Mdx } else { Syntax::Html },
        hash: !no_hash,
        hash_length: hash_length.unwrap_or(DEFAULT_HASH_LENGTH),
    };
    run_write("stamp", &files, write, |md| {
        let res = stamp(md, &opts, || {
            mint_id(DEFAULT_ID_LENGTH, DEFAULT_ALPHABET, |n| {
                os_random(n).expect("system RNG verified by rng_preflight")
            })
        });
        (res.text, format!("{} id(s) minted", res.minted.len()))
    })
}

fn cmd_restamp(args: &[String]) -> ExitCode {
    let mut write = false;
    let mut add_missing = false;
    let mut hash_length: Option<usize> = None;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--write" => write = true,
            "--add-missing" => add_missing = true,
            "--hash-length" => {
                i += 1;
                match args.get(i).map(|s| parse_positive(s)) {
                    Some(Ok(n)) => hash_length = Some(n),
                    _ => return arg_err("--hash-length needs a positive integer"),
                }
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => return arg_err(&format!("unknown option {}", other)),
            other => files.push(other.to_string()),
        }
        i += 1;
    }

    let opts = RestampOptions {
        hash_length,
        add_missing,
    };
    run_write("restamp", &files, write, |md| {
        let res = restamp(md, &opts);
        (res.text, format!("{} hash(es) refreshed", res.refreshed.len()))
    })
}

fn cmd_repair(args: &[String]) -> ExitCode {
    let mut write = false;
    let mut files: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--write" => write = true,
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => return arg_err(&format!("unknown option {}", other)),
            other => files.push(other.to_string()),
        }
        i += 1;
    }

    if let Err(code) = rng_preflight() {
        return code;
    }
    run_write("repair", &files, write, |md| {
        let res = repair_duplicates(md, || {
            mint_id(DEFAULT_ID_LENGTH, DEFAULT_ALPHABET, |n| {
                os_random(n).expect("system RNG verified by rng_preflight")
            })
        });
        (
            res.text,
            format!("{} duplicate id(s) re-minted", res.renamed.len()),
        )
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
        "lint" => cmd_lint(&argv[1..]),
        "stamp" => cmd_stamp(&argv[1..]),
        "restamp" => cmd_restamp(&argv[1..]),
        "repair" => cmd_repair(&argv[1..]),
        other => arg_err(&format!("unknown command {:?}", other)),
    }
}
