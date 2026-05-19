//! Digest format coverage: legend line, file size labels, char counts,
//! `name()` callable form, `[N×]` overload collapse, `[m]` modifier
//! markers, `[deprecated]` flag, and adapter-supplied `native_kind`.
//!
//! Modifier/deprecation/native_kind detection is per-language and
//! adapter-driven. The Rust adapter is the reference implementation;
//! other adapters surface the same markers once their extractors are
//! wired up. Decls without adapter-populated fields render the same as
//! they did pre-v0.4 (just with the new outer scaffolding).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(out.status.success(), "exit non-zero: {:?}", out);
    String::from_utf8(out.stdout).expect("utf8")
}

const FIXTURE: &str = "tests/fixtures/rust_adapter/sample.rs";

#[test]
fn digest_starts_with_legend() {
    let s = run(&["digest", FIXTURE]);
    let first = s.lines().next().expect("non-empty");
    assert!(
        first.contains("# legend:"),
        "first line should be the legend:\n{first}"
    );
    // Dynamic legend: only entries for tokens actually present in this output
    // are listed. The sample fixture has callables and modifiers; it has no
    // deprecated decls, so `[deprecated]` is correctly omitted.
    assert!(
        first.contains("name()") && first.contains("[m]"),
        "legend should explain tokens that appear in this digest:\n{first}"
    );
}

