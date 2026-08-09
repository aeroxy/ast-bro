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

#[test]
fn go_nested_module_wins_over_enclosing_one() {
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/go_multi_module/main.go",
        "--depth",
        "1",
        "--rebuild",
    ]);
    // `example.com/multi/util` belongs to the outer module.
    assert!(s.contains("util/util.go"), "expected util/util.go:\n{s}");
    // `example.com/multi/tools/helper` matches both module prefixes. The
    // outer module holds the importer, so it is tried first — and its
    // `tools/` directory does not exist, which is the case a candidate has
    // to be *skipped* for rather than treated as the answer. The search
    // falls through to the nested `toolkit/go.mod`, whose declared path
    // does not mirror its directory, and the import lands in
    // `toolkit/helper/`.
    assert!(
        s.contains("toolkit/helper/helper.go"),
        "expected toolkit/helper/helper.go:\n{s}"
    );
}

#[test]
fn go_duplicate_module_resolves_within_the_importers_copy() {
    // `backup/` and `service/` both declare `example.com/dup` — the shape a
    // nested checkout or a vendored copy leaves in the tree. Each importer
    // must stay inside its own copy. Both directions are checked because
    // directory-walk order is not alphabetical: picking a fixed copy would
    // satisfy one direction by luck.
    for (own, other) in [("service", "backup"), ("backup", "service")] {
        let s = run_ok(&[
            "deps",
            &format!("tests/fixtures/deps/go_dup_module/{own}/main.go"),
            "--depth",
            "1",
            "--rebuild",
        ]);
        assert!(
            s.contains(&format!("{own}/util/util.go")),
            "expected {own}/util/util.go:\n{s}"
        );
        assert!(
            !s.contains(&format!("{other}/util/util.go")),
            "import from {own} escaped into {other}:\n{s}"
        );
    }
}

#[test]
fn go_importers_own_module_wins_over_a_sibling_declaring_a_longer_path() {
    // `testdata/broken/go.mod` declares `example.com/sibling/sub`, a longer
    // path than the module holding the importer — and it holds an `inner/`
    // package, so both candidates resolve. Ranking on prefix length alone
    // sends the import into `testdata/`, a separate module that is not part
    // of the importer's build at all.
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/go_sibling_module/main.go",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(
        s.contains("sub/inner/inner.go"),
        "expected sub/inner/inner.go:\n{s}"
    );
    assert!(
        !s.contains("testdata/broken/inner/inner.go"),
        "import escaped into the sibling module under testdata/:\n{s}"
    );
}

#[test]
fn go_import_of_the_module_root_package_resolves() {
    // `import "example.com/rootpkg"` names the package sitting in the module
    // directory itself — the layout most single-package Go libraries use.
    // It has no path segment after the module prefix, so a rule that only
    // understands `<module>/<subpackage>` drops it into the external bucket.
    let s = run_ok(&[
        "deps",
        "tests/fixtures/deps/go_root_package/cmd/main.go",
        "--depth",
        "1",
        "--rebuild",
    ]);
    assert!(s.contains("rootpkg.go"), "expected rootpkg.go:\n{s}");
    assert!(
        !s.contains("[external] example.com/rootpkg"),
        "module-root package left in the external bucket:\n{s}"
    );
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

// ---- Depth-cap frontier reporting (issue #32) ----

/// A three-file import chain, so `--depth 1` demonstrably stops with a file
/// left to walk and `--depth 3` demonstrably does not.
fn frontier_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write("Cargo.toml", "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write("src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\n");
    // a -> b -> c
    write("src/a.rs", "use crate::b::B;\npub struct A(pub B);\n");
    write("src/b.rs", "use crate::c::C;\npub struct B(pub C);\n");
    write("src/c.rs", "pub struct C;\n");
    tmp
}

fn run_in(dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).expect("utf8 stdout"),
        String::from_utf8(out.stderr).expect("utf8 stderr"),
    )
}

fn frontier_flag(json: &str) -> bool {
    let v: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    v["frontier_truncated"].as_bool().expect("frontier_truncated present")
}

