//! End-to-end coverage for the Elixir adapter against
//! `tests/fixtures/elixir_adapter/`. Asserts:
//!   1. `defmodule` / `defprotocol` / `defimpl` — generic `call` nodes in
//!      tree-sitter-elixir, recognised by their target name — map to
//!      `Class` (`Struct` with `defstruct`/`defexception`), `Interface` and
//!      `Class`; a nested module qualifies under its parent.
//!   2. Every `def`-family macro surfaces, with `defp` / `defmacrop` /
//!      `defguardp` private, and the clauses of one name/arity fold into a
//!      single entry spanning all of them while other arities stay apart.
//!   3. `@doc` / `@moduledoc` become docs, `@doc false` marks `[hidden]`,
//!      and `@spec` / `@impl` / `@deprecated` attach to the next definition.
//!   4. `@behaviour` and `defimpl` feed `implements`.
//!   5. A `.exs` script with top-level code parses.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

const FIXTURE: &str = "tests/fixtures/elixir_adapter/sample.ex";
const SCRIPT: &str = "tests/fixtures/elixir_adapter/script.exs";
const DIR: &str = "tests/fixtures/elixir_adapter";

fn run(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(out.status.success(), "exit non-zero: {:?}", out);
    String::from_utf8(out.stdout).expect("utf8")
}

fn map_json(path: &str) -> serde_json::Value {
    let s = run(&["map", "--json", path]);
    let v: serde_json::Value = serde_json::from_str(&s).expect("json");
    v["files"][0].clone()
}

fn find<'a>(decls: &'a serde_json::Value, name: &str) -> Vec<&'a serde_json::Value> {
    decls
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["name"] == name)
        .collect()
}

fn module<'a>(file: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    find(&file["declarations"], name)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("module {name} missing: {file:#}"))
}

#[test]
fn fixtures_parse_without_errors() {
    for path in [FIXTURE, SCRIPT] {
        let f = map_json(path);
        assert_eq!(f["language"], "elixir");
        assert_eq!(f["error_count"], 0, "{path} has parse errors");
    }
}

#[test]
fn module_kinds_follow_the_defining_macro() {
    let f = map_json(FIXTURE);
    let account = module(&f, "Billing.Account");
    // `defstruct` in the body makes the module its struct type.
    assert_eq!(account["kind"], "struct");
    assert_eq!(account["native_kind"], "module");
    assert_eq!(module(&f, "Billing.Ledger")["kind"], "class");
    let proto = module(&f, "Billing.Printable");
    assert_eq!(proto["kind"], "interface");
    assert_eq!(proto["native_kind"], "protocol");
    let imp = module(&f, "Billing.Printable.Billing.Account");
    assert_eq!(imp["native_kind"], "impl");
    assert_eq!(
        imp["signature"],
        "defimpl Billing.Printable, for: Billing.Account"
    );
    // `defexception` is a struct too.
    assert_eq!(module(&f, "Billing.Error")["kind"], "struct");
}

#[test]
fn nested_module_qualifies_under_its_parent() {
    let s = run(&["digest", FIXTURE]);
    assert!(
        s.contains("module Billing.Account.Statement"),
        "nested module not qualified:\n{s}"
    );
    // `show` matches the dotted name segment by segment.
    let s = run(&["show", FIXTURE, "Billing.Account.Statement.render"]);
    assert!(
        s.contains("def render(acct)"),
        "nested function not found:\n{s}"
    );
    let s = run(&["show", FIXTURE, "Account.Statement"]);
    assert!(
        s.contains("defmodule Statement do"),
        "suffix match failed:\n{s}"
    );
}

#[test]
fn every_def_macro_surfaces_with_visibility() {
    let f = map_json(FIXTURE);
    let kids = &module(&f, "Billing.Account")["children"];
    let expect = [
        ("open", "def", ""),
        ("round_cents", "defp", "private"),
        ("audit", "defmacro", ""),
        ("trace", "defmacrop", "private"),
        ("is_positive", "defguard", ""),
        ("is_tiny", "defguardp", "private"),
        ("format", "defdelegate", ""),
    ];
    for (name, keyword, vis) in expect {
        let d = find(kids, name);
        assert_eq!(d.len(), 1, "{name}: {kids:#}");
        assert_eq!(d[0]["kind"], "function", "{name}");
        assert_eq!(d[0]["native_kind"], keyword, "{name}");
        assert_eq!(d[0]["visibility"], vis, "{name}");
    }
    let s = run(&["map", FIXTURE]);
    assert!(
        s.contains("defguard is_positive(x) when is_integer(x) and x > 0"),
        "{s}"
    );
    assert!(
        s.contains("defdelegate format(amount), to: Billing.Currency"),
        "{s}"
    );
    // The digest's public view drops the private ones.
    let d = run(&["digest", FIXTURE]);
    assert!(!d.contains("round_cents") && !d.contains("trace()"), "{d}");
    assert!(
        d.contains("[macro] audit()") && d.contains("[guard] is_positive()"),
        "{d}"
    );
}

