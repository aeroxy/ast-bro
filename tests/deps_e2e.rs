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
    code,
    stdout,
    stderr
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
  let (code, stdout, _stderr) = run(&["cycles", "tests/fixtures/deps/rust_cycle", "--rebuild"]);
  assert_eq!(code, 3, "cycles command should exit 3 when cycle present");
  assert!(stdout.contains("cycle"), "missing cycle word: {stdout}");
  assert!(stdout.contains("a.rs"), "missing a.rs: {stdout}");
  assert!(stdout.contains("b.rs"), "missing b.rs: {stdout}");
}

#[test]
fn rust_simple_no_cycle() {
  let (code, stdout, _) = run(&["cycles", "tests/fixtures/deps/rust_simple", "--rebuild"]);
  assert_eq!(code, 0, "expected exit 0 (no cycles): {stdout}");
  assert!(
    stdout.contains("no cycles"),
    "expected no cycles message: {stdout}"
  );
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
  assert!(
    s.contains("helpers.py"),
    "expected helpers.py in deps:\n{s}"
  );
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
  let s2 = run_ok(&["graph", "tests/fixtures/deps/rust_simple", "--json"]);
  // Strip `built_at` since it differs run-to-run.
  let extract = |s: &str| {
    let v: serde_json::Value = serde_json::from_str(s).unwrap();
    (
      v["file_count"].clone(),
      v["edge_count"].clone(),
      v["edges"].clone(),
    )
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
  assert!(
    s.contains("util.cpp"),
    "expected util.cpp as importer:\n{s}"
  );
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

// ---- Elixir ----

const SHOP: &str = "tests/fixtures/deps/elixir_modules";

#[test]
fn elixir_directives_and_references_resolve_by_module_name() {
  let s = run_ok(&[
    "deps",
    &format!("{SHOP}/lib/shop.ex"),
    "--depth",
    "1",
    "--rebuild",
  ]);
  for (file, kind) in [
    // `alias Shop.{Cart, Pricing}` is two edges.
    ("shop/cart.ex", "(alias)"),
    ("shop/pricing.ex", "(alias)"),
    ("shop/inventory.ex", "(alias) [as Stock]"),
    ("shop/format.ex", "(import)"),
    ("shop/macros.ex", "(use)"),
    // No directive: a remote call, a struct, an `unquote` in a macro.
    ("shop/payment.ex", "(ref)"),
    ("shop/receipt.ex", "(ref)"),
    ("shop/unquoted.ex", "(ref)"),
  ] {
    assert!(
      s.lines().any(|l| l.contains(file) && l.contains(kind)),
      "expected {file} {kind} in deps:\n{s}"
    );
  }
  // A typespec and the inside of a `quote` are not this file's deps.
  assert!(!s.contains("typed.ex"), "{s}");
  assert!(!s.contains("shop/quoted.ex"), "{s}");
  assert!(s.contains("[external] Logger"), "{s}");
}

#[test]
fn elixir_aliases_expand_and_nested_modules_resolve() {
  let s = run_ok(&[
    "deps",
    &format!("{SHOP}/lib/shop/pricing.ex"),
    "--depth",
    "1",
    "--rebuild",
  ]);
  // `alias Shop.Rules` then `Rules.Tax.rate/1` names `Shop.Rules.Tax`.
  assert!(s.contains("shop/rules/tax.ex"), "{s}");
  // `Shop.Cart.Line` is defined nested inside cart.ex.
  assert!(s.contains("shop/cart.ex"), "{s}");
  // An exact name only: `Shop.Rules` is defined nowhere, and must not
  // fall back to the file defining `Shop`.
  assert!(s.contains("[external] Shop.Rules"), "{s}");
  assert!(!s.contains("lib/shop.ex"), "{s}");
}

#[test]
fn elixir_reverse_deps() {
  let s = run_ok(&[
    "reverse-deps",
    &format!("{SHOP}/lib/shop/cart.ex"),
    "--depth",
    "1",
    "--rebuild",
  ]);
  assert!(s.contains("lib/shop.ex"), "{s}");
  assert!(s.contains("shop/pricing.ex"), "{s}");
}

#[test]
fn elixir_defimpl_depends_on_its_protocol_and_types() {
  let s = run_ok(&[
    "deps",
    &format!("{SHOP}/lib/shop/impls.ex"),
    "--depth",
    "1",
    "--rebuild",
  ]);
  // The protocol is an `impl` edge, so `cycles` counts it by default.
  for (file, kind) in [
    ("shop/proto.ex", "(impl)"),
    ("shop/payment.ex", "(ref)"),
    ("shop/receipt.ex", "(ref)"),
  ] {
    assert!(
      s.lines().any(|l| l.contains(file) && l.contains(kind)),
      "expected {file} {kind} in deps:\n{s}"
    );
  }
}

#[test]
fn elixir_cycles_leave_out_ref_edges_by_default() {
  let dir = "tests/fixtures/deps/elixir_cycles";
  // Only the `import` cycle counts; the note names the runtime one.
  let (code, stdout, stderr) = run(&["cycles", dir, "--rebuild"]);
  assert_eq!(code, 3, "stdout: {stdout}\nstderr: {stderr}");
  assert!(stdout.starts_with("1 cycle(s)"), "{stdout}");
  assert!(
    stdout.contains("c.ex") && !stdout.contains("a.ex"),
    "{stdout}"
  );
  assert!(stderr.contains("2 cycle(s), not 1"), "{stderr}");

  let (_, stdout, _) = run(&["cycles", dir, "--json", "--compact"]);
  assert!(stdout.contains("\"cycles_with_refs\":2"), "{stdout}");

  let (code, stdout, stderr) = run(&["cycles", dir, "--include-refs"]);
  assert_eq!(code, 3, "stdout: {stdout}\nstderr: {stderr}");
  assert!(stdout.starts_with("2 cycle(s)"), "{stdout}");
  assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn other_languages_do_not_import_elixir_files_by_path() {
  let s = run_ok(&["graph", SHOP, "--rebuild"]);
  // `import shop` / `import config` / `from "cart"` name no Elixir file.
  for from in ["scripts/gen.py", "assets/app.js"] {
    let block: String = s
      .split("\n\n")
      .filter(|b| b.lines().next().is_some_and(|l| l.ends_with(from)))
      .collect();
    assert!(
      !block.contains(".ex"),
      "{from} reached an Elixir file:\n{s}"
    );
  }
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
  write(
    "Cargo.toml",
    "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
  );
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
  v["frontier_truncated"]
    .as_bool()
    .expect("frontier_truncated present")
}

#[test]
fn deps_depth_cutoff_reports_frontier() {
  let tmp = frontier_fixture();
  let root = tmp.path();

  // Depth 1 reaches b.rs and stops; b.rs still imports c.rs.
  let (code, out, err) = run_in(
    root,
    &[
      "deps",
      "src/a.rs",
      "--depth",
      "1",
      "--json",
      "--compact",
      "--rebuild",
    ],
  );
  assert_eq!(code, 0, "{out}{err}");
  assert!(frontier_flag(&out), "depth 1 left c.rs unwalked: {out}");

  // Depth 3 exhausts the chain — the flag must clear rather than stay on.
  let (_, out, _) = run_in(
    root,
    &["deps", "src/a.rs", "--depth", "3", "--json", "--compact"],
  );
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
    &[
      "reverse-deps",
      "src/c.rs",
      "--depth",
      "1",
      "--json",
      "--compact",
      "--rebuild",
    ],
  );
  assert_eq!(code, 0, "{out}{err}");
  let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
  assert_eq!(v["total"], 2, "{out}");
  assert_eq!(v["truncated"], false, "--limit did not cut anything: {out}");
  assert_eq!(v["frontier_truncated"], true, "--depth did: {out}");

  let (_, out, _) = run_in(
    root,
    &[
      "reverse-deps",
      "src/c.rs",
      "--depth",
      "3",
      "--json",
      "--compact",
    ],
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
    &[
      "reverse-deps",
      "src/c.rs",
      "--depth",
      "3",
      "--limit",
      "1",
      "--json",
      "--compact",
      "--rebuild",
    ],
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

  let (code, out, err) = run_in(
    root,
    &["reverse-deps", "src/c.rs", "--depth", "1", "--rebuild"],
  );
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

// ---- Incremental update parity (issue #93) ----
//
// An incremental update must give the answer a cold build gives. Each test
// builds the cache, changes the tree, reads the graph through the update,
// and compares it with `--rebuild`. The change never touches the importer,
// so only resolving it again can move its edge.

fn write_in(root: &std::path::Path, rel: &str, body: &str) {
  let p = root.join(rel);
  std::fs::create_dir_all(p.parent().unwrap()).unwrap();
  std::fs::write(p, body).unwrap();
}

/// File count, edges, and external imports of `graph . --json`, the part
/// of the answer an incremental update can get wrong.
fn graph_answer(
  root: &std::path::Path,
  rebuild: bool,
) -> (serde_json::Value, serde_json::Value, serde_json::Value) {
  let mut args = vec!["graph", ".", "--json"];
  if rebuild {
    args.push("--rebuild");
  }
  let (code, out, err) = run_in(root, &args);
  assert_eq!(code, 0, "stdout: {}\nstderr: {}", out, err);
  let v: serde_json::Value = serde_json::from_str(&out).unwrap();
  (
    v["file_count"].clone(),
    v["edges"].clone(),
    v["external"].clone(),
  )
}

fn has_edge(edges: &serde_json::Value, from: &str, to: &str) -> bool {
  edges
    .as_array()
    .unwrap()
    .iter()
    .any(|e| e["from"] == from && e["to"] == to)
}

/// Build the cache, apply `change`, then check the incremental answer
/// against a cold build. Returns the incremental edges.
fn incremental_matches_cold(
  root: &std::path::Path,
  change: impl FnOnce(&std::path::Path),
) -> serde_json::Value {
  std::fs::create_dir_all(root.join(".git")).unwrap();
  graph_answer(root, true);
  change(root);
  let incremental = graph_answer(root, false);
  let cold = graph_answer(root, true);
  assert_eq!(
    incremental, cold,
    "incremental update differs from a cold build"
  );
  incremental.1
}

#[test]
fn incremental_update_resolves_unchanged_files_after_an_add_and_a_delete() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "a.py", "import b\nimport c\nimport d\n");
  write_in(root, "b.py", "X = 1\n");
  write_in(root, "c.py", "Y = 2\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, "d.py", "Z = 3\n");
    std::fs::remove_file(root.join("c.py")).unwrap();
  });
  assert!(has_edge(&edges, "a.py", "d.py"), "{}", edges);
  assert!(!has_edge(&edges, "a.py", "c.py"), "{}", edges);
}

#[test]
fn incremental_update_follows_an_elixir_module_to_its_new_file() {
  // No file is added or deleted: the module moves between two files, and
  // only their contents say where it is.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "mix.exs", "defmodule App.MixProject do\nend\n");
  write_in(root, "lib/a.ex", "defmodule A do\n  alias Foo.Bar\nend\n");
  write_in(root, "lib/x.ex", "defmodule Foo.Bar do\nend\n");
  write_in(root, "lib/y.ex", "defmodule Other do\nend\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, "lib/x.ex", "defmodule Other.Moved do\nend\n");
    write_in(root, "lib/y.ex", "defmodule Foo.Bar do\n  # moved\nend\n");
  });
  assert!(has_edge(&edges, "lib/a.ex", "lib/y.ex"), "{}", edges);
  assert!(!has_edge(&edges, "lib/a.ex", "lib/x.ex"), "{}", edges);
}

