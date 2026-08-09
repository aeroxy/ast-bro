//! Per-language chunker coverage.
//!
//! The kind predicates in the parent module match tree-sitter node names by
//! suffix or substring across every grammar `ast-grep` parses, so a language
//! that spells a container differently loses member granularity with nothing
//! to catch it. These fixtures are what catches it.
//!
//! Two tables feed the assertions. [`FIXTURES`] holds one file per
//! `SupportLang`, built from six members each comfortably under [`MAX_CHARS`]
//! and adding up to well over it; the table is checked against
//! `SupportLang::all_langs()`, so a language added to `ast-grep` without a
//! fixture here fails the build rather than shipping unmeasured. [`VARIANTS`]
//! holds a second spelling for languages that have one, because a single
//! fixture measures the style that fixture happens to use — Ruby passed on
//! `class` plus `def` while `def self.x`, `class << self`, a `do` block holding
//! members, and four levels of nesting all still collapsed.
//!
//! [`KNOWN_GAPS`] names the languages that still collapse, with the cause.
//! An entry there is not a waiver: `known_gaps_still_fail` asserts each one
//! still reproduces, so repairing a language turns that test red and forces
//! its entry out alongside the fix.
//!
//! Data formats have no members to find and assert chunk size only.
use super::*;

/// What a fixture is expected to yield beyond small chunks.
enum Expect {
    /// The member walk must find at least this many declarations.
    Members(usize),
    /// A data format: split points come from top-level children, not members.
    SplitsOnly,
}

struct Fixture {
    lang: SupportLang,
    /// Whole file. `{body}` is replaced by the concatenated members.
    file: &'static str,
    /// One member. `{i}` is the index, `{pad}` the generated padding.
    member: &'static str,
    /// One padding line inside a member body. `{j}` is the line index.
    pad_line: &'static str,
    expect: Expect,
}

const MEMBERS: usize = 6;
const PAD_LINES: usize = 12;

fn render(f: &Fixture) -> String {
    let body: String = (0..MEMBERS)
        .map(|i| {
            let pad: String = (0..PAD_LINES)
                .map(|j| f.pad_line.replace("{j}", &j.to_string()))
                .collect();
            f.member
                .replace("{pad}", &pad)
                .replace("{i}", &i.to_string())
        })
        .collect();
    f.file.replace("{body}", &body)
}

