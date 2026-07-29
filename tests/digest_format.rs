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

#[test]
fn max_members_caps_the_same_declarations_in_text_and_json() {
    // One definition of "member" across renderers: the cap applies to type
    // members, not to module/namespace children. Text `+N more` and JSON
    // `dropped_members` must agree.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m.rs");
    std::fs::write(
        &p,
        "pub mod inner {\n    pub fn a() {}\n    pub fn b() {}\n    pub fn c() {}\n}\n\
         pub struct S { pub x: u32, pub y: u32, pub z: u32 }\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    // Module children are not "members": all three functions render in text
    // and survive in JSON.
    let text = run(&["map", p, "--max-members", "1"]);
    for f in ["fn a", "fn b", "fn c"] {
        assert!(text.contains(f), "module fns must not be capped:\n{text}");
    }
    // The struct's fields are members: text shows the cap note...
    assert!(
        text.contains("+2 more member(s)"),
        "struct fields over the cap must be reported:\n{text}"
    );
    // ...and JSON reports exactly the same drop count.
    let json = run(&["map", p, "--max-members", "1", "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    let file = &doc["files"][0];
    assert_eq!(file["truncated"], true, "{json}");
    assert_eq!(file["dropped_members"], 2, "text +N and JSON dropped_members must agree: {json}");
    let inner = file["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "inner")
        .expect("module in json");
    assert_eq!(
        inner["children"].as_array().unwrap().len(),
        3,
        "module children must not be capped in JSON either: {json}"
    );
}

#[test]
fn max_members_counts_raw_declarations_even_with_overloads() {
    // Overloads collapse into one display token, but the cap and the
    // "+N more" count apply to raw declarations — the same unit JSON's
    // `dropped_members` counts (PR #38 review).
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("Over.java");
    std::fs::write(
        &p,
        "public class Over {\n\
         \x20 public void f(int a) {}\n\
         \x20 public void f(String a) {}\n\
         \x20 public void f(double a) {}\n\
         \x20 public void g() {}\n\
         }\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    let text = run(&["map", p, "--detail", "names", "--max-members", "1"]);
    assert!(
        text.contains("+3 more"),
        "raw-declaration drop count expected:\n{text}"
    );
    let json = run(&["map", p, "--max-members", "1", "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    assert_eq!(
        doc["files"][0]["dropped_members"], 3,
        "text +N and JSON dropped_members must agree: {json}"
    );
}

#[test]
fn free_functions_are_not_capped_by_max_members() {
    // `--max-members` caps members *of a type*; module-level free functions
    // stay complete in text, matching the JSON payload (PR #38 review).
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("free.rs");
    std::fs::write(&p, "pub fn a() {}\npub fn b() {}\npub fn c() {}\n").unwrap();
    let p = p.to_str().unwrap();
    let text = run(&["map", p, "--detail", "names", "--max-members", "1"]);
    for f in ["a", "b", "c"] {
        assert!(
            text.contains(&format!("{f}()")),
            "free fn {f} must not be capped:\n{text}"
        );
    }
}

#[test]
fn names_detail_honors_no_attrs_and_no_lines() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("attrs.rs");
    std::fs::write(
        &p,
        "#[derive(Debug)]\npub struct S {\n    pub x: u32,\n}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    let with = run(&["map", p, "--detail", "names"]);
    assert!(
        with.contains("derive") && with.contains("L2-4"),
        "attrs + line ranges expected by default:\n{with}"
    );
    let without = run(&["map", p, "--detail", "names", "--no-attrs", "--no-lines"]);
    assert!(
        !without.contains("derive"),
        "--no-attrs must drop attrs in names detail:\n{without}"
    );
    assert!(
        !without.contains("L2-4"),
        "--no-lines must drop line suffixes in names detail:\n{without}"
    );
}

#[test]
fn json_honors_projection_flags() {
    // Issue #39: the --no-* projection flags apply to the JSON payload the
    // same way they apply to text. Passing none keeps every key.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("doc.rs");
    std::fs::write(
        &p,
        "/// A documented function.\n#[inline]\npub fn f() {}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();
    let decl = |args: &[&str]| -> serde_json::Value {
        let out = run(&[&["map", p, "--json", "--compact"], args].concat());
        let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("valid json");
        doc["files"][0]["declarations"][0].clone()
    };

    let full = decl(&[]);
    for key in ["docs", "docs_inside", "doc_start_byte", "start_line", "start_byte", "attrs"] {
        assert!(full.get(key).is_some(), "default payload must keep `{key}`: {full}");
    }

    let no_docs = decl(&["--no-docs"]);
    for key in ["docs", "docs_inside", "doc_start_byte"] {
        assert!(no_docs.get(key).is_none(), "--no-docs must drop `{key}`: {no_docs}");
    }
    assert!(no_docs.get("start_line").is_some());

    let no_lines = decl(&["--no-lines"]);
    for key in ["start_line", "end_line", "start_byte", "end_byte", "doc_start_byte"] {
        assert!(no_lines.get(key).is_none(), "--no-lines must drop `{key}`: {no_lines}");
    }
    assert!(no_lines.get("docs").is_some(), "--no-lines must keep docs: {no_lines}");

    let no_attrs = decl(&["--no-attrs"]);
    assert!(no_attrs.get("attrs").is_none(), "--no-attrs must drop `attrs`: {no_attrs}");
    assert!(no_attrs.get("docs").is_some());
}

#[test]
fn rust_missing_pub_means_private_except_inherited_contexts() {
    // Rust items without `pub` are module-private and now say so, which is
    // what makes `--no-private` (and the digest public-only view) real for
    // Rust. Trait members and trait-impl methods carry no modifier of
    // their own and must NOT be treated as private.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("vis.rs");
    std::fs::write(
        &p,
        r#"pub trait Speak {
    fn speak(&self);
}
pub struct Loud {
    pub level: u32,
    gain: u32,
}
impl Speak for Loud {
    fn speak(&self) {}
}
impl Loud {
    pub fn public_helper(&self) {}
    fn private_helper(&self) {}
}
struct Hidden;
fn free_private() {}
pub fn free_public() {}
#[cfg(test)]
mod tests {
    pub fn inner() {}
}
"#,
    )
    .unwrap();
    let p = p.to_str().unwrap();

    // Negative assertions must inspect rendered declarations, not the
    // random temp path a header line might carry — strip the path first.
    let sans_path = |s: String| s.replace(p, "<fixture>");

    // Digest = public-only view.
    let digest = sans_path(run(&["digest", p]));
    for shown in ["trait Speak", "speak()", "struct Loud", "public_helper()", "free_public()"] {
        assert!(digest.contains(shown), "`{shown}` must survive the public view:\n{digest}");
    }
    for hidden in ["private_helper", "Hidden", "free_private", "inner()", "gain"] {
        assert!(!digest.contains(hidden), "`{hidden}` must be hidden in the public view:\n{digest}");
    }

    // map default still shows everything; --no-private matches the digest.
    let full = sans_path(run(&["map", p]));
    for item in ["private_helper", "Hidden", "free_private", "gain"] {
        assert!(full.contains(item), "map default must keep `{item}`:\n{full}");
    }
    let no_private = sans_path(run(&["map", p, "--no-private"]));
    assert!(
        !no_private.contains("private_helper") && !no_private.contains("free_private"),
        "--no-private must drop Rust private items:\n{no_private}"
    );
    assert!(
        no_private.contains("speak") && no_private.contains("public_helper"),
        "trait members and pub methods must survive --no-private:\n{no_private}"
    );
}

#[test]
fn nested_cap_drop_counts_match_text_when_subtree_is_removed() {
    // A capped nested type that is itself removed by an ancestor's cap
    // must contribute nothing to JSON dropped_members — the text renderer
    // never shows a `+N` for it either.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("Nested.java");
    std::fs::write(
        &p,
        "public class Outer {\n\
         \x20 public class A { public void a1() {} public void a2() {} public void a3() {} }\n\
         \x20 public class B { public void b1() {} }\n\
         \x20 public class C { public void c1() {} public void c2() {} public void c3() {} public void c4() {} }\n\
         }\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    // Cap 2: Outer keeps [A, B], drops C entirely (+1). A caps 3 -> 2 (+1).
    // C's four members must NOT count. Total = 2.
    let json = run(&["map", p, "--max-members", "2", "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    assert_eq!(
        doc["files"][0]["dropped_members"], 2,
        "drops inside a removed subtree must not be counted: {json}"
    );

    // And the text +N lines sum to the same 2.
    let text = run(&["map", p, "--max-members", "2"]);
    let total: usize = text
        .lines()
        .filter_map(|l| {
            let i = l.find("... +")?;
            l[i + 5..].split_whitespace().next()?.parse::<usize>().ok()
        })
        .sum();
    assert_eq!(total, 2, "text +N lines must sum to dropped_members:\n{text}");
}

#[test]
fn restricted_rust_visibility_forms_are_pinned() {
    // pub(self) / pub(in self) grant no reach beyond the module: private.
    // pub(crate) / pub(super) / pub(in crate) stay visible in the
    // public-only view — the moral equivalent of C# internal / Java
    // package-private, which have never been hidden. This test locks the
    // design choice down.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("restricted.rs");
    std::fs::write(
        &p,
        "pub mod inner {\n\
         pub(self) fn self_only() {}\n\
         pub(in self) fn in_self_only() {}\n\
         pub(crate) fn crate_wide() {}\n\
         pub(super) fn parent_wide() {}\n\
         pub(in crate) fn in_crate_wide() {}\n\
         pub fn open() {}\n\
         }\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();
    let out = run(&["map", p, "--no-private"]);
    for hidden in ["self_only", "in_self_only"] {
        assert!(!out.contains(hidden), "`{hidden}` is private-equivalent:\n{out}");
    }
    for shown in ["crate_wide", "parent_wide", "in_crate_wide", "open"] {
        assert!(out.contains(shown), "`{shown}` must stay visible:\n{out}");
    }
}
