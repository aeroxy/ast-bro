//! End-to-end tests for `ast-bro context` — token-budgeted context packs.
//!
//! Exercises the budget-degradation ladder (full body → signature only),
//! the `truncated` flag when the budget runs out before transitive
//! context, and dedup of symbols that are both callee and caller.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run_in(dir: &std::path::Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    (stdout, out.status.code().unwrap_or(-1))
}

fn write(p: &std::path::Path, body: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

fn scaffold(root: &std::path::Path, lib: &str) {
    write(
        &root.join("Cargo.toml"),
        "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    );
    write(&root.join("src/lib.rs"), lib);
}

const BASIC_LIB: &str = r#"
pub fn helper_dep() -> u32 { 41 }

pub fn target_fn() -> u32 {
    helper_dep() + 1
}

pub fn top_caller() -> u32 {
    target_fn()
}
"#;

#[test]
fn large_budget_includes_target_body_and_neighbours() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root, BASIC_LIB);

    let (out, code) = run_in(
        root,
        &["context", "target_fn", ".", "--budget", "8000", "--json", "--rebuild"],
    );
    assert_eq!(code, 0, "context exited non-zero: {}", out);

    assert!(out.contains(r#""label": "target""#), "expected full-body target entry, got:\n{}", out);
    assert!(out.contains("helper_dep() + 1"), "expected target body inline, got:\n{}", out);
    assert!(out.contains("helper_dep"), "expected direct dependency helper_dep, got:\n{}", out);
    assert!(out.contains("top_caller"), "expected direct dependent top_caller, got:\n{}", out);
    assert!(out.contains(r#""truncated": false"#), "nothing should be truncated at 8000 tokens, got:\n{}", out);
    assert!(out.contains(r#""target_omitted": false"#), "target must not be omitted, got:\n{}", out);
}

#[test]
fn tiny_budget_degrades_target_to_signature() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Body well over 10 tokens (~40 bytes), signature line well under.
    scaffold(
        root,
        r#"
pub fn target_fn() -> String {
    let mut s = String::new();
    s.push_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    s.push_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    s.push_str("cccccccccccccccccccccccccccccccccccccccc");
    s
}
"#,
    );

    let (out, code) = run_in(
        root,
        &["context", "target_fn", ".", "--budget", "10", "--json", "--rebuild"],
    );
    assert_eq!(code, 0, "context exited non-zero: {}", out);
    assert!(out.contains(r#""target_omitted": true"#), "expected target body omitted, got:\n{}", out);
    assert!(
        out.contains("signature only — budget"),
        "expected signature-only degradation label, got:\n{}",
        out
    );
    assert!(
        !out.contains("aaaaaaaaaaaaaaaaaaaa"),
        "body must not leak into a signature-only entry, got:\n{}",
        out
    );
}

#[test]
fn budget_exhaustion_sets_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Small target; callee whose body AND signature both blow the budget.
    scaffold(
        root,
        r#"
pub fn big_callee_with_an_extremely_long_name_and_parameter_list(first_parameter_with_long_name: u64, second_parameter_with_long_name: u64, third_parameter_with_long_name: u64) -> u64 {
    first_parameter_with_long_name + second_parameter_with_long_name + third_parameter_with_long_name + 111111111 + 222222222 + 333333333 + 444444444 + 555555555
}

pub fn target_fn() -> u64 {
    big_callee_with_an_extremely_long_name_and_parameter_list(1, 2, 3)
}
"#,
    );

    let (out, code) = run_in(
        root,
        &["context", "target_fn", ".", "--budget", "40", "--json", "--rebuild"],
    );
    assert_eq!(code, 0, "context exited non-zero: {}", out);
    assert!(out.contains(r#""truncated": true"#), "expected truncated=true when the callee can't fit, got:\n{}", out);
}

#[test]
fn symbol_that_is_both_callee_and_caller_appears_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // buddy calls target_fn AND is called by it (mutual recursion):
    // it qualifies as both a direct dependency and a direct dependent.
    scaffold(
        root,
        r#"
pub fn buddy(n: u32) -> u32 {
    if n == 0 { 0 } else { target_fn(n - 1) }
}

pub fn target_fn(n: u32) -> u32 {
    if n == 0 { 0 } else { buddy(n - 1) }
}
"#,
    );

    let (out, code) = run_in(
        root,
        &["context", "target_fn", ".", "--budget", "8000", "--json", "--rebuild"],
    );
    assert_eq!(code, 0, "context exited non-zero: {}", out);
    let occurrences = out.matches(r#""qn": "src/lib.rs::buddy""#).count();
    assert_eq!(
        occurrences, 1,
        "buddy must appear exactly once (deduped), found {} times in:\n{}",
        occurrences, out
    );
}