#[rustfmt::skip]
const FIXTURES: &[Fixture] = &[
    Fixture { lang: SupportLang::Bash, file: "{body}",
        member: "m{i}() {\n{pad}  echo {i}\n}\n\n",
        pad_line: "  # padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::C, file: "{body}",
        member: "void m{i}(void) {\n{pad}}\n\n",
        pad_line: "    // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Cpp, file: "class A {\npublic:\n{body}};\n",
        member: "    void m{i}() {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::CSharp, file: "namespace N;\npublic class A {\n{body}}\n",
        member: "    void M{i}() {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Css, file: "{body}",
        member: ".c{i} {\n{pad}  color: red;\n}\n\n",
        pad_line: "  /* padding line {j} that exists only to grow this rule */\n",
        expect: Expect::SplitsOnly },
    Fixture { lang: SupportLang::Dart, file: "class A {\n{body}}\n",
        member: "  void m{i}() {\n{pad}  }\n",
        pad_line: "    // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Elixir, file: "defmodule A do\n{body}end\n",
        member: "  def m{i}(x) do\n{pad}    x\n  end\n",
        pad_line: "    # padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Go, file: "package a\n\n{body}",
        member: "func M{i}() {\n{pad}}\n\n",
        pad_line: "    // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Haskell, file: "module A where\n\n{body}",
        member: "m{i} :: Int -> Int\nm{i} x =\n{pad}    x\n\n",
        pad_line: "    -- padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Hcl, file: "{body}",
        member: "resource \"null_resource\" \"m{i}\" {\n{pad}  count = 1\n}\n\n",
        pad_line: "  # padding line {j} that exists only to grow this block\n",
        expect: Expect::SplitsOnly },
    Fixture { lang: SupportLang::Html, file: "<html><body>\n{body}</body></html>\n",
        member: "<div id=\"m{i}\">\n{pad}</div>\n",
        pad_line: "  <!-- padding line {j} that exists only to grow this node -->\n",
        expect: Expect::SplitsOnly },
    Fixture { lang: SupportLang::Java, file: "public class A {\n{body}}\n",
        member: "    void m{i}() {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::JavaScript, file: "class A {\n{body}}\n",
        member: "  m{i}() {\n{pad}  }\n",
        pad_line: "    // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Json, file: "{\n{body}  \"tail\": 1\n}\n",
        member: "  \"m{i}\": {\n{pad}    \"v\": 1\n  },\n",
        pad_line: "    \"p{j}\": \"padding value that exists only to grow this object\",\n",
        expect: Expect::SplitsOnly },
    Fixture { lang: SupportLang::Kotlin, file: "class A {\n{body}}\n",
        member: "    fun m{i}() {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Lua, file: "{body}",
        member: "function m{i}(x)\n{pad}  return x\nend\n\n",
        pad_line: "  -- padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Nix, file: "{\n{body}}\n",
        member: "  m{i} = {\n{pad}    v = 1;\n  };\n",
        pad_line: "    # padding line {j} that exists only to grow this set\n",
        expect: Expect::SplitsOnly },
    Fixture { lang: SupportLang::Php, file: "<?php\nclass A {\n{body}}\n",
        member: "    function m{i}() {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Python, file: "class A:\n{body}",
        member: "    def m{i}(self):\n{pad}        pass\n",
        pad_line: "        # padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Ruby, file: "class A\n{body}end\n",
        member: "  def m{i}\n{pad}  end\n",
        pad_line: "    # padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Rust, file: "pub struct A;\n\nimpl A {\n{body}}\n",
        member: "    pub fn m{i}(&self) {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Scala, file: "class A {\n{body}}\n",
        member: "  def m{i}(): Unit = {\n{pad}  }\n",
        pad_line: "    // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Solidity, file: "contract A {\n{body}}\n",
        member: "    function m{i}() public {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Swift, file: "class A {\n{body}}\n",
        member: "    func m{i}() {\n{pad}    }\n",
        pad_line: "        // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Tsx, file: "{body}",
        member: "export function M{i}() {\n{pad}  return <div />;\n}\n\n",
        pad_line: "  // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::TypeScript, file: "export class A {\n{body}}\n",
        member: "  m{i}(): void {\n{pad}  }\n",
        pad_line: "    // padding line {j} that exists only to grow this body\n",
        expect: Expect::Members(MEMBERS) },
    Fixture { lang: SupportLang::Yaml, file: "{body}",
        member: "m{i}:\n{pad}  v: 1\n",
        pad_line: "  p{j}: padding value that exists only to grow this entry\n",
        expect: Expect::SplitsOnly },
];

/// Adding a language to `ast-grep` without a fixture here is a build failure,
/// not a silently unmeasured language.
#[test]
fn every_supported_language_has_a_fixture() {
    let missing: Vec<String> = SupportLang::all_langs()
        .iter()
        .filter(|l| !FIXTURES.iter().any(|f| f.lang == **l))
        .map(|l| format!("{l:?}"))
        .collect();
    assert!(
        missing.is_empty(),
        "no chunker fixture for {missing:?} — add one to FIXTURES"
    );
}

/// Languages the chunker does not handle yet, and what stops it.
///
/// None of these has a `map` adapter: `ast-grep` parses them, but they are not
/// first-class anywhere else in the tool, so the collapse costs less than the
/// per-grammar special-casing a fix would take.
///
/// An entry is not a waiver. `known_gaps_still_fail` asserts every one of them
/// still fails, so fixing a language turns that test red and the entry has to
/// be deleted along with the fix.
const KNOWN_GAPS: &[(SupportLang, &str)] = &[
    (
        SupportLang::Dart,
        "a method is a `class_member` wrapping sibling `method_signature` and \
         `function_body` nodes, so no single node spans the member",
    ),
    (
        SupportLang::Elixir,
        "every form is a `call` carrying a `do_block`; `def` is indistinguishable \
         from any other macro call by node kind alone",
    ),
    (
        SupportLang::Haskell,
        "top-level `declarations` is not a container, and a definition is split \
         into sibling `signature` and `function` nodes",
    ),
    (
        SupportLang::Hcl,
        "the parse root holds one `body` node, so blocks sit at depth 1 where \
         only member kinds qualify",
    ),
    (
        SupportLang::Html,
        "the parse root holds one `element`, so nested elements sit at depth 1 \
         where only member kinds qualify",
    ),
    (
        SupportLang::Json,
        "the parse root holds one `object`, so pairs sit at depth 1 where only \
         member kinds qualify",
    ),
    (
        SupportLang::Nix,
        "the parse root holds one `attrset_expression`, so bindings sit at depth \
         1 where only member kinds qualify",
    ),
    (
        SupportLang::Yaml,
        "the parse root holds one `document`, so mapping entries sit at depth 1 \
         where only member kinds qualify",
    ),
];

fn is_known_gap(lang: SupportLang) -> bool {
    KNOWN_GAPS.iter().any(|(l, _)| *l == lang)
}

/// Property 1: the member walk sees every declaration the fixture declares.
fn member_failure(f: &Fixture) -> Option<String> {
    let Expect::Members(want) = f.expect else {
        return None;
    };
    let found = build_split_plan(&render(f), f.lang).members.len();
    (found < want).then(|| format!("{:?}: found {found} members, want >= {want}", f.lang))
}

/// Property 2: no `Source` chunk swallows the whole file.
fn split_failure(f: &Fixture) -> Option<String> {
    let src = render(f);
    let limit = MAX_CHARS + src.len() / MEMBERS;
    let chunks = chunk_source(&src, "fixture", ChunkerKind::AstGrep(f.lang));
    let worst = chunks
        .iter()
        .filter(|c| c.kind == ChunkKind::Source)
        .map(|c| c.content.len())
        .max()
        .unwrap_or(0);
    (worst > limit).then(|| {
        format!(
            "{:?}: largest Source chunk is {worst} chars of a {} char file (limit {limit}, {} chunks)",
            f.lang,
            src.len(),
            chunks.len()
        )
    })
}

/// The member walk must see every declaration the fixture declares.
///
/// Zero members means the grammar's node kinds fall outside `is_member_kind`
/// and `is_member_container_kind`. The file then has no outline, no
/// breadcrumbs, and no member-level split points.
#[test]
fn every_language_finds_its_members() {
    let failures: Vec<String> = FIXTURES
        .iter()
        .filter(|f| !is_known_gap(f.lang))
        .filter_map(member_failure)
        .collect();
    assert!(failures.is_empty(), "member walk incomplete:\n  {}", failures.join("\n  "));
}

/// No `Source` chunk may swallow the whole file.
///
/// Each fixture member is well under `MAX_CHARS`, so a correct split never
/// exceeds the target by more than the member that crossed it. A language
/// whose container the walk cannot open produces one chunk holding everything,
/// which is what this catches.
#[test]
fn every_language_splits_a_large_file() {
    let failures: Vec<String> = FIXTURES
        .iter()
        .filter(|f| !is_known_gap(f.lang))
        .filter_map(split_failure)
        .collect();
    assert!(failures.is_empty(), "chunks not split:\n  {}", failures.join("\n  "));
}

/// Every `KNOWN_GAPS` entry still reproduces. Fixing one fails here on purpose.
#[test]
fn known_gaps_still_fail() {
    let fixed: Vec<&str> = KNOWN_GAPS
        .iter()
        .filter(|(lang, _)| {
            let f = FIXTURES.iter().find(|f| f.lang == *lang).expect("gap without a fixture");
            member_failure(f).is_none() && split_failure(f).is_none()
        })
        .map(|(_, reason)| *reason)
        .collect();
    assert!(
        fixed.is_empty(),
        "these languages now chunk correctly — delete their KNOWN_GAPS entries:\n  {}",
        fixed.join("\n  ")
    );
}


/// One oversized callable, in a shape a real codebase uses.
///
/// `{pad}` is replaced by statement-sized filler that puts the body past
/// `MAX_CHARS`. Only languages with a `map` adapter appear here: these are the
/// shapes users actually search for.
struct Shape {
    label: &'static str,
    lang: SupportLang,
    src: &'static str,
}

/// Enough lines that even the tersest statement style clears `MAX_CHARS`.
const SHAPE_PAD_LINES: usize = 70;

fn render_shape(s: &Shape, pad_line: &str) -> String {
    let pad: String = (0..SHAPE_PAD_LINES)
        .map(|j| pad_line.replace("{j}", &j.to_string()))
        .collect();
    s.src.replace("{pad}", &pad)
}

/// Padding must be real statements, not comments. Several grammars attach a
/// leading comment to the *declaration* rather than to its body, so a body of
/// comments has nothing for the statement walk to split on — an artifact of the
/// fixture, not of the chunker.
const CISH: &str = "        int pad{j} = {j}; // grows the body\n";
const TSISH: &str = "        let pad{j} = {j}; // grows the body\n";
const GOISH: &str = "        pad{j} := {j}; _ = pad{j}\n";
const RSISH: &str = "        let pad{j} = {j}; // grows the body\n";
const PYISH: &str = "        pad{j} = {j}  # grows the body\n";
const RBISH: &str = "        pad{j} = {j} # grows the body\n";
const PHPISH: &str = "        $pad{j} = {j}; // grows the body\n";
const KTISH: &str = "        val pad{j} = {j} // grows the body\n";

#[rustfmt::skip]
fn shapes() -> Vec<(Shape, &'static str)> {
    vec![
        (Shape { label: "C# method", lang: SupportLang::CSharp,
            src: "namespace N;\nclass A {\n  public void M() {\n{pad}  }\n}\n" }, CISH),
        (Shape { label: "C# constructor", lang: SupportLang::CSharp,
            src: "namespace N;\nclass A {\n  public A() {\n{pad}  }\n}\n" }, CISH),
        (Shape { label: "C# operator", lang: SupportLang::CSharp,
            src: "namespace N;\nclass A {\n  public static A operator +(A a, A b) {\n{pad}    return a;\n  }\n}\n" }, CISH),
        (Shape { label: "C# indexer", lang: SupportLang::CSharp,
            src: "namespace N;\nclass A {\n  public int this[int i] {\n    get {\n{pad}      return i;\n    }\n  }\n}\n" }, CISH),
        (Shape { label: "C# property accessor", lang: SupportLang::CSharp,
            src: "namespace N;\nclass A {\n  public int P {\n    get {\n{pad}      return 1;\n    }\n  }\n}\n" }, CISH),
        (Shape { label: "Java method", lang: SupportLang::Java,
            src: "public class A {\n  void m() {\n{pad}  }\n}\n" }, CISH),
        (Shape { label: "Java constructor", lang: SupportLang::Java,
            src: "public class A {\n  public A() {\n{pad}  }\n}\n" }, CISH),
        (Shape { label: "TS method", lang: SupportLang::TypeScript,
            src: "export class A {\n  m(): void {\n{pad}  }\n}\n" }, TSISH),
        (Shape { label: "TS exported function", lang: SupportLang::TypeScript,
            src: "export function f(): void {\n{pad}}\n" }, TSISH),
        (Shape { label: "TS arrow const", lang: SupportLang::TypeScript,
            src: "export const f = (): void => {\n{pad}};\n" }, TSISH),
        (Shape { label: "Go function", lang: SupportLang::Go,
            src: "package a\n\nfunc F() {\n{pad}}\n" }, GOISH),
        (Shape { label: "Go method", lang: SupportLang::Go,
            src: "package a\n\ntype A struct{}\n\nfunc (a *A) M() {\n{pad}}\n" }, GOISH),
        (Shape { label: "Rust method", lang: SupportLang::Rust,
            src: "pub struct A;\n\nimpl A {\n  pub fn m(&self) {\n{pad}  }\n}\n" }, RSISH),
        (Shape { label: "Python method", lang: SupportLang::Python,
            src: "class A:\n    def m(self):\n{pad}        pass\n" }, PYISH),
        (Shape { label: "Ruby method", lang: SupportLang::Ruby,
            src: "class A\n  def m\n{pad}  end\nend\n" }, RBISH),
        (Shape { label: "Kotlin method", lang: SupportLang::Kotlin,
            src: "class A {\n  fun m() {\n{pad}  }\n}\n" }, KTISH),
        (Shape { label: "Scala method", lang: SupportLang::Scala,
            src: "class A {\n  def m(): Unit = {\n{pad}  }\n}\n" }, KTISH),
        (Shape { label: "PHP method", lang: SupportLang::Php,
            src: "<?php\nclass A {\n  function m() {\n{pad}  }\n}\n" }, PHPISH),
        (Shape { label: "C++ method", lang: SupportLang::Cpp,
            src: "class A {\npublic:\n  void m() {\n{pad}  }\n};\n" }, CISH),
    ]
}

/// An oversized callable must be split into `Part` chunks beside the whole.
///
/// Without this the member is one chunk larger than `MAX_EMBED_CHARS`, whose
/// vector is dropped from dense retrieval, so only BM25 can reach it.
#[test]
fn an_oversized_callable_is_split_into_parts() {
    let mut failures = Vec::new();
    for (s, pad_line) in shapes() {
        let src = render_shape(&s, pad_line);
        // Guard the fixture, not the chunker: a shape whose member stopped
        // clearing MAX_CHARS would pass the real assertion for the wrong reason.
        let biggest = build_split_plan(&src, s.lang)
            .members
            .iter()
            .map(|m| m.end - m.start)
            .max()
            .unwrap_or(0);
        assert!(
            biggest > MAX_CHARS,
            "{}: largest member is {biggest} chars, under MAX_CHARS — grow SHAPE_PAD_LINES",
            s.label
        );
        let chunks = chunk_source(&src, "fixture", ChunkerKind::AstGrep(s.lang));
        let parts = chunks.iter().filter(|c| c.kind == ChunkKind::Part).count();
        if parts < 2 {
            failures.push(format!(
                "{}: {parts} Part chunk(s) for a {} char body ({} chunks total)",
                s.label,
                src.len(),
                chunks.len()
            ));
        }
    }
    assert!(failures.is_empty(), "oversized callables not split:\n  {}", failures.join("\n  "));
}


/// A second way the same language spells a container or a member.
///
/// One fixture per language measures the style that fixture happens to use.
/// Ruby shipped "fixed" on `class` + `def` while `def self.x`, `class << self`,
/// a `do` block holding members, and four levels of nesting all still collapsed
/// into one chunk — the table below is what would have caught that.
struct Variant {
    label: &'static str,
    lang: SupportLang,
    /// A whole file. `{pad}` is replaced per member; `{i}` is the index.
    src: &'static str,
}

fn render_variant(v: &Variant, members: usize, pad_lines: usize, pad_line: &str) -> String {
    let one: String = (0..members)
        .map(|i| {
            let pad: String = (0..pad_lines)
                .map(|j| pad_line.replace("{j}", &j.to_string()))
                .collect();
            v.src
                .split("{member}")
                .nth(1)
                .unwrap_or("")
                .replace("{pad}", &pad)
                .replace("{i}", &i.to_string())
        })
        .collect();
    let (head, tail) = v.src.split_once("{member}").unwrap();
    let tail = tail.split_once("{member}").map(|(_, t)| t).unwrap_or("");
    format!("{head}{one}{tail}")
}

#[rustfmt::skip]
fn variants() -> Vec<(Variant, &'static str)> {
    vec![
        (Variant { label: "Ruby def self.", lang: SupportLang::Ruby,
            src: "class A\n{member}  def self.m{i}\n{pad}  end\n{member}end\n" }, RBISH),
        (Variant { label: "Ruby class << self", lang: SupportLang::Ruby,
            src: "class A\n  class << self\n{member}    def m{i}\n{pad}    end\n{member}  end\nend\n" }, RBISH),
        (Variant { label: "Ruby nested 4 deep", lang: SupportLang::Ruby,
            src: "module O\n  module I\n    class M\n      class D\n{member}        def m{i}\n{pad}        end\n{member}      end\n    end\n  end\nend\n" }, RBISH),
        (Variant { label: "Ruby do-block members", lang: SupportLang::Ruby,
            src: "S = Struct.new(:x) do\n{member}  def m{i}\n{pad}  end\n{member}end\n" }, RBISH),
        (Variant { label: "C++ extern \"C\"", lang: SupportLang::Cpp,
            src: "extern \"C\" {\n{member}void m{i}(void) {\n{pad}}\n{member}}\n" }, CISH),
        (Variant { label: "C# nested namespace", lang: SupportLang::CSharp,
            src: "namespace N {\n  namespace M {\n    class A {\n{member}      void M{i}() {\n{pad}      }\n{member}    }\n  }\n}\n" }, CISH),
        (Variant { label: "Python nested class", lang: SupportLang::Python,
            src: "class Outer:\n    class Inner:\n{member}        def m{i}(self):\n{pad}            pass\n{member}" }, PYISH),
        (Variant { label: "TS namespace", lang: SupportLang::TypeScript,
            src: "export namespace N {\n  export class A {\n{member}    m{i}(): void {\n{pad}    }\n{member}  }\n}\n" }, TSISH),
    ]
}

/// Every spelling of a container must split, not just the one the main fixture
/// happens to use.
#[test]
fn every_member_shape_splits() {
    let mut failures = Vec::new();
    for (v, pad_line) in variants() {
        let src = render_variant(&v, MEMBERS, PAD_LINES, pad_line);
        let limit = MAX_CHARS + src.len() / MEMBERS;
        let chunks = chunk_source(&src, "fixture", ChunkerKind::AstGrep(v.lang));
        let worst = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::Source)
            .map(|c| c.content.len())
            .max()
            .unwrap_or(0);
        if worst > limit {
            failures.push(format!(
                "{}: largest Source chunk is {worst} chars of a {} char file (limit {limit}, {} chunks)",
                v.label,
                src.len(),
                chunks.len()
            ));
        }
    }
    assert!(failures.is_empty(), "member shapes not split:\n  {}", failures.join("\n  "));
}


