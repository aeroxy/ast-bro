//! End-to-end tests for `ast-bro deps|reverse-deps|cycles|graph`.
//! Each test shells out to the built binary against a fixture directory
//! under `tests/fixtures/deps/`. Tests assert invariants on the output
//! (presence/absence of specific edges) rather than full snapshots.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    let code = out.status.code().unwrap_or(-1);
    (code, stdout, stderr)
}

fn run_ok(args: &[&str]) -> String {
    let (code, stdout, stderr) = run(args);
    assert!(
        code == 0,
        "expected exit 0, got {}\nstdout: {}\nstderr: {}",
        code, stdout, stderr
    );
    stdout
}

// ---- Rust ----

#[test]
fn rust_simple_forward_deps() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/rust_simple/src/net.rs",
        "--depth",
        "2",
        "--rebuild",
    ]);
    // net.rs imports error.rs via `use crate::error::Error`.
    assert!(s.contains("error.rs"), "expected error.rs in deps:\n{s}");
}

#[test]
fn rust_simple_reverse_deps() {
    let s = run_ok(&[
        "reverse-deps",
        "tests/fixtures/deps/rust_simple/src/error.rs",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("net.rs"), "expected net.rs as importer:\n{s}");
}

#[test]
fn rust_cycle_detected() {
    let (code, stdout, _stderr) = run(&[
        "cycles",
        "tests/fixtures/deps/rust_cycle",
        "--rebuild",
    ]);
    assert_eq!(code, 3, "cycles command should exit 3 when cycle present");
    assert!(stdout.contains("cycle"), "missing cycle word: {stdout}");
    assert!(stdout.contains("a.rs"), "missing a.rs: {stdout}");
    assert!(stdout.contains("b.rs"), "missing b.rs: {stdout}");
}

#[test]
fn rust_simple_no_cycle() {
    let (code, stdout, _) = run(&[
        "cycles",
        "tests/fixtures/deps/rust_simple",
        "--rebuild",
    ]);
    assert_eq!(code, 0, "expected exit 0 (no cycles): {stdout}");
    assert!(stdout.contains("no cycles"), "expected no cycles message: {stdout}");
}

// ---- Python ----

#[test]
fn python_relative_from_import() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/python_pkg/pkg/sub.py",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("helpers.py"), "expected helpers.py in deps:\n{s}");
}

#[test]
fn python_init_resolves_to_init_py() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/python_pkg/pkg/__init__.py",
        "--depth",
        "1",
        "--rebuild",
    ]);
    // __init__.py imports both .helpers and .sub.
    assert!(s.contains("helpers.py"), "expected helpers.py:\n{s}");
    assert!(s.contains("sub.py"), "expected sub.py:\n{s}");
}

// ---- TypeScript ----

#[test]
fn ts_barrel_resolves_relative_imports() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/ts_barrel/src/index.ts",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("client.ts"), "expected client.ts:\n{s}");
    assert!(s.contains("util.ts"), "expected util.ts:\n{s}");
}

#[test]
fn ts_client_imports_util() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/ts_barrel/src/client.ts",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("util.ts"), "expected util.ts:\n{s}");
}

// ---- Java ----

#[test]
fn java_fqn_resolves_via_package_index() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/java_basic/com/example/Greeter.java",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("Formatter.java"),
        "expected Formatter.java via FQN suffix index:\n{s}"
    );
}

// ---- Go ----

#[test]
fn go_module_prefix_strips_correctly() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/go_module/main.go",
        "--depth",
        "1",
        "--rebuild",
    ]);
    // util/util.go is the resolved file for `example.com/myapp/util`.
    assert!(s.contains("util.go"), "expected util.go:\n{s}");
    // External `fmt` should not appear as a resolved edge.
    assert!(!s.contains("fmt.go"), "unexpected stdlib resolution:\n{s}");
}

// ---- Graph emission ----

#[test]
fn graph_json_carries_schema() {
    let s = run_ok(&[
        "graph",
        "tests/fixtures/deps/rust_simple",
        "--json",
        "--rebuild",
    ]);
    assert!(
        s.contains("ast-bro.graph.v1"),
        "schema constant missing:\n{s}"
    );
}

// ---- Cache freshness ----

#[test]
fn cache_round_trip_returns_same_graph() {
    // First build (with --rebuild) produces edges; second call (no
    // --rebuild) should hit the cache and return the same edge count.
    let s1 = run_ok(&[
        "graph",
        "tests/fixtures/deps/rust_simple",
        "--json",
        "--rebuild",
    ]);
    let s2 = run_ok(&[
        "graph",
        "tests/fixtures/deps/rust_simple",
        "--json",
    ]);
    // Strip `built_at` since it differs run-to-run.
    let extract = |s: &str| {
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        (v["file_count"].clone(), v["edge_count"].clone(), v["edges"].clone())
    };
    let (f1, e1, edges1) = extract(&s1);
    let (f2, e2, edges2) = extract(&s2);
    assert_eq!(f1, f2);
    assert_eq!(e1, e2);
    assert_eq!(edges1, edges2);
}

