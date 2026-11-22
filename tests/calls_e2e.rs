//! End-to-end tests for `callers` / `callees` against a small fixture
//! repo containing inter-file calls in Rust, Python, and TypeScript.
//!
//! These don't try to assert the full graph — just that the resolver
//! finds the *right* callers/callees and doesn't include obvious noise.

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
fn rust_callers_finds_cross_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Minimal Cargo project so the dep resolver detects this as a project root.
    write(&root.join("Cargo.toml"), "[package]\nname = \"smoke\"\nversion = \"0.0.0\"\nedition = \"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub mod helper;
pub fn run() {
    helper::greet();
}
"#,
    );
    write(
        &root.join("src/helper.rs"),
        r#"
pub fn greet() {
    println!("hi");
}
"#,
    );

    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("src/lib.rs") && out.contains("run"),
        "expected lib.rs::run in callers output, got:\n{}",
        out
    );
}

#[test]
fn rust_callees_lists_local_call() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname = \"smoke\"\nversion = \"0.0.0\"\nedition = \"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub fn helper() {}
pub fn run() {
    helper();
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "run", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(
        out.contains("helper"),
        "expected `helper` in callees output, got:\n{}",
        out
    );
}

#[test]
fn python_callers_finds_cross_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // pyproject.toml so the dep resolver picks this dir as the root.
    write(
        &root.join("pyproject.toml"),
        "[project]\nname = \"smoke\"\nversion = \"0.0.0\"\n",
    );
    write(
        &root.join("smoke/__init__.py"),
        "",
    );
    write(
        &root.join("smoke/helper.py"),
        "def greet():\n    print('hi')\n",
    );
    write(
        &root.join("smoke/main.py"),
        "from smoke.helper import greet\n\ndef run():\n    greet()\n",
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("smoke/main.py") && out.contains("run"),
        "expected smoke/main.py::run in callers, got:\n{}",
        out
    );
}

/// A class whose getter calls `load` and whose setter calls `validate`, beside
/// a plain method that calls `validate` too.
fn python_property_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pyproject.toml"), "[project]\nname=\"x\"\nversion=\"0\"\n");
    write(
        &root.join("m.py"),
        r#"def load():
    pass

def validate(v):
    pass

class C:
    @property
    def size(self):
        return load()

    @size.setter
    def size(self, v):
        validate(v)

    def run(self):
        validate(1)
"#,
    );
    tmp
}

/// The `(source, line)` pairs `callers <target>` reports, sorted.
fn caller_sites(root: &std::path::Path, target: &str) -> Vec<(String, u64)> {
    let (out, code) = run_in(root, &["callers", target, ".", "--rebuild", "--json", "--compact"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let mut sites: Vec<(String, u64)> = doc["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .map(|m| (m["source"].as_str().unwrap().to_string(), m["line"].as_u64().unwrap()))
        .collect();
    sites.sort();
    sites
}

#[test]
fn python_property_setter_is_reported_as_a_caller() {
    let tmp = python_property_project();
    assert_eq!(
        caller_sites(tmp.path(), "validate"),
        vec![("m.py::C::run".to_string(), 17), ("m.py::C::size".to_string(), 14)],
        "callers of validate"
    );
}

#[test]
fn python_property_getter_is_reported_as_a_caller() {
    let tmp = python_property_project();
    assert_eq!(
        caller_sites(tmp.path(), "load"),
        vec![("m.py::C::size".to_string(), 10)],
        "callers of load"
    );
}

#[test]
fn python_property_that_calls_nothing_resolves_as_a_callee_target() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pyproject.toml"), "[project]\nname=\"x\"\nversion=\"0\"\n");
    write(
        &root.join("m.py"),
        r#"class C:
    @property
    def size(self):
        return self._s

    @size.setter
    def size(self, v):
        self._s = v
"#,
    );
    let (out, code) = run_in(root, &["callees", "C.size", ".", "--rebuild", "--json", "--compact"]);
    assert_eq!(code, 0, "callees C.size must find the property: {out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["total"], 0, "{out}");
}

#[test]
fn typescript_callers_finds_cross_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("package.json"),
        r#"{"name":"smoke","version":"0.0.0"}"#,
    );
    write(
        &root.join("src/helper.ts"),
        "export function greet(): void { console.log('hi'); }\n",
    );
    write(
        &root.join("src/main.ts"),
        "import { greet } from './helper';\n\nexport function run(): void {\n  greet();\n}\n",
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("src/main.ts") && out.contains("run"),
        "expected src/main.ts::run in callers, got:\n{}",
        out
    );
}

#[test]
fn typescript_callees_from_arrow_const() {
    // Regression: arrow functions / function expressions bound to a const
    // had their bodies skipped, so the call graph saw zero calls from them.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("package.json"),
        r#"{"name":"smoke","version":"0.0.0"}"#,
    );
    write(
        &root.join("src/main.ts"),
        "function beta(): void {}\n\
         // block-body arrow\n\
         const gamma = (): void => { beta(); };\n\
         // async block-body arrow\n\
         const delta = async (): Promise<void> => { beta(); };\n\
         // concise expression-body arrow\n\
         const epsilon = (): void => beta();\n\
         // function expression\n\
         const zeta = function (): void { beta(); };\n",
    );

    for sym in ["gamma", "delta", "epsilon", "zeta"] {
        let (out, code) = run_in(root, &["callees", sym, ".", "--rebuild"]);
        assert_eq!(code, 0, "callees {} exited non-zero: {}", sym, out);
        assert!(
            out.contains("beta"),
            "expected `beta` in callees of {}, got:\n{}",
            sym,
            out
        );
    }
}

#[test]
fn typescript_callees_descends_into_anonymous_closures() {
    // Calls inside anonymous closures — curried arrows, returned closures,
    // and inline callbacks — are attributed to the enclosing declaration.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("package.json"),
        r#"{"name":"smoke","version":"0.0.0"}"#,
    );
    write(
        &root.join("src/main.ts"),
        "function beta(): void {}\n\
         // curried arrow: outer body IS the inner arrow\n\
         const curried = (x: number) => (y: number) => beta();\n\
         // block body returning an anonymous closure\n\
         const returns = () => { return () => beta(); };\n\
         // inline callback inside a plain function declaration\n\
         function withCallback(): void { [1].forEach(() => beta()); }\n\
         // inline `function` expression callback (legacy `function` node kind)\n\
         function withFnExpr(): void { [1].forEach(function () { beta(); }); }\n",
    );

    for sym in ["curried", "returns", "withCallback", "withFnExpr"] {
        let (out, code) = run_in(root, &["callees", sym, ".", "--rebuild"]);
        assert_eq!(code, 0, "callees {} exited non-zero: {}", sym, out);
        assert!(
            out.contains("beta"),
            "expected `beta` in callees of {}, got:\n{}",
            sym,
            out
        );
    }
}

#[test]
fn typescript_callees_bubbles_block_local_const_closure() {
    // A const-bound closure declared *inside* a function body is not extracted
    // as its own declaration, so — like an inline callback — its calls are
    // attributed to the enclosing function. (Intentionally the opposite of the
    // named-nested-function case below.)
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("package.json"),
        r#"{"name":"smoke","version":"0.0.0"}"#,
    );
    write(
        &root.join("src/main.ts"),
        "function beta(): void {}\n\
         function outer(): void { const inner = () => beta(); inner(); }\n",
    );
    let (out, code) = run_in(root, &["callees", "outer", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(
        out.contains("beta"),
        "expected `beta` to bubble up to outer from the const closure, got:\n{}",
        out
    );
}

#[test]
fn typescript_callees_still_stops_at_named_nested_scope() {
    // A *named* nested function is its own scope: calls inside it must NOT
    // bubble up to the enclosing declaration.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("package.json"),
        r#"{"name":"smoke","version":"0.0.0"}"#,
    );
    write(
        &root.join("src/main.ts"),
        "function beta(): void {}\n\
         function outer(): void {\n\
           function nested(): void { beta(); }\n\
         }\n",
    );
    let (out, code) = run_in(root, &["callees", "outer", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(
        !out.contains("beta"),
        "beta should NOT be a callee of outer (it is inside named `nested`), got:\n{}",
        out
    );
}

#[test]
fn typescript_callers_includes_arrow_const() {
    // The flip side: `beta`'s callers must include the arrow-const callers.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("package.json"),
        r#"{"name":"smoke","version":"0.0.0"}"#,
    );
    write(
        &root.join("src/main.ts"),
        "function beta(): void {}\n\
         function alpha(): void { beta(); }\n\
         const gamma = (): void => { beta(); };\n\
         const epsilon = (): void => beta();\n",
    );
    let (out, code) = run_in(root, &["callers", "beta", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    for caller in ["alpha", "gamma", "epsilon"] {
        assert!(
            out.contains(caller),
            "expected `{}` in callers of beta, got:\n{}",
            caller,
            out
        );
    }
}

#[test]
fn callers_with_file_filter_narrows_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    // Two functions named `helper` in different files. `use` brings each
    // into scope so pass A resolves the calls precisely (no receiver).
    write(
        &root.join("src/lib.rs"),
        r#"
pub mod a;
pub mod b;
pub mod consumer_a;
pub mod consumer_b;
"#,
    );
    write(&root.join("src/a.rs"), "pub fn helper() {}\n");
    write(&root.join("src/b.rs"), "pub fn helper() {}\n");
    write(
        &root.join("src/consumer_a.rs"),
        "use crate::a::helper;\npub fn run_a() { helper(); }\n",
    );
    write(
        &root.join("src/consumer_b.rs"),
        "use crate::b::helper;\npub fn run_b() { helper(); }\n",
    );

    // With the file filter, only callers of `src/a.rs::helper` should appear.
    let (out, code) = run_in(root, &["callers", "src/a.rs:helper", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("run_a"),
        "expected run_a (caller of a::helper), got:\n{}",
        out
    );
    assert!(
        !out.contains("run_b"),
        "did not expect run_b (caller of b::helper), got:\n{}",
        out
    );
}

#[test]
fn callers_with_flag_form_matches_positional_form() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod h;\nuse crate::h::greet;\npub fn run() { greet(); }\n");
    write(&root.join("src/h.rs"), "pub fn greet() {}\n");

    let (positional_out, code1) =
        run_in(root, &["callers", "src/h.rs:greet", ".", "--rebuild"]);
    assert_eq!(code1, 0);

    // `--file` / `--symbol` form. Note: omit the trailing positional path
    // (defaults to "."); clap can't disambiguate optional-positional vs
    // optional-target when both are present, same shape as `find-related`.
    let (flag_out, code2) = run_in(
        root,
        &["callers", "--file", "src/h.rs", "--symbol", "greet", "--rebuild"],
    );
    assert_eq!(code2, 0);

    // Strip the header line which differs ("for 'X:Y'" vs "for 'X:Y'") —
    // both spell the target the same way after compose_target, so they
    // should match exactly. We compare the body lines for safety.
    let body_pos: Vec<&str> = positional_out
        .lines()
        .filter(|l| l.starts_with("src/") || l.starts_with("src\\"))
        .collect();
    let body_flag: Vec<&str> = flag_out
        .lines()
        .filter(|l| l.starts_with("src/") || l.starts_with("src\\"))
        .collect();
    assert_eq!(body_pos, body_flag, "flag form should match positional form");
    assert!(
        body_pos.iter().any(|l| l.contains("run")),
        "expected `run` in callers output, got:\n{}",
        positional_out
    );
}

#[test]
fn callers_file_filter_unknown_path_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub fn foo() {}\n");
    let out = Command::new(bin())
        .args(["callers", "src/nope.rs:foo", "."])
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "expected exit 2 when file filter has no matches");
}

#[test]
fn passing_subdir_as_path_walks_up_to_project_root() {
    // Regression: `ast-bro callers <sym> ./src` used to treat ./src as
    // the project root, producing qns like `main.rs::run` instead of
    // `src/main.rs::run`. The `<file>:<symbol>` filter then silently missed.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod h;\nuse crate::h::greet;\npub fn run() { greet(); }\n");
    write(&root.join("src/h.rs"), "pub fn greet() {}\n");

    let (out, code) = run_in(
        root,
        &["callers", "src/h.rs:greet", "./src", "--rebuild"],
    );
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("run"),
        "expected `run` (caller of greet) when project root is walked up to, got:\n{}",
        out
    );
}

#[test]
fn rust_callers_on_trait_returns_implementations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub trait Animal { fn speak(&self); }

pub struct Dog;
impl Animal for Dog { fn speak(&self) { println!("woof"); } }

pub struct Cat;
impl Animal for Cat { fn speak(&self) { println!("meow"); } }
"#,
    );
    let (out, code) = run_in(root, &["callers", "Animal", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("implementation(s)"),
        "expected implementations group, got:\n{}",
        out
    );
    assert!(
        out.contains("Dog") && out.contains("Cat"),
        "expected both impls listed, got:\n{}",
        out
    );
}

#[test]
fn rust_callers_on_struct_returns_constructions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub struct Greeter;
impl Greeter {
    pub fn hello(&self) {}
}

pub fn run() {
    Greeter.hello();
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "Greeter", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("construction(s)"),
        "expected constructions group, got:\n{}",
        out
    );
    assert!(
        out.contains("run"),
        "expected `run` (caller of Greeter.hello) in constructions, got:\n{}",
        out
    );
}

#[test]
fn callees_on_subtype_walks_to_ancestor_and_lists_its_methods() {
    // `callees <Type>` is the inverse of `callers <Type>` on the type
    // relationship graph: callers = downstream uses; callees = upstream
    // bases + the methods declared on those bases.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub trait Animal {
    fn speak(&self);
    fn breathe(&self);
}

pub struct Dog;
impl Animal for Dog {
    fn speak(&self) {}
    fn breathe(&self) {}
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "Dog", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(
        out.contains("ancestor(s) of struct Dog"),
        "expected ancestor header, got:\n{}",
        out
    );
    assert!(
        out.contains("trait Animal"),
        "expected `Animal` ancestor listed, got:\n{}",
        out
    );
    assert!(
        out.contains("speak") && out.contains("breathe"),
        "expected ancestor's method signatures listed, got:\n{}",
        out
    );
}