#[test]
fn incremental_update_moves_an_edge_to_a_closer_new_file() {
  // The importer's edge already resolves; a file added nearer to it wins
  // the same suffix, as it does in a cold build.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "x/src/util.rs", "pub fn f() {}\n");
  write_in(
    root,
    "y/src/main.rs",
    "use crate::util::f;\nfn main() { f(); }\n",
  );
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, "y/src/util.rs", "pub fn f() {}\n");
  });
  assert!(
    has_edge(&edges, "y/src/main.rs", "y/src/util.rs"),
    "{}",
    edges
  );
  assert!(
    !has_edge(&edges, "y/src/main.rs", "x/src/util.rs"),
    "{}",
    edges
  );
}

#[test]
fn incremental_update_parses_an_edited_file_again() {
  // The edited file's own imports change, so its cached imports must not
  // be kept: one import line replaced, one added.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "a.py", "import b\n");
  write_in(root, "b.py", "X = 1\n");
  write_in(root, "c.py", "Y = 2\n");
  write_in(root, "d.py", "Z = 3\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, "a.py", "import c\nimport d\n");
  });
  assert!(has_edge(&edges, "a.py", "c.py"), "{}", edges);
  assert!(has_edge(&edges, "a.py", "d.py"), "{}", edges);
  assert!(!has_edge(&edges, "a.py", "b.py"), "{}", edges);
}