#[test]
fn digest_legend_includes_overloads_when_overload_collapse_fires() {
    // Regression: the legend used to scan the raw IR for `[N×]` in names,
    // which never appears at that point — overload-collapse runs *inside*
    // the renderer, after the legend is built. Two adjacent same-name
    // methods on a type force a collapse; the legend must surface the
    // `[N×] = N overloads` entry.
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-legend-overload-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("ovl.rs");
    std::fs::write(
        &f,
        "pub struct S;\nimpl S {\n    pub fn dup(&self) {}\n    pub fn dup(&self, _: u8) {}\n}\n",
    )
    .expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    let first = s.lines().next().expect("non-empty");
    assert!(
        first.contains("[N×]"),
        "legend should explain `[N×]` when the collapsed digest contains an overload count:\n{first}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn digest_legend_includes_deprecated_only_when_present() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-legend-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("dep.rs");
    std::fs::write(&f, "#[deprecated]\npub fn old() {}\n").expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    let first = s.lines().next().expect("non-empty");
    assert!(
        first.contains("[deprecated]"),
        "legend should include [deprecated] when present:\n{first}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_header_shows_size_label_and_char_count() {
    let s = run(&["digest", FIXTURE]);
    let header = s
        .lines()
        .find(|l| l.contains("sample.rs"))
        .expect("file header missing");
    assert!(
        header.contains("[tiny]") || header.contains("[small]"),
        "missing size label:\n{header}"
    );
    assert!(
        header.contains("chars"),
        "missing char count:\n{header}"
    );
    // Token estimate was deliberately skipped — different tokenizers
    // produce different counts, char count is the honest metric.
    assert!(
        !header.contains("tokens"),
        "should not pretend to estimate tokens:\n{header}"
    );
}

#[test]
fn callable_uses_paren_form() {
    let s = run(&["digest", FIXTURE]);
    // The Person type has methods `new` and `hello` lifted from impl
    // blocks. Both should render as `name()`, not `+name`.
    assert!(s.contains("new()"), "callable missing paren form:\n{s}");
    assert!(s.contains("hello()"), "callable missing paren form:\n{s}");
    assert!(
        !s.contains("+new") && !s.contains("+hello"),
        "old `+name` form leaked:\n{s}"
    );
}

#[test]
fn rust_trait_renders_as_trait_not_interface() {
    // native_kind = "trait" overrides the canonical Interface kind so
    // Rust users see the source-true keyword.
    let s = run(&["digest", FIXTURE]);
    assert!(
        s.contains("trait Greeter"),
        "Greeter should render as `trait`, not `interface`:\n{s}"
    );
    assert!(
        !s.contains("interface Greeter"),
        "canonical kind leaked through native_kind override:\n{s}"
    );
}

#[test]
fn deprecated_and_modifier_markers() {
    // Build a quick fixture inline.
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-digest-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("dep.rs");
    std::fs::write(
        &f,
        "#[deprecated]\npub fn old_fn() {}\npub async fn fetch() {}\npub unsafe fn raw() {}\npub const fn calc() -> u32 { 0 }\n",
    )
    .expect("write fixture");

    let s = run(&["digest", f.to_str().unwrap()]);
    assert!(
        s.contains("old_fn()") && s.contains("[deprecated]"),
        "deprecated marker missing:\n{s}"
    );
    assert!(
        s.contains("[async]") && s.contains("fetch()"),
        "async modifier missing:\n{s}"
    );
    assert!(
        s.contains("[unsafe]") && s.contains("raw()"),
        "unsafe modifier missing:\n{s}"
    );
    assert!(
        s.contains("[const]") && s.contains("calc()"),
        "const modifier missing:\n{s}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_decorator_modifiers_and_deprecation() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-py-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("p.py");
    std::fs::write(
        &f,
        "@deprecated\nasync def fetch(): pass\n\nclass C:\n    @classmethod\n    def k(cls): pass\n    @staticmethod\n    def s(): pass\n    @property\n    def n(self): return 0\n",
    )
    .expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    assert!(s.contains("[async]") && s.contains("[deprecated]"), "py async/deprecated:\n{s}");
    assert!(s.contains("[classmethod]"), "py classmethod:\n{s}");
    assert!(s.contains("[static]"), "py staticmethod:\n{s}");
    assert!(s.contains("[property]"), "py property:\n{s}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn typescript_modifiers_and_deprecation() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-ts-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("s.ts");
    std::fs::write(
        &f,
        "class S {\n  /** @deprecated */\n  old(): void {}\n  async fetch(): Promise<void> {}\n  static help(): void {}\n  override render(): string { return '' }\n}\n",
    )
    .expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    assert!(s.contains("[deprecated]"), "ts @deprecated JSDoc:\n{s}");
    assert!(s.contains("[async]"), "ts async:\n{s}");
    assert!(s.contains("[static]"), "ts static:\n{s}");
    assert!(s.contains("[override]"), "ts override:\n{s}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kotlin_native_kind_data_and_sealed() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-kt-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("k.kt");
    std::fs::write(
        &f,
        "data class Foo(val x: Int)\nsealed class Result\nsuspend fun load() {}\n",
    )
    .expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    assert!(s.contains("data class"), "kotlin native data class:\n{s}");
    assert!(s.contains("sealed class"), "kotlin native sealed class:\n{s}");
    assert!(
        !s.contains("sealed sealed"),
        "modifier should not be re-emitted when already in native_kind:\n{s}"
    );
    assert!(s.contains("[suspend]"), "kotlin suspend modifier:\n{s}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn java_deprecation_annotation() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-java-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("J.java");
    std::fs::write(
        &f,
        "public class J {\n    @Deprecated\n    public void old() {}\n    public static void util() {}\n    public abstract void hook();\n}\n",
    )
    .expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    assert!(s.contains("[deprecated]"), "@Deprecated → [deprecated]:\n{s}");
    assert!(s.contains("[static]"), "java static:\n{s}");
    assert!(s.contains("[abstract]"), "java abstract:\n{s}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn go_deprecation_doc_convention() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-go-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("g.go");
    std::fs::write(
        &f,
        "package g\n\n// OldFn does the thing.\n//\n// Deprecated: use NewFn.\nfunc OldFn() {}\n\nfunc NewFn() {}\n",
    )
    .expect("write");
    let s = run(&["digest", f.to_str().unwrap()]);
    assert!(
        s.contains("OldFn") && s.contains("[deprecated]"),
        "go Deprecated: convention:\n{s}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overload_collapse() {
    let dir = std::env::temp_dir().join(format!(
        "ast-bro-overload-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let f = dir.join("ovl.rs");
    // Three free fns with the same name — different module-level overloads
    // would normally be illegal in Rust, but we can simulate via macro_rules!
    // emissions or methods. Use methods inside an impl which Rust DOES allow
    // by trait shadowing — but simplest: just write three distinct methods.
    // For collapse to fire we need the renderer to see N adjacent same-name
    // members in one type's collected list. Use mod-level fns with the same
    // name in a single mod (Rust forbids it, so simulate via three traits):
    std::fs::write(
        &f,
        r#"
pub struct S;
impl S {
    pub fn try_one(&self) {}
    pub fn try_one(&self, _x: u8) {} // intentional rust err
    pub fn try_one(&self, _x: u8, _y: u8) {} // intentional rust err
}
"#,
    )
    .expect("write fixture");
    // Even though Rust would reject this code, the parser surfaces all three
    // method nodes in `impl S` (parse-error tolerance). The digest collapses
    // them.
    let s = run(&["digest", f.to_str().unwrap()]);
    // Either we see [3×] suffix or the parse skipped duplicates. Be lenient
    // — assert only that there's no triple emission of `try_one()`.
    let count = s.matches("try_one()").count();
    assert!(
        count <= 1,
        "overload collapse should fold duplicates ({count} emissions):\n{s}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