// ---- PHP ----

#[test]
fn php_psr4_use_resolves() {
    // `use App\Account;` in src/User.php resolves to src/Account.php via
    // the composer.json psr-4 mapping `App\\: src/`.
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/php_psr4/src/User.php",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("Account.php"),
        "expected Account.php in deps:\n{s}"
    );
}

#[test]
fn php_require_literal_resolves_relative() {
    // `require_once 'helpers.php'` inside src/User.php resolves to
    // src/helpers.php (resolver treats it as relative to the source file).
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/php_psr4/src/User.php",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("helpers.php"),
        "expected helpers.php in deps:\n{s}"
    );
}

#[test]
fn php_require_parenthesized_resolves() {
    // Regression: `require_once('utils.php')` wraps the string in a
    // `parenthesized_expression`. The extractor must descend into it.
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/php_psr4/src/User.php",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("utils.php"),
        "expected utils.php from parenthesized require_once:\n{s}"
    );
}

#[test]
fn php_reverse_deps() {
    let s = run_ok(&[
        "reverse-deps",
        "tests/fixtures/deps/php_psr4/src/Account.php",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("User.php"),
        "expected User.php as importer:\n{s}"
    );
}

// ---- C++ ----

#[test]
fn cpp_local_include_resolves() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/cpp_basic/src/main.cpp",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("lib.h"), "expected lib.h in deps:\n{s}");
}

#[test]
fn cpp_transitive_include() {
    // main.cpp → lib.h → util.h
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/cpp_basic/src/main.cpp",
        "--depth",
        "2",
        "--rebuild",
    ]);
    assert!(
        s.contains("util.h"),
        "expected util.h reachable transitively:\n{s}"
    );
}

#[test]
fn cpp_system_header_is_external() {
    // `<vector>` and `<string>` are system headers; they should appear in
    // the external-imports listing rather than as resolved edges.
    let s = run_ok(&[
        "graph",
        "tests/fixtures/deps/cpp_basic",
        "--include-external",
        "--json",
        "--rebuild",
    ]);
    let v: serde_json::Value = serde_json::from_str(&s).expect("graph json");
    let externals = v["external"].as_array().expect("external array");
    let main_specs: Vec<String> = externals
        .iter()
        .filter(|e| e["from"].as_str().is_some_and(|p| p.contains("main.cpp")))
        .filter_map(|e| e["spec"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        main_specs.iter().any(|s| s.contains("vector")),
        "expected <vector> external in main.cpp:\n{:?}",
        main_specs
    );
    // Local include should NOT appear in externals.
    assert!(
        !main_specs.iter().any(|s| s.contains("lib.h")),
        "lib.h should be a resolved edge, not external:\n{:?}",
        main_specs
    );
}

#[test]
fn cpp_reverse_deps() {
    let s = run_ok(&[
        "reverse-deps",
        "tests/fixtures/deps/cpp_basic/src/util.h",
        "--depth",
        "2",
        "--rebuild",
    ]);
    // util.h is included by lib.h and util.cpp; lib.h is included by main.cpp.
    assert!(s.contains("lib.h"), "expected lib.h as importer:\n{s}");
    assert!(s.contains("util.cpp"), "expected util.cpp as importer:\n{s}");
}

// ---- Ruby ----

#[test]
fn ruby_require_relative_resolves() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/ruby_relative/main.rb",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("helpers.rb"),
        "expected helpers.rb in deps:\n{s}"
    );
}

#[test]
fn ruby_gem_require_is_external() {
    // `require 'json'` should not resolve to any project file. Verify it
    // appears in the external-imports listing instead.
    let s = run_ok(&[
        "graph",
        "tests/fixtures/deps/ruby_relative",
        "--include-external",
        "--json",
        "--rebuild",
    ]);
    let v: serde_json::Value = serde_json::from_str(&s).expect("graph json");
    let externals = v["external"].as_array().expect("external array");
    let main_specs: Vec<String> = externals
        .iter()
        .filter(|e| e["from"].as_str().is_some_and(|p| p.contains("main.rb")))
        .filter_map(|e| e["spec"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        main_specs.iter().any(|s| s == "json"),
        "expected `json` as external in main.rb:\n{:?}",
        main_specs
    );
}

#[test]
fn ruby_reverse_deps() {
    let s = run_ok(&[
        "reverse-deps",
        "tests/fixtures/deps/ruby_relative/helpers.rb",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("main.rb"), "expected main.rb as importer:\n{s}");
    assert!(s.contains("lib.rb"), "expected lib.rb as importer:\n{s}");
}

// ---- Idempotency ----

#[test]
fn graph_build_is_idempotent() {
    let s1 = run_ok(&[
        "graph",
        "tests/fixtures/deps/python_pkg",
        "--json",
        "--rebuild",
    ]);
    let s2 = run_ok(&[
        "graph",
        "tests/fixtures/deps/python_pkg",
        "--json",
        "--rebuild",
    ]);
    let extract = |s: &str| {
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        v["edges"].clone()
    };
    assert_eq!(extract(&s1), extract(&s2));
}
