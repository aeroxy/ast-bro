//! End-to-end smoke tests for `ast-bro surface`. These shell out
//! to the built binary so they exercise the same code path users hit.
//!
//! Fixtures live in `tests/fixtures/surface/<name>/`. Each test asserts
//! a small set of invariants on the output (presence/absence of names,
//! re-export chains, etc.) rather than full snapshots — snapshots are
//! brittle to colour/whitespace changes and these are quick to read.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    // CARGO_BIN_EXE_<bin name> is set by cargo for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn surface(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    assert!(
        out.status.success(),
        "ast-bro surface failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

#[test]
fn rust_chained_glob_reexport() {
    let s = surface(&["surface", "tests/fixtures/surface/rust_chained"]);
    // `pub use net::client::*` should publish Client at the crate root.
    assert!(
        s.contains("rust_chained::Client"),
        "missing glob-reexported Client:\n{s}"
    );
    // Direct decls in lib.rs come through too.
    assert!(s.contains("rust_chained::Error"), "missing Error:\n{s}");
    // Canonical path through `pub mod net` chain.
    assert!(
        s.contains("rust_chained::net::client::Client"),
        "missing canonical Client path:\n{s}"
    );
    // Impl methods get lifted under the type.
    assert!(
        s.contains("rust_chained::net::client::Client::connect"),
        "missing impl method:\n{s}"
    );
    // Private helpers must not leak.
    assert!(!s.contains("private_helper"), "private leaked:\n{s}");
    assert!(!s.contains("_internal"), "_internal leaked:\n{s}");
}

#[test]
fn rust_rename_keeps_alias() {
    let s = surface(&["surface", "tests/fixtures/surface/rust_rename"]);
    assert!(s.contains("rust_rename::Quux"), "alias missing:\n{s}");
    // The original name `Bar` should NOT appear at the crate root —
    // it's not separately exported.
    assert!(
        !s.contains("rust_rename::Bar"),
        "unrenamed Bar leaked:\n{s}"
    );
}

#[test]
fn python_dunder_all_filters_imports() {
    let s = surface(&["surface", "tests/fixtures/surface/python_dunder"]);
    assert!(s.contains("python_dunder.Thing"));
    assert!(s.contains("python_dunder.help_me"));
    // `internal_too` is imported but NOT in __all__ — must be dropped.
    assert!(
        !s.contains("internal_too"),
        "internal_too leaked past __all__:\n{s}"
    );
    // `also_public` is defined in __init__.py but NOT in __all__ — drop.
    assert!(
        !s.contains("also_public"),
        "also_public leaked past __all__:\n{s}"
    );
}

#[test]
fn python_no_dunder_uses_underscore_convention() {
    let s = surface(&["surface", "tests/fixtures/surface/python_no_dunder"]);
    assert!(s.contains("python_no_dunder.public_fn"));
    assert!(s.contains("python_no_dunder.PublicClass"));
    assert!(
        !s.contains("_hidden"),
        "leading-underscore name leaked:\n{s}"
    );
    assert!(!s.contains("_HiddenClass"), "private class leaked:\n{s}");
}

#[test]
fn java_fallback_filters_visibility() {
    let s = surface(&[
        "surface",
        "tests/fixtures/surface/java_fallback",
        "--lang",
        "fallback",
    ]);
    assert!(s.contains("Greeter"), "public class missing:\n{s}");
    assert!(s.contains("greet"), "public method missing:\n{s}");
    assert!(!s.contains("internal"), "private method leaked:\n{s}");
}

#[test]
fn json_schema_present() {
    let s = surface(&[
        "surface",
        "tests/fixtures/surface/rust_chained",
        "--json",
        "--compact",
    ]);
    assert!(
        s.contains("\"schema\":\"ast-bro.surface.v1\""),
        "schema id missing:\n{s}"
    );
    assert!(s.contains("\"qualified_path\":\"rust_chained::Client\""));
}

#[test]
fn ts_barrel_resolves_named_glob_and_rename() {
    let s = surface(&["surface", "tests/fixtures/surface/ts_barrel"]);
    // Inline export class — picked up directly.
    assert!(s.contains("ts_barrel.Direct"), "Direct missing:\n{s}");
    // Lifted method.
    assert!(
        s.contains("ts_barrel.Direct.greet"),
        "Direct.greet missing:\n{s}"
    );
    // `export { Client } from './client'` — barrel resolution.
    assert!(s.contains("ts_barrel.Client"), "Client barrel missing:\n{s}");
    assert!(
        s.contains("ts_barrel.Client.connect"),
        "Client.connect missing:\n{s}"
    );
    // Rename via `export { Util as Helper }`.
    assert!(s.contains("ts_barrel.Helper"), "Helper rename missing:\n{s}");
    assert!(
        !s.contains("ts_barrel.Util"),
        "unrenamed Util leaked:\n{s}"
    );
    // `export *` glob — type and interface.
    assert!(s.contains("ts_barrel.Id"), "Id glob missing:\n{s}");
    assert!(s.contains("ts_barrel.Spec"), "Spec glob missing:\n{s}");
    assert!(
        s.contains("[via *]"),
        "glob marker missing:\n{s}"
    );
}

#[test]
fn ts_exports_field_picks_types_condition() {
    let s = surface(&["surface", "tests/fixtures/surface/ts_exports_field"]);
    // The `exports` field's `types` condition points at index.d.ts —
    // both `FromTypes` (with method) and `topLevel` should surface.
    assert!(
        s.contains("ts_exports_field.FromTypes"),
        "FromTypes missing:\n{s}"
    );
    assert!(
        s.contains("ts_exports_field.FromTypes.hello"),
        "lifted hello() missing:\n{s}"
    );
    assert!(
        s.contains("ts_exports_field.topLevel"),
        "topLevel missing:\n{s}"
    );
}

#[test]
fn scala_export_clauses_republish() {
    let s = surface(&["surface", "tests/fixtures/surface/scala_exports"]);
    // Direct top-level decls (visibility fallback path).
    assert!(s.contains("mypkg.Api"), "Api missing:\n{s}");
    assert!(s.contains("mypkg.PublicClass"), "PublicClass missing:\n{s}");
    assert!(
        s.contains("mypkg.PublicClass.publicMethod"),
        "lifted publicMethod missing:\n{s}"
    );
    // `export internal.Helper` — relative path, should land at mypkg.Helper.
    assert!(s.contains("mypkg.Helper"), "Helper re-export missing:\n{s}");
    // `export internal.utils.*` — glob expands util1 and util2.
    assert!(s.contains("mypkg.util1"), "util1 glob missing:\n{s}");
    assert!(s.contains("mypkg.util2"), "util2 glob missing:\n{s}");
    // private class is filtered.
    assert!(
        !s.contains("HiddenClass"),
        "private HiddenClass leaked:\n{s}"
    );
}

#[test]
fn unknown_lang_errors_cleanly() {
    // Unknown --lang is a handled CLI error: stay rc=0, print a `# note:`
    // on stdout so agentic harnesses don't abort the surrounding bash batch.
    let out = Command::new(bin())
        .args(&["surface", ".", "--lang", "cobol"])
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(out.status.success(), "should exit 0 on handled error");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("# note:") && stdout.contains("unknown --lang"),
        "expected friendly note on stdout:\nstdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