#[test]
fn clauses_of_one_arity_fold_and_other_arities_stay_apart() {
    let f = map_json(FIXTURE);
    let kids = &module(&f, "Billing.Account")["children"];
    let charge = find(kids, "charge");
    assert_eq!(charge.len(), 2, "charge/2 and charge/1: {kids:#}");
    // charge/2: two clauses, L30-36 — plus the @doc/@spec above, L24.
    assert_eq!(charge[0]["start_line"], 24);
    assert_eq!(charge[0]["end_line"], 36);
    assert_eq!(
        charge[0]["signature"],
        "def charge(%__MODULE__{} = acct, amount) when amount > @default_limit"
    );
    assert_eq!(charge[1]["start_line"], 38);
    let rounds = find(kids, "round_cents");
    assert_eq!(rounds[0]["start_line"], 52);
    assert_eq!(rounds[0]["end_line"], 53);
    // `show` returns every clause in one body.
    let s = run(&["show", FIXTURE, "Account.round_cents"]);
    assert!(
        s.contains("when is_integer(x), do: x") && s.contains("do: round(x)"),
        "clauses missing:\n{s}"
    );
}

#[test]
fn docs_specs_and_annotations_attach_to_the_next_def() {
    let f = map_json(FIXTURE);
    let account = module(&f, "Billing.Account");
    assert_eq!(account["docs_inside"], true);
    assert!(account["docs"][1]
        .as_str()
        .unwrap()
        .contains("Accounts and their balances."));

    let kids = &account["children"];
    let open = find(kids, "open")[0];
    assert_eq!(open["docs"][0], "@doc \"Opens an account.\"");
    assert_eq!(open["attrs"][0], "@spec open(integer(), String.t()) :: t()");

    let charge = find(kids, "charge")[0];
    let docs: Vec<&str> = charge["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    // Heredoc lines are dedented to the attribute's column.
    assert_eq!(docs[0], "@doc \"\"\"");
    assert!(docs.contains(&"Multi-line docs survive."), "{docs:?}");

    let hidden = find(kids, "internal_hook")[0];
    assert!(hidden.get("docs").is_none());
    assert_eq!(hidden["modifiers"][0], "hidden");

    let init = find(kids, "init")[0];
    assert_eq!(init["attrs"][0], "@impl true");
    assert_eq!(init["modifiers"][0], "impl");

    let bill = find(kids, "bill")[0];
    assert_eq!(bill["deprecated"], true);

    // @doc false on a module is `@moduledoc false`.
    let statement = find(kids, "Statement")[0];
    assert_eq!(statement["modifiers"][0], "hidden");
}

#[test]
fn module_attributes_types_and_structs_are_fields() {
    let f = map_json(FIXTURE);
    let kids = &module(&f, "Billing.Account")["children"];
    let limit = find(kids, "default_limit")[0];
    assert_eq!(limit["kind"], "field");
    assert_eq!(limit["signature"], "@default_limit 1_000");
    let t = find(kids, "t")[0];
    assert_eq!(t["native_kind"], "type");
    assert_eq!(t["visibility"], "");
    assert_eq!(find(kids, "cents")[0]["visibility"], "private");
    let st = find(kids, "defstruct")[0];
    assert_eq!(st["signature"], "defstruct [:id, :owner, balance: 0]");
    // Directives are not declarations.
    for name in [
        "Invoice",
        "Cur",
        "Enum",
        "Logger",
        "GenServer",
        "Billing.Ledger",
    ] {
        assert!(
            find(kids, name).is_empty(),
            "{name} leaked as a declaration"
        );
    }
}

#[test]
fn behaviour_callbacks_surface() {
    let f = map_json(FIXTURE);
    let kids = &module(&f, "Billing.Ledger")["children"];
    let post = find(kids, "post")[0];
    assert_eq!(post["kind"], "function");
    assert_eq!(post["native_kind"], "callback");
    assert_eq!(post["docs"][0], "@doc \"Posts an entry.\"");
    let audit = find(kids, "audit")[0];
    assert_eq!(audit["native_kind"], "macrocallback");
}

#[test]
fn behaviour_and_defimpl_feed_implements() {
    let s = run(&["implements", "Billing.Ledger", DIR]);
    assert!(
        s.contains("Billing.Account"),
        "@behaviour not followed:\n{s}"
    );
    let s = run(&["implements", "Printable", DIR]);
    assert!(
        s.contains("Billing.Printable.Billing.Account"),
        "defimpl not followed:\n{s}"
    );
}

#[test]
fn exs_script_parses() {
    let s = run(&["map", SCRIPT]);
    assert!(s.contains("defmodule Script.Helper"), "{s}");
    assert!(s.contains("def greet(name)"), "{s}");
}
