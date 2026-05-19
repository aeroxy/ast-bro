//! markdown headings match by case-insensitive substring per
//! dotted part (`show README.md install` → `## Installation`). Code
//! symbols remain exact suffix-equality.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(out.status.success());
    String::from_utf8(out.stdout).expect("utf8")
}

#[test]
fn markdown_heading_substring_match() {
    let s = run(&["show", "README.md", "install"]);
    assert!(
        s.contains("## Install") || s.contains("## Installation"),
        "expected substring match against README install heading:\n{s}"
    );
    assert!(
        !s.contains("# note: no symbol"),
        "should not report no-match:\n{s}"
    );
}

#[test]
fn markdown_heading_case_insensitive() {
    let s = run(&["show", "README.md", "INSTALL"]);
    assert!(
        s.contains("## Install"),
        "expected case-insensitive match:\n{s}"
    );
}

#[test]
fn code_symbol_match_stays_exact() {
    // Substring would have matched `find_implementations` — but for code
    // symbols we want exact-suffix equality.
    let s = run(&["show", "src/core.rs", "find_imp"]);
    assert!(
        s.contains("# note: no symbol matching"),
        "code symbol substring match leaked:\n{s}"
    );
    let s2 = run(&["show", "src/core.rs", "find_implementations"]);
    assert!(
        s2.contains("find_implementations") && s2.contains("function"),
        "exact code symbol match broken:\n{s2}"
    );
}