#[test]
fn incremental_update_drops_a_deleted_importer() {
  // The deleted file imports something, so a graph that kept its entry or
  // its cached imports would still show its edge and count it.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "a.py", "import b\n");
  write_in(root, "b.py", "X = 1\n");
  write_in(root, "gone.py", "import b\nimport missing\n");
  let edges = incremental_matches_cold(root, |root| {
    std::fs::remove_file(root.join("gone.py")).unwrap();
  });
  assert!(has_edge(&edges, "a.py", "b.py"), "{}", edges);
  assert!(!has_edge(&edges, "gone.py", "b.py"), "{}", edges);
}

// ---- Change detection covers every deps input (issue #123) ----
//
// Each change below is the only change in the tree. The cache must notice
// it on its own: a file the deps graph reads but the freshness check does
// not track keeps its old answer until `--rebuild`.

#[test]
fn incremental_update_sees_an_edit_to_a_hidden_file() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, ".cfg/h.py", "import x\n");
  write_in(root, "x.py", "X = 1\n");
  write_in(root, "y.py", "Y = 1\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, ".cfg/h.py", "import y\n");
  });
  assert!(has_edge(&edges, ".cfg/h.py", "y.py"), "{}", edges);
}

#[test]
fn incremental_update_sees_an_edit_to_a_file_over_the_search_size_cap() {
  // The search index skips files over 2 MB; the deps graph reads them.
  let pad = "#".repeat(2_100_000);
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "big.py", &format!("import x\n{}\n", pad));
  write_in(root, "x.py", "X = 1\n");
  write_in(root, "y.py", "Y = 1\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, "big.py", &format!("import y\n{}\n", pad));
  });
  assert!(has_edge(&edges, "big.py", "y.py"), "{}", edges);
}