#[test]
fn callees_on_root_type_reports_no_ancestors() {
    // A type with no `bases` (e.g. a top-level trait or a unit struct
    // without `impl X for` blocks) returns gracefully without errors.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub trait Animal { fn speak(&self); }\n",
    );
    let (out, code) = run_in(root, &["callees", "Animal", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees on root type should not error, got exit {}", code);
    assert!(
        out.contains("no ancestors"),
        "expected `no ancestors` notice, got:\n{}",
        out
    );
}

#[test]
fn callees_on_type_walks_multiple_levels_with_depth() {
    // `--depth 2` should chase grandparents in a Java-style hierarchy
    // (Rust traits don't typically nest, but Scala / Java / Kotlin do).
    // Use Java for this test since multi-level hierarchies are idiomatic
    // there and tree-sitter-java is in our adapter set.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>x</groupId><artifactId>x</artifactId><version>0.0.0</version></project>\n",
    );
    write(
        &root.join("src/Animal.java"),
        "package smoke;\npublic interface Animal { void speak(); }\n",
    );
    write(
        &root.join("src/Mammal.java"),
        "package smoke;\npublic interface Mammal extends Animal { void nurse(); }\n",
    );
    write(
        &root.join("src/Dog.java"),
        "package smoke;\npublic class Dog implements Mammal { public void speak() {} public void nurse() {} }\n",
    );
    let (out, code) = run_in(root, &["callees", "Dog", ".", "--depth", "2", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(
        out.contains("Mammal"),
        "expected direct ancestor `Mammal`, got:\n{}",
        out
    );
    assert!(
        out.contains("Animal"),
        "expected grandparent `Animal` at depth=2, got:\n{}",
        out
    );
    assert!(
        out.contains("depth=2"),
        "expected `depth=2` annotation, got:\n{}",
        out
    );
}

#[test]
fn java_callers_finds_intra_file_caller() {
    // Single-file Java: pingTwice() calls greet(). Pass A in resolve.rs
    // (same-file lookup) should promote the bare `greet` name to a
    // Resolved qn pointing back to `Greeter::greet`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>x</groupId><artifactId>x</artifactId><version>0.0.0</version></project>\n",
    );
    write(
        &root.join("src/Greeter.java"),
        r#"
package smoke;
public class Greeter {
    public void greet() { System.out.println("hi"); }
    public void pingTwice() { greet(); greet(); }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers output, got:\n{}",
        out
    );
}

#[test]
fn java_callees_lists_construct_and_invocation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>x</groupId><artifactId>x</artifactId><version>0.0.0</version></project>\n",
    );
    write(
        &root.join("src/Demo.java"),
        r#"
package smoke;
public class Demo {
    public static String make() { return new String("x"); }
    public static int len() { return make().length(); }
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "len", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(
        out.contains("make") || out.contains("length"),
        "expected `make` or `length` in callees, got:\n{}",
        out
    );
}

#[test]
fn csharp_callers_finds_intra_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("Smoke.csproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>\n",
    );
    write(
        &root.join("src/Greeter.cs"),
        r#"
namespace Smoke;
public class Greeter {
    public void Greet() { System.Console.WriteLine("hi"); }
    public void PingTwice() { Greet(); Greet(); }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "Greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("PingTwice"),
        "expected `PingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn csharp_dotted_namespace_matches_segment_by_segment() {
    // `namespace Shop.Billing` is two scopes, the way the query is split:
    // `Billing.Pricing.Tax` must find the method like `Pricing.Tax` does.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("src/Pricing.cs"),
        r#"
namespace Shop.Billing
{
    public class Pricing
    {
        public int Sum(int x) { return Tax(x); }
        public int Tax(int x) { return x; }
    }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "Billing.Pricing.Tax", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(out.contains("Sum"), "expected `Sum` in callers, got:\n{}", out);
}

#[test]
fn php_function_named_like_a_class_keeps_its_callers() {
    // The function `Foo` and the class `Foo` share a qn; both are queried.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("php/foo.php"),
        "<?php\nfunction Foo() { return 1; }\nclass Foo { public function bar() {} }\nfunction g() { Foo(); }\nfunction h() { $x = new Foo(); }\n",
    );
    let (out, code) = run_in(root, &["callers", "php/foo.php:Foo", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("php/foo.php::g  (Exact)"), "{}", out);
}

#[test]
fn csharp_dotted_query_prefers_the_type_it_names_in_full() {
    // `Billing.Pricing` names `namespace Billing`'s class; the classes in
    // `Shop.Billing` and `Other.Billing` only end with those parts.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("src/Pricing.cs"),
        r#"
namespace Shop.Billing { public class Pricing { } }
namespace Other.Billing { public class Pricing { } }
namespace Billing
{
    public class Pricing { }
    public class Use { public object A() { return new Pricing(); } }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "Billing.Pricing", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert_eq!(out.matches("construction(s) of").count(), 1, "one type expected:\n{}", out);
    // The class in a dotted namespace is found by its full name.
    let (out, _) = run_in(root, &["callers", "Shop.Billing.Pricing", "."]);
    assert_eq!(out.matches("construction(s) of").count(), 1, "{}", out);
}

/// An Elixir project: `Shop.Pricing` is reached through an alias, `Shop.Util`
/// through an import, and `Shop.Other` defines homonyms of Kernel and local
/// names that no call may bind to.
fn elixir_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/shop/pricing.ex"),
        r#"
defmodule Shop.Pricing do
  defstruct [:total]

  def sum(lines), do: Enum.reduce(lines, 0, &add/2)
  defp add(x, acc), do: x + acc
end
"#,
    );
    write(
        &root.join("lib/shop/util.ex"),
        "defmodule Shop.Util do\n  def fmt(x), do: x\nend\n",
    );
    write(
        &root.join("lib/shop/other.ex"),
        "defmodule Shop.Other do\n  def elem(_, _), do: :no\n  def total(_), do: :no\nend\n",
    );
    write(
        &root.join("lib/shop/cart.ex"),
        r#"
defmodule Shop.Cart do
  alias Shop.Pricing
  import Shop.Util

  def total(cart) do
    sum = Pricing.sum(cart.lines)
    fmt(elem({sum}, 0))
  end

  def checkout(cart), do: %Pricing{total: total(cart)}
end
"#,
    );
    tmp
}

#[test]
fn elixir_callers_finds_intra_file_caller() {
    let tmp = elixir_project();
    let (out, code) = run_in(tmp.path(), &["callers", "Cart.total", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(out.contains("Shop.Cart::checkout"), "expected `checkout`, got:\n{}", out);
}

#[test]
fn elixir_callers_survives_a_long_operator_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let chain = vec!["\"a\""; 20_000].join(" <> ");
    write(
        &tmp.path().join("lib/chain.ex"),
        &format!("defmodule Chain do\n  def s, do: {chain} <> t()\n  def t, do: 1\nend\n"),
    );
    let (out, code) = run_in(tmp.path(), &["callers", "t", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(out.contains("Chain::s"), "expected `s`, got:\n{}", out);
}

#[test]
fn elixir_callees_lists_construct_and_remote_call() {
    // The alias is expanded, the import followed, and a stdlib or Kernel
    // call is external — never the project's homonym in `Shop.Other`.
    let tmp = elixir_project();
    let (out, code) = run_in(tmp.path(), &["callees", "Cart.total", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(out.contains("Shop.Pricing::sum"), "alias not followed:\n{}", out);
    assert!(out.contains("Shop.Util::fmt"), "import not followed:\n{}", out);
    assert!(out.contains("[external] elem"), "Kernel call not external:\n{}", out);
    assert!(!out.contains("Shop.Other::"), "bound to a homonym:\n{}", out);
    // `cart.lines` reads a field; it is not a call.
    assert!(!out.contains("lines"), "field access taken for a call:\n{}", out);

    let (out, _) = run_in(tmp.path(), &["callees", "Pricing.sum", "."]);
    assert!(out.contains("[external] Enum.reduce"), "{}", out);
    assert!(out.contains("Shop.Pricing::add"), "capture not a call:\n{}", out);

    let (out, _) = run_in(tmp.path(), &["callers", "Shop.Pricing", "."]);
    assert!(out.contains("checkout"), "struct literal not a construction:\n{}", out);
}

#[test]
fn elixir_callees_follow_pipes_delegates_and_relative_names() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("lib/calc.ex"),
        r#"
defmodule Shop.Calc do
  require Shop.Macros
  defdelegate half(x), to: Shop.Calc.Sub
  def piped(x), do: x |> double
  def nested(x) do
    alias Shop.Calc.Sub
    Sub.f(x)
  end
  def relative(x), do: __MODULE__.Sub.f(x)
  def quoted(x), do: quote(do: unquote(double(x)) + 1)
  def required, do: bar()
  defp double(x), do: x * 2
end

defmodule Shop.Calc.Sub do
  def f(x), do: x
  def half(x), do: x
end

defmodule Shop.Macros do
  defmacro bar, do: 1
end
"#,
    );
    let callees = |sym: &str| {
        let (out, code) = run_in(tmp.path(), &["callees", sym, "."]);
        assert_eq!(code, 0, "callees {sym} exited non-zero: {out}");
        out
    };
    let out = callees("Calc.half");
    assert!(out.contains("Shop.Calc.Sub::half"), "delegate not followed:\n{out}");
    let out = callees("Calc.piped");
    assert!(out.contains("Shop.Calc::double"), "pipe target not a call:\n{out}");
    let out = callees("Calc.nested");
    assert!(out.contains("Shop.Calc.Sub::f"), "body alias not expanded:\n{out}");
    let out = callees("Calc.relative");
    assert!(out.contains("Shop.Calc.Sub::f"), "`__MODULE__.Sub` not expanded:\n{out}");
    let out = callees("Calc.quoted");
    assert!(out.contains("Shop.Calc::double"), "`unquote` not walked:\n{out}");
    // `require` brings no functions into scope.
    let out = callees("Calc.required");
    assert!(out.contains("[external] bar"), "bound through `require`:\n{out}");
}

#[test]
fn elixir_calls_stay_in_elixir() {
    // `Shop::Util.fmt` in Ruby has the module path `Shop.Util` imports.
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("lib/cart.ex"),
        "defmodule Shop.Cart do\n  import Shop.Util\n  def total(c), do: fmt(c)\nend\n",
    );
    write(
        &tmp.path().join("app/util.rb"),
        "module Shop\n  module Util\n    def self.fmt(x); x; end\n  end\nend\n",
    );
    let (out, code) = run_in(tmp.path(), &["callees", "Cart.total", ".", "--rebuild"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(out.contains("[external] fmt"), "bound into Ruby:\n{}", out);
}

#[test]
fn elixir_trace_follows_an_alias_and_a_capture() {
    let tmp = elixir_project();
    let (out, code) = run_in(tmp.path(), &["trace", "Cart.total", "Pricing.add", ".", "--rebuild"]);
    assert_eq!(code, 0, "trace exited non-zero: {}", out);
    assert!(out.contains("Shop.Pricing::sum"), "no path through `sum`:\n{}", out);
    assert!(out.contains("Shop.Pricing::add"), "capture not followed:\n{}", out);
}

#[test]
fn elixir_partial_update_resolves_new_calls_the_same_way() {
    let tmp = elixir_project();
    let root = tmp.path();
    run_in(root, &["callers", "Pricing.sum", ".", "--rebuild"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(
        &root.join("lib/shop/report.ex"),
        "defmodule Shop.Report do\n  alias Shop.Pricing\n  def run(c), do: elem({Pricing.sum(c)}, 0)\nend\n",
    );
    let (out, code) = run_in(root, &["callees", "Report.run", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("Shop.Pricing::sum"), "{}", out);
    assert!(out.contains("[external] elem"), "{}", out);
    assert!(!out.contains("Shop.Other::"), "{}", out);
}

#[test]
fn elixir_partial_update_links_a_new_module_to_its_callers() {
    // `Shop.Later.go()` is external until `later.ex` defines it; adding the
    // file must link the unchanged caller, as a cold build would.
    let tmp = elixir_project();
    let root = tmp.path();
    write(
        &root.join("lib/shop/early.ex"),
        "defmodule Shop.Early do\n  def run, do: Shop.Later.go()\nend\n",
    );
    let (out, _) = run_in(root, &["callees", "Early.run", ".", "--rebuild"]);
    assert!(out.contains("[external] Shop.Later.go"), "{}", out);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(
        &root.join("lib/shop/later.ex"),
        "defmodule Shop.Later do\n  def go, do: :ok\nend\n",
    );
    let (out, code) = run_in(root, &["callers", "Later.go", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("Shop.Early::run"), "caller not linked:\n{}", out);
}

#[test]
fn elixir_dotted_protocol_lists_its_implementations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("lib/printable.ex"),
        r#"
defprotocol Billing.Printable do
  def print(value)
end

defimpl Billing.Printable, for: Integer do
  def print(n), do: n
end

defmodule Billing.Text do
  alias Billing.Printable

  defimpl Printable do
    def print(t), do: t
  end
end
"#,
    );
    let (out, code) = run_in(root, &["callers", "Billing.Printable", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    // `defimpl Printable` implements `Billing.Printable` through the alias.
    assert!(
        out.contains("2 implementation"),
        "expected the Integer and Text impls in callers, got:\n{}",
        out
    );
    // And the impl finds the protocol as its ancestor.
    let (out, code) = run_in(root, &["callees", "Billing.Printable.Integer", "."]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    assert!(out.contains("protocol Billing.Printable"), "ancestor not found:\n{}", out);
}

/// Modules whose last parts collide: `Shop.Pricing` and `Other.Pricing`,
/// `Shop.Repo` and the dependency `Ecto.Repo`, and `Shop.Money` with its
/// `Inspect` implementation `Inspect.Shop.Money`.
fn elixir_types_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/money.ex"),
        r#"
defmodule Shop.Money do
  defstruct [:amount]
  def new(a), do: %Shop.Money{amount: a}
end

defimpl Inspect, for: Shop.Money do
  def inspect(m, _opts), do: m.amount
end
"#,
    );
    write(
        &root.join("lib/store.ex"),
        "defmodule Shop.Store do\n  @callback get(term) :: term\nend\n",
    );
    write(
        &root.join("lib/mem_store.ex"),
        "defmodule Shop.MemStore do\n  alias Shop.Store\n  @behaviour Store\n  def get(k), do: k\nend\n",
    );
    write(
        &root.join("lib/repo.ex"),
        "defmodule Shop.Repo do\n  @behaviour Ecto.Repo\n  def all(q), do: [q]\nend\n",
    );
    write(
        &root.join("lib/pricing.ex"),
        r#"
defmodule Shop.Pricing do
  defstruct [:total]
  def sum(xs), do: Enum.sum(xs)
end

defmodule Other.Pricing do
  defstruct [:total]
  def build, do: %Other.Pricing{total: 0}
end
"#,
    );
    write(
        &root.join("lib/cart.ex"),
        r#"
defmodule Shop.Cart do
  alias Shop.Pricing

  def build(xs) do
    total = Pricing.sum(xs)
    %Pricing{total: total}
  end
end
"#,
    );
    tmp
}

#[test]
fn elixir_type_query_names_one_module() {
    // `Inspect.Shop.Money` ends with `Shop.Money` but is another module.
    let tmp = elixir_types_project();
    let root = tmp.path();
    let (out, code) = run_in(root, &["callers", "Shop.Money", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(!out.contains("Inspect"), "the impl matched too:\n{}", out);
    assert_eq!(out.matches("Shop.Money::new").count(), 1, "{}", out);
    let (out, code) = run_in(root, &["context", "lib/money.ex:Shop.Money", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(!out.contains("Inspect"), "the impl matched too:\n{}", out);
}

#[test]
fn elixir_behaviour_implementors_agree_across_commands() {
    // `@behaviour Store` names `Shop.Store` through the alias.
    let tmp = elixir_types_project();
    let root = tmp.path();
    for cmd in ["callers", "impact", "context"] {
        let (out, code) = run_in(root, &[cmd, "Shop.Store", ".", "--rebuild"]);
        assert_eq!(code, 0, "{}: {}", cmd, out);
        assert!(out.contains("Shop.MemStore"), "{} lists no implementor:\n{}", cmd, out);
    }
}

#[test]
fn elixir_types_are_keyed_by_full_module_name() {
    let tmp = elixir_types_project();
    let root = tmp.path();
    // `Shop.Repo` implements `Ecto.Repo`, a dependency, not itself.
    let (out, code) = run_in(root, &["callers", "Shop.Repo", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(!out.contains("implementation"), "implements itself:\n{}", out);
    let (out, code) = run_in(root, &["callees", "Shop.Repo", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("[external] Ecto.Repo"), "base not listed:\n{}", out);
    // `%Pricing{}` builds `Shop.Pricing`, and `Pricing.sum` is called on it;
    // `%Other.Pricing{}` builds another module.
    let (out, code) = run_in(root, &["callers", "Shop.Pricing", "."]);
    assert_eq!(code, 0, "{}", out);
    // The file column uses the platform separator.
    let out = out.replace('\\', "/");
    assert!(out.contains("lib/cart.ex:7"), "struct literal missing:\n{}", out);
    assert!(out.contains("lib/cart.ex:6"), "`Pricing.sum()` missing:\n{}", out);
    assert!(!out.contains("Other.Pricing"), "another module's struct:\n{}", out);
    let (out, code) = run_in(root, &["callees", "Shop.Cart.build", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("lib/pricing.ex::Shop.Pricing  (Exact)"), "{}", out);
}

#[test]
fn elixir_nested_modules_are_keyed_by_full_name() {
    // A nested `defmodule Pricing` is `Shop.Pricing`, as a struct literal and
    // a behaviour name it from outside.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/shop.ex"),
        r#"defmodule Shop do
  defmodule Pricing do
    defstruct [:total]

    defimpl Inspect do
      def inspect(p, _), do: p.total
    end
  end

  defmodule Store do
    @callback get(term) :: term
  end

  defmodule MemStore do
    @behaviour Shop.Store
    def get(k), do: k
  end
end
"#,
    );
    write(
        &root.join("lib/cart.ex"),
        "defmodule Shop.Cart do\n  def build(), do: %Shop.Pricing{total: 0}\n  def show(p), do: Inspect.Shop.Pricing.inspect(p, [])\nend\n",
    );
    let (out, code) = run_in(root, &["callers", "Shop.Pricing", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    // The file column uses the platform separator.
    let out = out.replace('\\', "/");
    assert!(out.contains("lib/cart.ex:2"), "struct literal missing:\n{}", out);
    let (out, _) = run_in(root, &["callees", "Shop.Cart.build", "."]);
    assert!(out.contains("lib/shop.ex::Shop::Pricing  (Exact)"), "{}", out);
    for cmd in ["callers", "impact", "context"] {
        let (out, _) = run_in(root, &[cmd, "Shop.Store", "."]);
        assert!(out.contains("MemStore"), "{} lists no implementor:\n{}", cmd, out);
    }
    let (out, _) = run_in(root, &["callees", "Shop.MemStore", "."]);
    assert!(out.contains("module Store"), "ancestor not found:\n{}", out);
    // A `defimpl` in the module is named in full, `Inspect.Shop.Pricing`.
    let (out, _) = run_in(root, &["callers", "Inspect.Shop.Pricing", "."]);
    let out = out.replace('\\', "/");
    assert!(out.contains("lib/cart.ex:3"), "call on the impl missing:\n{}", out);
    // An incremental update keys the module the same way.
    let shop = std::fs::read_to_string(root.join("lib/shop.ex")).unwrap();
    write(
        &root.join("lib/shop.ex"),
        &shop.replace(
            "def get(k), do: k",
            "def get(k), do: k\n    def put(k), do: k",
        ),
    );
    // The update ran: the new function is in the graph.
    let (out, code) = run_in(root, &["callees", "Shop.MemStore.put", "."]);
    assert_eq!(code, 0, "update not applied:\n{}", out);
    let (out, _) = run_in(root, &["callees", "Shop.Cart.build", "."]);
    assert!(out.contains("lib/shop.ex::Shop::Pricing  (Exact)"), "{}", out);
}

#[test]
fn elixir_incremental_update_keeps_full_type_keys() {
    let tmp = elixir_types_project();
    let root = tmp.path();
    let (out, _) = run_in(root, &["callers", "Shop.Store", ".", "--rebuild"]);
    assert!(out.contains("Shop.MemStore"), "{}", out);
    write(
        &root.join("lib/mem_store.ex"),
        "defmodule Shop.MemStore do\n  alias Shop.Store\n  @behaviour Store\n  def get(k), do: k\n  def put(k), do: k\nend\n",
    );
    // The update ran: the new function is in the graph.
    let (out, code) = run_in(root, &["callees", "Shop.MemStore.put", "."]);
    assert_eq!(code, 0, "update not applied:\n{}", out);
    let (out, code) = run_in(root, &["callers", "Shop.Store", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("Shop.MemStore"), "implementor lost after the update:\n{}", out);
    // A struct literal's target is a type, which the update re-resolves.
    let (out, _) = run_in(root, &["callees", "Shop.Cart.build", "."]);
    assert!(out.contains("lib/pricing.ex::Shop.Pricing  (Exact)"), "{}", out);
}

/// Imports scoped by module, function and block, with `only:` and `except:`.
fn elixir_imports_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/util.ex"),
        r#"defmodule Shop.Util do
  def fmt(x), do: x
  def fmt(x, y), do: {x, y}
  def pad(x), do: x
  def wrap(x, opts), do: {x, opts}
end

defmodule Shop do
  def foo(), do: :ok
end
"#,
    );
    write(
        &root.join("lib/a.ex"),
        r#"defmodule Shop.A do
  import Shop.Util, only: [fmt: 1, wrap: 2]
  def one(x), do: fmt(x)
  def two(x), do: fmt(x, x)
  def three(x), do: pad(x)
  def piped(x), do: x |> fmt(x)
  def captured(), do: &fmt/2
  def block(x) do
    wrap x do
      :ok
    end
  end
end

defmodule Shop.B do
  def four(x), do: fmt(x)
end

defmodule Shop.C do
  import Shop.Util, except: [pad: 1]
  alias Shop
  def five(x), do: pad(x)
  def six(x), do: fmt(x)
  def seven(), do: foo()
end
"#,
    );
    write(
        &root.join("lib/d.ex"),
        r#"defmodule Shop.D do
  def eight(x) do
    import Shop.Util
    pad(x)
  end

  def nine(x), do: pad(x)

  def ten(x) do
    if x do
      alias Shop.Util, as: U
      U.pad(x)
    end
    U.pad(x)
  end

  def eleven(x) do
    require Shop.Util, as: R
    R.pad(x)
  end
end
"#,
    );
    tmp
}

#[test]
fn elixir_imports_are_lexical_and_honour_selectors() {
    let tmp = elixir_imports_project();
    let root = tmp.path();
    run_in(root, &["callers", "fmt", ".", "--rebuild"]);
    let callees = |sym: &str| {
        let (out, code) = run_in(root, &["callees", sym, "."]);
        assert_eq!(code, 0, "callees {sym} exited non-zero: {out}");
        // The file column uses the platform separator.
        out.replace('\\', "/")
    };
    let exact = |sym: &str, target: &str| {
        let out = callees(sym);
        assert!(out.contains(&format!("{target}  (Exact)")), "{sym}: {target} not Exact:\n{out}");
    };
    let external = |sym: &str, name: &str| {
        let out = callees(sym);
        assert!(out.contains(&format!("[external] {name}")), "{sym}: {name} not external:\n{out}");
    };
    exact("Shop.A.one", "Shop.Util::fmt");
    // `only: [fmt: 1]` imports neither `fmt/2` nor `pad/1`.
    external("Shop.A.two", "fmt");
    external("Shop.A.three", "pad");
    // A pipe and a `do` block each pass one more argument.
    external("Shop.A.piped", "fmt");
    external("Shop.A.captured", "fmt");
    exact("Shop.A.block", "Shop.Util::wrap");
    // A sibling module in the same file imports nothing.
    external("Shop.B.four", "fmt");
    external("Shop.C.five", "pad");
    exact("Shop.C.six", "Shop.Util::fmt");
    // `alias Shop` names `Shop`; it imports nothing.
    external("Shop.C.seven", "foo");
    // An `import` in a function ends with it.
    exact("Shop.D.eight", "Shop.Util::pad");
    external("Shop.D.nine", "pad");
    // An `alias` in an `if` ends with the block.
    let out = callees("Shop.D.ten");
    assert!(out.contains("lib/d.ex:12    lib/util.ex::Shop.Util::pad  (Exact)"), "{out}");
    assert!(!out.contains("lib/d.ex:14    lib/util.ex"), "alias leaked out of the `if`:\n{out}");
    // `require … as:` aliases the module.
    exact("Shop.D.eleven", "Shop.Util::pad");
}

#[test]
fn elixir_callees_cover_defaults_bound_quotes_and_split_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/a.ex"),
        r#"defmodule Shop.A do
  def compute(), do: 1
  def f(x \\ compute()), do: x
  def q() do
    quote bind_quoted: [v: compute()] do
      v
    end
  end
end
"#,
    );
    // `Shop.M` is defined in two files; each calls its own `b`.
    write(&root.join("lib/m1.ex"), "defmodule Shop.M do\n  def a(), do: b()\n  def b(), do: 1\nend\n");
    write(&root.join("lib/m2.ex"), "defmodule Shop.M do\n  def b(), do: 2\nend\n");
    for sym in ["Shop.A.f", "Shop.A.q"] {
        let (out, code) = run_in(root, &["callees", sym, ".", "--rebuild"]);
        assert_eq!(code, 0, "{}", out);
        assert!(out.contains("Shop.A::compute  (Exact)"), "{sym}:\n{out}");
    }
    let (out, _) = run_in(root, &["callees", "lib/m1.ex:Shop.M.a", "."]);
    assert!(out.contains("lib/m1.ex::Shop.M::b  (Exact)"), "{}", out);
}

#[test]
fn elixir_struct_patterns_are_not_constructions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/pricing.ex"),
        r#"defmodule Shop.Pricing do
  defstruct [:total]
  def build(), do: %Shop.Pricing{total: 0}
  def total(%Shop.Pricing{total: t} = p), do: {t, p}
  def match(p) do
    %Shop.Pricing{total: t} = p
    case p do
      %Shop.Pricing{} -> t
      x when is_cheap(x) -> x
    end
  end
  defguard is_cheap(x) when x < 10
  def anon(), do: fn %Shop.Pricing{} = p -> p end
  def from(x) do
    with %Shop.Pricing{} = p <- fetch(x), do: p
  end
  def fetch(x), do: x
  def check(x) do
    cond do
      valid?(x) -> :ok
      true -> :no
    end
  end
  def valid?(_), do: true
end
"#,
    );
    let (out, code) = run_in(root, &["callers", "Shop.Pricing", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("# 1 caller"), "a pattern counted as a construction:\n{}", out);
    // The file column uses the platform separator.
    let out = out.replace('\\', "/");
    assert!(out.contains("lib/pricing.ex:3 "), "{}", out);
    // A `with` clause calls its right side, and a `cond` clause its condition.
    let (out, _) = run_in(root, &["callees", "Shop.Pricing.from", "."]);
    assert!(out.contains("Shop.Pricing::fetch  (Exact)"), "{}", out);
    let (out, _) = run_in(root, &["callees", "Shop.Pricing.check", "."]);
    assert!(out.contains("Shop.Pricing::valid?  (Exact)"), "{}", out);
    // A clause's guard is not a pattern.
    let (out, _) = run_in(root, &["callees", "Shop.Pricing.match", "."]);
    assert!(out.contains("Shop.Pricing::is_cheap  (Exact)"), "{}", out);
}

#[test]
fn elixir_bare_calls_compare_arity() {
    // A local `elem/3` or `length/2` does not take Kernel's `elem/2` or
    // `length/1`; a default argument accepts one argument fewer.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/util.ex"),
        "defmodule Shop.Util do\n  def length(a, b), do: {a, b}\nend\n",
    );
    write(
        &root.join("lib/t.ex"),
        r#"defmodule Shop.T do
  import Shop.Util
  def elem(a, b, c), do: {a, b, c}
  def opt(a, b \\ 1), do: {a, b}
  def guarded(a, b \\ 1) when is_integer(a), do: {a, b}
  def pair(a), do: a
  def pair(a, b), do: {a, b}
  def kernel(t), do: {elem(t, 0), length(t)}
  def own(t), do: {elem(t, 1, 2), length(t, 1)}
  def defaults(t), do: {opt(t), opt(t, 2), guarded(t), pair(t)}
end
"#,
    );
    let callees = |sym: &str| run_in(root, &["callees", sym, ".", "--rebuild"]).0;
    let out = callees("Shop.T.kernel");
    assert!(out.contains("[external] elem"), "{}", out);
    assert!(out.contains("[external] length"), "{}", out);
    assert!(!out.contains("(Exact)"), "{}", out);
    let out = callees("Shop.T.own");
    assert!(out.contains("Shop.T::elem  (Exact)"), "{}", out);
    assert!(out.contains("Shop.Util::length  (Exact)"), "{}", out);
    let out = callees("Shop.T.defaults");
    assert_eq!(out.matches("Shop.T::opt  (Exact)").count(), 2, "{}", out);
    assert!(out.contains("Shop.T::guarded  (Exact)"), "{}", out);
    // `pair/1` and `pair/2` share a qn; each definition counts.
    assert!(out.contains("Shop.T::pair  (Exact)"), "{}", out);
}

#[test]
fn elixir_incremental_update_resolves_against_the_new_definitions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/util.ex"),
        "defmodule Shop.Util do\n  def pad(x), do: x\nend\n",
    );
    write(&root.join("lib/dup.ex"), "defmodule Shop.Dup do\n  def f(), do: 1\nend\n");
    write(&root.join("lib/dup2.ex"), "defmodule Shop.Dup do\n  def f(), do: 2\nend\n");
    write(
        &root.join("lib/e.ex"),
        "defmodule Shop.E do\n  import Shop.Util\n  import Shop.Dup\n  def go(x), do: extra(x)\n  def dup(), do: f()\n  def mk(), do: %Shop.Later{}\nend\n",
    );
    let (out, _) = run_in(root, &["callees", "Shop.E.go", ".", "--rebuild"]);
    assert!(out.contains("[external] extra"), "{}", out);
    let (out, _) = run_in(root, &["callees", "Shop.E.dup", "."]);
    assert!(out.contains("Ambiguous"), "{}", out);
    // A function added to an imported module, and a module left in one file.
    write(
        &root.join("lib/util.ex"),
        "defmodule Shop.Util do\n  def pad(x), do: x\n  def extra(x), do: x\nend\n",
    );
    std::fs::remove_file(root.join("lib/dup2.ex")).unwrap();
    write(&root.join("lib/later.ex"), "defmodule Shop.Later do\n  defstruct [:a]\nend\n");
    let (out, code) = run_in(root, &["callees", "Shop.E.go", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("Shop.Util::extra  (Exact)"), "added function not found:\n{}", out);
    let (out, _) = run_in(root, &["callees", "Shop.E.dup", "."]);
    assert!(out.contains("lib/dup.ex::Shop.Dup::f  (Exact)"), "deleted candidate kept:\n{}", out);
    // A struct literal of a module added since resolves to it.
    let (out, _) = run_in(root, &["callees", "Shop.E.mk", "."]);
    assert!(out.contains("lib/later.ex::Shop.Later  (Exact)"), "{}", out);
}

#[test]
fn elixir_callees_cover_less_common_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/forms.ex"),
        r#"defmodule Shop.Target do
  def send(v), do: v
end

defmodule Shop.Forms do
  def greet(name, # who
            opts \\ []), do: {name, opts}
  def run, do: greet("a")

  def default_value, do: 1
  defdelegate send(v \\ default_value()), to: Shop.Target

  def permitted(_), do: true
  def fetch, do: {:ok, 1}
  def guarded do
    with {:ok, v} when permitted(v) <- fetch() do
      v
    end
  end

  def timeout_ms, do: 10
  def fallback, do: :none
  def waits do
    receive do
      :ready -> :ok
    after
      timeout_ms() -> fallback()
    end
  end

  def helper(x), do: x
  def captures(offset), do: &(offset + helper(&1))
end

defmodule Shop.MyMacros do
  defmacro if(c, v), do: {c, v}
end

defmodule Shop.OwnIf do
  import Kernel, except: [if: 2]
  import Shop.MyMacros, only: [if: 2]
  def u, do: if(true, :ok)
end

defmodule Shop.AnyIf do
  import Kernel, except: [if: 2]
  import Shop.MyMacros
  def u, do: if(true, :ok)
end

defmodule Shop.KernelIf do
  import Shop.MyMacros, only: [if: 2]
  import Kernel, except: [unless: 2]
  def k(x), do: if(x, do: 1)
end
"#,
    );
    let callees = |sym: &str| run_in(root, &["callees", sym, ".", "--rebuild"]).0;
    // A comment in a head does not hide its default argument.
    let out = callees("Shop.Forms.run");
    assert!(out.contains("Shop.Forms::greet  (Exact)"), "{}", out);
    // A delegate evaluates its default arguments.
    let out = callees("Shop.Forms.send");
    assert!(
        out.contains("Shop.Forms::default_value  (Exact)"),
        "{}",
        out
    );
    assert!(out.contains("Shop.Target::send  (Exact)"), "{}", out);
    // A guard on the left of `<-` is code; the pattern is not.
    let out = callees("Shop.Forms.guarded");
    assert!(out.contains("Shop.Forms::permitted  (Exact)"), "{}", out);
    assert!(out.contains("Shop.Forms::fetch  (Exact)"), "{}", out);
    // A `receive` timeout is an expression.
    let out = callees("Shop.Forms.waits");
    assert!(out.contains("Shop.Forms::timeout_ms  (Exact)"), "{}", out);
    assert!(out.contains("Shop.Forms::fallback  (Exact)"), "{}", out);
    // A capture of an expression calls what the expression calls.
    let out = callees("Shop.Forms.captures");
    assert!(out.contains("Shop.Forms::helper  (Exact)"), "{}", out);
    assert!(!out.contains("offset"), "{}", out);
    // A Kernel macro excluded for an import that names it is a call.
    let out = callees("Shop.OwnIf.u");
    assert!(out.contains("Shop.MyMacros::if  (Exact)"), "{}", out);
    let out = callees("Shop.AnyIf.u");
    assert!(out.contains("Shop.MyMacros::if  (Exact)"), "{}", out);
    // Kernel's own `if` is not.
    let out = callees("Shop.KernelIf.k");
    assert!(out.contains("# 0 callee"), "{}", out);
}

#[test]
fn elixir_imports_select_as_elixir_does() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/imports.ex"),
        r#"defmodule Shop.A do
  def fmt(x), do: x
  def pad(x), do: x
  def other(x), do: x
end

defmodule Shop.B do
  def fmt(x), do: x
end

defmodule Shop.Reimport do
  import Shop.A, only: [fmt: 1, pad: 1]
  def f(x) do
    import Shop.A, except: [fmt: 1]
    import Shop.B, only: [fmt: 1]
    {fmt(x), pad(x), other(x)}
  end
  def g(x), do: fmt(x)
end

defmodule Shop.MacA do
  defmacro mfmt(x), do: x
end

defmodule Shop.FunB do
  def mfmt(x), do: x
end

defmodule Shop.Kinds do
  import Shop.MacA, only: :functions
  import Shop.FunB, only: [mfmt: 1]
  def k(x), do: mfmt(x)
end

defmodule Shop.Macros do
  import Shop.MacA, only: :macros
  import Shop.FunB, only: :macros
  def m(x), do: mfmt(x)
end

defmodule Shop.Guards do
  defguard is_pos(x) when is_integer(x) and x > 0
end

defmodule Shop.GuardMacros do
  import Shop.Guards, only: :macros
  def ok(x) when is_pos(x), do: x
end

defmodule Shop.GuardFns do
  import Shop.Guards, only: :functions
  def ok(x) when is_pos(x), do: x
end

defmodule Shop.Tools do
  def pad(x), do: x
  def plain(x), do: x
  defmacro twice(x), do: x
end

defmodule Shop.ExceptAfterFunctions do
  import Shop.Tools, only: :functions
  def e(x) do
    import Shop.Tools, except: [pad: 1]
    {twice(x), pad(x), plain(x)}
  end
end

defmodule Shop.ExceptAfterMacros do
  import Shop.Tools, only: :macros
  def e(x) do
    import Shop.Tools, except: [twice: 1]
    {twice(x), plain(x)}
  end
end

defmodule Shop.FunctionsExcept do
  import Shop.Tools
  def e(x) do
    import Shop.Tools, only: :functions, except: [pad: 1]
    {twice(x), pad(x), plain(x)}
  end
end

defmodule Shop.Tuple do
  import Shop.A, only: [{:fmt, 1}]
  def t(x), do: fmt(x)
end

defmodule Shop.Hidden do
  defp work(x), do: x
  def own(x), do: work(x)
end

defmodule Shop.Public do
  def work(x), do: x
end

defmodule Shop.Priv do
  import Shop.Hidden
  import Shop.Public
  def p(x), do: work(x)
end
"#,
    );
    let callees = |sym: &str| run_in(root, &["callees", sym, ".", "--rebuild"]).0;
    // `except:` filters the earlier import of the same module, and the
    // module-level import is back once the function ends.
    let out = callees("Shop.Reimport.f");
    assert!(out.contains("Shop.B::fmt  (Exact)"), "{}", out);
    assert!(out.contains("Shop.A::pad  (Exact)"), "{}", out);
    assert!(out.contains("[external] other"), "{}", out);
    let out = callees("Shop.Reimport.g");
    assert!(out.contains("Shop.A::fmt  (Exact)"), "{}", out);
    // `only: :functions` leaves out a macro, `only: :macros` a function.
    let out = callees("Shop.Kinds.k");
    assert!(out.contains("Shop.FunB::mfmt  (Exact)"), "{}", out);
    let out = callees("Shop.Macros.m");
    assert!(out.contains("Shop.MacA::mfmt  (Exact)"), "{}", out);
    // A guard is a macro.
    let out = callees("Shop.GuardMacros.ok");
    assert!(out.contains("Shop.Guards::is_pos  (Exact)"), "{}", out);
    let out = callees("Shop.GuardFns.ok");
    assert!(out.contains("[external] is_pos"), "{}", out);
    // `except:` alone applies per kind: it filters the kind imported
    // before and brings in the kind that was not.
    let out = callees("Shop.ExceptAfterFunctions.e");
    assert!(out.contains("Shop.Tools::twice  (Exact)"), "{}", out);
    assert!(out.contains("Shop.Tools::plain  (Exact)"), "{}", out);
    assert!(out.contains("[external] pad"), "{}", out);
    let out = callees("Shop.ExceptAfterMacros.e");
    assert!(out.contains("Shop.Tools::plain  (Exact)"), "{}", out);
    assert!(out.contains("[external] twice"), "{}", out);
    // With `only:` it replaces the earlier import.
    let out = callees("Shop.FunctionsExcept.e");
    assert!(out.contains("Shop.Tools::plain  (Exact)"), "{}", out);
    assert!(out.contains("[external] twice"), "{}", out);
    assert!(out.contains("[external] pad"), "{}", out);
    // `[{:fmt, 1}]` is `[fmt: 1]`.
    let out = callees("Shop.Tuple.t");
    assert!(out.contains("Shop.A::fmt  (Exact)"), "{}", out);
    // An import brings only public functions; the module calls its own.
    let out = callees("Shop.Priv.p");
    assert!(out.contains("Shop.Public::work  (Exact)"), "{}", out);
    let out = callees("Shop.Hidden.own");
    assert!(out.contains("Shop.Hidden::work  (Exact)"), "{}", out);
}

#[test]
fn elixir_unresolved_call_on_another_module_is_not_a_possible_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule MyApp.MixProject do\nend\n");
    write(
        &root.join("lib/repo.ex"),
        r#"defmodule MyApp.Repo do
  use Ecto.Repo, otp_app: :my_app
end

defmodule MyApp.Accounts do
  def all(x), do: x
end

defmodule MyApp.Web do
  alias MyApp.Repo
  def list, do: MyApp.Repo.all(:users)
  def short, do: Repo.all(:users)
  def own, do: MyApp.Accounts.all(:a, :b)
  def go(repo), do: repo.all(:users)
end
"#,
    );
    // `all` comes from `use Ecto.Repo`, so the call stays unresolved, but
    // its receiver names a project module that is not the target's.
    let (out, _) = run_in(root, &["callers", "MyApp.Accounts.all", ".", "--rebuild"]);
    assert!(!out.contains("recv=MyApp.Repo"), "{}", out);
    assert!(out.contains("MyApp.Web::own recv=MyApp.Accounts"), "{}", out);
    assert!(out.contains("MyApp.Web::go recv=repo"), "{}", out);
}

#[test]
fn elixir_remote_calls_compare_arity() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/remote.ex"),
        r#"defmodule Shop.Tools do
  def fmt(a, b), do: {a, b}
  def zero, do: :z
  def one(a), do: a
end

defmodule Shop.Hid do
  defp secret, do: :s
  def own, do: __MODULE__.secret()
end

defmodule Shop.Remote do
  def private(), do: Shop.Hid.secret()
  def wrong(x), do: Shop.Tools.fmt(x)
  def right(x), do: Shop.Tools.fmt(x, x)
  def piped(x), do: x |> Shop.Tools.fmt(1)
  def captured, do: &Shop.Tools.fmt/2
  def miscaptured, do: &Shop.Tools.fmt/1
  def bare, do: Shop.Tools.fmt
  def bare_piped(x), do: x |> Shop.Tools.fmt
  def bare_zero, do: Shop.Tools.zero
  def bare_one(x), do: x |> Shop.Tools.one
end

defmodule Shop.Delegates do
  defdelegate one(x, y), to: Shop.Tools
  defdelegate fmt(x), to: Shop.Tools
  defdelegate full(x, y), to: Shop.Tools, as: :fmt
  defdelegate defaulted(x, y \\ 1), to: Shop.Tools, as: :fmt
end
"#,
    );
    let callees = |sym: &str| run_in(root, &["callees", sym, ".", "--rebuild"]).0;
    let out = callees("Shop.Remote.wrong");
    assert!(!out.contains("(Exact)"), "{}", out);
    // A remote call, even into its own module, cannot reach a `defp`.
    let out = callees("Shop.Remote.private");
    assert!(!out.contains("(Exact)"), "{}", out);
    let out = callees("Shop.Hid.own");
    assert!(!out.contains("(Exact)"), "{}", out);
    let out = callees("Shop.Remote.right");
    assert!(out.contains("Shop.Tools::fmt  (Exact)"), "{}", out);
    let out = callees("Shop.Remote.piped");
    assert!(out.contains("Shop.Tools::fmt  (Exact)"), "{}", out);
    let out = callees("Shop.Remote.captured");
    assert!(out.contains("Shop.Tools::fmt  (Exact)"), "{}", out);
    let out = callees("Shop.Remote.miscaptured");
    assert!(!out.contains("(Exact)"), "{}", out);
    // Without parentheses a remote call takes no arguments, or the piped one.
    for sym in ["Shop.Remote.bare", "Shop.Remote.bare_piped"] {
        let out = callees(sym);
        assert!(!out.contains("(Exact)"), "{}: {}", sym, out);
    }
    let out = callees("Shop.Remote.bare_zero");
    assert!(out.contains("Shop.Tools::zero  (Exact)"), "{}", out);
    let out = callees("Shop.Remote.bare_one");
    assert!(out.contains("Shop.Tools::one  (Exact)"), "{}", out);
    // A delegate passes its target every argument of its head, defaults too.
    for sym in ["Shop.Delegates.one", "Shop.Delegates.fmt"] {
        let out = callees(sym);
        assert!(!out.contains("(Exact)"), "{}: {}", sym, out);
    }
    for sym in ["Shop.Delegates.full", "Shop.Delegates.defaulted"] {
        let out = callees(sym);
        assert!(out.contains("Shop.Tools::fmt  (Exact)"), "{}: {}", sym, out);
    }
    // An incremental update compares arity too.
    write(
        &root.join("lib/remote.ex"),
        &std::fs::read_to_string(root.join("lib/remote.ex"))
            .unwrap()
            .replace("fmt(a, b)", "fmt(a)"),
    );
    let (out, _) = run_in(root, &["callees", "Shop.Remote.wrong", "."]);
    assert!(out.contains("Shop.Tools::fmt  (Exact)"), "{}", out);
    let (out, _) = run_in(root, &["callees", "Shop.Remote.right", "."]);
    assert!(!out.contains("(Exact)"), "{}", out);
}

#[test]
fn elixir_callbacks_take_no_calls() {
    // A `@callback` declares a function for other modules to define; no
    // call reaches it, at any arity (Ecto.Association: `struct(s, d)` next
    // to `@callback struct(module, field, opts)` calls `Kernel.struct/2`).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/beh.ex"),
        r#"defmodule Shop.Beh do
  @callback struct(module, atom, keyword) :: term
  @callback fetch(term) :: term
  @macrocallback build(term) :: Macro.t()
  @callback get(term) :: term
  def get(k), do: k

  def apply_defaults(s, d), do: struct(s, d)
  def own(k), do: get(k)
end

defmodule Shop.User do
  import Shop.Beh
  def remote(x), do: Shop.Beh.fetch(x)
  def macro(x), do: Shop.Beh.build(x)
  def imported(x), do: fetch(x)
end
"#,
    );
    let callees = |sym: &str| run_in(root, &["callees", sym, ".", "--rebuild"]).0;
    for (sym, name) in [
        ("Shop.Beh.apply_defaults", "struct"),
        ("Shop.User.remote", "fetch"),
        ("Shop.User.macro", "build"),
        ("Shop.User.imported", "fetch"),
    ] {
        let out = callees(sym);
        assert!(!out.contains(&format!("Shop.Beh::{name}  (")), "{}: {}", sym, out);
    }
    // A `def` of the callback's name still takes its calls.
    let out = callees("Shop.Beh.own");
    assert!(out.contains("Shop.Beh::get  (Exact)"), "{}", out);
    // An incremental update that defines the function binds the call.
    write(
        &root.join("lib/beh.ex"),
        &std::fs::read_to_string(root.join("lib/beh.ex"))
            .unwrap()
            .replace("  def get(k), do: k\n", "  def get(k), do: k\n  def fetch(k), do: k\n"),
    );
    let (out, _) = run_in(root, &["callees", "Shop.User.remote", "."]);
    assert!(out.contains("Shop.Beh::fetch  (Exact)"), "{}", out);
}

#[test]
fn elixir_and_rust_types_of_one_name_stay_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/worker.ex"),
        "defmodule Shop.Worker do\n  @behaviour Service\n  def call, do: :ok\nend\n",
    );
    write(
        &root.join("src/lib.rs"),
        "pub trait Service { fn call(&self); }\npub struct RustImpl;\nimpl Service for RustImpl { fn call(&self) {} }\n",
    );
    for cmd in ["callers", "impact", "context"] {
        let (out, _) = run_in(root, &[cmd, "Service", ".", "--rebuild"]);
        assert!(
            out.contains("RustImpl"),
            "{} lists no implementor:\n{}",
            cmd,
            out
        );
        assert!(
            !out.contains("Worker"),
            "{} lists an Elixir module:\n{}",
            cmd,
            out
        );
    }
    let (out, _) = run_in(root, &["callees", "Shop.Worker", "."]);
    assert!(!out.contains("trait Service"), "{}", out);
    assert!(out.contains("[external] Service"), "{}", out);
    let (out, _) = run_in(root, &["callees", "RustImpl", "."]);
    assert!(out.contains("trait Service"), "{}", out);
    // The incremental update keeps them apart too.
    write(
        &root.join("lib/worker.ex"),
        "defmodule Shop.Worker do\n  @behaviour Service\n  def call, do: :ok\n  def stop, do: :ok\nend\n",
    );
    let (out, _) = run_in(root, &["callers", "Service", "."]);
    assert!(!out.contains("Worker"), "{}", out);
}

#[test]
fn elixir_and_ruby_types_of_one_name_keep_their_constructions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/shop.ex"),
        "defmodule Shop do\n  defstruct [:a]\n  def fmt(x), do: x\nend\n\ndefmodule Shop.Cart do\n  def build, do: %Shop{a: 1}\nend\n",
    );
    write(
        &root.join("lib/shop.rb"),
        "class Shop\n  def self.fmt(x)\n    x\n  end\nend\n\ndef go\n  Shop.fmt(1)\nend\n",
    );
    let (out, _) = run_in(root, &["callers", "lib/shop.ex:Shop", ".", "--rebuild"]);
    assert!(out.contains("Shop.Cart::build"), "{}", out);
    assert!(
        !out.contains("shop.rb"),
        "a Ruby call under an Elixir module:\n{}",
        out
    );
    let (out, _) = run_in(root, &["callers", "lib/shop.rb:Shop", "."]);
    assert!(out.contains("lib/shop.rb::go"), "{}", out);
    assert!(
        !out.contains("Shop.Cart"),
        "an Elixir struct under a Ruby class:\n{}",
        out
    );
}

#[test]
fn elixir_struct_impact_counts_each_construction_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule Shop.MixProject do\nend\n");
    write(
        &root.join("lib/pricing.ex"),
        "defmodule Shop.Pricing do\n  defstruct total: 0\nend\n",
    );
    write(
        &root.join("lib/cart.ex"),
        "defmodule Shop.Cart do\n  def build, do: %Shop.Pricing{total: 1}\nend\n",
    );
    write(
        &root.join("test/cart_test.exs"),
        "defmodule Shop.CartTest do\n  use ExUnit.Case\n  def both do\n    Shop.Cart.build()\n    %Shop.Pricing{total: 2}\n  end\nend\n",
    );
    let (out, code) = run_in(
        root,
        &["impact", "Shop.Pricing", ".", "--json", "--rebuild"],
    );
    assert_eq!(code, 0, "{}", out);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let report = &v["impacts"][0];
    let sections = report["sections"].as_array().unwrap();
    let called_by = sections
        .iter()
        .find(|s| s["title"].as_str().unwrap().contains("called by"))
        .unwrap();
    assert_eq!(called_by["total"], 2, "{}", out);
    assert_eq!(report["test_count"], 1, "{}", out);
    // A struct's location is where its module is defined.
    let (out, _) = run_in(root, &["impact", "Shop.Cart.build", ".", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let calls = &v["impacts"][0]["sections"][0]["entries"][0];
    assert_eq!(calls["qn"], "lib/pricing.ex::Shop.Pricing", "{}", out);
    // The file uses the platform separator.
    let file = calls["file"].as_str().unwrap().replace('\\', "/");
    assert_eq!(file, "lib/pricing.ex", "{}", out);
    assert_eq!(calls["line"], 1, "{}", out);
}

#[test]
fn kotlin_callers_finds_intra_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("build.gradle.kts"),
        "plugins { kotlin(\"jvm\") version \"1.9.0\" }\n",
    );
    write(
        &root.join("src/main/kotlin/Greeter.kt"),
        r#"
package smoke
class Greeter {
    fun greet() { println("hi") }
    fun pingTwice() { greet(); greet() }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn scala_callers_finds_intra_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("build.sbt"),
        "name := \"smoke\"\nscalaVersion := \"2.13.10\"\n",
    );
    write(
        &root.join("src/main/scala/Greeter.scala"),
        r#"
package smoke
object Greeter {
  def greet(): Unit = println("hi")
  def pingTwice(): Unit = { greet(); greet() }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn cpp_callers_finds_intra_file_caller() {
    // Single-file C++: pingTwice() calls greet(). The C++ adapter walks
    // call_expression nodes inside the function body; pass A in resolve.rs
    // promotes the bare `greet` to the same-file qn.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.10)\nproject(smoke)\n",
    );
    write(
        &root.join("src/greeter.cpp"),
        r#"
#include <cstdio>
void greet() { printf("hi"); }
void pingTwice() { greet(); greet(); }
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn cpp_callees_lists_construct_and_invocation() {
    // `run()` exercises both call kinds the C++ adapter classifies:
    //   `new Greeter()` → CallKind::Construct, name="Greeter"
    //   `g->greet()`    → CallKind::Call,      name="greet"
    // The construct stays unresolved (the resolver matches against
    // callables, not types), so we pass `--external` to surface
    // unresolved/external edges. JSON output is asserted to make the
    // two edges distinguishable — text output would let "Greeter"
    // match the substring inside `Greeter::greet`, hiding bugs.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.10)\nproject(smoke)\n",
    );
    write(
        &root.join("src/demo.cpp"),
        r#"
class Greeter {
public:
    int greet() { return 42; }
};

int run() {
    Greeter* g = new Greeter();
    int n = g->greet();
    delete g;
    return n;
}
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "run", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({}):\n{}", e, out));
    let matches = v["matches"].as_array().expect("matches array");

    // The `[unresolved] Greeter` target asserts current resolver behaviour:
    // pass A/B/C match against callable declarations only, so a Construct
    // whose name is a *type* never gets resolved to that type's constructors.
    // If the resolver is later taught to map type names → constructors, this
    // assertion will need to switch to the resolved qn.
    let has_construct = matches.iter().any(|m| {
        m["kind"] == "construct" && m["target"].as_str() == Some("[unresolved] Greeter")
    });
    assert!(
        has_construct,
        "expected a construct edge targeting `Greeter`, got:\n{}",
        out
    );

    let has_call = matches.iter().any(|m| {
        m["kind"] == "call" && m["target"].as_str() == Some("src/demo.cpp::Greeter::greet")
    });
    assert!(
        has_call,
        "expected a call edge resolved to `Greeter::greet`, got:\n{}",
        out
    );
}

#[test]
fn go_callers_finds_intra_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("go.mod"), "module smoke\n\ngo 1.21\n");
    write(
        &root.join("greeter.go"),
        r#"
package smoke

import "fmt"

func greet() { fmt.Println("hi") }
func pingTwice() { greet(); greet() }
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn go_method_callers_finds_receiver_method_caller() {
    // Method on a receiver type: pingTwice() calls g.greet(). Selector
    // expression's `field` carries the bare name; pass A maps it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("go.mod"), "module smoke\n\ngo 1.21\n");
    write(
        &root.join("greeter.go"),
        r#"
package smoke

import "fmt"

type Greeter struct{}

func (g *Greeter) greet() { fmt.Println("hi") }
func pingTwice() {
    g := &Greeter{}
    g.greet()
    g.greet()
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn php_callers_finds_intra_file_caller() {
    // Single-file PHP: pingTwice() invokes helper() and $g->greet().
    // Pass A promotes both bare names to same-file qns.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // composer.json marks the dir as a PHP project root (mirrors the C++/Go
    // marker convention used elsewhere in this file).
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Greeter.php"),
        r#"<?php
class Greeter {
    public function greet(): int { return 42; }
}
function helper(): void {}
function pingTwice(): void {
    $g = new Greeter();
    $g->greet();
    helper();
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "helper", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("pingTwice"),
        "expected `pingTwice` in callers, got:\n{}",
        out
    );
}

#[test]
fn php_callees_lists_construct_member_and_scoped() {
    // `run()` exercises three PHP call shapes:
    //   `new Greeter()` → CallKind::Construct, name="Greeter"
    //   `$g->greet()`   → CallKind::Call,      name="greet" (member_call_expression)
    //   `Greeter::greet()` → CallKind::Call,   name="greet" (scoped_call_expression)
    // JSON output is asserted so substring overlap between target qns can't
    // mask a missing edge (see cpp_callees_lists_construct_and_invocation).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Demo.php"),
        r#"<?php
class Greeter {
    public function greet(): int { return 42; }
    public static function greetStatic(): int { return 7; }
}
function run(): int {
    $g = new Greeter();
    $a = $g->greet();
    $b = Greeter::greetStatic();
    return $a + $b;
}
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "run", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({}):\n{}", e, out));
    let matches = v["matches"].as_array().expect("matches array");

    let has_construct = matches.iter().any(|m| {
        m["kind"] == "construct" && m["target"].as_str() == Some("[unresolved] Greeter")
    });
    assert!(
        has_construct,
        "expected a construct edge targeting `Greeter`, got:\n{}",
        out
    );

    let has_member_call = matches.iter().any(|m| {
        m["kind"] == "call"
            && m["target"].as_str() == Some("src/Demo.php::Greeter::greet")
    });
    assert!(
        has_member_call,
        "expected a member call edge resolved to `Greeter::greet`, got:\n{}",
        out
    );

    let has_scoped_call = matches.iter().any(|m| {
        m["kind"] == "call"
            && m["target"].as_str() == Some("src/Demo.php::Greeter::greetStatic")
    });
    assert!(
        has_scoped_call,
        "expected a scoped call edge resolved to `Greeter::greetStatic`, got:\n{}",
        out
    );
}

#[test]
fn ruby_callers_finds_intra_file_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/greeter.rb"),
        r#"
class Greeter
  def greet
    42
  end
  def ping_twice
    g = Greeter.new
    g.greet
    g.greet()
  end
end
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("ping_twice"),
        "expected `ping_twice` in callers, got:\n{}",
        out
    );
}

#[test]
fn ruby_callees_lists_construct_and_method_call() {
    // `run` exercises both Ruby call kinds the adapter classifies:
    //   `Greeter.new` → CallKind::Construct, name="Greeter" (constant receiver
    //                    triggers Construct classification, mirroring `new T()`
    //                    in C++/C#/TS — Ruby has no separate `new` expression)
    //   `g.greet`     → CallKind::Call,      name="greet"
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/demo.rb"),
        r#"
class Greeter
  def greet
    42
  end
end
def run
  g = Greeter.new
  g.greet
end
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "run", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({}):\n{}", e, out));
    let matches = v["matches"].as_array().expect("matches array");

    let has_construct = matches.iter().any(|m| {
        m["kind"] == "construct" && m["target"].as_str() == Some("[unresolved] Greeter")
    });
    assert!(
        has_construct,
        "expected a construct edge targeting `Greeter`, got:\n{}",
        out
    );

    let has_call = matches.iter().any(|m| {
        m["kind"] == "call" && m["target"].as_str() == Some("lib/demo.rb::Greeter::greet")
    });
    assert!(
        has_call,
        "expected a call edge resolved to `Greeter::greet`, got:\n{}",
        out
    );
}

#[test]
fn php_namespaced_call_resolves_as_free_function() {
    // `\App\bar()` and `bar()` are both free-function calls — the namespace
    // prefix is a *qualifier*, not a method receiver. The adapter must drop
    // the namespace so pass B in resolve.rs (which gates single-match
    // promotion on `!has_receiver`) can resolve the bare name.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/App.php"),
        r#"<?php
namespace App;
function bar(): int { return 1; }
function caller(): int {
    return \App\bar() + bar();
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "bar", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("caller"),
        "expected `caller` in callers (qualified+bare both resolve), got:\n{}",
        out
    );
}

#[test]
fn php_dynamic_new_is_skipped() {
    // `new $cls()` names a runtime value, not a callable — emitting a `$cls`
    // Construct edge would create perpetually-unresolved noise. The adapter
    // returns None for variable_name class refs, so only the static
    // `new Greeter()` shows up.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Demo.php"),
        r#"<?php
class Greeter {}
function run(): void {
    $cls = "Greeter";
    $a = new $cls();
    $b = new Greeter();
}
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "run", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({}):\n{}", e, out));
    let matches = v["matches"].as_array().expect("matches array");

    let constructs: Vec<&serde_json::Value> = matches
        .iter()
        .filter(|m| m["kind"] == "construct")
        .collect();
    assert_eq!(
        constructs.len(),
        1,
        "expected exactly one Construct edge (static `new Greeter()`), got {}:\n{}",
        constructs.len(),
        out
    );
    assert_eq!(
        constructs[0]["target"].as_str(),
        Some("[unresolved] Greeter"),
        "expected Construct target=Greeter, got:\n{}",
        out
    );

    let dollar_targets = matches
        .iter()
        .any(|m| m["target"].as_str().is_some_and(|s| s.contains('$')));
    assert!(
        !dollar_targets,
        "no edge should reference a `$variable` class name, got:\n{}",
        out
    );
}

#[test]
fn ruby_class_method_call_resolves() {
    // `Greeter.shout` — class-method invocation via dot syntax. The receiver
    // is the class constant `Greeter`, not an instance. Pass A should still
    // promote `shout` to the same-file qn since the class defines it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/demo.rb"),
        r#"
class Greeter
  def self.shout
    "HEY"
  end
end
def run
  Greeter.shout
end
"#,
    );
    let (out, code) = run_in(root, &["callers", "shout", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("run"),
        "expected `run` to be a caller of `shout`, got:\n{}",
        out
    );
}

#[test]
fn php_dynamic_function_call_is_skipped() {
    // `$func()` calls a runtime value — no static target exists. The adapter
    // should drop the edge entirely rather than emit a `$func` name that
    // can't match any declaration.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Demo.php"),
        r#"<?php
function helper(): int { return 1; }
function run(): int {
    $func = "helper";
    return $func() + helper();
}
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "run", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({}):\n{}", e, out));
    let matches = v["matches"].as_array().expect("matches array");

    // Only the static `helper()` call should appear; `$func()` must not.
    let has_helper = matches
        .iter()
        .any(|m| m["target"].as_str() == Some("src/Demo.php::helper"));
    assert!(has_helper, "expected `helper` callee, got:\n{}", out);

    let has_dollar_target = matches
        .iter()
        .any(|m| m["name"].as_str().is_some_and(|s| s.contains('$'))
            || m["target"].as_str().is_some_and(|s| s.contains('$')));
    assert!(
        !has_dollar_target,
        "no edge should reference a `$variable` callable, got:\n{}",
        out
    );
}

#[test]
fn php_self_static_parent_keywords_drop_receiver() {
    // `self::method()`, `static::method()`, and `parent::method()` use
    // late-binding keywords as the scope. They aren't class names, so the
    // adapter drops them — pass B in resolve.rs then promotes the bare
    // method name against the global symbol table (gated on `!has_receiver`).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Demo.php"),
        r#"<?php
class Base {
    public static function shared(): int { return 1; }
}
class Greeter extends Base {
    public static function inner(): int { return 2; }
    public static function caller(): int {
        return self::inner() + static::inner() + parent::shared();
    }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "shared", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("caller"),
        "expected `parent::shared()` to find caller via dropped receiver, got:\n{}",
        out
    );

    // `self::inner()` and `static::inner()` should both resolve to inner —
    // verify by counting the callees of `caller`.
    let (out, code) = run_in(
        root,
        &["callees", "caller", ".", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    let matches = v["matches"].as_array().unwrap();
    let inner_calls = matches
        .iter()
        .filter(|m| m["target"].as_str() == Some("src/Demo.php::Greeter::inner"))
        .count();
    assert_eq!(
        inner_calls, 2,
        "expected both `self::inner()` and `static::inner()` to resolve, got:\n{}",
        out
    );
}

#[test]
fn php_anonymous_class_emits_no_construct_edge() {
    // `new class { ... }` has no name — there's nothing to record as a target.
    // The adapter filters `anonymous_class` from the children search, so
    // such expressions emit no Construct edge.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Demo.php"),
        r#"<?php
function run(): object {
    return new class {
        public function ping(): int { return 1; }
    };
}
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "run", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    let matches = v["matches"].as_array().unwrap();
    let has_construct = matches.iter().any(|m| m["kind"] == "construct");
    assert!(
        !has_construct,
        "anonymous class should not emit a Construct edge, got:\n{}",
        out
    );
}

#[test]
fn ruby_self_receiver_resolves_via_pass_b() {
    // `self.greet` uses the `self` keyword as receiver. The resolver in
    // resolve.rs:142 explicitly treats `self` as a non-receiver, letting
    // pass B promote the bare name against the global symbol table.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/demo.rb"),
        r#"
class Greeter
  def greet
    42
  end
  def via_self
    self.greet
  end
end
"#,
    );
    let (out, code) = run_in(root, &["callers", "greet", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("via_self"),
        "expected `via_self` (uses self.greet) in callers, got:\n{}",
        out
    );
}

/// A Ruby project where `Shop::Cart.total` calls `File.join` and
/// `Pricing.price`, and two modules named `Pricing` define `price`.
fn ruby_scope_receiver_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/shop/cart.rb"),
        r#"
module Shop
  class Cart
    def self.total(items)
      File.join("a", "b")
      Pricing.price(items)
    end

    def join(other); end
  end
end
"#,
    );
    write(
        &root.join("lib/shop/pricing.rb"),
        "module Shop\n  module Pricing\n    def self.price(items); end\n  end\nend\n",
    );
    write(
        &root.join("lib/other/pricing.rb"),
        "module Other\n  module Pricing\n    def self.price(items); end\n  end\nend\n",
    );
    tmp
}

fn callee_targets(root: &std::path::Path, symbol: &str, rebuild: bool) -> Vec<(String, String)> {
    let mut args = vec!["callees", symbol, ".", "--json", "--compact"];
    if rebuild {
        args.push("--rebuild");
    }
    let (out, code) = run_in(root, &args);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let mut targets: Vec<(String, String)> = doc["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|m| {
            (
                m["target"].as_str().unwrap_or_default().to_string(),
                m["confidence"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    targets.sort();
    targets
}

#[test]
fn ruby_constant_receiver_binds_only_in_that_scope() {
    // `File.join` names Ruby's `File`, which the project does not define, so
    // it is external — not the project's `Cart#join`, which the name-based
    // passes used to pick. `Pricing` is looked up from the caller's scope
    // outward, so it is `Shop::Pricing`, not `Other::Pricing`.
    let tmp = ruby_scope_receiver_project();
    let root = tmp.path();
    assert_eq!(
        callee_targets(root, "Shop.Cart.total", true),
        vec![
            ("[external] File.join".to_string(), "Exact".to_string()),
            (
                "lib/shop/pricing.rb::Shop::Pricing::price".to_string(),
                "Exact".to_string()
            ),
        ]
    );
    let (out, code) = run_in(
        root,
        &["callers", "Shop.Cart.join", ".", "--json", "--compact"],
    );
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(
        doc["total"], 0,
        "`File.join` is not a caller of `Cart#join`: {out}"
    );
}

#[test]
fn ruby_absolute_constant_receiver_skips_nearer_scopes() {
    // `::Pricing` names the top-level `Pricing`, even inside `Shop`, where
    // a plain `Pricing` would be `Shop::Pricing`.
    let tmp = ruby_scope_receiver_project();
    let root = tmp.path();
    write(
        &root.join("lib/pricing.rb"),
        "module Pricing\n  def self.price(items); end\nend\n",
    );
    write(
        &root.join("lib/shop/checkout.rb"),
        "module Shop\n  class Checkout\n    def self.run(items)\n      ::Pricing.price(items)\n    end\n  end\nend\n",
    );
    assert_eq!(
        callee_targets(root, "Shop.Checkout.run", true),
        vec![(
            "lib/pricing.rb::Pricing::price".to_string(),
            "Exact".to_string()
        )]
    );
}

#[test]
fn ruby_constant_receiver_found_through_an_include_is_not_external() {
    // Ruby also finds a constant through ancestors: after `include Jekyll`,
    // `Utils` is `Jekyll::Utils`. The lexical lookup misses it, but a scope
    // of that name exists, so the calls stay unresolved instead of wrong
    // externals, also for a method that `Utils` inherits.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/jekyll/utils.rb"),
        r#"
module Jekyll
  class Base
    def self.slugify(s); end
  end

  class Utils < Base
    def self.deep_merge_hashes(a, b); end
  end
end
"#,
    );
    write(
        &root.join("test/helper.rb"),
        r#"
include Jekyll
def merged
  Utils.deep_merge_hashes({}, {})
  Utils.slugify("x")
end
"#,
    );
    assert_eq!(
        callee_targets(root, "merged", true),
        vec![
            (
                "[unresolved] deep_merge_hashes".to_string(),
                "Ambiguous".to_string()
            ),
            ("[unresolved] slugify".to_string(), "Ambiguous".to_string()),
        ]
    );
}

#[test]
fn ruby_module_with_no_extracted_methods_is_a_scope() {
    // `class << self` methods are not extracted, so `Config` encloses no
    // callable. It is still a project scope, so the call stays unresolved
    // instead of a wrong external.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/config.rb"),
        r#"
module Config
  class << self
    def load; end
  end
end
def boot
  Config.load
end
"#,
    );
    assert_eq!(
        callee_targets(root, "boot", true),
        vec![("[unresolved] load".to_string(), "Ambiguous".to_string())]
    );
}

#[test]
fn ruby_constant_path_split_across_lines_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/foo.rb"),
        "module Foo\n  class Bar\n    def self.x(n); end\n  end\nend\ndef go\n  Foo::\n    Bar.x(1)\nend\n",
    );
    assert_eq!(
        callee_targets(root, "go", true),
        vec![("lib/foo.rb::Foo::Bar::x".to_string(), "Exact".to_string())]
    );
}

#[test]
fn impact_shows_the_callers_file_for_an_external_callee() {
    // `[external] Missing::Thing.call` names no file; the text must not
    // read `Missing` as one.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/go.rb"),
        "def go\n  Missing::Thing.call\nend\n",
    );
    let (out, code) = run_in(root, &["impact", "go", ".", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let row = out
        .lines()
        .find(|l| l.contains("Missing::Thing.call"))
        .unwrap_or_else(|| panic!("no external row: {out}"));
    assert!(row.contains("(lib/go.rb)"), "{row}");
}

#[test]
fn scope_receiver_binds_only_in_the_callers_language() {
    // A Ruby `User` and a TypeScript `User` are different scopes: the Ruby
    // call stays unresolved instead of binding to the TypeScript method, in
    // a cold build and after an update that adds the TypeScript file.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(&root.join("package.json"), "{\"name\": \"app\"}\n");
    write(&root.join("app/models/user.rb"), "class User\nend\n");
    write(
        &root.join("app/controllers/users_controller.rb"),
        "class UsersController\n  def show\n    User.find(1)\n  end\nend\n",
    );
    let expected = vec![("[unresolved] find".to_string(), "Ambiguous".to_string())];
    assert_eq!(callee_targets(root, "UsersController.show", true), expected);

    write(
        &root.join("app/javascript/models/user.ts"),
        "export class User {\n  static find(id: number) {}\n}\n",
    );
    let incremental = callee_targets(root, "UsersController.show", false);
    let cold = callee_targets(root, "UsersController.show", true);
    assert_eq!(
        incremental, cold,
        "a partial update and a cold build diverged"
    );
    assert_eq!(cold, expected);
}

#[test]
fn ruby_all_caps_constant_receiver_is_not_a_scope() {
    // An all-caps constant may hold a value (`DEFAULT_PARSER = Parser.new`),
    // so its calls keep the name-based resolution of any other receiver.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/demo.rb"),
        r#"
class Parser
  def parse(text); end
end
DEFAULT_PARSER = Parser.new
def run
  DEFAULT_PARSER.parse("x")
end
"#,
    );
    let targets = callee_targets(root, "run", true);
    assert!(
        targets
            .iter()
            .any(|(t, _)| t == "lib/demo.rb::Parser::parse"),
        "expected `DEFAULT_PARSER.parse` to reach `Parser#parse`, got {targets:?}"
    );
}

#[test]
fn incremental_scope_receiver_matches_a_cold_build() {
    // The scope a call names can appear in a later delta without the caller's
    // file changing: the external edge must then resolve as a cold build
    // resolves it.
    let tmp = ruby_scope_receiver_project();
    let root = tmp.path();
    std::fs::remove_file(root.join("lib/shop/pricing.rb")).unwrap();
    std::fs::remove_file(root.join("lib/other/pricing.rb")).unwrap();
    let before = callee_targets(root, "Shop.Cart.total", true);
    assert!(
        before.contains(&("[external] Pricing.price".to_string(), "Exact".to_string())),
        "premise: the project defines no `Pricing` yet: {before:?}"
    );

    write(
        &root.join("lib/shop/pricing.rb"),
        "module Shop\n  module Pricing\n    def self.price(items); end\n  end\nend\n",
    );
    let incremental = callee_targets(root, "Shop.Cart.total", false);
    let cold = callee_targets(root, "Shop.Cart.total", true);
    assert_eq!(
        incremental, cold,
        "a partial update and a cold build diverged"
    );
    assert!(
        cold.contains(&(
            "lib/shop/pricing.rb::Shop::Pricing::price".to_string(),
            "Exact".to_string()
        )),
        "{cold:?}"
    );
}

#[test]
fn incremental_nearer_scope_matches_a_cold_build() {
    // A resolved edge is retried too: a delta that defines a scope nearer to
    // the caller changes what the receiver names.
    let tmp = ruby_scope_receiver_project();
    let root = tmp.path();
    std::fs::remove_file(root.join("lib/shop/pricing.rb")).unwrap();
    write(
        &root.join("lib/pricing.rb"),
        "module Pricing\n  def self.price(items); end\nend\n",
    );
    let before = callee_targets(root, "Shop.Cart.total", true);
    assert!(
        before.contains(&(
            "lib/pricing.rb::Pricing::price".to_string(),
            "Exact".to_string()
        )),
        "premise: only the top-level `Pricing` is in scope: {before:?}"
    );

    write(
        &root.join("lib/shop/pricing.rb"),
        "module Shop\n  module Pricing\n    def self.price(items); end\n  end\nend\n",
    );
    let incremental = callee_targets(root, "Shop.Cart.total", false);
    let cold = callee_targets(root, "Shop.Cart.total", true);
    assert_eq!(
        incremental, cold,
        "a partial update and a cold build diverged"
    );
    assert!(
        cold.contains(&(
            "lib/shop/pricing.rb::Shop::Pricing::price".to_string(),
            "Exact".to_string()
        )),
        "{cold:?}"
    );
}

#[test]
fn incremental_removed_scope_matches_a_cold_build() {
    // Deleting the file that declares the scope leaves the caller's file
    // unchanged; its edge must still turn external, as a cold build does.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/config.rb"),
        "module Config\n  class << self\n    def load; end\n  end\nend\n",
    );
    write(&root.join("lib/boot.rb"), "def boot\n  Config.load\nend\n");
    let before = callee_targets(root, "boot", true);
    assert_eq!(
        before,
        vec![("[unresolved] load".to_string(), "Ambiguous".to_string())],
        "premise: `Config` is a project scope"
    );

    std::fs::remove_file(root.join("lib/config.rb")).unwrap();
    let incremental = callee_targets(root, "boot", false);
    let cold = callee_targets(root, "boot", true);
    assert_eq!(
        incremental, cold,
        "a partial update and a cold build diverged"
    );
    assert_eq!(
        cold,
        vec![("[external] Config.load".to_string(), "Exact".to_string())]
    );
}

#[test]
fn incremental_added_module_scope_matches_a_cold_build() {
    // An update that adds a module with no extracted method still declares
    // its scope, so the external edge must turn unresolved, as a cold build
    // makes it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(&root.join("lib/boot.rb"), "def boot\n  Config.load\nend\n");
    let before = callee_targets(root, "boot", true);
    assert_eq!(
        before,
        vec![("[external] Config.load".to_string(), "Exact".to_string())],
        "premise: the project defines no `Config` yet"
    );

    write(
        &root.join("lib/config.rb"),
        "module Config\n  class << self\n    def load; end\n  end\nend\n",
    );
    let incremental = callee_targets(root, "boot", false);
    let cold = callee_targets(root, "boot", true);
    assert_eq!(
        incremental, cold,
        "a partial update and a cold build diverged"
    );
    assert_eq!(
        cold,
        vec![("[unresolved] load".to_string(), "Ambiguous".to_string())]
    );
}

#[test]
fn ruby_scope_reopened_in_two_files_goes_to_pass_c() {
    // `Shop::Pricing` defines `price` in two files, so the scope rule hands
    // both to pass C. Removing one leaves a single callable, which a
    // partial update must bind as a cold build does.
    let tmp = ruby_scope_receiver_project();
    let root = tmp.path();
    write(
        &root.join("lib/shop/pricing_ext.rb"),
        "module Shop\n  module Pricing\n    def self.price(items); end\n  end\nend\n",
    );
    let before = callee_targets(root, "Shop.Cart.total", true);
    assert!(
        !before
            .iter()
            .any(|(t, c)| t.contains("Pricing::price") && c == "Exact"),
        "two candidates are not an exact binding: {before:?}"
    );

    std::fs::remove_file(root.join("lib/shop/pricing_ext.rb")).unwrap();
    let incremental = callee_targets(root, "Shop.Cart.total", false);
    let cold = callee_targets(root, "Shop.Cart.total", true);
    assert_eq!(
        incremental, cold,
        "a partial update and a cold build diverged"
    );
    assert!(
        cold.contains(&(
            "lib/shop/pricing.rb::Shop::Pricing::price".to_string(),
            "Exact".to_string()
        )),
        "{cold:?}"
    );
}

#[test]
fn ruby_block_calls_attributed_to_enclosing_method() {
    // Ruby blocks (`do ... end`, `{ ... }`) are closures, not separate
    // methods — calls inside them belong to the enclosing method, not to
    // some anonymous block scope. The adapter walker deliberately does NOT
    // bail on `block`/`do_block`, so `each` and `inner` here are both
    // attributed to `outer`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/demo.rb"),
        r#"
class Greeter
  def inner
    42
  end
  def outer
    [1, 2, 3].each do |_x|
      inner()
    end
  end
end
"#,
    );
    let (out, code) = run_in(root, &["callers", "inner", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("outer"),
        "expected `outer` (calls `inner()` inside a do_block) to be a caller, got:\n{}",
        out
    );
}

#[test]
fn php_uppercase_self_normalizes_to_lowercase_keyword() {
    // tree-sitter-php's `keyword()` helper aliases case-insensitive matches
    // to the lowercase canonical form, so `SELF::method` parses with the
    // scope text already normalized to `self`. Our adapter relies on that:
    // the late-binding check matches lowercase only. Pin this assumption
    // with three `shared` candidates so the test fails loudly if a future
    // grammar version stops normalizing — the case-mismatched receiver
    // would then escape pass A/B and force `Ambiguous`/`Inferred` instead
    // of `Exact`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("composer.json"), "{}\n");
    write(
        &root.join("src/Demo.php"),
        r#"<?php
class A { public static function shared(): int { return 1; } }
class B { public static function shared(): int { return 2; } }
class Greeter {
    public static function shared(): int { return 0; }
    public static function caller(): int {
        return SELF::shared() + Self::shared() + STATIC::shared();
    }
}
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "caller", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    let matches = v["matches"].as_array().unwrap();
    let exact_self_resolutions = matches
        .iter()
        .filter(|m| {
            m["target"].as_str() == Some("src/Demo.php::Greeter::shared")
                && m["confidence"].as_str() == Some("Exact")
        })
        .count();
    assert_eq!(
        exact_self_resolutions, 3,
        "all three case variants should resolve Exact to `Greeter::shared`, got:\n{}",
        out
    );
}

#[test]
fn ruby_paren_less_calls_with_args_are_captured() {
    // tree-sitter-ruby 0.23.1 unifies `puts "x"`, `require "y"`, and
    // `log_event :z` into the `call` node kind (no separate `command`
    // grammar). Lock this in: if a future grammar split them out, the
    // adapter would silently drop these and this test fails.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Gemfile"), "source 'https://rubygems.org'\n");
    write(
        &root.join("lib/demo.rb"),
        r#"
def caller_method
  puts "literal arg"
  require "some_lib"
  log_event :start
end
"#,
    );
    let (out, code) = run_in(
        root,
        &["callees", "caller_method", ".", "--rebuild", "--external", "--json", "--compact"],
    );
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("json");
    let matches = v["matches"].as_array().unwrap();
    let names: Vec<&str> = matches
        .iter()
        .filter_map(|m| m["target"].as_str())
        .collect();
    for needle in ["puts", "require", "log_event"] {
        assert!(
            names.iter().any(|t| t.contains(needle)),
            "expected paren-less `{}` to be captured as a callee, got:\n{}",
            needle,
            out
        );
    }
}

#[test]
fn cpp_out_of_line_method_signature_keeps_scope() {
    // `int Greeter::greet()` defined out-of-line. The adapter splits the
    // bare name (`greet`, used for symbol lookup) from the qualified name
    // (`Greeter::greet`, used for the rendered signature). The map output
    // must show the scope-qualified form — losing it produced the misleading
    // `int greet()` signature for years before the qname helper landed.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.10)\nproject(smoke)\n",
    );
    write(
        &root.join("src/demo.cpp"),
        r#"
class Greeter {
public:
    int greet();
};
int Greeter::greet() { return 42; }
"#,
    );
    let out = Command::new(bin())
        .args(["map", "src/demo.cpp"])
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Greeter::greet"),
        "expected scope-qualified `Greeter::greet` in signature, got:\n{}",
        stdout
    );
}

#[test]
fn callers_unknown_symbol_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub fn a() {}\n");
    let out = Command::new(bin())
        .args(["callers", "nonexistent_sym_xyz", "."])
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2), "expected exit 2 for unknown symbol");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no symbol matches"),
        "expected hint, got stderr:\n{}",
        stderr
    );
}

