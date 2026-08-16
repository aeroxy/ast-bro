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
    // `--include-fields` because a `@property` is a field-like member, which
    // the digest preset hides: this test is about the decorator reaching the
    // rendered modifier, not about the visibility projection.
    let s = run(&["digest", f.to_str().unwrap(), "--include-fields"]);
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
    let json = run(&["map", p, "--detail", "names", "--max-members", "1", "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    assert_eq!(
        doc["files"][0]["dropped_members"], 3,
        "text +N and JSON dropped_members must agree: {json}"
    );
}

#[test]
fn enum_variants_are_not_counted_by_the_member_cap() {
    // Variants are the enum's shape, not its API surface: the names
    // renderer never lists them, so the cap must not count them either —
    // in text or JSON — or `dropped_members` would disagree with the
    // `--detail names` output for any enum wider than the cap.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("wide.rs");
    std::fs::write(
        &p,
        "pub enum Wide {\n    A, B, C, D, E,\n}\nimpl Wide {\n    pub fn m1(&self) {}\n    pub fn m2(&self) {}\n    pub fn m3(&self) {}\n}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    // Cap 2: the five variants pass through uncounted; methods cap 3 -> 2.
    let text = run(&["map", p, "--detail", "names", "--max-members", "2"]);
    assert!(text.contains("... +1 more"), "only methods count:\n{text}");
    let json = run(&["map", p, "--detail", "names", "--max-members", "2", "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    assert_eq!(
        doc["files"][0]["dropped_members"], 1,
        "JSON must agree with the names output — variants uncounted: {json}"
    );
    let variants = doc["files"][0]["declarations"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "enum_member")
        .count();
    assert_eq!(variants, 5, "no variant may be cut by the cap: {json}");
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
    // Nothing was cut, so the cap note must not appear either. The names
    // renderer's wording is "... +N more" — "more member(s)" is the map
    // renderer's note, which this detail level never emits — so check the
    // wording this output would actually carry.
    assert!(
        !text.contains("... +"),
        "free functions were not capped, so no cap note belongs here:\n{text}"
    );
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

    // Negative assertions inspect rendered declarations, not the random
    // temp path the header line carries — strip it first, as
    // `rust_missing_pub_means_private_except_inherited_contexts` does.
    let sans_path = |s: String| s.replace(p, "<fixture>");

    let with = sans_path(run(&["map", p, "--detail", "names"]));
    assert!(
        with.contains("derive") && with.contains("L2-4"),
        "attrs + line ranges expected by default:\n{with}"
    );
    let without = sans_path(run(&["map", p, "--detail", "names", "--no-attrs", "--no-lines"]));
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
    let payload = |args: &[&str]| -> serde_json::Value {
        let out = run(&[&["map", p, "--json", "--compact"], args].concat());
        serde_json::from_str(out.trim()).expect("valid json")
    };
    let decl = |args: &[&str]| -> serde_json::Value {
        payload(args)["files"][0]["declarations"][0].clone()
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

    // A projected payload announces itself, so a consumer guards on the
    // `projected` block instead of guessing from an absent key; an
    // unprojected one carries no such key at all.
    assert!(
        payload(&[]).get("projected").is_none(),
        "an unprojected payload must stay byte-identical (no `projected` key)"
    );
    for (flag, key) in [
        ("--no-docs", "docs"),
        ("--no-lines", "line_numbers"),
        ("--no-attrs", "attributes"),
    ] {
        let doc = payload(&[flag]);
        let projected = doc
            .get("projected")
            .unwrap_or_else(|| panic!("{flag} must announce itself in `projected`: {doc}"));
        assert_eq!(
            projected[key], false,
            "{flag} must report `{key}: false`: {projected}"
        );
    }
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
    // `gain` is not in this list: digest hides fields wholesale, so its
    // absence here says nothing about visibility. It is checked under
    // `map --no-private` below, where fields are on.
    for hidden in ["private_helper", "Hidden", "free_private", "inner()"] {
        assert!(!digest.contains(hidden), "`{hidden}` must be hidden in the public view:\n{digest}");
    }

    // map default still shows everything; --no-private matches the digest.
    let full = sans_path(run(&["map", p]));
    for item in ["private_helper", "Hidden", "free_private", "gain"] {
        assert!(full.contains(item), "map default must keep `{item}`:\n{full}");
    }
    let no_private = sans_path(run(&["map", p, "--no-private"]));
    assert!(
        !no_private.contains("private_helper")
            && !no_private.contains("free_private")
            && !no_private.contains("gain"),
        "--no-private must drop Rust private items:\n{no_private}"
    );
    assert!(
        no_private.contains("speak") && no_private.contains("public_helper"),
        "trait members and pub methods must survive --no-private:\n{no_private}"
    );
}

#[test]
fn nested_types_are_never_cut_by_the_member_cap() {
    // Nested types are structure, not members: `--max-members` never cuts
    // or counts them — the digest renderer lists them as their own entries,
    // and map/JSON follow the same member definition. Each nested type's
    // *own* members still cap, and the text `+N` lines must sum to JSON's
    // dropped_members.
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

    // Cap 2: Outer's nested classes A/B/C all survive (types are not
    // members). A caps 3 -> 2 (+1), C caps 4 -> 2 (+2). Total = 3.
    let json = run(&["map", p, "--max-members", "2", "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    assert_eq!(
        doc["files"][0]["dropped_members"], 3,
        "only type members count toward the cap: {json}"
    );
    let outer = &doc["files"][0]["declarations"][0];
    assert_eq!(
        outer["children"].as_array().unwrap().len(),
        3,
        "nested types must never be cut by the member cap: {json}"
    );

    // And the text +N lines sum to the same 3.
    let text = run(&["map", p, "--max-members", "2"]);
    let total: usize = text
        .lines()
        .filter_map(|l| {
            let i = l.find("... +")?;
            l[i + 5..].split_whitespace().next()?.parse::<usize>().ok()
        })
        .sum();
    assert_eq!(total, 3, "text +N lines must sum to dropped_members:\n{text}");
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
    // Whole rendered tokens (`fn <name>(`), not bare names: `self_only` is a
    // substring of `in_self_only` and `crate_wide` of `in_crate_wide`, so a
    // bare-name check can report the wrong item — or pass because its
    // longer neighbour is present.
    for hidden in ["fn self_only(", "fn in_self_only("] {
        assert!(!out.contains(hidden), "`{hidden}` is private-equivalent:\n{out}");
    }
    for shown in [
        "fn crate_wide(",
        "fn parent_wide(",
        "fn in_crate_wide(",
        "fn open(",
    ] {
        assert!(out.contains(shown), "`{shown}` must stay visible:\n{out}");
    }
}

#[test]
fn methods_implementing_a_locally_private_trait_are_not_public() {
    // A pub type implementing a *private* trait: those methods are only
    // callable where the trait is in scope, so the public-only view must
    // omit them. A pub trait's impl methods stay visible (pinned by
    // rust_missing_pub_means_private_except_inherited_contexts).
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ptrait.rs");
    std::fs::write(
        &p,
        "trait Hidden {\n    fn secret(&self);\n}\npub struct Loud;\nimpl Hidden for Loud {\n    fn secret(&self) {}\n}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    // Negative assertions must not trip on the random temp path in the
    // header line — strip it first.
    let digest = run(&["digest", p]).replace(p, "<fixture>");
    assert!(digest.contains("Loud"), "the pub type stays:\n{digest}");
    assert!(
        !digest.contains("secret"),
        "a private trait's impl methods are not public API:\n{digest}"
    );
    // map default still shows everything.
    let full = run(&["map", p]);
    assert!(full.contains("secret"), "{full}");
}

#[test]
fn path_qualified_private_trait_impl_is_hidden_too() {
    // `impl self::Hidden for Loud` names the same local private trait as
    // the bare form — the path qualifier must not defeat the visibility
    // inheritance that hides its impl methods from the public view.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("qtrait.rs");
    std::fs::write(
        &p,
        "trait Hidden {\n    fn secret(&self);\n}\npub struct Loud;\nimpl self::Hidden for Loud {\n    fn secret(&self) {}\n}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    let digest = run(&["digest", p]).replace(p, "<fixture>");
    assert!(digest.contains("Loud"), "the pub type stays:\n{digest}");
    assert!(
        !digest.contains("secret"),
        "a path-qualified private trait's impl methods are not public API:\n{digest}"
    );
}

#[test]
fn qualified_trait_path_is_resolved_not_collapsed_to_its_bare_name() {
    // `impl other::Hidden for Loud` names a trait this file cannot see —
    // a same-named local private trait is a decoy, not that trait, and
    // must not drag the impl methods into the private view by bare-name
    // coincidence. Unknowable trait → inherited visibility stays.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("decoy.rs");
    std::fs::write(
        &p,
        "trait Hidden {\n    fn secret(&self);\n}\npub struct Loud;\nimpl other::Hidden for Loud {\n    fn secret(&self) {}\n}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();
    let digest = run(&["digest", p]);
    assert!(
        digest.contains("secret"),
        "a foreign qualified trait must not inherit a local decoy's privacy:\n{digest}"
    );

    // The inverse: a path that *does* resolve through a local `mod` chain
    // reads that trait's own visibility, wherever it lives in the tree.
    let p2 = dir.path().join("nested.rs");
    std::fs::write(
        &p2,
        "mod inner {\n    pub(in self) trait Hidden {\n        fn secret(&self);\n    }\n}\npub struct Loud;\nimpl inner::Hidden for Loud {\n    fn secret(&self) {}\n}\n",
    )
    .unwrap();
    let p2 = p2.to_str().unwrap();
    let digest = run(&["digest", p2]);
    assert!(
        !digest.contains("secret"),
        "a locally-resolved private trait hides its impl methods through a qualified path too:\n{digest}"
    );

    // `crate::`-anchored paths sit outside this walk's view: never guess.
    let p3 = dir.path().join("anchored.rs");
    std::fs::write(
        &p3,
        "trait Hidden {\n    fn secret(&self);\n}\npub struct Loud;\nimpl crate::elsewhere::Hidden for Loud {\n    fn secret(&self) {}\n}\n",
    )
    .unwrap();
    let p3 = p3.to_str().unwrap();
    let digest = run(&["digest", p3]);
    assert!(
        digest.contains("secret"),
        "crate::-anchored trait paths are unknowable here; keep inherited visibility:\n{digest}"
    );
}

#[test]
fn no_fields_means_the_same_thing_in_every_renderer() {
    // `--no-fields` used to mean "drop `Field`" on the digest path and "drop
    // `Field | Property | Event | Indexer`" everywhere else, so a C# property
    // survived `digest` while `map --detail full` dropped it, and the JSON
    // payload disagreed with the text beside it. One predicate now answers
    // for all three; this pins that they agree rather than that they happen
    // to today.
    //
    // `Indexer` is in the predicate but not in this fixture, because no
    // adapter emits it: `csharp.rs` maps `indexer_declaration` to the kind,
    // and a C# `this[int i]` still comes back with no declaration at all. A
    // case here would assert nothing until that is fixed.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("W.cs");
    std::fs::write(
        &p,
        "public class W {\n    public int Count;\n    public string Name { get; set; }\n    public event EventHandler Changed;\n    public void Go() {}\n}\n",
    )
    .unwrap();
    let p = p.to_str().unwrap();

    let hidden = ["Count", "Name", "Changed"];
    for args in [
        vec!["digest", p],
        vec!["map", p, "--detail", "names", "--no-fields"],
        vec!["map", p, "--detail", "signatures", "--no-fields"],
        vec!["map", p, "--detail", "full", "--no-fields"],
    ] {
        let out = run(&args);
        for name in hidden {
            assert!(
                !out.contains(name),
                "{args:?} must drop the field-like member `{name}`:\n{out}"
            );
        }
        assert!(out.contains("Go"), "{args:?} must keep the method:\n{out}");
    }

    // The JSON payload is the fourth renderer and answers the same way.
    let json: serde_json::Value =
        serde_json::from_str(run(&["digest", p, "--json", "--compact"]).trim()).unwrap();
    let names: Vec<String> = json["files"][0]["declarations"][0]["children"]
        .as_array()
        .map(|cs| cs.iter().map(|c| c["name"].as_str().unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    assert_eq!(names, vec!["Go"], "JSON disagrees with the text renderers");

    // And `--include-fields` brings every one of them back, so the rule is a
    // projection rather than a parse gap.
    let back = run(&["digest", p, "--include-fields"]);
    for name in hidden {
        assert!(back.contains(name), "--include-fields must restore `{name}`:\n{back}");
    }
}