#[test]
fn incremental_update_sees_an_edit_to_go_mod() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "go.mod", "module example.com/old\n");
  write_in(
    root,
    "main.go",
    "package main\n\nimport \"example.com/new/pkg\"\n\nfunc main() { pkg.P() }\n",
  );
  write_in(root, "pkg/p.go", "package pkg\n\nfunc P() {}\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(root, "go.mod", "module example.com/new\n");
  });
  assert!(has_edge(&edges, "main.go", "pkg/p.go"), "{}", edges);
}

#[test]
fn incremental_update_sees_an_edit_to_tsconfig_json() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "paths": { "@old/*": ["lib/*"] } } }"#,
  );
  write_in(root, "a.ts", "import { u } from \"@lib/u\";\n");
  write_in(root, "lib/u.ts", "export const u = 1;\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(
      root,
      "tsconfig.json",
      r#"{ "compilerOptions": { "paths": { "@lib/*": ["lib/*"] } } }"#,
    );
  });
  assert!(has_edge(&edges, "a.ts", "lib/u.ts"), "{}", edges);
}

#[test]
fn incremental_update_sees_an_edit_to_a_gitignored_tsconfig_json() {
  // The walk skips an ignored file, but the alias reader opens the root
  // manifests by name, so the freshness check adds them by name too.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  std::fs::create_dir_all(root.join(".git")).unwrap();
  write_in(root, ".gitignore", "tsconfig.json\n");
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "paths": { "@old/*": ["lib/*"] } } }"#,
  );
  write_in(root, "a.ts", "import { u } from \"@lib/u\";\n");
  write_in(root, "lib/u.ts", "export const u = 1;\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(
      root,
      "tsconfig.json",
      r#"{ "compilerOptions": { "paths": { "@lib/*": ["lib/*"] } } }"#,
    );
  });
  assert!(has_edge(&edges, "a.ts", "lib/u.ts"), "{}", edges);
}

