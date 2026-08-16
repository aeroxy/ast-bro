//! End-to-end coverage for the PHP adapter against
//! `tests/fixtures/php_adapter/sample.php`. Asserts:
//!   1. namespace / interface / class declarations all surface.
//!   2. Method visibility (`public` / `private` / `protected`) and modifiers
//!      (`static`, `abstract`) are pulled off the declaration's direct
//!      children — they're not exposed as named fields by tree-sitter-php.
//!   3. Property declarations carry visibility, modifiers, and the `$`
//!      prefix that tree-sitter-php already includes in the name token.
//!   4. Top-level functions co-exist with class bodies.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

const FIXTURE: &str = "tests/fixtures/php_adapter/sample.php";

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
fn namespace_interface_class_render() {
    let s = run(&["map", FIXTURE]);
    assert!(s.contains("namespace App\\Billing"), "namespace missing:\n{s}");
    assert!(s.contains("interface Payable"), "interface missing:\n{s}");
    assert!(s.contains("class Account"), "class missing:\n{s}");
}

#[test]
fn property_visibility_and_modifiers_render() {
    let s = run(&["map", FIXTURE]);
    // Properties keep their `$` prefix from tree-sitter-php.
    assert!(s.contains("public $email"), "public property missing:\n{s}");
    assert!(
        s.contains("protected static $count"),
        "protected static property missing:\n{s}"
    );
}

#[test]
fn method_visibility_and_modifiers_render() {
    let s = run(&["map", FIXTURE]);
    // Default visibility is "public" on `__construct`.
    assert!(
        s.contains("public function __construct"),
        "ctor visibility missing:\n{s}"
    );
    // Abstract method keeps `abstract` modifier on a public method.
    assert!(s.contains("abstract"), "abstract modifier missing:\n{s}");
    assert!(s.contains("balance"), "abstract method name missing:\n{s}");
    // Private + static combine into one signature.
    assert!(
        s.contains("private static function bump"),
        "private static method missing:\n{s}"
    );
}

#[test]
fn top_level_function_surfaced() {
    let s = run(&["map", FIXTURE]);
    assert!(
        s.contains("function format_amount"),
        "top-level function missing:\n{s}"
    );
}

/// A namespace is spelled two ways, and each surface takes the one it needs.
///
/// `map` prints the file as written, so the clause keeps PHP's `\`. Every
/// other surface prints a *qualified path*, and those are joined with dots —
/// which produced `App\Lib.Service`, a spelling that is neither PHP nor a
/// target this tool takes back. C++17's `namespace A::B` had the same shape.
/// The declaration's name therefore carries the dots and its signature keeps
/// the source, so `show` still names the enclosing clause as written.
#[test]
fn a_multi_segment_namespace_is_spelled_for_the_surface_that_prints_it() {
    let dir = "tests/fixtures/implements_collision/namespace_separators/php";
    let mapped = run(&["map", dir]);
    assert!(
        mapped.contains("namespace App\\Base"),
        "map lost PHP's own spelling:\n{mapped}"
    );

    let surfaced = run(&["surface", dir]);
    for want in ["App.Base.Root", "App.Models.Child"] {
        assert!(
            surfaced.contains(want),
            "surface did not spell the qualified path in dots, expected {want:?}:\n{surfaced}"
        );
    }
    assert!(
        !surfaced.contains("App\\"),
        "a qualified path kept a separator only PHP understands:\n{surfaced}"
    );

    // The name `surface` printed is a name `show` accepts, and the clause it
    // reports around it is still the source's.
    let shown = run(&["show", &format!("{dir}/child.php"), "App.Models.Child"]);
    assert!(
        shown.contains("class Child") && shown.contains("namespace App\\Models"),
        "the printed qualified path did not resolve back:\n{shown}"
    );
}