// ---------- Per-file invalidation tests ----------
//
// Each of these builds the cache by running a query, mutates a single file,
// re-runs the query (without `--rebuild`), and asserts the in-memory graph
// reflects the change without the user opting into a rebuild. The cache file
// is written to `.ast-bro/deps/graph.bin` under each fixture root.

fn cache_mtime(root: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(root.join(".ast-bro/deps/graph.bin"))
        .ok()?
        .modified()
        .ok()
}

// All per-file invalidation tests below mutate file *content* between the
// prime and re-query steps, with the new size differing from the old.
// Delta detection in `src/search/cache.rs` triggers on mismatched
// `(mtime, size)` and (when only mtime matched) on a content-hash mismatch
// — so a size-bumping edit always fires the delta path regardless of
// filesystem mtime resolution. No explicit sleep needed.

#[test]
fn deps_partial_invalidation_picks_up_new_import() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod a; pub mod b;\n");
    write(&root.join("src/a.rs"), "pub fn ping() {}\n");
    write(&root.join("src/b.rs"), "// no imports yet\n");

    // Prime the cache.
    let (out, code) = run_in(root, &["deps", "src/b.rs"]);
    assert_eq!(code, 0, "first deps call failed: {out}");
    let cache_before = cache_mtime(root).expect("cache should exist after first call");

    // Edit b.rs to import a. Bump mtime so delta detection fires reliably.
    write(&root.join("src/b.rs"), "use crate::a;\npub fn pong() { a::ping(); }\n");

    // Same query without --rebuild should pick up the new edge.
    let (out2, code2) = run_in(root, &["deps", "src/b.rs"]);
    assert_eq!(code2, 0, "second deps call failed: {out2}");
    assert!(
        out2.contains("a.rs"),
        "expected new edge to a.rs after partial invalidation, got:\n{out2}"
    );
    let cache_after = cache_mtime(root).expect("cache should still exist");
    assert!(
        cache_after >= cache_before,
        "cache should have been re-saved after delta",
    );
}

