//! every handled CLI error exits 0 and emits a `# note:`
//! line on stdout, so agentic harnesses don't abort
//! the surrounding parallel-bash batch.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    (
        out.status.success(),
        String::from_utf8(out.stdout).expect("utf8"),
        String::from_utf8(out.stderr).expect("utf8"),
    )
}

#[test]
fn map_typo_path_exits_zero_with_note() {
    let (ok, stdout, _) = run(&["map", "/tmp/ast-bro-does-not-exist-xyz"]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note: path not found:"),
        "missing path-not-found note:\n{stdout}"
    );
}

#[test]
fn no_command_prints_help_exits_zero() {
    let (ok, stdout, _stderr) = run(&[]);
    assert!(ok, "must exit 0 even with no command");
    assert!(
        stdout.contains("Commands:"),
        "expected help text on stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("# note:"),
        "expected explanatory note:\n{stdout}"
    );
}

#[test]
fn unknown_subcommand_prints_help_exits_zero() {
    let (ok, stdout, _) = run(&["totally-bogus-command"]);
    assert!(ok, "must exit 0 on bad subcommand");
    assert!(stdout.contains("Commands:"), "expected help:\n{stdout}");
}

#[test]
fn digest_typo_path_exits_zero_with_note() {
    let (ok, stdout, _) = run(&["digest", "/tmp/ast-bro-does-not-exist-xyz"]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note: path not found:"),
        "missing path-not-found note:\n{stdout}"
    );
}

#[test]
fn implements_typo_path_exits_zero_with_note() {
    let (ok, stdout, _) = run(&[
        "implements",
        "Foo",
        "/tmp/ast-bro-does-not-exist-xyz",
    ]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note: path not found:"),
        "missing path-not-found note:\n{stdout}"
    );
}

#[test]
fn show_missing_path_exits_zero_with_note() {
    let (ok, stdout, _) = run(&[
        "show",
        "/tmp/ast-bro-does-not-exist-xyz",
        "Foo",
    ]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note: path not found:"),
        "missing path-not-found note:\n{stdout}"
    );
}

#[test]
fn show_missing_symbol_exits_zero_with_note() {
    let (ok, stdout, _) = run(&["show", "src/core.rs", "ZzNonexistentSymbolZz"]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note: no symbol matching"),
        "missing no-symbol note:\n{stdout}"
    );
}

#[test]
fn show_unsupported_file_exits_zero_with_note() {
    let (ok, stdout, _) = run(&["show", "Cargo.lock", "package"]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note: unsupported file type"),
        "missing unsupported-file note:\n{stdout}"
    );
}

#[test]
fn find_related_bad_target_exits_zero_with_note() {
    let (ok, stdout, _) = run(&["find-related", "no-colon-here"]);
    assert!(ok, "must exit 0");
    assert!(
        stdout.contains("# note:") && stdout.contains("FILE>:<LINE"),
        "missing bad-target note:\n{stdout}"
    );
}

#[test]
fn happy_path_map_works() {
    let (ok, stdout, _) = run(&["map", "src/core.rs"]);
    assert!(ok, "must exit 0");
    assert!(!stdout.is_empty(), "expected non-empty map output");
    assert!(
        !stdout.contains("# note: path not found:"),
        "should not emit a path-not-found note for an existing file:\n{stdout}"
    );
}