#[test]
fn incremental_update_sees_an_edit_to_composer_json() {
  // Two classes share a name, so only the PSR-4 mapping decides which one
  // `use Vendor\Account;` reaches.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(
    root,
    "composer.json",
    r#"{ "autoload": { "psr-4": { "Vendor\\": "a/" } } }"#,
  );
  write_in(root, "User.php", "<?php\nuse Vendor\\Account;\n");
  write_in(root, "a/Account.php", "<?php\nclass Account {}\n");
  write_in(root, "b/Account.php", "<?php\nclass Account {}\n");
  let edges = incremental_matches_cold(root, |root| {
    write_in(
      root,
      "composer.json",
      r#"{ "autoload": { "psr-4": { "Vendor\\": "b/" } } }"#,
    );
  });
  assert!(has_edge(&edges, "User.php", "b/Account.php"), "{}", edges);
}

#[test]
fn incremental_update_sees_a_non_source_import_target_come_and_go() {
  // A relative import may name any file, and the resolver checks that it
  // exists. The graph depends on those files existing, whatever they hold.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(
    root,
    "a.ts",
    "import d from \"./data.json\";\nimport l from \"./later.json\";\n",
  );
  write_in(root, "data.json", "{}\n");
  write_in(root, "b.ts", "import s from \"./logo.svg\";\n");
  write_in(root, "logo.svg", "<svg/>\n");
  let edges = incremental_matches_cold(root, |root| {
    std::fs::remove_file(root.join("data.json")).unwrap();
    std::fs::remove_file(root.join("logo.svg")).unwrap();
    write_in(root, "later.json", "{}\n");
  });
  assert!(has_edge(&edges, "a.ts", "later.json"), "{}", edges);
  assert!(!has_edge(&edges, "a.ts", "data.json"), "{}", edges);
  assert!(!has_edge(&edges, "b.ts", "logo.svg"), "{}", edges);
}

#[test]
fn a_bare_typescript_import_is_external_without_a_base_url() {
  // A bare specifier names a package unless `tsconfig.json` maps it: it
  // is not the project file whose path happens to end in it (#127).
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "scripts/react.py", "def useState():\n    pass\n");
  write_in(root, "source/types/ky.ts", "export class NetworkError {}\n");
  write_in(root, "src/util.ts", "export const u = 1;\n");
  write_in(
    root,
    "src/c.ts",
    "import { useState } from \"react\";\nimport { NetworkError } from \"ky\";\nimport { u } from \"./util\";\n",
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(has_edge(&edges, "src/c.ts", "src/util.ts"), "{}", edges);
  assert!(
    !has_edge(&edges, "src/c.ts", "scripts/react.py"),
    "{}",
    edges
  );
  assert!(
    !has_edge(&edges, "src/c.ts", "source/types/ky.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_bare_typescript_import_resolves_under_the_base_url() {
  // `baseUrl` alone, without `paths`, makes `components/Button` a project
  // path, anchored at that directory.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
  );
  write_in(root, "src/components/Button.ts", "export const b = 1;\n");
  write_in(root, "other/components/Button.ts", "export const b = 2;\n");
  write_in(root, "src/components/index.ts", "export const i = 1;\n");
  write_in(
    root,
    "src/app.ts",
    "import { b } from \"components/Button\";\nimport { i } from \"components\";\n",
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/components/Button.ts"),
    "{}",
    edges
  );
  assert!(
    has_edge(&edges, "src/app.ts", "src/components/index.ts"),
    "{}",
    edges
  );
  assert!(
    !has_edge(&edges, "src/app.ts", "other/components/Button.ts"),
    "{}",
    edges
  );
}

#[test]
fn python_an_absolute_from_import_resolves_to_the_module_file() {
  // `from a import real` names `real` in `a.py`: the name is not a file.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "a.py", "def real():\n    pass\n");
  write_in(root, "pkg/__init__.py", "");
  write_in(root, "pkg/mod.py", "def f():\n    pass\n");
  write_in(
    root,
    "c.py",
    "from a import real as alias\nfrom pkg.mod import f\nfrom pkg import mod\n",
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "d.py", "\n"));
  assert!(has_edge(&edges, "c.py", "a.py"), "{}", edges);
  assert!(has_edge(&edges, "c.py", "pkg/mod.py"), "{}", edges);
}