#[test]
fn deps_partial_invalidation_drops_removed_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod a; pub mod gone;\n");
    write(&root.join("src/a.rs"), "use crate::gone;\npub fn run() { gone::say(); }\n");
    write(&root.join("src/gone.rs"), "pub fn say() {}\n");

    // Prime + verify the edge exists.
    let (out, _) = run_in(root, &["deps", "src/a.rs"]);
    assert!(out.contains("gone.rs"), "baseline missing gone.rs: {out}");

    // Remove gone.rs and the lib mod declaration so it's truly gone from the index.
    std::fs::remove_file(root.join("src/gone.rs")).unwrap();
    write(&root.join("src/lib.rs"), "pub mod a;\n");

    // Re-query reverse-deps on a.rs — the partial update should have dropped
    // gone.rs entirely; asking reverse-deps for it should error out.
    let (_, code) = run_in(root, &["reverse-deps", "src/gone.rs"]);
    assert_eq!(code, 2, "removed file should not be part of dep graph anymore");
}

#[test]
fn calls_partial_invalidation_demotes_stale_target() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod a; pub mod b;\n");
    write(&root.join("src/a.rs"), "pub fn helper() {}\n");
    write(
        &root.join("src/b.rs"),
        "use crate::a::helper;\npub fn caller() { helper(); }\n",
    );

    // Prime the calls graph.
    let (out, code) = run_in(root, &["callers", "helper", "."]);
    assert_eq!(code, 0, "first callers failed: {out}");
    assert!(out.contains("caller"), "baseline missing caller: {out}");

    // Rename helper -> renamed in a.rs; b.rs's edge to helper now points to
    // a qn that doesn't exist anymore. The partial path demotes it to Bare.
    write(&root.join("src/a.rs"), "pub fn renamed() {}\n");

    // After invalidation, `helper` no longer matches any callable — query
    // for it should now fail (the qn is gone from symbol_table).
    let (_, code2) = run_in(root, &["callers", "helper", "."]);
    assert_eq!(
        code2, 2,
        "helper should be unknown after rename; got exit {code2}"
    );

    // The renamed function should be discoverable.
    let (out3, code3) = run_in(root, &["callers", "renamed", "."]);
    assert_eq!(code3, 0, "renamed lookup failed: {out3}");
}

