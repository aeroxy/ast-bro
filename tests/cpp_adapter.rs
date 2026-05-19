//! End-to-end coverage for the C++ adapter. Each test asserts a distinct
//! shape the adapter must produce against
//! `tests/fixtures/cpp_adapter/sample.cpp`:
//!   1. `class_specifier` / `struct_specifier` / `enum_specifier` are the
//!      right tree-sitter-cpp kinds (the adapter previously used
//!      `class_definition` and emitted nothing).
//!   2. Constructor and destructor declarations (`declaration` with
//!      `function_declarator`, no return type) are surfaced as methods.
//!   3. Free functions and namespaces still parse alongside class bodies.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

const FIXTURE: &str = "tests/fixtures/cpp_adapter/sample.cpp";

fn run(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(out.status.success(), "exit non-zero: {:?}", out);
    String::from_utf8(out.stdout).expect("utf8")
}

#[test]
fn class_struct_enum_namespace_all_render() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("namespace geom"), "namespace missing:\n{s}");
    assert!(s.contains("struct Point"), "struct missing:\n{s}");
    assert!(s.contains("enum Color"), "enum missing:\n{s}");
    assert!(s.contains("class Shape"), "class missing:\n{s}");
}

#[test]
fn ctor_and_dtor_appear_under_class() {
    // Constructor and destructor are the headline regression case for the
    // C++ adapter: tree-sitter-cpp emits them as `declaration` nodes (not
    // `function_definition`) so the adapter needs an explicit branch.
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("Shape(int sides)"), "ctor missing:\n{s}");
    assert!(s.contains("~Shape()"), "dtor missing:\n{s}");
}

#[test]
fn struct_fields_and_class_field_surfaced() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("int x"), "struct field x missing:\n{s}");
    assert!(s.contains("int y"), "struct field y missing:\n{s}");
    assert!(s.contains("int sides_"), "private class field missing:\n{s}");
}

#[test]
fn top_level_free_function_surfaced() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("free_function"), "free function missing:\n{s}");
}