#[test]
fn a_bare_typescript_import_under_a_root_base_url_is_still_a_package() {
  // `"baseUrl": "."` makes `react` the root's `react.ts` if there is one,
  // not any file whose path ends in `react`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "baseUrl": "." } }"#,
  );
  write_in(root, "scripts/react.py", "def useState():\n    pass\n");
  write_in(root, "src/util.ts", "export const u = 1;\n");
  write_in(
    root,
    "src/c.ts",
    "import { useState } from \"react\";\nimport { u } from \"src/util\";\n",
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    !has_edge(&edges, "src/c.ts", "scripts/react.py"),
    "{}",
    edges
  );
  assert!(has_edge(&edges, "src/c.ts", "src/util.ts"), "{}", edges);
}

#[test]
fn a_python_import_resolves_only_to_a_python_file() {
  // The suffix index holds every language's files; `requests` and `os`
  // here are TypeScript, so the imports stay external (#127).
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "web/requests.ts", "export function get() {}\n");
  write_in(root, "web/os.ts", "export const sep = 1;\n");
  write_in(root, "util.py", "def f():\n    pass\n");
  write_in(root, "tools/xml.py", "def f():\n    pass\n");
  write_in(
    root,
    "c.py",
    "import requests\nimport os.path\nfrom requests import get\nfrom util import f\nimport xml.etree\n",
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "d.py", "\n"));
  assert!(!has_edge(&edges, "c.py", "web/requests.ts"), "{}", edges);
  assert!(!has_edge(&edges, "c.py", "web/os.ts"), "{}", edges);
  assert!(has_edge(&edges, "c.py", "util.py"), "{}", edges);
  // `import xml.etree` names the module `xml.etree`, so only a
  // `from … import` drops the last segment: `tools/xml.py` is not it.
  assert!(!has_edge(&edges, "c.py", "tools/xml.py"), "{}", edges);
}

/// `src/app.ts` imports `lib/helpers` and `@app/util`; under `"baseUrl":
/// "src"` they are `src/lib/helpers.ts` and `src/app/util.ts`.
fn base_url_fixture(root: &std::path::Path) {
  write_in(root, "src/lib/helpers.ts", "export const h = 1;\n");
  write_in(root, "src/app/util.ts", "export const u = 1;\n");
  write_in(
    root,
    "src/app.ts",
    "import { h } from \"lib/helpers\";\nimport { u } from \"@app/util\";\n",
  );
}

#[test]
fn a_base_url_is_read_from_a_tsconfig_with_block_comments() {
  // What `tsc --init` writes: block comments, some after a value and some
  // spanning lines, beside strings that hold `/*` and `*/` themselves.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "tsconfig.json",
    r#"{
  /* Visit https://aka.ms/tsconfig to read more about this file */
  "compilerOptions": {
    "target": "es2016",                                  /* Set the JavaScript language version. */
    /* "rootDir": "./",
       spans lines */
    "baseUrl": "./src",                                  /* Specify the base directory. */
    "paths": { "@app/*": ["app/*"] },                    // `paths` too
  },
  "include": ["src/**/*", "types/*.d.ts"],
}
"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/lib/helpers.ts"),
    "{}",
    edges
  );
  assert!(
    has_edge(&edges, "src/app.ts", "src/app/util.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_base_url_is_read_from_a_tsconfig_saved_with_a_byte_order_mark() {
  // Visual Studio saves `tsconfig.json` with a UTF-8 BOM, and `tsc`
  // reads it as if the BOM were not there.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "tsconfig.json",
    "\u{feff}{ \"compilerOptions\": { \"baseUrl\": \"src\", \"paths\": { \"@app/*\": [\"app/*\"] } } }",
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/lib/helpers.ts"),
    "{}",
    edges
  );
  assert!(
    has_edge(&edges, "src/app.ts", "src/app/util.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_null_base_url_or_paths_clears_the_inherited_setting() {
  // `tsc` clears an inherited `baseUrl` or `paths` on an explicit
  // `null`. A root `lib/helpers.ts` and `app/util.ts` would otherwise
  // be the targets under the cleared `"baseUrl": "."`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(root, "lib/helpers.ts", "export const h = 2;\n");
  write_in(root, "app/util.ts", "export const u = 2;\n");
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "..", "paths": { "@app/*": ["app/*"] } } }"#,
  );
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base.json", "compilerOptions": { "baseUrl": null, "paths": null } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  for target in [
    "lib/helpers.ts",
    "app/util.ts",
    "src/lib/helpers.ts",
    "src/app/util.ts",
  ] {
    assert!(!has_edge(&edges, "src/app.ts", target), "{target}: {edges}");
  }
}