#[test]
fn calls_partial_invalidation_picks_up_new_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod a;\n");
    write(&root.join("src/a.rs"), "pub fn helper() {}\npub fn first() { helper(); }\n");

    // Prime — `helper` has one caller.
    let (out, _) = run_in(root, &["callers", "helper", "."]);
    assert!(out.contains("first"), "baseline missing first: {out}");
    assert!(!out.contains("second"), "second should not exist yet: {out}");

    // Add a second caller in the same file.
    write(
        &root.join("src/a.rs"),
        "pub fn helper() {}\npub fn first() { helper(); }\npub fn second() { helper(); }\n",
    );

    let (out2, code2) = run_in(root, &["callers", "helper", "."]);
    assert_eq!(code2, 0, "second callers failed: {out2}");
    assert!(
        out2.contains("first") && out2.contains("second"),
        "expected both first and second after partial invalidation, got:\n{out2}"
    );
}

#[test]
fn rust_callers_on_struct_finds_struct_literal_construction() {
    // `Foo { .. }` literal syntax goes through `_call_site_from_struct`
    // (struct_expression), unlike the receiver-based `Foo.method()` path
    // covered above.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"
pub struct Config { pub debug: bool }

pub fn load_config() -> Config {
    Config { debug: false }
}
"#,
    );
    let (out, code) = run_in(root, &["callers", "Config", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("construction(s)"),
        "expected constructions group, got:\n{}",
        out
    );
    assert!(
        out.contains("load_config"),
        "expected `load_config` (constructs Config via struct literal), got:\n{}",
        out
    );
}

