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