/// One declaration per language, and the signature its outline entry carries.
///
/// The shape tables above assert member counts and `Part` splitting, never
/// signature *text*, which is how Go and C# shipped with their parameters cut
/// off: both name the parameter node `parameter_list` as a direct child of the
/// declaration, so a body-start rule based on kind alone fired on it.
struct SigCase {
    label: &'static str,
    lang: SupportLang,
    src: &'static str,
    /// The signature one member of `src` must carry, whitespace collapsed.
    want: &'static str,
}

#[rustfmt::skip]
fn sig_cases() -> Vec<SigCase> {
    vec![
        SigCase { label: "Java method", lang: SupportLang::Java,
            src: "public class Db {\n  public Conn connect(String host, int port, int retries) {\n    return null;\n  }\n}\n",
            want: "public Conn connect(String host, int port, int retries)" },
        SigCase { label: "Rust struct", lang: SupportLang::Rust,
            src: "pub struct Config {\n    pub host: String,\n    pub port: u16,\n}\n",
            want: "pub struct Config" },
        SigCase { label: "Rust method", lang: SupportLang::Rust,
            src: "pub struct C;\nimpl C {\n    pub fn connect(&self, host: &str, port: u16) -> Result<S, String> {\n        Ok(())\n    }\n}\n",
            want: "pub fn connect(&self, host: &str, port: u16) -> Result<S, String>" },
        SigCase { label: "Python method", lang: SupportLang::Python,
            src: "class S:\n    def connect(self, host: str, port: int) -> None:\n        pass\n",
            want: "def connect(self, host: str, port: int) -> None:" },
        SigCase { label: "TypeScript method", lang: SupportLang::TypeScript,
            src: "export class S {\n  connect(host: string, port: number): Promise<void> {\n    return null;\n  }\n}\n",
            want: "connect(host: string, port: number): Promise<void>" },
        SigCase { label: "Ruby method", lang: SupportLang::Ruby,
            src: "class S\n  def connect(host, port, retries)\n    nil\n  end\nend\n",
            want: "def connect(host, port, retries)" },
        SigCase { label: "Kotlin method", lang: SupportLang::Kotlin,
            src: "class S {\n    fun connect(host: String, port: Int): String {\n        return \"\"\n    }\n}\n",
            want: "fun connect(host: String, port: Int): String" },
        SigCase { label: "Scala method", lang: SupportLang::Scala,
            src: "class S {\n  def connect(host: String, port: Int): String = {\n    \"\"\n  }\n}\n",
            want: "def connect(host: String, port: Int): String" },
        SigCase { label: "PHP method", lang: SupportLang::Php,
            src: "<?php\nclass S {\n  function connect($host, $port) {\n    return null;\n  }\n}\n",
            want: "function connect($host, $port)" },
        SigCase { label: "C++ method", lang: SupportLang::Cpp,
            src: "namespace ns {\nclass Widget {\npublic:\n  int method0(int a, int b) {\n    return a + b;\n  }\n};\n}\n",
            want: "int method0(int a, int b)" },
        SigCase { label: "C# class", lang: SupportLang::CSharp,
            src: "namespace N;\npublic class Db {\n  public Conn Connect(string host, int port) {\n    return null;\n  }\n}\n",
            want: "public class Db" },
        SigCase { label: "C# method", lang: SupportLang::CSharp,
            src: "namespace N;\npublic class Db {\n  public Conn Connect(string host, int port) {\n    return null;\n  }\n}\n",
            want: "public Conn Connect(string host, int port)" },
        SigCase { label: "Go function", lang: SupportLang::Go,
            src: "package a\n\ntype C struct{}\n\nfunc Connect(host string, port int) (*C, error) {\n\treturn nil, nil\n}\n",
            want: "func Connect(host string, port int) (*C, error)" },
        SigCase { label: "Go method", lang: SupportLang::Go,
            src: "package a\n\ntype C struct{}\n\nfunc (c *C) Query(sql string) error {\n\treturn nil\n}\n",
            want: "func (c *C) Query(sql string) error" },
    ]
}

/// An outline entry must name the declaration and carry its parameters.
///
/// The shape tables assert member counts and `Part` splitting; neither notices
/// a signature cut in half, which is how Go shipped an entry reading `func` and
/// naming nothing. Outlines are the only input to phase-one file ranking, so a
/// language whose entries are truncated cannot earn the prior its neighbour
/// gets on the same query.
#[test]
fn every_language_keeps_its_parameters_in_the_signature() {
    let mut failures = Vec::new();
    for c in sig_cases() {
        let got: Vec<String> = build_split_plan(c.src, c.lang)
            .members
            .into_iter()
            .map(|m| m.signature)
            .collect();
        if !got.iter().any(|s| s == c.want) {
            failures.push(format!("{}: want {:?}, got {:?}", c.label, c.want, got));
        }
    }
    assert!(failures.is_empty(), "signatures truncated:\n  {}", failures.join("\n  "));
}