#[test]
fn callers_tests_flag_reaches_tests_through_production_intermediate() {
    // tests/it_test.rs::t → wrapper (production) → target. With `--tests`
    // the depth-1 hop fails the filter, but the traversal must still pass
    // through it so the depth-2 test caller is found: the predicate gates
    // output, not reachability.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn target() {}\npub fn wrapper() { target(); }\n",
    );
    write(
        &root.join("tests/it_test.rs"),
        "use x::wrapper;\n#[test]\nfn t() { wrapper(); }\n",
    );
    let (out, code) = run_in(
        root,
        &["callers", "target", ".", "--depth", "2", "--tests", "--rebuild"],
    );
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("it_test.rs") && out.contains("depth=2"),
        "expected the depth-2 test caller reached through a non-test intermediate, got:\n{}",
        out
    );
    assert!(
        !out.contains("wrapper ") && !out.contains("src/lib.rs:"),
        "wrapper (non-test) must not be reported under --tests, got:\n{}",
        out
    );
}

// ---------------------------------------------------------------------------
// Issue #31 — a same-name method in the calling file must not shadow an
// explicit receiver, and `callers` must surface call sites the resolver
// could not attribute instead of silently dropping them.
// ---------------------------------------------------------------------------

/// vlsi's minimal reproduction: `Caller` declares its own `getCtx()` while
/// `viaChain()` calls `connection.getCtx().getPrefs().forDate()`. The head
/// of the chain has an explicit receiver (`connection`), so pass A must not
/// bind it to the local `getCtx` — and the rest of the chain must keep
/// resolving.
fn issue31_fixture(root: &std::path::Path) {
    write(
        &root.join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>x</groupId><artifactId>x</artifactId><version>0.0.0</version></project>\n",
    );
    write(
        &root.join("src/main/java/com/example/api/Prefs.java"),
        "package com.example.api;\npublic class Prefs {\n  public boolean forDate() { return true; }\n}\n",
    );
    write(
        &root.join("src/main/java/com/example/api/Ctx.java"),
        "package com.example.api;\npublic class Ctx {\n  private final Prefs prefs = new Prefs();\n  public Prefs getPrefs() { return prefs; }\n}\n",
    );
    write(
        &root.join("src/main/java/com/example/api/Conn.java"),
        "package com.example.api;\npublic class Conn {\n  private final Ctx ctx = new Ctx();\n  public Ctx getCtx() { return ctx; }\n}\n",
    );
    write(
        &root.join("src/main/java/com/example/impl/Caller.java"),
        r#"package com.example.impl;

import com.example.api.Conn;
import com.example.api.Ctx;

public class Caller {
  private final Conn connection = new Conn();

  protected Ctx getCtx() {
    return connection.getCtx();
  }

  public boolean viaChain() {
    return connection.getCtx().getPrefs().forDate();
  }
}
"#,
    );
}

#[test]
fn java_local_homonym_does_not_shadow_explicit_receiver() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    issue31_fixture(root);

    // The chain head `connection.getCtx()` must not be claimed by the local
    // `Caller::getCtx` with Exact confidence, and the mid-chain `getPrefs`
    // must keep resolving to `Ctx::getPrefs`.
    // Asserted over JSON, not rendered text: a text `contains` check for
    // "Caller::getCtx  (Exact)" passes the moment the renderer changes its
    // column spacing, which would hide the very regression this pins.
    let (out, code) = run_in(root, &["callees", "viaChain", ".", "--rebuild", "--json", "--compact"]);
    assert_eq!(code, 0, "callees exited non-zero: {}", out);
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let matches = doc["matches"].as_array().expect("matches array");
    assert!(
        !matches.iter().any(|m| {
            m["target"].as_str().unwrap_or("").ends_with("Caller::getCtx")
                && m["confidence"] == "Exact"
        }),
        "local homonym must not claim the explicit-receiver call as Exact, got:\n{}",
        out
    );
    assert!(
        matches
            .iter()
            .any(|m| m["target"].as_str().unwrap_or("").contains("Ctx::getPrefs")),
        "mid-chain link should still resolve, got:\n{}",
        out
    );

    // And the mid-chain caller relationship is visible from the other side.
    let (out, code) = run_in(root, &["callers", "getPrefs", "."]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("viaChain"),
        "expected viaChain as a caller of getPrefs, got:\n{}",
        out
    );
}

#[test]
fn callers_surfaces_unattributed_call_sites() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    issue31_fixture(root);

    // `forDate` is called at the end of a chain whose receiver type is not
    // statically known to the resolver; the edge stays unattributed. It must
    // still show up in `callers` output instead of reading as "0 callers".
    let (out, code) = run_in(root, &["callers", "forDate", ".", "--rebuild"]);
    assert_eq!(code, 0, "callers exited non-zero: {}", out);
    assert!(
        out.contains("unresolved call site(s)") && out.contains("viaChain"),
        "expected the unattributed call site in viaChain to be surfaced, got:\n{}",
        out
    );

    // JSON carries the same information under `unattributed`.
    let (out, code) = run_in(root, &["callers", "forDate", ".", "--json", "--compact"]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let unattributed = doc["unattributed"].as_array().expect("unattributed array");
    assert_eq!(unattributed.len(), 1, "one unattributed site, got: {}", out);
    assert!(
        unattributed[0]["source"].as_str().unwrap_or("").contains("viaChain"),
        "unattributed source should be viaChain, got: {}",
        out
    );
    assert_eq!(doc["unattributed_hidden"], 0, "{}", out);

    // With --hide-ambiguous the sites leave the list but the count stays in
    // the body — JSON consumers (MCP especially) have no stderr to read the
    // note from.
    let (out, code) = run_in(
        root,
        &["callers", "forDate", ".", "--hide-ambiguous", "--json", "--compact"],
    );
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["unattributed"].as_array().unwrap().len(), 0, "{}", out);
    assert_eq!(
        doc["unattributed_hidden"], 1,
        "hidden count must survive in the JSON body: {}",
        out
    );
}

// ---------------------------------------------------------------------------
// Issue #32 — a capped result must be distinguishable from a complete one.
// ---------------------------------------------------------------------------

#[test]
fn callers_limit_reports_true_total() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn target() {}\npub fn a() { target(); }\npub fn b() { target(); }\npub fn c() { target(); }\n",
    );

    let (out, code) = run_in(root, &["callers", "target", ".", "--limit", "1", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("# 3 caller(s)") && out.contains("showing 1") && out.contains("--limit"),
        "header must carry the true total and the flag that lifts the cap:\n{out}"
    );

    let (out, code) = run_in(
        root,
        &["callers", "target", ".", "--limit", "1", "--json", "--compact"],
    );
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["total"], 3, "json total must be pre-limit: {out}");
    assert_eq!(doc["truncated"], true);
    assert_eq!(doc["matches"].as_array().unwrap().len(), 1);
}

#[test]
fn callers_depth_cutoff_reports_frontier() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn target() {}\npub fn mid() { target(); }\npub fn top() { mid(); }\n",
    );
    // depth 1 stops at `mid`, which still has an incoming edge from `top`:
    // the JSON must say the walk stopped, not the graph.
    let (out, code) = run_in(
        root,
        &["callers", "target", ".", "--depth", "1", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["frontier_truncated"], true, "{out}");

    // depth 2 exhausts the graph — the flag must clear.
    let (out, _) = run_in(
        root,
        &["callers", "target", ".", "--depth", "2", "--json", "--compact"],
    );
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["frontier_truncated"], false, "{out}");
}

#[test]
fn callees_limit_is_available_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn x() {}\npub fn y() {}\npub fn z() {}\npub fn top() { x(); y(); z(); }\n",
    );
    let (out, code) = run_in(
        root,
        &["callees", "top", ".", "--depth", "2", "--limit", "1", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["total"], 3, "{out}");
    assert_eq!(doc["truncated"], true);
}