#[test]
fn deps_depth_cutoff_reports_frontier() {
    let tmp = frontier_fixture();
    let root = tmp.path();

    // Depth 1 reaches b.rs and stops; b.rs still imports c.rs.
    let (code, out, err) = run_in(
        root,
        &["deps", "src/a.rs", "--depth", "1", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert!(frontier_flag(&out), "depth 1 left c.rs unwalked: {out}");

    // Depth 3 exhausts the chain — the flag must clear rather than stay on.
    let (_, out, _) = run_in(root, &["deps", "src/a.rs", "--depth", "3", "--json", "--compact"]);
    assert!(!frontier_flag(&out), "depth 3 walks the whole chain: {out}");
}

#[test]
fn deps_frontier_note_lands_on_stderr() {
    let tmp = frontier_fixture();
    let (code, out, err) = run_in(
        tmp.path(),
        &["deps", "src/a.rs", "--depth", "1", "--rebuild"],
    );
    assert_eq!(code, 0, "a qualified answer is still an answer: {out}{err}");
    assert!(
        err.contains("--depth 1") && err.contains("raise --depth"),
        "the note must name the cap and the flag that lifts it: {err}"
    );
    assert!(
        !out.contains("raise --depth"),
        "stdout carries results only (#36): {out}"
    );
}

#[test]
fn reverse_deps_depth_cutoff_reports_frontier() {
    let tmp = frontier_fixture();
    let root = tmp.path();

    // Importers of c.rs: b.rs and lib.rs (`pub mod c;`) at depth 1, a.rs at
    // depth 2. `total` counts only what the walk reached, so without the flag
    // a capped 2 is indistinguishable from a file with exactly two importers
    // (issue #32).
    let (code, out, err) = run_in(
        root,
        &["reverse-deps", "src/c.rs", "--depth", "1", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["total"], 2, "{out}");
    assert_eq!(v["truncated"], false, "--limit did not cut anything: {out}");
    assert_eq!(v["frontier_truncated"], true, "--depth did: {out}");

    let (_, out, _) = run_in(
        root,
        &["reverse-deps", "src/c.rs", "--depth", "3", "--json", "--compact"],
    );
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["total"], 3, "{out}");
    assert_eq!(v["frontier_truncated"], false, "{out}");
}

#[test]
fn frontier_flag_is_absent_of_limit_truncation() {
    // The two caps are orthogonal: `--limit` cuts the display of a complete
    // walk, and must not set the depth-frontier flag.
    let tmp = frontier_fixture();
    let (_, out, _) = run_in(
        tmp.path(),
        &["reverse-deps", "src/c.rs", "--depth", "3", "--limit", "1", "--json", "--compact", "--rebuild"],
    );
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["total"], 3, "{out}");
    assert_eq!(v["truncated"], true, "{out}");
    assert_eq!(v["frontier_truncated"], false, "{out}");
}

#[test]
fn reverse_deps_stdout_header_qualifies_a_depth_bounded_total() {
    // The count in the header is the number a reader quotes, and at `--depth
    // 1` it counts only part of the cone. The stderr note says so, but stdout
    // is where the result is (issue #32).
    let tmp = frontier_fixture();
    let root = tmp.path();

    let (code, out, err) = run_in(root, &["reverse-deps", "src/c.rs", "--depth", "1", "--rebuild"]);
    assert_eq!(code, 0, "{out}{err}");
    // Assert the exact form, not just the substring: the qualifier has to
    // follow `N total`, so a `(\d+) total` matcher keeps working on the
    // capped output as well as the complete one.
    assert!(
        out.contains("(2 total within --depth 1)"),
        "a depth-bounded total must say so on stdout, after the count: {out}"
    );

    // A walk that reached the end of the graph needs no qualification.
    let (_, out, _) = run_in(root, &["reverse-deps", "src/c.rs", "--depth", "3"]);
    assert!(out.contains("(3 total)"), "{out}");
    assert!(
        !out.contains("within --depth"),
        "an exhausted walk must not carry the qualifier: {out}"
    );
}

#[test]
fn reverse_deps_header_shapes_are_all_pinned() {
    // `render_reverse_deps_text` emits four headers, one per (depth cap ×
    // limit cap) combination, from one template. Pin all four: the template
    // has been rewritten twice, and the round it broke, the break was in the
    // shape no test asserted.
    let tmp = frontier_fixture();
    let root = tmp.path();
    let header = |args: &[&str]| {
        let mut argv = vec!["reverse-deps", "src/c.rs"];
        argv.extend(args);
        let (code, out, err) = run_in(root, &argv);
        assert_eq!(code, 0, "{out}{err}");
        out.lines().next().unwrap_or_default().to_string()
    };

    // Neither cap: the plain form the other three vary from.
    assert!(header(&["--depth", "3", "--rebuild"]).contains("(3 total)"));
    // `--limit` only — the pre-existing contract this change had to preserve.
    assert!(header(&["--depth", "3", "--limit", "1"])
        .contains("(3 total; showing 1 — raise --limit to see the rest)"));
    // `--depth` only.
    assert!(header(&["--depth", "1"]).contains("(2 total within --depth 1)"));
    // Both at once.
    assert!(header(&["--depth", "1", "--limit", "1"])
        .contains("(2 total within --depth 1; showing 1 — raise --limit to see the rest)"));
}
