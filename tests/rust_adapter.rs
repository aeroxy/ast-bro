//! End-to-end coverage for the Rust adapter. Each test asserts a
//! distinct shape the adapter must produce:
//!   1. impl regrouping — `impl Trait for Foo` lifts Trait into Foo.bases
//!      so `implements Trait` finds the real struct (not a synthetic shadow)
//!   2. extern "C" blocks — surfaced as Namespace with fn/static children
//!   3. macro_rules! — surfaced as Delegate; #[macro_export] → public
//!   4. tuple/unit structs — positional fields named "0", "1", …
//!   5. trait associated types and consts — surfaced as Field

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

const FIXTURE: &str = "tests/fixtures/rust_adapter/sample.rs";

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
fn implements_finds_struct_not_impl_shadow() {
    // Before the fix: `implements Greeter` returned `class impl_Person`.
    // After: it returns `struct Person` — the actual type users navigate.
    let s = run(&["implements", "Greeter", FIXTURE]);
    assert!(
        s.contains("struct Person"),
        "should find Person as a struct:\n{s}"
    );
    assert!(
        !s.contains("impl_Person"),
        "should not leak the synthetic impl_Person name:\n{s}"
    );
}

#[test]
fn impl_methods_lifted_into_target_type() {
    // After regrouping, the methods declared in `impl Person` and
    // `impl Greeter for Person` should appear as children of Person.
    let s = run(&["map", FIXTURE]);
    let person_block = s
        .split("pub struct Person")
        .nth(1)
        .expect("Person header missing")
        .split("\n\n")
        .next()
        .unwrap();
    assert!(
        person_block.contains("fn new("),
        "inherent method `new` missing under Person:\n{person_block}"
    );
    assert!(
        person_block.contains("fn hello("),
        "trait method `hello` missing under Person:\n{person_block}"
    );
}

#[test]
fn extern_block_surfaced_as_namespace() {
    let s = run(&["map", FIXTURE]);
    assert!(
        s.contains("namespace extern \"C\""),
        "extern block not surfaced as namespace:\n{s}"
    );
    assert!(
        s.contains("pub fn libc_strlen"),
        "foreign fn missing:\n{s}"
    );
    assert!(
        s.contains("pub static LIBC_ERRNO"),
        "foreign static missing:\n{s}"
    );
}

#[test]
fn macro_rules_surfaced_with_export_visibility() {
    let s = run(&["map", FIXTURE]);
    assert!(
        s.contains("macro_rules! shout"),
        "exported macro missing:\n{s}"
    );
    assert!(
        s.contains("macro_rules! private_helper"),
        "private macro missing:\n{s}"
    );
    // `#[macro_export]` should make the public form prefix the line.
    let shout_line = s.lines().find(|l| l.contains("shout")).expect("shout line");
    assert!(
        shout_line.contains("pub") || shout_line.contains("#[macro_export]"),
        "exported macro should reflect public visibility:\n{shout_line}"
    );
}

#[test]
fn tuple_struct_has_positional_fields() {
    let s = run(&["map", FIXTURE]);
    // Pair should list two fields, named "0" and "1".
    let pair_block = s
        .split("pub struct Pair")
        .nth(1)
        .expect("Pair header missing")
        .split("\n\n")
        .next()
        .unwrap();
    assert!(
        pair_block.contains("0") && pair_block.contains("1"),
        "tuple positional fields missing:\n{pair_block}"
    );
}

#[test]
fn unit_struct_has_no_body() {
    let s = run(&["map", FIXTURE]);
    // Marker is `pub struct Marker;` — should appear with no children.
    let marker_block = s
        .split("pub struct Marker")
        .nth(1)
        .expect("Marker header missing")
        .split("\n\n")
        .next()
        .unwrap();
    let body_lines: Vec<&str> = marker_block
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(
        body_lines.is_empty() || body_lines.iter().all(|l| !l.starts_with("    ")),
        "unit struct should have no indented body:\n{marker_block}"
    );
}

#[test]
fn trait_assoc_types_and_consts_surfaced() {
    let s = run(&["map", FIXTURE]);
    let storage_block = s
        .split("pub trait Storage")
        .nth(1)
        .expect("Storage header missing");
    assert!(
        storage_block.contains("type Key"),
        "associated type missing:\n{storage_block}"
    );
    assert!(
        storage_block.contains("const VERSION"),
        "associated const missing:\n{storage_block}"
    );
}

#[test]
fn implements_finds_local_adapter_traits() {
    // Real-world regression check: the adapters in src/adapters/ all
    // implement LanguageAdapter. Before the fix, `implements
    // LanguageAdapter` returned `impl_RustAdapter` shadows; after, it
    // should return the real struct names.
    let s = run(&["implements", "LanguageAdapter", "src/adapters/"]);
    for adapter in &[
        "RustAdapter",
        "PythonAdapter",
        "TypeScriptAdapter",
        "JavaAdapter",
        "GoAdapter",
        "KotlinAdapter",
        "ScalaAdapter",
        "CSharpAdapter",
    ] {
        assert!(
            s.contains(&format!("struct {adapter}")),
            "missing real struct {adapter} (impl shadow leaking?):\n{s}"
        );
    }
    assert!(
        !s.contains("impl_"),
        "synthetic impl_X names should not appear:\n{s}"
    );
}