#[test]
fn callers_limit_bounds_the_unattributed_section_too() {
    // The unattributed section has its own UNATTRIBUTED_SAMPLE (25) cap,
    // which --limit can only *lower*: `min(--limit, 25)`. A capped query
    // must not be followed by an unbounded list, but raising --limit past 25
    // doesn't widen this section either (review finding on issue #31/#32).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    // Three call sites in lib.rs whose receivers can't be typed; the
    // homonym in other.rs is in lib.rs's dep closure, so pass C keeps
    // both candidates and every site stays honestly Ambiguous —
    // unattributed from T.target's point of view.
    write(
        &root.join("src/lib.rs"),
        "pub mod other;\npub struct T;\nimpl T { pub fn target(&self) {} }\n\
         pub fn direct1(t: &T) { t.target(); }\n\
         pub fn direct2(t: &T) { t.target(); }\n\
         pub fn direct3(t: &T) { t.target(); }\n",
    );
    write(
        &root.join("src/other.rs"),
        "pub struct U;\nimpl U { pub fn target(&self) {} }\n",
    );

    let (out, code) = run_in(root, &["callers", "T.target", ".", "--limit", "1", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let shown = doc["unattributed"].as_array().unwrap().len();
    let total = doc["unattributed_total"].as_u64().unwrap() as usize;
    // The fixture exists to exercise the budget: if the resolver ever
    // starts attributing these sites, this must fail loudly, not pass
    // vacuously with an empty section.
    assert!(
        total >= 2,
        "fixture must yield enough unattributed sites to exercise truncation: {out}"
    );
    assert!(
        shown <= 1,
        "unattributed section must respect the remaining --limit budget: {out}"
    );
    assert_eq!(doc["unattributed_truncated"], true, "{out}");
}

#[test]
fn parameter_named_parent_is_not_self_like() {
    // `parent`/`static` are PHP scope keywords that the PHP adapter already
    // strips before the resolver sees them; as bare receiver strings they
    // are ordinary variable names and must NOT get same-file Exact binding.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pyproject.toml"), "[project]\nname=\"x\"\nversion=\"0\"\n");
    write(
        &root.join("tree.py"),
        r#"class Node:
    def add_child(self, c):
        pass

    def attach(self, parent):
        parent.add_child(self)

    def attach_static(self, static):
        static.add_child(self)

class Other:
    def add_child(self, c):
        pass
"#,
    );
    // Pin that both call sites are actually reported before checking their
    // labels — a resolver that dropped them entirely would also contain no
    // "(Exact)", passing the label check vacuously.
    let (out, code) = run_in(root, &["callers", "Node.add_child", ".", "--rebuild", "--json", "--compact"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(
        doc["unattributed_total"], 2,
        "both keyword-named-receiver sites must be reported: {out}"
    );
    for site in doc["unattributed"].as_array().unwrap() {
        assert_ne!(
            site["confidence"], "Exact",
            "receivers named `parent`/`static` must not bind Exact: {out}"
        );
    }

    let (out, code) = run_in(root, &["callers", "Node.add_child", "."]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !out.contains("(Exact)"),
        "receivers named `parent`/`static` must not bind Exact to a same-file homonym:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// PR #38 review — frontier reporting on the callees one-hop path, and the
// --limit budget applied to type-caller groups.
// ---------------------------------------------------------------------------

#[test]
fn callees_depth1_reports_frontier_in_json() {
    // a → b → c: at the default depth 1, `callees a` shows only b, and the
    // JSON flag must say the walk stopped with edges left (b calls c).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn c() {}\npub fn b() { c(); }\npub fn a() { b(); }\n",
    );
    let (out, code) = run_in(root, &["callees", "a", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(
        doc["frontier_truncated"], true,
        "depth-1 one-hop path must still report the frontier: {out}"
    );

    // A leaf callee (c calls nothing): flag stays false.
    let (out, code) = run_in(root, &["callees", "b", ".", "--json", "--compact"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["frontier_truncated"], false, "{out}");
}

#[test]
fn hide_external_frontier_counts_only_visible_continuations() {
    // `frontier_truncated` is the promise that raising --depth shows more.
    // Under --hide-external an external continuation is not more to see, so
    // counting one makes the promise false — on both the depth-1 fast path
    // and the deeper walk, which used to compute the flag before applying
    // the filter.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod a;\npub mod b;\npub mod c;\n");
    write(&root.join("src/a.rs"), "use crate::b;\npub fn a() { b::b(); }\n");
    write(&root.join("src/b.rs"), "use crate::c;\npub fn b() { c::c(); }\n");
    // `c`'s only outgoing call leaves the project.
    write(&root.join("src/c.rs"), "pub fn c() { some_external_thing(); }\n");

    let frontier = |args: &[&str]| -> bool {
        let (out, code) = run_in(root, args);
        assert_eq!(code, 0, "{out}");
        let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        doc["frontier_truncated"].as_bool().expect("frontier flag")
    };

    // Depth-1 fast path: one hop from `b` is `c`, whose continuation is
    // external.
    assert!(
        frontier(&["callees", "b", ".", "--depth", "1", "--json", "--compact", "--rebuild"]),
        "the external continuation is real when externals are shown"
    );
    assert!(
        !frontier(&["callees", "b", ".", "--depth", "1", "--hide-external", "--json", "--compact"]),
        "with externals hidden, raising --depth would show nothing new"
    );

    // Deeper walk: the depth-2 cap lands on `c`, same asymmetry.
    assert!(frontier(&["callees", "a", ".", "--depth", "2", "--json", "--compact"]));
    assert!(!frontier(&[
        "callees", "a", ".", "--depth", "2", "--hide-external", "--json", "--compact"
    ]));

    // A resolved continuation past the cap still counts with externals
    // hidden — the fix must not silence the flag wholesale.
    assert!(
        frontier(&["callees", "a", ".", "--depth", "1", "--hide-external", "--json", "--compact"]),
        "b → c is a visible continuation beyond depth 1"
    );
}

#[test]
fn callers_limit_bounds_type_groups_too() {
    // A trait with three implementors and three construction sites: --limit 2
    // must bound the *combined* display, with true totals in the JSON.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub trait Widget { fn go(&self); }\n\
         pub struct A; impl Widget for A { fn go(&self) {} }\n\
         pub struct B; impl Widget for B { fn go(&self) {} }\n\
         pub struct C; impl Widget for C { fn go(&self) {} }\n",
    );
    let (out, code) = run_in(root, &["callers", "Widget", ".", "--limit", "2", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let group = &doc["types"][0];
    let impls_shown = group["implementations"].as_array().unwrap().len();
    let impls_total = group["implementations_total"].as_u64().unwrap() as usize;
    assert_eq!(impls_total, 3, "true total must survive truncation: {out}");
    assert!(
        impls_shown <= 2,
        "type-group rows must respect --limit (got {impls_shown}): {out}"
    );
    assert_eq!(group["truncated"], true, "{out}");
    assert_eq!(
        doc["truncated"], true,
        "top-level truncated must reflect the combined shown count: {out}"
    );

    // Text: the section header carries the true total plus a showing note.
    let (out, code) = run_in(root, &["callers", "Widget", ".", "--limit", "2"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("3 implementation(s)") && out.contains("showing 2"),
        "capped section must say so:\n{out}"
    );
}

#[test]
fn qualified_receiver_does_not_rebind_to_local_homonym() {
    // `other::Type::method()` explicitly names another scope's `Type`;
    // a same-file `Type` with the same terminal name must not claim the
    // edge as Exact (PR #38 review: no terminal-segment fallback).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub mod other;\npub struct Type;\nimpl Type { pub fn method() {} }\n\
         pub fn caller() { crate::other::Type::method(); }\n",
    );
    write(
        &root.join("src/other.rs"),
        "pub struct Type;\nimpl Type { pub fn method() {} }\n",
    );
    let (out, code) = run_in(root, &["callees", "caller", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let exact_to_local = doc["matches"].as_array().unwrap().iter().any(|m| {
        m["confidence"] == "Exact"
            && m["target"].as_str().unwrap_or("").contains("src/lib.rs")
            && m["target"].as_str().unwrap_or("").contains("method")
    });
    assert!(
        !exact_to_local,
        "qualified receiver must not bind Exact to the same-file homonym: {out}"
    );
}

#[test]
fn type_qualified_call_resolves_to_the_callers_lexical_scope() {
    // Two modules each declare `Foo::method`. An unqualified `Foo::method()`
    // inside `mod a` means a's Foo — never whichever suffix match came
    // first — and a caller outside both scopes must get an honest
    // non-Exact edge instead of an arbitrary pick.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod a {
    pub struct Foo;
    impl Foo { pub fn method() {} }
    pub fn caller_in_a() { Foo::method(); }
}
pub mod b {
    pub struct Foo;
    impl Foo { pub fn method() {} }
}
pub fn caller_outside() { Foo::method(); }
"#,
    );
    let (out, code) = run_in(root, &["callees", "caller_in_a", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let m = &doc["matches"][0];
    assert_eq!(m["confidence"], "Exact", "{out}");
    assert!(
        m["target"].as_str().unwrap_or("").contains("::a::Foo::method"),
        "must bind to the enclosing scope's Foo, got: {out}"
    );

    let (out, code) = run_in(root, &["callees", "caller_outside", ".", "--json", "--compact"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let outside_exact = doc["matches"].as_array().unwrap().iter().any(|m| {
        m["confidence"] == "Exact"
    });
    assert!(
        !outside_exact,
        "a caller outside both scopes must not get an arbitrary Exact pick: {out}"
    );
}

#[test]
fn single_same_file_decoy_does_not_bind_exact() {
    // Same rule as above with only ONE same-file candidate: a lone
    // `Foo::method` in a sibling module must not get same-file Exact
    // binding just for being alone — the caller's scope does not enclose
    // it, so the edge defers to pass B/C.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod b {
    pub struct Foo;
    impl Foo { pub fn method() {} }
}
pub fn caller_outside() { Foo::method(); }
"#,
    );
    let (out, code) = run_in(root, &["callees", "caller_outside", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let decoy_exact = doc["matches"].as_array().unwrap().iter().any(|m| {
        m["confidence"] == "Exact"
            && m["target"].as_str().unwrap_or("").contains("::b::Foo::method")
    });
    assert!(
        !decoy_exact,
        "a single same-file decoy in a sibling module must not bind Exact: {out}"
    );
}

#[test]
fn rust_self_relative_receiver_prefixes_resolve_against_caller_scope() {
    // `crate::a::Foo::method()`, `self::Foo::method()`, and
    // `super::Foo::method()` never match a qn textually (qns start with
    // the file path). Each must resolve against the caller's scope chain
    // by anchored equality and bind Exact to `a`'s Foo — not fall through
    // as ambiguous, and never bind `b`'s decoy.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod a {
    pub struct Foo;
    impl Foo { pub fn method() {} }
    pub fn via_self() { self::Foo::method(); }
    pub mod deep {
        pub fn via_super() { super::Foo::method(); }
    }
}
pub mod b {
    pub struct Foo;
    impl Foo { pub fn method() {} }
}
pub fn via_crate() { crate::a::Foo::method(); }
"#,
    );
    for (caller, first) in [
        ("via_crate", true),
        ("via_self", false),
        ("via_super", false),
    ] {
        let mut args = vec!["callees", caller, ".", "--json", "--compact"];
        if first {
            args.push("--rebuild");
        }
        let (out, code) = run_in(root, &args);
        assert_eq!(code, 0, "{out}");
        let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        let m = &doc["matches"][0];
        assert_eq!(
            m["confidence"], "Exact",
            "{caller} must bind Exact via its scope chain: {out}"
        );
        assert!(
            m["target"].as_str().unwrap_or("").contains("::a::Foo::method"),
            "{caller} must bind a's Foo, not b's: {out}"
        );
    }
}

#[test]
fn super_prefix_skips_the_callers_own_scope() {
    // An ancestor decoy: `deep` declares its own `Foo::method`, but
    // `super::Foo::method()` from inside `deep` means the *parent*'s Foo.
    // The walk must skip the caller's own level, not bind the first
    // anchored match it finds on the way up.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod a {
    pub struct Foo;
    impl Foo { pub fn method() {} }
    pub mod deep {
        pub struct Foo;
        impl Foo { pub fn method() {} }
        pub fn via_super() { super::Foo::method(); }
    }
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "via_super", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let m = &doc["matches"][0];
    assert_eq!(m["confidence"], "Exact", "{out}");
    let target = m["target"].as_str().unwrap_or("");
    assert!(
        target.contains("::a::Foo::method") && !target.contains("::deep::"),
        "super:: must bind the parent's Foo, not the caller-level decoy: {out}"
    );
}

#[test]
fn multi_level_super_chain_binds_the_right_ancestor() {
    // `super::super::helper()` arrives with receiver "super::super" — the
    // last keyword has no trailing `::`, so a prefix-only strip loop leaves
    // a literal "super" segment behind and the lookup can never match. The
    // chain must count both levels and bind the grandparent's helper, not
    // fall through, and not bind either decoy on the way up.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub fn helper() {}
pub mod a {
    pub fn helper() {}
    pub mod deep {
        pub fn helper() {}
        pub fn two_level() { super::super::helper(); }
    }
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "two_level", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let m = &doc["matches"][0];
    assert_eq!(m["confidence"], "Exact", "{out}");
    let target = m["target"].as_str().unwrap_or("");
    assert!(
        target.ends_with("src/lib.rs::helper"),
        "super::super must bind the crate-root helper, not a::helper or deep::helper: {out}"
    );
}

#[test]
fn bare_crate_prefix_binds_the_root_not_the_callers_scope() {
    // `crate::helper()` from inside `mod a` names the crate-root helper.
    // The receiver arrives as the single keyword "crate" — it must take
    // the anchored walk, not self-like sibling preference, which would
    // hand the edge to `a::helper` tagged Exact.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub fn helper() {}
pub mod a {
    pub fn helper() {}
    pub fn via_crate() { crate::helper(); }
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "via_crate", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let m = &doc["matches"][0];
    assert_eq!(m["confidence"], "Exact", "{out}");
    let target = m["target"].as_str().unwrap_or("");
    assert!(
        target.ends_with("src/lib.rs::helper"),
        "crate:: must bind the root helper, not the caller-scope sibling: {out}"
    );
}

#[test]
fn scope_keywords_do_not_promote_a_single_match_from_a_ruled_out_scope() {
    // `crate::helper()` names `src/lib.rs::helper`, which does not exist —
    // the only `helper` lives in a sibling module the keyword explicitly
    // rules out. Pass B's single-global-match promotion must not claim it
    // Exact (pass A already declines); the edge defers to pass C, whose
    // dep filter can at most call it Inferred. Same for a bare `super::`
    // whose parent scope lacks the name.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod a {
    pub fn helper() {}
    pub fn via_crate() { crate::helper(); }
}
pub mod b {
    pub mod deep {
        pub fn via_super() { super::sibling_only(); }
    }
}
pub mod c {
    pub fn sibling_only() {}
}
"#,
    );
    for (caller, first) in [("via_crate", true), ("via_super", false)] {
        let mut args = vec!["callees", caller, ".", "--json", "--compact"];
        if first {
            args.push("--rebuild");
        }
        let (out, code) = run_in(root, &args);
        assert_eq!(code, 0, "{out}");
        let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        let any_exact = doc["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["confidence"] == "Exact");
        assert!(
            !any_exact,
            "{caller}: a single match in a scope the keyword rules out must not bind Exact: {out}"
        );
    }
}

#[test]
fn bare_super_prefix_binds_the_parent_not_the_callers_scope() {
    // `super::helper()` from inside `a::deep` names the *parent* module's
    // helper. Sibling preference would bind the caller-level decoy
    // `deep::helper` — the walk must skip the caller's own scope.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod a {
    pub fn helper() {}
    pub mod deep {
        pub fn helper() {}
        pub fn via_super() { super::helper(); }
    }
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "via_super", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let m = &doc["matches"][0];
    assert_eq!(m["confidence"], "Exact", "{out}");
    let target = m["target"].as_str().unwrap_or("");
    assert!(
        target.contains("::a::helper") && !target.contains("::deep::"),
        "super:: must bind the parent's helper, not the caller-level decoy: {out}"
    );
}

#[test]
fn crate_prefix_anchors_at_the_file_root_only() {
    // `crate::a::Foo` from inside `a::deep` must anchor at the file root —
    // an intermediate scope declaring the same `a::Foo` path (here,
    // `a::deep::a::Foo`) must not shadow it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        r#"pub mod a {
    pub struct Foo;
    impl Foo { pub fn method() {} }
    pub mod deep {
        pub mod a {
            pub struct Foo;
            impl Foo { pub fn method() {} }
        }
        pub fn via_crate() { crate::a::Foo::method(); }
    }
}
"#,
    );
    let (out, code) = run_in(root, &["callees", "via_crate", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let m = &doc["matches"][0];
    assert_eq!(m["confidence"], "Exact", "{out}");
    let target = m["target"].as_str().unwrap_or("");
    assert!(
        target.contains("src/lib.rs::a::Foo::method") && !target.contains("::deep::"),
        "crate:: must bind the file-root path, not an intermediate decoy: {out}"
    );
}

// ---------------------------------------------------------------------------
// PR #38 review round 2 — the unattributed section must be usable on common
// method names, and `callees` must report truncation on stdout.
// ---------------------------------------------------------------------------

/// Five project types each declaring `close()`, plus call sites on receivers
/// of unknowable type. The shape of `PgConnection.close` on pgjdbc (39
/// declarers, 1124 sites) and `CliError.new` in this repo (9, 1020).
fn many_declarers_fixture(root: &std::path::Path) {
    write(&root.join("pom.xml"), "<project/>\n");
    for t in ["Conn", "Stream", "Blob", "Cursor", "Socket"] {
        write(
            &root.join(format!("src/{t}.java")),
            &format!("public class {t} {{ public void close() {{}} }}\n"),
        );
    }
    write(
        &root.join("src/Use.java"),
        "public class Use {\n  void a(Object os) { os.close(); }\n  \
         void b(Object lo) { lo.close(); }\n  \
         void c(Object connection) { connection.close(); }\n}\n",
    );
}

#[test]
fn unattributed_section_collapses_when_the_name_does_not_discriminate() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    many_declarers_fixture(root);

    let (out, code) = run_in(root, &["callers", "Conn.close", ".", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    // The count is still reported — dropping it would undercount a rename
    // (issue #31) — but with the reason attached and no rows, because every
    // site names all five `close` declarations equally.
    assert!(
        out.contains("declared by 5 symbols"),
        "header must carry the reason:\n{out}"
    );
    assert!(
        !out.contains("recv=os"),
        "rows must be withheld for a non-discriminating name:\n{out}"
    );

    let (out, code) = run_in(root, &["callers", "Conn.close", ".", "--json", "--compact"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert!(
        doc["unattributed_total"].as_u64().unwrap() >= 3,
        "the sites are still counted: {out}"
    );
    assert_eq!(doc["unattributed_declarers"], 5, "{out}");
    assert_eq!(doc["unattributed_suppressed"], true, "{out}");
    assert_eq!(
        doc["unattributed"].as_array().unwrap().len(),
        0,
        "a machine consumer must not receive rows it cannot attribute: {out}"
    );
}

#[test]
fn unattributed_rows_carry_the_receiver_when_the_name_is_specific() {
    // One declarer, so the sites are real evidence — and each row shows the
    // receiver as written, which is the only triage signal available without
    // type inference.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pom.xml"), "<project/>\n");
    write(
        &root.join("src/Prefs.java"),
        "public class Prefs { public void prefersJavaTime() {} }\n",
    );
    write(
        &root.join("src/Use.java"),
        "public class Use { void a(Object cfg) { cfg.prefersJavaTime(); } }\n",
    );

    let (out, code) = run_in(root, &["callers", "prefersJavaTime", ".", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("unresolved call site(s) naming") && out.contains("recv=cfg"),
        "a discriminating name keeps its rows, receiver included:\n{out}"
    );

    let (out, code) = run_in(
        root,
        &["callers", "prefersJavaTime", ".", "--json", "--compact"],
    );
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["unattributed_suppressed"], false, "{out}");
    assert_eq!(doc["unattributed"][0]["receiver"], "cfg", "{out}");
}

#[test]
fn unattributed_sample_is_capped_independently_of_limit() {
    // `--limit` sizes the resolved list; the leftover budget must not flow
    // into the unattributed section, or the worse the resolver did the more
    // noise a caller gets.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pom.xml"), "<project/>\n");
    write(
        &root.join("src/Prefs.java"),
        "public class Prefs { public void forDate() {} }\n",
    );
    let mut body = String::from("public class Use {\n");
    for i in 0..40 {
        body.push_str(&format!("  void m{i}(Object c) {{ c.forDate(); }}\n"));
    }
    body.push_str("}\n");
    write(&root.join("src/Use.java"), &body);

    let (out, code) = run_in(
        root,
        &[
            "callers", "forDate", ".", "--limit", "500", "--json", "--compact", "--rebuild",
        ],
    );
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let total = doc["unattributed_total"].as_u64().unwrap();
    assert!(total >= 30, "fixture must exceed the sample cap: {out}");
    assert_eq!(
        doc["unattributed"].as_array().unwrap().len(),
        25,
        "a generous --limit must not widen the sample past its own cap: {out}"
    );
    assert_eq!(doc["unattributed_truncated"], true, "{out}");
}

#[test]
fn unattributed_drops_sites_whose_receiver_names_another_project_type() {
    // Pass A's rule, applied one layer later: `Other.shared()` explicitly
    // named `Other`, so it is not evidence about `Target.shared`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pom.xml"), "<project/>\n");
    write(
        &root.join("src/Target.java"),
        "public class Target { public static void shared() {} }\n",
    );
    write(
        &root.join("src/Other.java"),
        "public class Other { public static void shared() {} }\n",
    );
    write(
        &root.join("src/Use.java"),
        "public class Use { void a() { Other.shared(); } }\n",
    );

    let (out, code) = run_in(root, &["callers", "Target.shared", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let rows = doc["unattributed"].as_array().unwrap();
    assert!(
        !rows.iter().any(|r| r["receiver"] == "Other"),
        "a receiver naming another project type must not be listed: {out}"
    );
    assert_eq!(
        doc["unattributed_total"], 0,
        "and it must not be counted either: {out}"
    );
}

#[test]
fn callees_text_header_reports_the_true_total_when_limit_trims() {
    // stdout is where `callees` gets read; a `# 2 callee(s)` header on a
    // capped list reads as complete even though stderr and JSON said 7.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\n\
         pub fn caller() { a(); b(); c(); d(); }\n",
    );

    let (out, code) = run_in(root, &["callees", "caller", ".", "--limit", "2", "--rebuild"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("# 4 callee(s)") && out.contains("showing 2"),
        "header must report the true total and that it was cut:\n{out}"
    );
}

#[test]
fn callees_type_ancestor_groups_count_toward_total_and_limit() {
    // Ancestor groups were neither bounded by --limit nor counted in
    // `total`, so `truncated` described only `matches`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pom.xml"), "<project/>\n");
    write(&root.join("src/A.java"), "public interface A {}\n");
    write(&root.join("src/B.java"), "public interface B {}\n");
    write(&root.join("src/C.java"), "public class C implements A, B {}\n");

    let (out, code) = run_in(
        root,
        &["callees", "C", ".", "--limit", "1", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(doc["total"], 2, "both ancestors count toward total: {out}");
    assert_eq!(doc["truncated"], true, "{out}");
    assert_eq!(
        doc["types"][0]["ancestors"].as_array().unwrap().len(),
        1,
        "ancestor groups share the --limit display budget: {out}"
    );
    assert_eq!(doc["types"][0]["ancestors_total"], 2, "{out}");

    // The text header carries the same facts.
    let (out, code) = run_in(root, &["callees", "C", ".", "--limit", "1"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("# 2 ancestor(s)") && out.contains("showing 1"),
        "text header must report the true total:\n{out}"
    );
}

#[test]
fn crate_prefix_arm_only_fires_from_the_crate_root() {
    // `crate::` anchors at the crate root, not at the caller's file, so the
    // arm must not bind `crate::inner::Foo::method()` in src/other.rs to a
    // same-file `mod inner` decoy — that is precisely where anchoring at the
    // caller's file is wrong. Off the crate root the edge falls through to
    // pass B/C, which may still guess, but never claims `Exact`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod inner;\npub mod other;\n");
    write(
        &root.join("src/inner.rs"),
        "pub struct Foo;\nimpl Foo { pub fn method() {} }\n",
    );
    write(
        &root.join("src/other.rs"),
        "pub mod inner {\n    pub struct Foo;\n    impl Foo { pub fn method() {} }\n}\n\
         pub fn caller() { crate::inner::Foo::method(); }\n",
    );

    let (out, code) = run_in(
        root,
        &["callees", "other.rs:caller", ".", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    let matches = doc["matches"].as_array().expect("matches array");
    assert_eq!(
        matches.len(),
        1,
        "the call site must still be reported at all: {out}"
    );
    let m = &matches[0];
    let target = m["target"].as_str().unwrap_or("");
    assert!(
        !(m["confidence"] == "Exact" && target.contains("other.rs::inner::Foo::method")),
        "the crate:: arm must not claim a same-file decoy as Exact: {out}"
    );
}

#[test]
fn incremental_update_matches_a_cold_build_of_the_same_content() {
    // A partial update used to assign each bare edge the *unfiltered* global
    // symbol-table entry as its candidate set, while a cold build assigns
    // only the candidates the caller's file can reach through the dep graph.
    // One edit to an unrelated file therefore changed answers project-wide:
    // measured on this repo, `callers CliError.new` reported 1023 unresolved
    // sites cold and 1073 after touching src/adapters/sql.rs, with no source
    // change behind the difference.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub mod target;\npub mod other;\npub mod third;\npub mod caller;\npub mod unrelated;\n",
    );
    // Three project types declare `shared`, so a bare `shared()` site is
    // ambiguous across all three and pass C's dep filter is the only thing
    // that narrows it.
    write(
        &root.join("src/target.rs"),
        "pub struct T;\nimpl T { pub fn shared(&self) {} }\n",
    );
    write(
        &root.join("src/other.rs"),
        "pub struct U;\nimpl U { pub fn shared(&self) {} }\n",
    );
    write(
        &root.join("src/third.rs"),
        "pub struct V;\nimpl V { pub fn shared(&self) {} }\n",
    );
    // `caller.rs` imports two of the three, so pass C's filter leaves two
    // survivors and drops `V::shared` — the site is not evidence about `V`.
    // The pre-fix updater overwrote that filtered set with all three global
    // matches, so after any unrelated edit the site started counting as a
    // possible caller of `V::shared` too.
    write(
        &root.join("src/caller.rs"),
        "use crate::target;\nuse crate::other;\n\
         pub fn calls(v: &Vec<u8>) { v.shared(); }\n\
         pub fn touch(t: &target::T, u: &other::U) { let _ = (t, u); }\n",
    );
    write(&root.join("src/unrelated.rs"), "pub fn noop() {}\n");

    let query = |args: &[&str]| -> serde_json::Value {
        let (out, code) = run_in(root, args);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(out.trim()).expect("valid json")
    };

    let cold = query(&["callers", "V.shared", ".", "--json", "--compact", "--rebuild"]);
    assert_eq!(
        cold["unattributed_total"], 0,
        "premise: the dep filter excludes V::shared from that site's candidates:\n{cold}"
    );

    // Touch a file that has nothing to do with the query, then re-query
    // *without* --rebuild so the delta path runs.
    write(
        &root.join("src/unrelated.rs"),
        "pub fn noop() {}\n// an edit that changes nothing about V::shared\n",
    );
    let incremental = query(&["callers", "V.shared", ".", "--json", "--compact"]);
    let cold_again = query(&["callers", "V.shared", ".", "--json", "--compact", "--rebuild"]);

    for key in [
        "total",
        "unattributed_total",
        "unattributed_declarers",
        "unattributed_suppressed",
    ] {
        assert_eq!(
            incremental[key], cold_again[key],
            "{key} diverged between a partial update and a cold build of the same content:\n\
             incremental={incremental}\ncold={cold_again}"
        );
        assert_eq!(
            cold[key], cold_again[key],
            "{key} changed across an edit that touched no relevant file:\n\
             before={cold}\nafter={cold_again}"
        );
    }
}

#[test]
fn incremental_promotion_matches_cold_build_confidence() {
    // Parity covers the confidence tag, not just the target. A bare edge in
    // an *unchanged* file that a later delta finally resolves is promoted by
    // `apply_delta_to_calls`, and that promotion used to tag `Inferred`
    // where pass B tags the same single-global-match-no-receiver case
    // `Exact` — so the same content answered differently depending on
    // whether you had rebuilt.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(&root.join("src/lib.rs"), "pub mod b;\n");
    write(&root.join("src/b.rs"), "pub fn caller() { only_one_of_these(); }\n");

    // Nothing declares the name yet: the edge is bare, and the query is a
    // legitimate miss.
    let (_, code) = run_in(root, &["callers", "only_one_of_these", ".", "--rebuild"]);
    assert_eq!(code, 2, "premise: the symbol does not exist yet");

    // The definition arrives in a later delta; b.rs is untouched.
    write(&root.join("src/lib.rs"), "pub mod a;\npub mod b;\n");
    write(&root.join("src/a.rs"), "pub fn only_one_of_these() {}\n");

    let query = |args: &[&str]| -> serde_json::Value {
        let (out, code) = run_in(root, args);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(out.trim()).expect("valid json")
    };
    let incremental = query(&["callers", "only_one_of_these", ".", "--json", "--compact"]);
    let cold = query(&["callers", "only_one_of_these", ".", "--json", "--compact", "--rebuild"]);

    let edges = |doc: &serde_json::Value| -> Vec<(String, String)> {
        doc["matches"]
            .as_array()
            .expect("matches array")
            .iter()
            .map(|m| {
                (
                    m["target"].as_str().unwrap_or_default().to_string(),
                    m["confidence"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(
        edges(&incremental),
        edges(&cold),
        "per-edge confidence diverged between a partial update and a cold build:\n\
         incremental={incremental}\ncold={cold}"
    );
    assert_eq!(
        incremental["matches"][0]["confidence"], "Exact",
        "a single global match with no receiver is pass B's `Exact`:\n{incremental}"
    );
}

#[test]
fn trace_separates_a_depth_cutoff_from_a_broken_chain() {
    // Both a capped walk and an exhausted graph produce "no path"; only the
    // second one justifies blaming dynamic dispatch (issue #32).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n");
    write(
        &root.join("src/lib.rs"),
        "pub fn deep() {}\npub fn mid() { deep(); }\npub fn top() { mid(); }\npub fn island() {}\n",
    );

    // top → mid → deep is two hops; --depth 1 stops one short of it.
    let (out, code) = run_in(
        root,
        &["trace", "top", "deep", ".", "--depth", "1", "--json", "--compact", "--rebuild"],
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["found"], false, "{out}");
    assert_eq!(v["frontier_truncated"], true, "{out}");

    // The text must say the walk stopped rather than assert a dynamic-dispatch
    // boundary the evidence doesn't support.
    let (out, _) = run_in(root, &["trace", "top", "deep", ".", "--depth", "1"]);
    assert!(out.contains("within --depth 1"), "{out}");
    assert!(out.contains("raise --depth"), "{out}");
    assert!(
        !out.contains("dynamic-dispatch"),
        "a capped walk is not evidence of a dispatch boundary: {out}"
    );

    // Raising the cap finds the path.
    let (out, _) = run_in(root, &["trace", "top", "deep", ".", "--depth", "2", "--json", "--compact"]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["found"], true, "{out}");
    assert_eq!(v["frontier_truncated"], false, "a found path was never cut short: {out}");

    // A genuinely unreachable target keeps the original diagnosis.
    let (out, _) = run_in(root, &["trace", "top", "island", ".", "--json", "--compact"]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
    assert_eq!(v["found"], false, "{out}");
    assert_eq!(v["frontier_truncated"], false, "the graph ended, not the walk: {out}");
    let (out, _) = run_in(root, &["trace", "top", "island", "."]);
    assert!(out.contains("dynamic-dispatch"), "{out}");
}

/// A Python call beside a Ruby method of the same name (issue #96), plus
/// whatever else the test adds.
fn python_beside_ruby() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("pyproject.toml"), "[project]\nname = \"mixed\"\nversion = \"0.0.0\"\n");
    write(&root.join("app.py"), "def main():\n    helper()\n");
    write(&root.join("util.rb"), "def helper\n  1\nend\n");
    tmp
}

#[test]
fn a_bare_call_does_not_bind_into_another_language() {
    // Pass B used to take the one project `helper`, a Ruby method, as the
    // Python call's target, tagged `Exact` (issue #96).
    let tmp = python_beside_ruby();
    let root = tmp.path();
    let (out, code) = run_in(root, &["callees", "main", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(!out.contains("util.rb"), "the Python call bound into Ruby:\n{}", out);
    let (out, code) = run_in(root, &["callers", "helper", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("# 0 caller(s)"), "{}", out);
    // Nor is it offered as a possible caller: Python cannot reach it.
    assert!(!out.contains("app.py"), "{}", out);
}

#[test]
fn a_bare_call_takes_its_own_language_over_another() {
    // Two `helper`s, one per language: the Python one is the only candidate
    // the Python call has, so it binds as a single match does.
    let tmp = python_beside_ruby();
    let root = tmp.path();
    write(&root.join("lib.py"), "def helper():\n    pass\n");
    let (out, code) = run_in(root, &["callees", "main", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("lib.py::helper  (Exact)"), "{}", out);
    assert!(!out.contains("util.rb"), "{}", out);
}

#[test]
fn a_bare_call_binds_within_a_language_family() {
    // TypeScript, TSX and JavaScript call each other, and so do Java,
    // Kotlin and Scala: a single match in the same family still binds.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("package.json"), "{\"name\": \"mixed\"}\n");
    write(&root.join("src/app.tsx"), "export function App() {\n  return formatName();\n}\n");
    write(&root.join("src/util.js"), "export function formatName() {\n  return 1;\n}\n");
    write(&root.join("jvm/Run.kt"), "fun runKt() {\n    javaHelper()\n}\n");
    write(&root.join("jvm/Util.java"), "class Util {\n    static void javaHelper() {}\n}\n");
    let (out, code) = run_in(root, &["callees", "App", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("src/util.js::formatName  (Exact)"), "{}", out);
    let (out, code) = run_in(root, &["callees", "runKt", "."]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.contains("jvm/Util.java::Util::javaHelper  (Exact)"), "{}", out);
}

#[test]
fn an_incremental_update_keeps_a_bare_call_in_its_language() {
    // The updater re-resolves bare calls itself, so it must apply the same
    // language rule as a cold build, whichever language's file comes or goes.
    let tmp = python_beside_ruby();
    let root = tmp.path();
    let callees = |rebuild: bool| {
        let mut args = vec!["callees", "main", ".", "--json", "--compact"];
        if rebuild {
            args.push("--rebuild");
        }
        run_in(root, &args).0
    };
    assert!(!callees(true).contains("util.rb"));
    // A Python `helper` arrives: the call binds to it.
    write(&root.join("lib.py"), "def helper():\n    pass\n");
    let warm = callees(false);
    assert!(warm.contains("lib.py::helper"), "{}", warm);
    assert_eq!(warm, callees(true), "incremental and cold builds disagree");
    // It goes again: the call must not fall back to the Ruby method.
    std::fs::remove_file(root.join("lib.py")).unwrap();
    let warm = callees(false);
    assert!(!warm.contains("util.rb"), "{}", warm);
    assert_eq!(warm, callees(true), "incremental and cold builds disagree");
    // Two Python `helper`s leave the call ambiguous between them; once both
    // are gone it names neither, and still not the Ruby one.
    write(&root.join("lib.py"), "def helper():\n    pass\n");
    write(&root.join("lib2.py"), "def helper():\n    pass\n");
    let warm = callees(false);
    assert!(warm.contains("lib2.py::helper"), "{}", warm);
    assert_eq!(warm, callees(true), "incremental and cold builds disagree");
    std::fs::remove_file(root.join("lib.py")).unwrap();
    std::fs::remove_file(root.join("lib2.py")).unwrap();
    let warm = callees(false);
    assert!(!warm.contains("lib") && !warm.contains("util.rb"), "{}", warm);
    assert_eq!(warm, callees(true), "incremental and cold builds disagree");
}

#[test]
fn unresolved_sites_count_declarers_in_the_targets_language_only() {
    // Four Python `shutdown_now`s used to push the declarer count past the
    // listing cap, hiding the one Rust call that can reach `Conn` (#96).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("Cargo.toml"), "[package]\nname = \"mixed\"\nversion = \"0.0.0\"\nedition = \"2021\"\n");
    write(&root.join("src/conn.rs"), "pub struct Conn;\nimpl Conn {\n    pub fn shutdown_now(&self) {}\n}\n");
    write(&root.join("src/stop.rs"), "pub fn stop(x: &Thing) {\n    x.shutdown_now();\n}\n");
    for n in ["a", "b", "c", "d"] {
        write(&root.join(format!("py/{n}.py")), "def shutdown_now():\n    pass\n");
    }
    let (out, code) = run_in(root, &["callers", "Conn.shutdown_now", ".", "--rebuild"]);
    assert_eq!(code, 0, "{}", out);
    // The row's qn, which uses `/` on every platform (its file column is native).
    assert!(out.contains("in src/stop.rs::stop"), "the Rust site is not listed:\n{}", out);
    assert!(!out.contains("declared by"), "{}", out);
}
