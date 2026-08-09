//! End-to-end tests for `ast-bro impact`.
//!
//! Verifies that impact analysis correctly identifies dependents (callers,
//! implementors, file reverse-deps) and transitively affected symbols.

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

#[test]
fn impact_on_type_finds_implementors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub trait Animal { fn speak(&self); }

pub struct Dog;
impl Animal for Dog { fn speak(&self) { println!("woof"); } }

pub fn create_animal() -> Box<dyn Animal> {
    Box::new(Dog)
}
"#,
    );

    let (out, code) = run_in(root, &["impact", "Animal", "--rebuild"]);
    assert_eq!(code, 0, "impact exited non-zero: {}", out);

    assert!(out.contains("Dog"), "expected Dog (implementor of Animal), got:\n{}", out);
    assert!(out.contains("struct"), "expected Dog to be labeled as struct, got:\n{}", out);
}

#[test]
fn impact_on_struct_finds_construction_sites() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub struct Config { pub debug: bool }

pub fn load_config() -> Config {
    Config { debug: false }
}
"#,
    );

    let (out, code) = run_in(root, &["impact", "Config", "--rebuild"]);
    assert_eq!(code, 0, "impact exited non-zero: {}", out);
    assert!(
        out.contains("load_config"),
        "expected load_config (constructs Config via struct literal) in dependents, got:\n{}",
        out
    );
}

#[test]
fn impact_on_type_dependents_mode_not_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub trait MyTrait {}
pub struct MyImpl;
impl MyTrait for MyImpl {}
"#,
    );

    let (out, code) = run_in(root, &["impact", "MyTrait", "--mode", "dependents", "--rebuild"]);
    assert_eq!(code, 0);
    assert!(out.contains("MyImpl"), "dependents mode should show MyImpl for MyTrait");
}

#[test]
fn impact_transitive_for_type() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub trait Service { fn run(&self); }
pub struct MyService;
impl Service for MyService { fn run(&self) {} }

pub fn make_service() -> MyService {
    MyService {}
}

pub fn main() {
    make_service().run();
}

pub fn factory() {
    make_service();
}
"#,
    );

    let (out, code) = run_in(root, &["impact", "Service", "--depth", "2", "--rebuild"]);
    assert_eq!(code, 0);

    // Depth 1: MyService is an implementor of Service.
    assert!(out.contains("MyService"), "expected MyService (implementor), got:\n{}", out);

    // Depth 2: make_service constructs the implementor (`MyService {}`),
    // so it depends on the base type transitively.
    assert!(
        out.contains("make_service"),
        "expected make_service (constructs implementor MyService) at depth 2, got:\n{}",
        out
    );
    assert!(
        out.contains("transitively affected"),
        "expected a transitive section, got:\n{}",
        out
    );
}

#[test]
fn impact_transitive_for_callable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub fn target() {}
pub fn caller1() { target(); }
pub fn caller2() { caller1(); }
"#,
    );

    let (out, code) = run_in(root, &["impact", "target", "--depth", "2", "--rebuild"]);
    assert_eq!(code, 0);
    assert!(out.contains("caller1"), "expected caller1 (depth 1), got:\n{}", out);
    assert!(out.contains("caller2"), "expected caller2 (depth 2), got:\n{}", out);
    assert!(out.contains("1 symbols transitively affected"), "expected 1 transitive symbol (caller2), got:\n{}", out);
}

#[test]
fn impact_depth_cutoff_reports_frontier() {
    // A four-link caller chain, so `--depth 2` demonstrably stops with a
    // caller left to walk. Without the flag, `transitive_count` reads as the
    // whole blast radius when it is only the part inside `--depth` (#32).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub fn target() {}
pub fn c1() { target(); }
pub fn c2() { c1(); }
pub fn c3() { c2(); }
"#,
    );

    let (out, code) = run_in(
        root,
        &["impact", "target", "--depth", "2", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["impacts"][0]["transitive_count"], 1, "{out}");
    assert_eq!(v["impacts"][0]["frontier_truncated"], true, "c3 was never walked: {out}");

    // Depth 3 exhausts the chain — the flag must clear rather than stay on.
    let (out, _) = run_in(
        root,
        &["impact", "target", "--depth", "3", "--json", "--compact"],
    );
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["impacts"][0]["transitive_count"], 2, "{out}");
    assert_eq!(v["impacts"][0]["frontier_truncated"], false, "{out}");
}