#[test]
fn a_missing_base_url_keeps_the_inherited_setting() {
  // An absent property is not `null`: the child inherits.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "../src", "paths": { "@app/*": ["app/*"] } } }"#,
  );
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base.json", "compilerOptions": { "strict": true } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  for target in ["src/lib/helpers.ts", "src/app/util.ts"] {
    assert!(has_edge(&edges, "src/app.ts", target), "{target}: {edges}");
  }
}

#[test]
fn a_null_paths_clears_only_the_inherited_aliases() {
  // `"paths": null` on its own: `@app/util` has no alias left, while the
  // inherited `baseUrl` still makes `lib/helpers` `src/lib/helpers.ts`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "../src", "paths": { "@app/*": ["app/*"] } } }"#,
  );
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base.json", "compilerOptions": { "paths": null } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/lib/helpers.ts"),
    "{edges}"
  );
  assert!(
    !has_edge(&edges, "src/app.ts", "src/app/util.ts"),
    "{edges}"
  );
}

#[test]
fn a_null_base_url_leaves_the_inherited_paths_under_their_own_config() {
  // `"baseUrl": null` on its own: `lib/helpers` is no longer under `src/`,
  // and with no `baseUrl` the inherited `paths` target resolves against the
  // directory of the config that sets it, so `@app/util` is
  // `configs/app/util.ts`, not `src/app/util.ts`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(root, "configs/app/util.ts", "export const u = 3;\n");
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "../src", "paths": { "@app/*": ["./app/*"] } } }"#,
  );
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base.json", "compilerOptions": { "baseUrl": null } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "configs/app/util.ts"),
    "{edges}"
  );
  for target in ["src/lib/helpers.ts", "src/app/util.ts"] {
    assert!(!has_edge(&edges, "src/app.ts", target), "{target}: {edges}");
  }
}

#[test]
fn a_non_ascii_base_url_names_its_directory() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "源码/helpers.ts", "export const h = 1;\n");
  write_in(root, "源码/app.ts", "import { h } from \"helpers\";\n");
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "baseUrl": "源码", }, }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "d.ts", "\n"));
  assert!(
    has_edge(&edges, "源码/app.ts", "源码/helpers.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_base_url_is_inherited_through_a_relative_extends() {
  // `baseUrl` and `paths` resolve against the file that sets them, and
  // `extends` may leave out `.json`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base", "compilerOptions": { "strict": true } }"#,
  );
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "../src", "paths": { "@app/*": ["app/*"] } } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/lib/helpers.ts"),
    "{}",
    edges
  );
  assert!(
    has_edge(&edges, "src/app.ts", "src/app/util.ts"),
    "{}",
    edges
  );
}

