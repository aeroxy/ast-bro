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
    // symbols we want exact-suffix equality. Under the error contract
    // (#36) a non-matching symbol is a rejected call: exit 2, stderr.
    let out = Command::new(bin())
        .args(["show", "src/implements.rs", "find_imp"])
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "substring must not match a code symbol");
    assert!(
        out.stdout.is_empty(),
        "a rejected call puts nothing on stdout (#36): {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    assert!(
        stderr.contains("no symbol matching"),
        "code symbol substring match leaked:\n{stderr}"
    );
    let s2 = run(&["show", "src/implements.rs", "find_implementations"]);
    assert!(
        s2.contains("find_implementations") && s2.contains("function"),
        "exact code symbol match broken:\n{s2}"
    );
}

/// A heading is prose, and prose has dots in it.
///
/// The trail a target is matched against splits a *namespace* name into
/// segments, because a namespace name is itself a qualified path. A heading
/// is one name however many dots it contains, and splitting it put every word
/// before the last dot in a segment no one-word query is ever compared
/// against: `## Internals (src/impact.rs)` stopped answering to `Internals`,
/// and so would `Node.js`, `vs.`, a version number or a filename.
#[test]
fn a_heading_with_a_dot_still_matches_its_first_word() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("README.md");
    std::fs::write(
        &file,
        "# Doc\n\n## Internals (src/impact.rs)\n\nbody\n\n## Node.js setup\n\nmore\n",
    )
    .expect("write");
    let path = file.to_string_lossy().to_string();
    for target in ["Internals", "impact", "Node", "setup"] {
        let s = run(&["show", &path, target]);
        assert!(
            s.contains("(heading)"),
            "{target:?} matched no heading:\n{s}"
        );
    }
}
