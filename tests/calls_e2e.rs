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
    let body_pos: Vec<&str> = positional_out.lines().filter(|l| l.starts_with("src/")).collect();
    let body_flag: Vec<&str> = flag_out.lines().filter(|l| l.starts_with("src/")).collect();
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
    let (out, code) = run_in(root, &["callers", "Node.add_child", ".", "--rebuild"]);
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
