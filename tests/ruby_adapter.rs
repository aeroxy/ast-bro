//! End-to-end coverage for the Ruby adapter against
//! `tests/fixtures/ruby_adapter/sample.rb`. Asserts:
//!   1. `module` maps to the canonical `Namespace` kind (Ruby modules are
//!      mixin/namespacing units, not classes), with `native_kind: "module"`
//!      preserved for outline rendering.
//!   2. `private` / `protected` are bare identifier tokens that toggle the
//!      visibility scope for following method definitions; the adapter
//!      tracks that state.
//!   3. `attr_reader` / `attr_accessor` and Rails-style association macros
//!      are surfaced as `Field`-kind declarations.
//!   4. Constants and singleton methods (`def self.find`) render.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

const FIXTURE: &str = "tests/fixtures/ruby_adapter/sample.rb";

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
fn module_renders_as_namespace() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("namespace Billing"), "module → namespace missing:\n{s}");
    assert!(s.contains("class Account"), "nested class missing:\n{s}");
}

#[test]
fn private_and_protected_visibility_tracked() {
    // The `private`/`protected` tokens between method definitions are
    // not fields on the AST — the adapter tracks them as state in the
    // class-body walk and applies them to every following method.
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("private def secret"), "private method missing:\n{s}");
    assert!(
        s.contains("protected def helper"),
        "protected method missing:\n{s}"
    );
    // Methods declared before `private` should not pick up the modifier.
    assert!(
        !s.contains("private def public_method") && !s.contains("private def initialize"),
        "visibility leaked onto pre-private methods:\n{s}"
    );
}

#[test]
fn attr_macros_and_rails_associations_surface() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("attr_accessor"), "attr_accessor missing:\n{s}");
    assert!(s.contains("attr_reader"), "attr_reader missing:\n{s}");
    assert!(s.contains("has_many"), "Rails association missing:\n{s}");
}

#[test]
fn constants_and_singleton_methods_render() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("VERSION"), "constant missing:\n{s}");
    assert!(s.contains("def self.find"), "singleton method missing:\n{s}");
}

#[test]
fn class_inheritance_captured() {
    let s = run(&["map", FIXTURE]);
    // `class User < Account` — both names must appear in the same outline.
    assert!(s.contains("class User"), "subclass missing:\n{s}");
    assert!(s.contains("Account"), "superclass missing in outline:\n{s}");
}