#[test]
fn an_edit_to_an_extended_tsconfig_reaches_the_cached_graph() {
  // Only `configs/base.json` changes; the cache must still see it.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(root, "lib/lib/helpers.ts", "export const h = 2;\n");
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": ["./configs/base.json"] }"#,
  );
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "../src" } }"#,
  );
  let edges = incremental_matches_cold(root, |root| {
    write_in(
      root,
      "configs/base.json",
      r#"{ "compilerOptions": { "baseUrl": "../lib" } }"#,
    )
  });
  assert!(
    has_edge(&edges, "src/app.ts", "lib/lib/helpers.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_javascript_project_reads_its_base_url_from_jsconfig() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "src/lib/helpers.js", "export const h = 1;\n");
  write_in(root, "src/app.js", "import { h } from \"lib/helpers\";\n");
  write_in(
    root,
    "jsconfig.json",
    r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.js", "\n"));
  assert!(
    has_edge(&edges, "src/app.js", "src/lib/helpers.js"),
    "{}",
    edges
  );
}

#[test]
fn a_shared_base_config_keeps_its_settings_under_a_later_parent() {
  // The root extends `[a, b]`, and both extend `shared`. `a` overrides
  // the `baseUrl` that `shared` sets; `b` inherits it, and the later
  // entry wins, as in `tsc`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(root, "lib/lib/helpers.ts", "export const h = 2;\n");
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": ["./configs/a.json", "./configs/b.json"] }"#,
  );
  write_in(
    root,
    "configs/shared.json",
    r#"{ "compilerOptions": { "baseUrl": "../src" } }"#,
  );
  write_in(
    root,
    "configs/a.json",
    r#"{ "extends": "./shared.json", "compilerOptions": { "baseUrl": "../lib" } }"#,
  );
  write_in(root, "configs/b.json", r#"{ "extends": "./shared.json" }"#);
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/lib/helpers.ts"),
    "{}",
    edges
  );
  assert!(
    !has_edge(&edges, "src/app.ts", "lib/lib/helpers.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_paths_target_that_leaves_its_config_directory_names_the_file() {
  // No `baseUrl`: the `paths` targets resolve against the config that
  // sets them, and `../src/app/*` from `configs/` is `src/app/*`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base.json" }"#,
  );
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "paths": { "@app/*": ["../src/app/*"] } } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/app.ts", "src/app/util.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_base_config_that_stops_parsing_reaches_the_cached_graph() {
  // `configs/base.json` is valid, then is not: a cold build then reads
  // no `baseUrl`, and the update must see the edit.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "tsconfig.json",
    r#"{ "extends": "./configs/base.json" }"#,
  );
  write_in(
    root,
    "configs/base.json",
    r#"{ "compilerOptions": { "baseUrl": "../src" } }"#,
  );
  let edges = incremental_matches_cold(root, |root| {
    write_in(
      root,
      "configs/base.json",
      r#"{ "compilerOptions": { "baseUrl": "#,
    )
  });
  assert!(
    !has_edge(&edges, "src/app.ts", "src/lib/helpers.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_base_url_import_with_a_parent_segment_names_the_file() {
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  base_url_fixture(root);
  write_in(
    root,
    "src/other.ts",
    "import { u } from \"lib/../app/util\";\n",
  );
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  assert!(
    has_edge(&edges, "src/other.ts", "src/app/util.ts"),
    "{}",
    edges
  );
}

#[test]
fn a_base_url_import_finds_each_file_form_the_probe_tries() {
  // A `.d.ts` file, a spec with its own extension, and a directory's
  // `index.ts` or `index.d.ts`, each under `baseUrl`.
  let tmp = tempfile::tempdir().unwrap();
  let root = tmp.path();
  write_in(root, "src/types/env.d.ts", "export type E = 1;\n");
  write_in(root, "src/lib/x.js", "export const x = 1;\n");
  write_in(root, "src/ui/index.ts", "export const u = 1;\n");
  write_in(root, "src/decl/index.d.ts", "export type D = 1;\n");
  write_in(
    root,
    "src/app/main.ts",
    "import { E } from \"types/env\";\nimport { x } from \"lib/x.js\";\nimport { u } from \"ui\";\nimport { D } from \"decl\";\n",
  );
  write_in(
    root,
    "tsconfig.json",
    r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
  );
  let edges = incremental_matches_cold(root, |root| write_in(root, "src/d.ts", "\n"));
  for target in [
    "src/types/env.d.ts",
    "src/lib/x.js",
    "src/ui/index.ts",
    "src/decl/index.d.ts",
  ] {
    assert!(
      has_edge(&edges, "src/app/main.ts", target),
      "{target}: {edges}"
    );
  }
}