#[test]
fn impact_frontier_note_lands_on_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn target() {}\npub fn c1() { target(); }\npub fn c2() { c1(); }\npub fn c3() { c2(); }\n",
    );

    let out = Command::new(bin())
        .args(["impact", "target", "--depth", "2", "--rebuild"])
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains("--depth 2") && stderr.contains("raise --depth"),
        "the note must name the cap and the flag that lifts it: {stderr}"
    );
    assert!(
        !stdout.contains("raise --depth"),
        "stdout carries results only (#36): {stdout}"
    );
}

#[test]
fn impact_on_a_type_reports_the_constructor_walk_frontier() {
    // Type targets take the multi-source construction-site walk rather than
    // the single-source caller BFS; its depth cap must report a frontier too.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub struct Widget { pub on: bool }
pub fn build() -> Widget { Widget { on: true } }
pub fn mid() { build(); }
pub fn top() { mid(); }
"#,
    );

    let (out, code) = run_in(
        root,
        &["impact", "Widget", "--depth", "2", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["impacts"][0]["frontier_truncated"], true, "top was never walked: {out}");

    let (out, _) = run_in(
        root,
        &["impact", "Widget", "--depth", "4", "--json", "--compact"],
    );
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["impacts"][0]["frontier_truncated"], false, "{out}");
}

#[test]
fn impact_mode_deps_does_not_claim_a_frontier_it_never_shows() {
    // `--mode deps` answers out of depth-1 sections only. The reverse walk
    // still runs internally, but flagging its cutoff would point the reader
    // at nothing in the output (issue #32).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn target() {}\npub fn c1() { target(); }\npub fn c2() { c1(); }\npub fn c3() { c2(); }\n",
    );

    for (mode, expected) in [("deps", false), ("dependents", false), ("all", true)] {
        let (out, code) = run_in(
            root,
            &["impact", "target", "--mode", mode, "--depth", "2", "--json", "--compact", "--rebuild"],
        );
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        assert_eq!(
            v["impacts"][0]["frontier_truncated"], expected,
            "--mode {mode}: {out}"
        );
    }
}

#[test]
fn impact_exclude_tests_empties_the_only_walk_derived_section() {
    // `--exclude-tests` hard-codes the affected-tests list to zero entries.
    // When that list was the report's only walk-derived output, a frontier
    // flag has nothing left to qualify — the same rule `--mode deps` follows.
    // `--mode all` is the control: its transitive section survives, so the
    // flag must too.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn target() {}\npub fn c1() { target(); }\npub fn c2() { c1(); }\npub fn c3() { c2(); }\n",
    );

    for (args, expected) in [
        (vec!["--mode", "tests", "--exclude-tests"], false),
        (vec!["--mode", "all", "--exclude-tests"], true),
        (vec!["--mode", "tests"], true),
    ] {
        let mut argv = vec!["impact", "target", "--depth", "2", "--json", "--compact", "--rebuild"];
        argv.extend(args.iter().copied());
        let (out, code) = run_in(root, &argv);
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        assert_eq!(
            v["impacts"][0]["frontier_truncated"], expected,
            "{args:?}: {out}"
        );
    }
}

#[test]
fn impact_all_mode_keeps_the_flag_on_a_zero_transitive_count() {
    // A zero produced by a depth cap is the confidently-wrong answer issue
    // #32 is about, so `--mode all` keeps the flag even when the transitive
    // section is empty. The BFS expands *through* predicate-rejected nodes,
    // so a qualifying caller behind an excluded one is genuinely reachable by
    // raising `--depth` — which is what makes the hint true rather than noise.
    //
    // Chain: target <- c1 (src) <- t2 (test file) <- r3 (src).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"smoke\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub mod probe_test;\npub mod deep;\npub fn target() {}\npub fn c1() { target(); }\n",
    );
    write(&root.join("src/probe_test.rs"), "use crate::c1;\npub fn t2() { c1(); }\n");
    write(&root.join("src/deep.rs"), "use crate::probe_test::t2;\npub fn r3() { t2(); }\n");

    let read = |depth: &str, rebuild: bool| {
        let mut argv = vec![
            "impact", "target", "--mode", "all", "--exclude-tests",
            "--depth", depth, "--json", "--compact",
        ];
        if rebuild {
            argv.push("--rebuild");
        }
        let (out, code) = run_in(root, &argv);
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        (
            v["impacts"][0]["transitive_count"].as_u64().unwrap(),
            v["impacts"][0]["frontier_truncated"].as_bool().unwrap(),
        )
    };

    // t2 is excluded, so depth 2 reports nothing — but r3 lies one hop past
    // the cap, which is exactly what the flag promises.
    assert_eq!(read("2", true), (0, true));
    // Raising the cap turns the zero into a one. Had the flag been cleared on
    // the empty section, that zero would have read as the whole answer.
    assert_eq!(read("3", false), (1, false));
}
