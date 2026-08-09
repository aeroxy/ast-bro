//! AST-aware code chunking for indexing.
//!
//! Three strategies, picked by file extension:
//!
//! - **Anything ast-grep can parse** (bash, cpp, css, c#, dart, elixir, go,
//!   haskell, hcl, html, java, json, kotlin, lua, nix, php, python, ruby, rust,
//!   scala, solidity, swift, ts/tsx/js, yaml) — split at member boundaries via
//!   ast-grep, descending through type containers to reach them. The chunker
//!   doesn't need a per-language outline adapter; it only needs the AST root +
//!   iteration of named children.
//! - **Markdown** (`.md`/`.markdown`/`.mdx`/`.mdown`) — split at `section`
//!   boundaries via raw `tree_sitter_md`.
//! - **Plain text** (`.toml`, PowerShell `.ps1`/`.psm1`/`.psd1`) — formats
//!   with no tree-sitter grammar in `SupportLang`; split at blank-line
//!   boundaries (LF or CRLF). Files without blank lines become one oversized
//!   chunk, which the packer already tolerates. Note: sources must be UTF-8 —
//!   UTF-16-encoded `.ps1` files fail `read_to_string` and are skipped, like
//!   everywhere else in the codebase.
//!
//! Both paths feed the same greedy packer, which targets `MAX_CHARS = 1500`.
//! Files outside both sets are not chunked — `is_indexable` is the single
//! source of truth for what gets indexed.
//!
//! Output is a hierarchy rather than a partition, described by [`ChunkKind`]:
//! `Source` and `Enclosing` tile the file once, `Part` re-covers the inside of
//! a member too large to stand alone, and `Outline` synthesizes a signature
//! list per file. Only the packer's own regions are size-bounded — a member is
//! never split down the middle to hit the target.

use ast_grep_core::{AstGrep, Language};
use ast_grep_language::{LanguageExt, SupportLang};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Target chunk size in characters. Chunks may exceed this when a declaration
/// is larger even at `MAX_SPLIT_DEPTH`; they are not split mid-declaration.
pub const MAX_CHARS: usize = 1500;

/// What a chunk holds, which decides how retrieval should treat it.
///
/// `Source` and `Enclosing` together tile the file exactly once. `Part` chunks
/// sit *inside* an `Enclosing` one, so the two levels overlap on purpose — a
/// query can match the specific statement or the method that contains it.
/// `Outline` is synthesized and covers no source bytes of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChunkKind {
    /// A contiguous slice of the file, packed at member boundaries.
    #[default]
    Source,
    /// A member too large for one chunk, kept whole beside its parts.
    Enclosing,
    /// A body-level slice of an `Enclosing` member.
    Part,
    /// Signatures only, synthesized for the whole file — no body text.
    Outline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    /// Repo-relative POSIX path (caller's responsibility — the chunker only
    /// passes through whatever `file_path` it receives).
    pub file_path: String,
    /// 1-indexed inclusive line range.
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    /// Always set — language is known per `is_indexable`.
    pub language: String,
    /// Enclosing declaration names, outermost first (`Outer > handle`). Empty
    /// when nothing encloses the chunk, or for languages with no `name` field.
    #[serde(default)]
    pub breadcrumb: String,
    #[serde(default)]
    pub kind: ChunkKind,
}

/// Strategy used to chunk a given file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkerKind {
    AstGrep(SupportLang),
    Markdown,
    /// Blank-line-delimited plain text; carries the language label to report
    /// (e.g. `"toml"`, `"powershell"`).
    Plain(&'static str),
}

impl ChunkerKind {
    /// Lowercase canonical name. Uses the `SupportLang` Debug variant for
    /// ast-grep languages so we automatically pick up new languages without
    /// having to maintain a per-variant match.
    fn language_name(self) -> String {
        match self {
            ChunkerKind::AstGrep(lang) => format!("{lang:?}").to_ascii_lowercase(),
            ChunkerKind::Markdown => "markdown".to_string(),
            ChunkerKind::Plain(name) => name.to_string(),
        }
    }
}

/// Decide whether `path` is indexable, and how.
///
/// Returns `Some(Markdown)` for `.md`/`.markdown`/`.mdx`/`.mdown` (handled via
/// `tree_sitter_md`), `Some(Plain(_))` for `.toml` and PowerShell
/// `.ps1`/`.psm1`/`.psd1` (blank-line-delimited plain text),
/// `Some(AstGrep(lang))` for any extension ast-grep claims it can parse, and
/// `None` otherwise.
pub fn is_indexable(path: &Path) -> Option<ChunkerKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(ext.as_deref(), Some("md" | "markdown" | "mdx" | "mdown")) {
        return Some(ChunkerKind::Markdown);
    }
    if ext.as_deref() == Some("toml") {
        return Some(ChunkerKind::Plain("toml"));
    }
    if matches!(ext.as_deref(), Some("ps1" | "psm1" | "psd1")) {
        return Some(ChunkerKind::Plain("powershell"));
    }
    SupportLang::from_path(path).map(ChunkerKind::AstGrep)
}

/// Read `path` from disk and chunk it. Returns an empty vec on read errors or
/// for unsupported file types (callers should normally pre-filter via
/// `is_indexable`).
pub fn chunk_file(path: &Path, file_path: &str) -> Vec<Chunk> {
    let Some(kind) = is_indexable(path) else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    chunk_source(&source, file_path, kind)
}

/// Chunk pre-read source text using the given `kind`.
///
/// The result is a hierarchy, not a partition. A member too large to be one
/// chunk is emitted whole *and* split into body-level parts, so a query can
/// match either the specific statement or the method that contains it. Every
/// source byte is still covered at least once.
pub fn chunk_source(source: &str, file_path: &str, kind: ChunkerKind) -> Vec<Chunk> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let lang_name = kind.language_name();
    let lines = LineIndex::new(source);
    let lang = match kind {
        ChunkerKind::AstGrep(lang) => lang,
        ChunkerKind::Plain(_) => {
            // Blank-line paragraphs, and nothing to synthesize an outline
            // from: a TOML or PowerShell file declares no members.
            let points = paragraph_split_points(source);
            return pack(source, file_path, &lang_name, &lines, &points, ChunkKind::Source, "");
        }
        ChunkerKind::Markdown => {
            let points = markdown_split_points(source);
            let mut chunks =
                pack(source, file_path, &lang_name, &lines, &points, ChunkKind::Source, "");
            // Markdown needs outlines as much as code does, and for a sharper
            // reason: two-phase retrieval can only ever *raise* a file's score,
            // so a file with no outline cannot compete with one that has them.
            // Without this a document loses to code on queries the document
            // answers — the file type decided the ranking, not the relevance.
            let plan = markdown_heading_plan(source);
            chunks.extend(build_outline_chunks(file_path, &lang_name, &lines, &plan));
            return chunks;
        }
    };

    let plan = build_split_plan(source, lang);
    let mut chunks = pack(
        source,
        file_path,
        &lang_name,
        &lines,
        &plan.member_points,
        ChunkKind::Source,
        "",
    );

    // Level 2: the inside of each member that was too big to stand alone. The
    // member itself stays in `chunks` above, retagged as Enclosing so ranking
    // can prefer the finer-grained parts.
    for deep in &plan.deep {
        for c in chunks.iter_mut() {
            if c.start_byte as usize <= deep.start && c.end_byte as usize >= deep.end {
                c.kind = ChunkKind::Enclosing;
                c.breadcrumb = deep.breadcrumb.clone();
            }
        }
        chunks.extend(pack(
            source,
            file_path,
            &lang_name,
            &lines,
            &deep.points,
            ChunkKind::Part,
            &deep.breadcrumb,
        ));
    }

    assign_breadcrumbs(&plan, &mut chunks);

    chunks.extend(build_outline_chunks(file_path, &lang_name, &lines, &plan));
    chunks
}

/// Byte offsets of every newline in one source file, built once so a line
/// number is a binary search instead of a scan.
///
/// Counting newlines from the start of the file per chunk is O(file) per call,
/// and both the packer and the outline builder call it twice per chunk. On a
/// 1.3 MB file producing 1289 chunks that came to 0.4 s of chunking, nearly
/// all of it rescanning the same prefix.
struct LineIndex {
    newlines: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        Self {
            newlines: source
                .as_bytes()
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// 1-indexed line containing `byte`.
    fn line_of(&self, byte: usize) -> u32 {
        self.newlines.partition_point(|&pos| pos < byte) as u32 + 1
    }
}

/// A member too large for one chunk: where it is, what encloses it, and the
/// body-level boundaries its parts are packed from.
struct DeepRegion {
    start: usize,
    end: usize,
    breadcrumb: String,
    points: Vec<usize>,
}

/// One member declaration seen during the walk, used for breadcrumbs and for
/// the synthesized outline document.
struct MemberSpan {
    start: usize,
    end: usize,
    breadcrumb: String,
    signature: String,
}

#[derive(Default)]
struct SplitPlan {
    member_points: Vec<usize>,
    deep: Vec<DeepRegion>,
    members: Vec<MemberSpan>,
}

/// Walk the parse once and collect everything the chunker needs: member
/// boundaries, oversized members worth splitting further, and the member list
/// the outline document is built from.
fn build_split_plan(source: &str, lang: SupportLang) -> SplitPlan {
    let ast: AstGrep<_> = lang.ast_grep(source);
    let root = ast.root();

    let mut plan = SplitPlan {
        member_points: vec![0],
        ..SplitPlan::default()
    };
    let mut trail: Vec<String> = Vec::new();
    collect_split_points(&root, source, &mut plan, &mut trail, Descent::root(source.len()));
    if *plan.member_points.last().unwrap() < source.len() {
        plan.member_points.push(source.len());
    }
    plan
}

/// Maximum *narrowing* levels the member walk descends.
///
/// Counted in levels that actually shrink the region, not in AST nodes. A
/// language that spends nodes on structure rather than on scope would
/// otherwise exhaust the budget before reaching any member: Ruby costs two
/// nodes per scope (the `class` and its `body_statement`), so
/// `module/module/class/class` sat eight nodes deep and its methods were never
/// opened.
const MAX_SPLIT_DEPTH: u32 = 6;

/// Backstop against pathological nesting, counted in AST nodes.
///
/// A chain of pass-through wrappers costs no levels, so [`MAX_SPLIT_DEPTH`]
/// does not bound the walk on its own. Minified sources and deeply nested data
/// nest far past anything a member walk needs.
const MAX_AST_DESCENT: u32 = 64;

/// A container this close in size to the one holding it is a pass-through
/// wrapper, and costs no level.
///
/// `module Outer` wrapping `module Inner` wrapping a class tells the reader
/// nothing new about size: each holds essentially the same bytes. Only a
/// container that meaningfully narrows the region is a level, which makes the
/// budget count real structure instead of grammar bookkeeping.
const WRAPPER_KEEP_RATIO: (usize, usize) = (9, 10);

/// How far the member walk has descended, and how much it has narrowed.
#[derive(Clone, Copy)]
struct Descent {
    /// Levels that narrowed the region. Bounded by `MAX_SPLIT_DEPTH`.
    levels: u32,
    /// Raw AST nodes descended. Bounded by `MAX_AST_DESCENT`.
    ast: u32,
    /// Byte length of the last container that counted as a level.
    kept_len: usize,
}

impl Descent {
    fn root(source_len: usize) -> Self {
        Descent {
            levels: 0,
            ast: 0,
            kept_len: source_len,
        }
    }

    /// Descend into a container of `child_len` bytes.
    fn into_child(self, child_len: usize) -> Self {
        let (num, den) = WRAPPER_KEEP_RATIO;
        let narrows = child_len * den < self.kept_len * num;
        Descent {
            levels: self.levels + u32::from(narrows),
            ast: self.ast + 1,
            kept_len: if narrows { child_len } else { self.kept_len },
        }
    }

    fn exhausted(self) -> bool {
        self.levels >= MAX_SPLIT_DEPTH || self.ast >= MAX_AST_DESCENT
    }
}

/// Reports whether `kind` names a member declaration, where a chunk may start.
///
/// Tree-sitter grammars name these consistently enough across languages that
/// suffix matching beats a per-language table: Java and Go use `*_declaration`,
/// Python `*_definition`, Rust `*_item`. Ruby names a method just `method`.
///
/// Matching on suffixes rather than substrings is what keeps `method_invocation`
/// out — a substring test for `method` accepts every call site, and a split
/// would then land in the middle of an expression.
///
/// Two grammars opt out of the convention and need naming, or their types are
/// neither members nor containers and descent never opens them: Ruby calls a
/// class `class` and a module `module`, and C/C++ call a type body a
/// `*_specifier`. Both collapsed a whole file into one chunk.
fn is_member_kind(kind: &str) -> bool {
    const SUFFIXES: [&str; 3] = ["_declaration", "_definition", "_item"];
    const EXACT: [&str; 11] = [
        "method",
        "function",
        // Ruby. `def self.x` parses as `singleton_method` and `class << self`
        // as `singleton_class`; neither carries a suffix any rule matches, so a
        // class written in either style offered no split point at all.
        "class",
        "module",
        "singleton_method",
        "singleton_class",
        // TypeScript: `namespace X { … }`.
        "internal_module",
        // C/C++
        "class_specifier",
        "struct_specifier",
        "union_specifier",
        "enum_specifier",
    ];
    if kind.starts_with("local_") {
        // `local_variable_declaration` and friends carry the `_declaration`
        // suffix but are statements, not members: accepting them puts a chunk
        // boundary between two lines of a constructor body.
        return false;
    }
    SUFFIXES.iter().any(|s| kind.ends_with(s)) || EXACT.contains(&kind)
}

/// Reports whether `kind` names a callable, whose body holds statements.
///
/// Constructors and destructors count. They are named `*_declaration` rather
/// than anything containing `method`, so a name-based rule alone files them as
/// containers: descent then enters the body and splits it at statement
/// boundaries, and an oversized constructor yields no `Part` chunks.
/// C# names three more callables without any of those words. An
/// `operator_declaration` holds statements, and so does each
/// `accessor_declaration` inside a property, an indexer, or an event. Omitting
/// them files all three as containers, whose bodies split at statement level
/// with no `Part` chunks to show for it. The full kind name is matched rather
/// than a bare `operator`, which would also accept `assignment_operator` and
/// friends inside an expression.
fn is_callable_kind(kind: &str) -> bool {
    const MARKERS: [&str; 8] = [
        "function",
        "method",
        "lambda",
        "closure",
        "constructor",
        "destructor",
        "operator_declaration",
        "accessor_declaration",
    ];
    MARKERS.iter().any(|m| kind.contains(m))
}

/// Reports whether `node` binds a callable to a name instead of declaring one.
///
/// `export const handler = () => {…}` is the dominant style in TypeScript and
/// JavaScript. The member is a `lexical_declaration`, which no kind test calls
/// callable, and the body belongs to an `arrow_function` two levels down.
/// Treated as a container it yielded no `Part` chunks at all, so an oversized
/// handler stayed one chunk past `MAX_EMBED_CHARS` and dropped out of dense
/// retrieval entirely.
///
/// The kind list is what keeps this off types. A `class_declaration` also has a
/// callable grandchild — every method it holds — and calling it callable would
/// split a class at statement level instead of into its members.
fn binds_a_callable<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    const BINDING_KINDS: [&str; 5] = [
        "lexical_declaration",
        "variable_declaration",
        "variable_declarator",
        "field_declaration",
        "public_field_definition",
    ];
    let kind = node.kind();
    if !BINDING_KINDS.iter().any(|k| *k == &*kind) {
        return false;
    }
    node.children().any(|c| {
        c.is_named()
            && (is_callable_kind(&c.kind())
                || c.children().any(|g| g.is_named() && is_callable_kind(&g.kind())))
    })
}

/// Reports whether `node` wraps a container that ends where `node` does.
///
/// Ruby writes `S = Struct.new(:x) do … end` as `assignment > call > do_block`
/// and `describe … do` as `call > do_block`. No kind in either chain is a
/// member or a container, so every member inside was unreachable and the file
/// stayed one chunk. Matched by shape, like `wraps_member`: the child has to
/// end where its parent ends, which is what makes it a wrapper rather than one
/// element among several.
///
/// Callers must exclude callables first. A method also ends with its body, and
/// descending into one would split it at expression level.
fn wraps_container<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    let end = node.range().end;
    node.children().any(|c| {
        c.is_named()
            && c.range().end == end
            && (is_member_container_kind(&c.kind())
                || c.children().any(|g| {
                    g.is_named() && g.range().end == end && is_member_container_kind(&g.kind())
                }))
    })
}

/// Reports whether `kind` holds member declarations, so descent should open it.
///
/// Two ways to qualify: a body that groups members (`class_body`,
/// `declaration_list`, Python's `block`), or a member declaration that is not
/// itself callable (a class, an `impl`, an inline module).
///
/// Descent never enters a callable, so a bare `block` reached below the top
/// level is always a type body, never a method body.
///
/// `body_statement` is named exactly rather than by suffix. It is Ruby's body
/// for a class and for a method alike, and a `_statement` suffix rule would
/// also admit `if_statement` and `expression_statement` in every other
/// language, putting split points inside control flow.
fn is_member_container_kind(kind: &str) -> bool {
    const BODY_SUFFIXES: [&str; 3] = ["_body", "_list", "_block"];
    const EXACT: [&str; 3] = [
        "block",
        // Ruby: the body of a class, a module, and a method alike.
        "body_statement",
        // C/C++: `extern "C" { … }`. Opaque, it makes RocksDB's `db/c.cc` a
        // single 463 KB chunk, far past `MAX_EMBED_CHARS`.
        "linkage_specification",
    ];
    if BODY_SUFFIXES.iter().any(|s| kind.ends_with(s)) || EXACT.contains(&kind) {
        return true;
    }
    is_member_kind(kind) && !is_callable_kind(kind)
}

/// Push end-bytes of `node`'s named children, descending into any child that is
/// itself larger than `MAX_CHARS`.
///
/// Splitting only at *top-level* children collapses whole files into one chunk
/// in class-per-file languages (Java, Kotlin, C#, Scala), where the single
/// top-level declaration is the entire class. Descending recovers per-member
/// granularity.
///
/// Depth 0 keeps every named child as a split point, which is what non-code
/// files (JSON, YAML, HTML) rely on to chunk at all. Below that only member
/// declarations qualify, so a chunk always begins at a member boundary.
///
/// An oversized callable is recorded as a `DeepRegion` rather than descended
/// into here — its parts are packed separately so both levels survive.
fn collect_split_points<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    source: &str,
    plan: &mut SplitPlan,
    trail: &mut Vec<String>,
    d: Descent,
) {
    let source_len = source.len();
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let range = child.range();
        let kind = child.kind();
        let too_big = range.end.saturating_sub(range.start) > MAX_CHARS;
        let member = is_member_kind(&kind);

        // A node that only wraps a member — `export_statement` in TS/JS,
        // `decorated_definition` in Python — is transparent. Detected by shape
        // rather than by kind name: it holds a member that ends where it ends.
        // Opaque, it hides `export function f()` from the member walk, and a
        // TypeScript outline then lists exactly the declarations that are
        // *not* exported.
        let wraps_member = child
            .children()
            .any(|c| c.is_named() && is_member_kind(&c.kind()) && c.range().end == range.end);

        if member && !wraps_member {
            let name = node_name(&child, source);
            let breadcrumb = join_trail(trail, name.as_deref());
            plan.members.push(MemberSpan {
                start: range.start,
                end: range.end,
                breadcrumb: breadcrumb.clone(),
                signature: signature_of(&child, source),
            });
            if too_big && (is_callable_kind(&kind) || binds_a_callable(&child)) {
                let mut points = vec![range.start];
                collect_body_points(&child, source_len, &mut points);
                if points.last() != Some(&range.end) && range.end <= source_len {
                    points.push(range.end);
                }
                if points.len() > 2 {
                    plan.deep.push(DeepRegion {
                        start: range.start,
                        end: range.end,
                        breadcrumb,
                        points,
                    });
                }
            }
        }

        // Descend into every container, not just oversized ones: the member
        // list feeds breadcrumbs and the outline, which a small file needs just
        // as much. Emitting more split points costs nothing — `pack` merges
        // members back together whenever they fit in one chunk.
        let transparent =
            wraps_member || (!is_callable_kind(&kind) && wraps_container(&child));
        if !d.exhausted() && (is_member_container_kind(&kind) || transparent) {
            let name = node_name(&child, source);
            // A wrapper contributes no name of its own — `export` is not a
            // level in the breadcrumb.
            // Only a member-wrapper is nameless. A container that happens to end
            // with its own body — every class — still contributes its name.
            let pushed = name.is_some() && !is_body_kind(&kind) && !wraps_member;
            if pushed {
                trail.push(name.unwrap());
            }
            let next = d.into_child(range.end.saturating_sub(range.start));
            collect_split_points(&child, source, plan, trail, next);
            if pushed {
                trail.pop();
            }
        }

        // Top level is an AST fact, not a narrowing one: at the file root every
        // named child is a split point, which is what non-code files rely on.
        if d.ast > 0 && !member {
            continue;
        }
        if range.end > *plan.member_points.last().unwrap() && range.end <= source_len {
            plan.member_points.push(range.end);
        }
    }
}

/// Statement-level end-bytes inside a callable body.
///
/// Unlike the member walk this does enter blocks — that is the point, since
/// the parts of an oversized method are its statements. It stays out of
/// expressions, so a boundary never lands inside a call or an argument list;
/// a lambda body qualifies because it is a block in its own right.
fn collect_body_points<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    source_len: usize,
    points: &mut Vec<usize>,
) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let range = child.range();
        let kind = child.kind();
        // `binds_a_callable` carries the walk through the declarator that sits
        // between `const f =` and the arrow function's own body.
        if is_body_kind(&kind) || is_callable_kind(&kind) || binds_a_callable(&child) {
            collect_body_points(&child, source_len, points);
            continue;
        }
        // Only statements directly inside a body become boundaries. Reaching
        // here from anything else means we are inside an expression.
        if is_body_kind(&node.kind()) && range.end > *points.last().unwrap() && range.end <= source_len {
            points.push(range.end);
        }
    }
}

/// Reports whether `kind` groups statements or members rather than being one.
///
/// Two are named exactly: Ruby's `body_statement`, and the `compound_statement`
/// that C, C++, and PHP use for a function body. Without them an oversized
/// method in those languages has no body to walk and never produces `Part`
/// chunks. A `_statement` suffix rule would instead admit `if_statement` and
/// every other control-flow node in every language.
fn is_body_kind(kind: &str) -> bool {
    const SUFFIXES: [&str; 3] = ["_body", "_list", "_block"];
    const EXACT: [&str; 3] = ["block", "body_statement", "compound_statement"];
    SUFFIXES.iter().any(|s| kind.ends_with(s)) || EXACT.contains(&kind)
}

/// The declaration's own name, via the `name` field every tree-sitter grammar
/// uses for it. `None` for anonymous declarations and grammars without it.
fn node_name<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>, source: &str) -> Option<String> {
    let field = node.field("name")?;
    let r = field.range();
    let name = source.get(r.start..r.end)?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn join_trail(trail: &[String], name: Option<&str>) -> String {
    let mut parts: Vec<&str> = trail.iter().map(String::as_str).collect();
    if let Some(n) = name {
        parts.push(n);
    }
    parts.join(" > ")
}

/// Longest signature kept in an outline. Past this the header is a wall of
/// generic parameters, and the outline is a list to scan, not a reference.
const MAX_SIGNATURE_CHARS: usize = 200;

/// The declaration's header: everything before its body, as one line.
///
/// The cut comes from the AST — the start of the first body-like child — not
/// from scanning for `{`. Scanning got two things wrong. A brace can appear
/// *inside* a parameter list, so `fn unpack(Config { host, port }: Config)`
/// came out as `fn unpack(Config`, and the same for every destructured
/// TypeScript prop. And stopping at the first newline dropped the parameters
/// of any signature wrapped across lines, which is most non-trivial ones.
///
/// Only when there is no body child — an interface method, an abstract
/// declaration, a field — does it fall back to the text, and then the newline
/// is the right cut because there is no body to find.
fn signature_of<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    source: &str,
) -> String {
    let range = node.range();
    // Prefer the `body` field the grammar labels itself. Kind matching alone
    // cuts Go and C# at the opening paren, because both name the parameter node
    // `parameter_list` as a *direct* child of the declaration, and that ends
    // with `_list` like every class body does. Go methods came out worse than
    // Go functions: the receiver is the first `parameter_list`, so the cut
    // landed before the method name and the outline entry read `func`, naming
    // nothing at all. C and C++ escaped only because their parameter list is
    // wrapped in a `function_declarator`.
    //
    // The kind scan stays as the fallback, for the grammars that label no
    // `body` field — narrowing it to statement bodies instead would break the
    // type signatures that rely on `declaration_list` and
    // `field_declaration_list`, and C++ callables on `compound_statement`.
    let body_start = node.field("body").map(|c| c.range().start).or_else(|| {
        node.children()
            .filter(|c| c.is_named() && is_body_kind(&c.kind()))
            .map(|c| c.range().start)
            .min()
    });
    // With no body child the declaration *is* the signature — an interface
    // method, an abstract declaration, a field. Take all of it: cutting at the
    // first newline instead lost the parameters of any such declaration written
    // across lines, which is exactly where the long ones are.
    let end = match body_start {
        Some(b) if b > range.start => b,
        _ => range.end,
    };
    let slice = source.get(range.start..end).unwrap_or("");
    // Collapse the wrapping: a signature spread over five lines is still one
    // signature, and the outline is line-per-member.
    let mut flat = String::with_capacity(slice.len());
    for word in slice.split_whitespace() {
        if !flat.is_empty() {
            flat.push(' ');
        }
        flat.push_str(word);
    }
    let flat = flat.trim_end_matches(&['(', ',', '='][..]).trim().to_string();
    if flat.len() <= MAX_SIGNATURE_CHARS {
        return flat;
    }
    let mut cut = MAX_SIGNATURE_CHARS;
    while !flat.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &flat[..cut])
}

/// A document's headings, shaped like a `SplitPlan` so the same outline builder
/// serves both markdown and code.
///
/// A heading is to a document what a signature is to a class: the line that
/// says what the section is for. Nesting is carried in the breadcrumb, so
/// `## Pipeline` under `# Semantic search` reads as `Semantic search`.
fn markdown_heading_plan(source: &str) -> SplitPlan {
    let mut plan = SplitPlan::default();
    let mut trail: Vec<(usize, String)> = Vec::new();
    let mut offset = 0usize;

    let mut fence: Option<&str> = None;
    // One line of lookback, so a `===` underline can promote what came before.
    let mut previous: Option<String> = None;
    let mut previous_start = 0usize;
    // YAML front matter opens and closes with `---`, and that closing line
    // looks exactly like a setext underline — it promoted the last metadata
    // line into a heading.
    let mut in_front_matter = source.starts_with("---\n") || source.starts_with("---\r\n");
    let mut first_line = true;
    for line in source.split_inclusive('\n') {
        if in_front_matter {
            if !first_line && line.trim_end() == "---" {
                in_front_matter = false;
            }
            first_line = false;
            offset += line.len();
            continue;
        }
        first_line = false;
        let trimmed = line.trim_start();

        // A `#` inside a fenced block is a shell comment, a preprocessor
        // directive, a Python comment — not a heading. Left unhandled it not
        // only invented sections, it poisoned the breadcrumb of every real
        // heading that followed.
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
            }
            offset += line.len();
            continue;
        }
        for marker in ["```", "~~~"] {
            if trimmed.starts_with(marker) {
                fence = Some(marker);
            }
        }
        if fence.is_some() {
            offset += line.len();
            continue;
        }

        // CommonMark allows at most three spaces before a heading marker;
        // four makes it an indented code block. Using that rule directly is
        // both correct and cheaper than modelling code blocks separately.
        let indent = line.len() - line.trim_start_matches(' ').len();
        let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
        let atx = indent <= 3
            && (1..=6).contains(&hashes)
            && trimmed[hashes..].starts_with(|c: char| c.is_whitespace());

        // Setext: a run of `=` or `-` underlining the previous paragraph line.
        let body = trimmed.trim_end();
        let setext_level = if indent <= 3 && !body.is_empty() {
            if body.bytes().all(|b| b == b'=') {
                Some(1)
            } else if body.bytes().all(|b| b == b'-') {
                Some(2)
            } else {
                None
            }
        } else {
            None
        };
        let setext = setext_level
            .zip(previous.clone())
            .filter(|(_, prev)| !prev.is_empty());

        if let Some((level, title)) = setext {
            trail.retain(|(l, _)| *l < level);
            let breadcrumb = trail
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join(" > ");
            plan.members.push(MemberSpan {
                start: previous_start,
                end: offset + line.trim_end().len(),
                breadcrumb,
                signature: title.clone(),
            });
            trail.push((level, title));
            previous = None;
            offset += line.len();
            continue;
        }

        previous_start = offset;
        let is_rule = !body.is_empty()
            && (body.bytes().all(|b| b == b'-') || body.bytes().all(|b| b == b'='));
        previous = if atx || body.is_empty() || is_rule {
            None
        } else {
            Some(body.to_string())
        };

        if atx {
            let title = trimmed[hashes..].trim().to_string();
            if !title.is_empty() {
                trail.retain(|(level, _)| *level < hashes);
                let breadcrumb = trail
                    .iter()
                    .map(|(_, t)| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" > ");
                plan.members.push(MemberSpan {
                    start: offset,
                    end: offset + line.trim_end().len(),
                    breadcrumb,
                    signature: title.clone(),
                });
                trail.push((hashes, title));
            }
        }
        offset += line.len();
    }
    plan
}

/// Blank-line end-bytes for plain-text formats (TOML, PowerShell) that have
/// no tree-sitter grammar. A "blank line" is a run of two or more line
/// terminators, where a terminator is `\n` or `\r\n` — PowerShell files
/// are routinely CRLF, so matching bare `\n\n` only would never split them.
/// A line holding only spaces/tabs is still blank: horizontal whitespace
/// between terminators is consumed with the run (`"\n \t\n"` separates
/// paragraphs just like `"\n\n"`).
///
/// Every emitted offset sits immediately after an ASCII `\n` byte, which is
/// always a valid UTF-8 boundary (`b'\n'` never appears inside a multi-byte
/// sequence), so `pack` can slice at these points safely.
fn paragraph_split_points(source: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut points: Vec<usize> = vec![0];
    let mut i = 0;
    while i < len {
        // A line terminator at `i` is either "\n" or "\r\n".
        let first = match bytes[i] {
            b'\n' => 1,
            b'\r' if i + 1 < len && bytes[i + 1] == b'\n' => 2,
            _ => {
                i += 1;
                continue;
            }
        };
        // Consume the whole run of terminators, skipping spaces/tabs
        // between them. The skip only commits when another terminator
        // follows, so indentation at the start of a non-blank line stays
        // with that line.
        let mut terminators = 1;
        i += first;
        loop {
            let mut j = i;
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < len && bytes[j] == b'\n' {
                i = j + 1;
            } else if j + 1 < len && bytes[j] == b'\r' && bytes[j + 1] == b'\n' {
                i = j + 2;
            } else {
                break;
            }
            terminators += 1;
        }
        // ≥2 terminators means at least one blank line — split after the run.
        if terminators >= 2 && i < len && i > *points.last().unwrap() {
            points.push(i);
        }
    }
    if *points.last().unwrap() < len {
        points.push(len);
    }
    points
}

/// Top-level `section` and `fenced_code_block` end-bytes for a markdown parse.
fn markdown_split_points(source: &str) -> Vec<usize> {
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .is_err()
    {
        // tree-sitter-md is bundled and version-pinned, so this shouldn't
        // happen — but if it does, emit one chunk covering the whole file
        // rather than dropping it.
        return vec![0, source.len()];
    }
    let Some(tree) = parser.parse(source, None) else {
        return vec![0, source.len()];
    };
    let root = tree.root_node();

    let mut points: Vec<usize> = vec![0];
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if matches!(child.kind(), "section" | "fenced_code_block") {
            let end = child.end_byte();
            if end > *points.last().unwrap() && end <= source.len() {
                points.push(end);
            }
        }
    }
    if *points.last().unwrap() < source.len() {
        points.push(source.len());
    }
    points
}

/// Greedy-pack regions defined by `split_points` into chunks ≤ `MAX_CHARS`.
///
/// Within one call every byte between the first and last split point is covered
/// exactly once: gaps between split points attach to the *following* chunk so
/// nothing is lost. Single regions bigger than the target are emitted on their
/// own (no mid-decl splits).
fn pack(
    source: &str,
    file_path: &str,
    lang_name: &str,
    lines: &LineIndex,
    split_points: &[usize],
    kind: ChunkKind,
    breadcrumb: &str,
) -> Vec<Chunk> {
    if split_points.len() < 2 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut cur_start = split_points[0];
    let mut cur_end = cur_start;

    let flush = |start: usize, end: usize, out: &mut Vec<Chunk>| {
        push_chunk(source, file_path, lang_name, lines, start, end, kind, breadcrumb, out);
    };

    for w in split_points.windows(2) {
        let region_size = w[1] - w[0];
        let cur_size = cur_end - cur_start;
        if cur_size == 0 {
            cur_start = w[0];
            cur_end = w[1];
        } else if cur_size + region_size <= MAX_CHARS {
            cur_end = w[1];
        } else {
            flush(cur_start, cur_end, &mut chunks);
            cur_start = w[0];
            cur_end = w[1];
        }
    }
    if cur_end > cur_start {
        flush(cur_start, cur_end, &mut chunks);
    }

    chunks
}

#[allow(clippy::too_many_arguments)]
fn push_chunk(
    source: &str,
    file_path: &str,
    lang_name: &str,
    lines: &LineIndex,
    start: usize,
    end: usize,
    kind: ChunkKind,
    breadcrumb: &str,
    out: &mut Vec<Chunk>,
) {
    let content = &source[start..end];
    if content.trim().is_empty() {
        return;
    }
    let start_line = lines.line_of(start);
    let last_byte = end.saturating_sub(1).max(start);
    let end_line = lines.line_of(last_byte);

    out.push(Chunk {
        content: content.to_string(),
        file_path: file_path.to_string(),
        start_line,
        end_line,
        start_byte: start as u32,
        end_byte: end as u32,
        language: lang_name.to_string(),
        breadcrumb: breadcrumb.to_string(),
        kind,
    });
}

/// Fill in the breadcrumb of every chunk that does not already carry one.
///
/// A chunk holding exactly one member is named after it. A chunk that groups
/// several members — the packer merges small ones — is named after the
/// container instead, since naming just one of them would misdescribe the rest.
fn assign_breadcrumbs(plan: &SplitPlan, chunks: &mut [Chunk]) {
    for c in chunks.iter_mut() {
        if !c.breadcrumb.is_empty() {
            continue;
        }
        let (start, end) = (c.start_byte as usize, c.end_byte as usize);
        if let Some(m) = plan
            .members
            .iter()
            .filter(|m| m.start <= start && m.end >= end)
            .min_by_key(|m| m.end - m.start)
        {
            c.breadcrumb = m.breadcrumb.clone();
            continue;
        }
        // Chunk boundaries do not line up with member boundaries — the packer
        // starts at the previous member's end and stops when full. Name the
        // member the chunk covers the largest *fraction* of: an enclosing type
        // always overlaps more bytes, but the member the chunk is really about
        // is the one it nearly exhausts.
        //
        // Ties break toward the *earliest* member in the chunk, so the label
        // names what a reader sees at the top of it. Breaking toward the
        // smallest instead named a chunk after whichever short member happened
        // to sit inside: one opening on `render_index_stats_json` came back
        // labelled `hit_to_json`, forty lines below, and an agent jumping to
        // the reported line landed on the wrong function.
        let best = plan
            .members
            .iter()
            .filter_map(|m| {
                let overlap = m.end.min(end).saturating_sub(m.start.max(start));
                let size = m.end.saturating_sub(m.start);
                if overlap == 0 || size == 0 {
                    return None;
                }
                // Integer permille — f32 has no total order to sort on.
                Some((overlap * 1000 / size, std::cmp::Reverse(m.start), m))
            })
            .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        if let Some((_, _, m)) = best {
            c.breadcrumb = m.breadcrumb.clone();
        }
    }
}

/// How large one outline document may get before it is split in two.
///
/// Well under the embedder's own size cap, because an outline that exceeds that
/// cap is dropped from the dense index entirely — and the files with the
/// longest outlines are the large, central ones a query is most likely to want.
/// Leaving them unembedded made file-level retrieval miss exactly
/// `QueryExecutorImpl`, `PgConnection`, `PgResultSet` and their kind.
const OUTLINE_MAX_CHARS: usize = 3000;

/// Synthesized signature-list chunks for a file, one per `OUTLINE_MAX_CHARS`.
///
/// They carry no body text, so they answer "which file exposes this API" for
/// queries that name a type or method — the case where body-level chunks are
/// scattered and each one matches only weakly. They sit in the index beside the
/// source chunks rather than replacing them; ranking decides which is better.
///
/// A split piece spans the lines of the members it lists rather than the whole
/// file, so a hit points at a region instead of at everything. The pieces do
/// not overlap: carrying signatures across the boundary measured worse (MRR@10
/// 0.332 with no overlap, 0.316 at three signatures, and recall@5 dropped at
/// twenty). A piece is a list of independent signatures, not prose, so there is
/// no evidence straddling the cut for an overlap to rescue — it only duplicates
/// terms and makes near-identical pieces compete.
fn build_outline_chunks(
    file_path: &str,
    lang_name: &str,
    lines: &LineIndex,
    plan: &SplitPlan,
) -> Vec<Chunk> {
    let listed: Vec<(&MemberSpan, String)> = plan
        .members
        .iter()
        .filter(|m| !m.signature.is_empty())
        .map(|m| {
            let line = if m.breadcrumb.is_empty() {
                m.signature.clone()
            } else {
                format!("{} :: {}", m.breadcrumb, m.signature)
            };
            (m, line)
        })
        .collect();
    if listed.is_empty() {
        return Vec::new();
    }

    // A path long enough to eat the whole budget would underflow every size
    // calculation below. Nothing forbids one — the walk follows whatever
    // directory tree it is pointed at — so the prefix is dropped rather than
    // charged when it does not fit. `Chunk.file_path` still carries it.
    let prefix = if file_path.len() + 1 < OUTLINE_MAX_CHARS {
        file_path
    } else {
        ""
    };
    let base_len = prefix.len() + 1;
    let line_budget = OUTLINE_MAX_CHARS - base_len;

    let mut out = Vec::new();
    let mut batch: Vec<&str> = Vec::new();
    let mut batch_len = base_len;
    let mut first: Option<&MemberSpan> = None;
    let mut last: Option<&MemberSpan> = None;

    let flush = |batch: &mut Vec<&str>,
                     first: &mut Option<&MemberSpan>,
                     last: &mut Option<&MemberSpan>,
                     out: &mut Vec<Chunk>| {
        let (Some(f), Some(l)) = (*first, *last) else {
            return;
        };
        let mut content = String::with_capacity(base_len);
        content.push_str(prefix);
        for line in batch.iter() {
            content.push('\n');
            content.push_str(line);
        }
        out.push(Chunk {
            content,
            file_path: file_path.to_string(),
            start_line: lines.line_of(f.start),
            end_line: lines.line_of(l.end.saturating_sub(1)),
            start_byte: f.start as u32,
            end_byte: l.end as u32,
            language: lang_name.to_string(),
            breadcrumb: String::new(),
            kind: ChunkKind::Outline,
        });
        batch.clear();
        *first = None;
        *last = None;
    };

    // A single signature can exceed the budget on its own — a one-line
    // declaration with a long parameter list, or generated code. Truncating it
    // keeps every piece embeddable, which is the whole point of splitting: an
    // outline over the embedder's cap loses its dense vector and drops out of
    // phase-one file retrieval.
    let truncated: Vec<String> = listed
        .iter()
        .map(|(_, line)| {
            if line.len() <= line_budget {
                line.clone()
            } else {
                const ELLIPSIS: &str = "…";
                let mut cut = line_budget.saturating_sub(ELLIPSIS.len());
                while cut > 0 && !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}{ELLIPSIS}", &line[..cut])
            }
        })
        .collect();

    for ((m, _), line) in listed.iter().zip(truncated.iter()) {
        if batch_len + line.len() > OUTLINE_MAX_CHARS && !batch.is_empty() {
            flush(&mut batch, &mut first, &mut last, &mut out);
            batch_len = base_len;
        }
        if first.is_none() {
            first = Some(m);
        }
        last = Some(m);
        batch_len += line.len() + 1;
        batch.push(line);
    }
    flush(&mut batch, &mut first, &mut last, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rust(src: &str) -> Vec<Chunk> {
        chunk_source(src, "f.rs", ChunkerKind::AstGrep(SupportLang::Rust))
    }

    /// The chunks that tile the file exactly once — everything except the
    /// overlapping `Part` level and the synthesized outline.
    fn tiling(src: &str) -> Vec<Chunk> {
        rust(src)
            .into_iter()
            .filter(|c| matches!(c.kind, ChunkKind::Source | ChunkKind::Enclosing))
            .collect()
    }

    fn of_kind(chunks: &[Chunk], kind: ChunkKind) -> Vec<&Chunk> {
        chunks.iter().filter(|c| c.kind == kind).collect()
    }

    #[test]
    fn empty_source_yields_no_chunks() {
        assert!(rust("").is_empty());
        assert!(rust("   \n\t\n").is_empty());
    }

    #[test]
    fn is_indexable_rejects_truly_unknown() {
        assert!(is_indexable(&PathBuf::from("foo.txt")).is_none());
        assert!(is_indexable(&PathBuf::from("foo.bin")).is_none());
        // No extension → none.
        assert!(is_indexable(&PathBuf::from("Makefile")).is_none());
    }

    #[test]
    fn is_indexable_accepts_adapter_languages() {
        assert_eq!(
            is_indexable(&PathBuf::from("a.rs")),
            Some(ChunkerKind::AstGrep(SupportLang::Rust))
        );
        assert_eq!(
            is_indexable(&PathBuf::from("a.py")),
            Some(ChunkerKind::AstGrep(SupportLang::Python))
        );
        assert_eq!(
            is_indexable(&PathBuf::from("a.kt")),
            Some(ChunkerKind::AstGrep(SupportLang::Kotlin))
        );
        assert_eq!(
            is_indexable(&PathBuf::from("README.md")),
            Some(ChunkerKind::Markdown)
        );
        assert_eq!(
            is_indexable(&PathBuf::from("doc.MDX")), // case-insensitive
            Some(ChunkerKind::Markdown)
        );
    }

    #[test]
    fn is_indexable_accepts_extra_ast_grep_languages() {
        // We rely on whatever ast-grep claims it can parse — these are languages
        // ast-bro has no outline adapter for, but the chunker still works.
        for ext in ["lua", "yaml", "yml", "json", "html", "css", "rb", "php"] {
            let p = PathBuf::from(format!("a.{ext}"));
            assert!(
                is_indexable(&p).is_some(),
                "expected ast-grep to claim .{ext}"
            );
        }
    }

    #[test]
    fn rust_one_function_one_chunk() {
        let chunks = tiling("fn hello() {\n    println!(\"hi\");\n}\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].language, "rust");
        assert!(chunks[0].content.contains("fn hello"));
    }

    #[test]
    fn rust_groups_small_decls_under_max_chars() {
        let chunks = tiling("fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("fn a"));
        assert!(chunks[0].content.contains("fn c"));
    }

    #[test]
    fn rust_splits_when_total_exceeds_max() {
        let big_body: String = "    let x = 0;\n".repeat(80); // ~1200 chars per body
        let src = format!(
            "fn one() {{\n{big_body}}}\nfn two() {{\n{big_body}}}\nfn three() {{\n{big_body}}}\n"
        );
        let chunks = rust(&src);
        assert!(chunks.len() >= 2, "expected packer to split, got {}", chunks.len());
        for c in &chunks {
            assert!(!c.content.trim().is_empty());
        }
    }

    #[test]
    fn ast_chunks_are_contiguous_and_cover_source() {
        let src = "// header comment\nfn a() {}\n\nfn b() {}\n// trailer\n";
        let chunks = tiling(src);
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(joined, src);
    }

    #[test]
    fn handles_multibyte_chars_in_source() {
        // Box-drawing char `─` is 3 bytes in UTF-8. A previous bug had us
        // slicing `&str[..end-1]` for line counting, which panicked when
        // the char before `end` was multi-byte.
        let src = "// ── header ──\nfn a() {}\n";
        let chunks = rust(src);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn line_numbers_are_one_indexed_and_correct() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let chunks = tiling(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
    }

    // ── Markdown ──────────────────────────────────────────────────────────

    /// Markdown chunks that tile the source, i.e. everything but the outline.
    fn md_tiling(src: &str) -> Vec<Chunk> {
        md(src)
            .into_iter()
            .filter(|c| c.kind != ChunkKind::Outline)
            .collect()
    }

    fn md(src: &str) -> Vec<Chunk> {
        chunk_source(src, "README.md", ChunkerKind::Markdown)
    }

    #[test]
    fn markdown_single_section_one_chunk() {
        let src = "# Title\n\nSome paragraph.\n";
        let chunks = md_tiling(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].language, "markdown");
        assert!(chunks[0].content.contains("# Title"));
    }

    #[test]
    fn markdown_groups_small_sections() {
        let src = "\
# Section A
content a

# Section B
content b

# Section C
content c
";
        let chunks = md_tiling(src);
        // All three small sections should pack into one chunk.
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("Section A"));
        assert!(chunks[0].content.contains("Section C"));
    }

    #[test]
    fn markdown_splits_when_section_exceeds_max() {
        let big: String = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(40);
        let src = format!("# A\n{big}\n\n# B\n{big}\n\n# C\n{big}\n");
        let chunks = md(&src);
        assert!(chunks.len() >= 2, "expected splitting, got {}", chunks.len());
    }

    #[test]
    fn markdown_chunks_cover_source() {
        let src = "# Hello\n\nworld\n\n# Goodbye\n\ncruel world\n";
        let chunks = md_tiling(src);
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(joined, src);
    }

    // ── hierarchy, breadcrumbs, outline ────────────────────────────────────

    fn java(src: &str) -> Vec<Chunk> {
        chunk_source(src, "Outer.java", ChunkerKind::AstGrep(SupportLang::Java))
    }

    /// `n` lines of filler, enough to push a body past `MAX_CHARS`.
    fn filler(n: usize, tag: &str) -> String {
        (0..n)
            .map(|i| format!("    // {tag} line {i:03} aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"))
            .collect()
    }

    #[test]
    fn java_class_splits_into_members() {
        let src = format!(
            "package p;\npublic class Outer {{\n  void a() {{\n{}  }}\n  void b() {{\n{}  }}\n}}\n",
            filler(20, "a"),
            filler(20, "b")
        );
        let tiles: Vec<Chunk> = java(&src)
            .into_iter()
            .filter(|c| matches!(c.kind, ChunkKind::Source | ChunkKind::Enclosing))
            .collect();
        assert!(
            tiles.len() >= 2,
            "expected the class to split per member, got {}",
            tiles.len()
        );
        // The whole-class chunk of the old top-level-only walk is gone.
        assert!(tiles.iter().all(|c| c.content.len() <= MAX_CHARS * 2));
    }

    #[test]
    fn oversized_method_is_kept_whole_beside_its_parts() {
        let src = format!(
            "public class Outer {{\n  void big() {{\n{}    list.forEach(item -> {{\n{}    }});\n  }}\n}}\n",
            filler(25, "pre"),
            filler(25, "lambda")
        );
        let chunks = java(&src);
        let enclosing = of_kind(&chunks, ChunkKind::Enclosing);
        let parts = of_kind(&chunks, ChunkKind::Part);
        assert_eq!(enclosing.len(), 1, "the big method should survive whole");
        assert!(parts.len() >= 2, "and also be split, got {}", parts.len());
        // Every part lies inside the member it came from.
        let whole = enclosing[0];
        for p in &parts {
            assert!(p.start_byte >= whole.start_byte && p.end_byte <= whole.end_byte);
        }
    }

    /// A chunk boundary never lands inside an expression.
    #[test]
    fn splits_never_land_inside_an_expression() {
        // A substring test for "method" accepts `method_invocation`, which puts
        // `list.forEach` and `(item -> {` in different chunks.
        let src = format!(
            "public class Outer {{\n  void big() {{\n{}    list.forEach(item -> {{\n{}    }});\n  }}\n}}\n",
            filler(25, "pre"),
            filler(25, "lambda")
        );
        for c in java(&src) {
            let head = c.content.trim_start();
            assert!(
                !head.starts_with('(') && !head.starts_with(';') && !head.starts_with("->"),
                "chunk starts mid-expression: {:?}",
                &head[..head.len().min(40)]
            );
        }
    }

    #[test]
    fn nested_class_members_get_breadcrumbs() {
        let src = format!(
            "public class Outer {{\n  public static class Nested {{\n    void deep() {{\n{}    }}\n    void tiny() {{}}\n  }}\n}}\n",
            filler(25, "d")
        );
        let chunks = java(&src);
        assert!(
            chunks
                .iter()
                .any(|c| c.breadcrumb.contains("Outer") && c.breadcrumb.contains("Nested")),
            "expected a breadcrumb naming the nested class, got {:?}",
            chunks.iter().map(|c| &c.breadcrumb).collect::<Vec<_>>()
        );
    }

    #[test]
    fn outline_chunk_lists_signatures_without_bodies() {
        let src = "public class Outer {\n  void alpha() { int x = 1; }\n  void beta() { int y = 2; }\n}\n";
        let chunks = java(src);
        let outlines = of_kind(&chunks, ChunkKind::Outline);
        assert_eq!(outlines.len(), 1);
        let text = &outlines[0].content;
        assert!(text.contains("alpha"), "outline missing alpha: {text}");
        assert!(text.contains("beta"), "outline missing beta: {text}");
        assert!(!text.contains("int x = 1"), "outline leaked a body: {text}");
    }

    /// Every outline piece stays inside the size the embedder accepts.
    #[test]
    fn long_outline_splits_so_every_piece_stays_embeddable() {
        // Enough members that one outline document would exceed the size the
        // embedder accepts, which drops the biggest files out of the dense
        // index entirely unless the outline is split.
        let members: String = (0..400)
            .map(|i| format!("  public void someDescriptiveMethodName{i:03}(String argument) {{}}\n"))
            .collect();
        let src = format!("public class Big {{\n{members}}}\n");
        let outlines: Vec<Chunk> = java(&src)
            .into_iter()
            .filter(|c| c.kind == ChunkKind::Outline)
            .collect();
        assert!(outlines.len() > 1, "expected a split, got {}", outlines.len());
        for o in &outlines {
            assert!(
                o.content.len() <= OUTLINE_MAX_CHARS * 2,
                "outline piece too large: {}",
                o.content.len()
            );
            assert!(o.start_line >= 1 && o.end_line >= o.start_line);
        }
        // Every member is still listed somewhere.
        let all: String = outlines.iter().map(|o| o.content.as_str()).collect();
        assert!(all.contains("someDescriptiveMethodName000"));
        assert!(all.contains("someDescriptiveMethodName399"));
    }

    #[test]
    fn outline_pieces_point_at_the_members_they_list() {
        let members: String = (0..400)
            .map(|i| format!("  public void someDescriptiveMethodName{i:03}(String argument) {{}}\n"))
            .collect();
        let src = format!("public class Big {{\n{members}}}\n");
        let outlines: Vec<Chunk> = java(&src)
            .into_iter()
            .filter(|c| c.kind == ChunkKind::Outline)
            .collect();
        // Pieces after the first must not all claim to start at line 1 — that
        // was the whole-file range the unsplit outline used.
        assert!(
            outlines.iter().skip(1).any(|o| o.start_line > 1),
            "split pieces still span the whole file"
        );
    }

    #[test]
    fn rust_impl_block_splits_per_method() {
        let body = filler(20, "x").replace("//", "//");
        let src = format!(
            "pub struct S;\nimpl S {{\n  pub fn one(&self) {{\n{body}  }}\n  pub fn two(&self) {{\n{body}  }}\n}}\n"
        );
        let tiles: Vec<Chunk> = rust(&src)
            .into_iter()
            .filter(|c| matches!(c.kind, ChunkKind::Source | ChunkKind::Enclosing))
            .collect();
        assert!(
            tiles.len() >= 2,
            "expected the impl block to split per method, got {}",
            tiles.len()
        );
        assert!(rust(&src).iter().any(|c| c.breadcrumb.contains("S")));
    }

    #[test]
    fn oversized_constructor_splits_like_a_method() {
        // A constructor is named `*_declaration`, not `*method*`, so before it
        // counted as a callable it was treated as a container: descent entered
        // the body and `local_variable_declaration` — which also ends in
        // `_declaration` — became a split point in the middle of it.
        let body: String = (0..60)
            .map(|i| format!("    int v{i:03} = compute({i});\n"))
            .collect();
        let src = format!("public class Ctor {{\n  public Ctor(String arg) {{\n{body}  }}\n  void tiny() {{}}\n}}\n");
        let chunks = java(&src);
        for c in &chunks {
            if matches!(c.kind, ChunkKind::Part) {
                continue; // a Part starts inside the body — that is its job
            }
            let head = c.content.trim_start();
            assert!(
                !head.starts_with("int v"),
                "the tiling splits mid-constructor: {:?}",
                &head[..head.len().min(40)]
            );
        }
        // Whole beside its parts, exactly as an oversized method behaves.
        assert_eq!(of_kind(&chunks, ChunkKind::Enclosing).len(), 1);
        assert!(!of_kind(&chunks, ChunkKind::Part).is_empty());
        assert!(chunks.iter().any(|c| c.breadcrumb == "Ctor > Ctor"));
    }

    #[test]
    fn a_single_huge_signature_cannot_blow_the_outline_budget() {
        // One declaration whose header alone exceeds the outline budget: the
        // batching check is guarded by "batch is non-empty", so without
        // truncation this landed in an empty batch and came out oversized.
        let params: String = (0..400)
            .map(|i| format!("String parameterNumber{i:03}, "))
            .collect();
        let src = format!("public class Wide {{\n  void wide({params}int last) {{}}\n}}\n");
        for outline in of_kind(&java(&src), ChunkKind::Outline) {
            assert!(
                outline.content.len() <= OUTLINE_MAX_CHARS,
                "outline piece is {} chars, over the {OUTLINE_MAX_CHARS} budget",
                outline.content.len()
            );
        }
    }

    #[test]
    fn outline_pieces_budget_for_the_path_prefix() {
        let members: String = (0..400)
            .map(|i| format!("  public void someDescriptiveMethodName{i:03}(String argument) {{}}\n"))
            .collect();
        let src = format!("public class Big {{\n{members}}}\n");
        // The rendered chunk opens with the file path, so the budget has to
        // cover the prefix or every piece comes out that much over.
        for outline in of_kind(&java(&src), ChunkKind::Outline) {
            assert!(outline.content.len() <= OUTLINE_MAX_CHARS);
        }
    }

    #[test]
    fn markdown_gets_an_outline_from_its_headings() {
        // Without one, two-phase retrieval — which can only raise a score —
        // leaves documents unable to compete with code on any query.
        let src = "# Semantic search\n\nintro\n\n## Pipeline\n\nbody\n\n### RRF\n\nmore\n";
        let chunks = chunk_source(src, "wiki/search.md", ChunkerKind::Markdown);
        let outlines = of_kind(&chunks, ChunkKind::Outline);
        assert_eq!(outlines.len(), 1, "expected one outline, got {}", outlines.len());
        let text = &outlines[0].content;
        for heading in ["Semantic search", "Pipeline", "RRF"] {
            assert!(text.contains(heading), "outline missing {heading}: {text}");
        }
        assert!(!text.contains("intro"), "outline leaked body text: {text}");
    }

    #[test]
    fn markdown_outline_nests_headings_in_the_breadcrumb() {
        let src = "# Top\n\n## Middle\n\n### Leaf\n\ntext\n";
        let chunks = chunk_source(src, "d.md", ChunkerKind::Markdown);
        let text = &of_kind(&chunks, ChunkKind::Outline)[0].content;
        assert!(text.contains("Top > Middle :: Leaf"), "breadcrumb not nested: {text}");
    }

    #[test]
    fn a_hash_that_is_not_a_heading_is_ignored() {
        let src = "# Real\n\n#nospace is not a heading\n\n####### seven hashes either\n";
        let chunks = chunk_source(src, "d.md", ChunkerKind::Markdown);
        let text = &of_kind(&chunks, ChunkKind::Outline)[0].content;
        assert!(text.contains("Real"));
        assert!(!text.contains("nospace"), "picked up a non-heading: {text}");
        assert!(!text.contains("seven hashes"), "picked up seven hashes: {text}");
    }

    #[test]
    fn breadcrumb_names_the_earliest_member_in_the_chunk() {
        // A chunk holding several whole members used to be named after the
        // smallest of them, so the label could point dozens of lines past the
        // line the result reports.
        let src = "fn alpha() { let a = 1; let b = 2; }\nfn b() {}\n";
        for c in rust(src) {
            if c.kind == ChunkKind::Outline || c.breadcrumb.is_empty() {
                continue;
            }
            assert_eq!(c.breadcrumb, "alpha", "chunk labelled {:?}", c.breadcrumb);
        }
    }

    fn outline_of(src: &str, path: &str, kind: ChunkerKind) -> String {
        chunk_source(src, path, kind)
            .into_iter()
            .filter(|c| c.kind == ChunkKind::Outline)
            .map(|c| c.content)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn signature_keeps_a_brace_that_belongs_to_the_parameters() {
        // Cutting at the first `{` truncated every destructured parameter:
        // `fn unpack(Config { host, port }: Config)` came out as `fn unpack(Config`.
        let src = "pub fn unpack(Config { host, port }: Config) -> String { String::new() }\n";
        let text = outline_of(src, "b.rs", ChunkerKind::AstGrep(SupportLang::Rust));
        assert!(text.contains("host, port"), "parameters truncated: {text}");
        assert!(text.contains("-> String"), "return type truncated: {text}");
        assert!(!text.contains("String::new"), "body leaked: {text}");
    }

    #[test]
    fn signature_survives_being_wrapped_across_lines() {
        // Cutting at the first newline dropped the parameters of any signature
        // long enough to need wrapping, which is most non-trivial ones.
        let src = "pub fn configure(\n    name: &str,\n    retries: usize,\n) -> Result<C, E> {\n    todo!()\n}\n";
        let text = outline_of(src, "b.rs", ChunkerKind::AstGrep(SupportLang::Rust));
        assert!(text.contains("name: &str"), "parameters lost: {text}");
        assert!(text.contains("retries: usize"), "parameters lost: {text}");
        assert!(text.contains("-> Result<C, E>"), "return type lost: {text}");
        assert!(!text.contains("todo!"), "body leaked: {text}");
        assert_eq!(text.lines().count(), 2, "signature not flattened: {text}");
    }

    #[test]
    fn exported_declarations_reach_the_outline() {
        // `export_statement` is neither a member nor a container, so an opaque
        // wrapper hid exactly the declarations that form the public API: a
        // TypeScript outline listed only what was *not* exported.
        let src = "export function exported() {}\nfunction plain() {}\nexport class Widget { render() {} }\n";
        let text = outline_of(src, "e.ts", ChunkerKind::AstGrep(SupportLang::TypeScript));
        for name in ["exported", "plain", "Widget", "render"] {
            assert!(text.contains(name), "outline missing {name}: {text}");
        }
        assert!(!text.contains("export ::"), "wrapper became its own entry: {text}");
    }

    #[test]
    fn a_wrapper_adds_no_level_to_the_breadcrumb() {
        let src = "export class Widget { render() {} }\n";
        let text = outline_of(src, "e.ts", ChunkerKind::AstGrep(SupportLang::TypeScript));
        assert!(text.contains("Widget > render"), "breadcrumb wrong: {text}");
    }

    #[test]
    fn a_decorator_is_not_a_member_of_its_own() {
        let src = "@decorator\ndef decorated(a, b):\n    return a\n";
        let text = outline_of(src, "f.py", ChunkerKind::AstGrep(SupportLang::Python));
        assert!(text.contains("decorated"), "declaration missing: {text}");
        assert!(
            !text.lines().any(|l| l.trim() == "@decorator"),
            "decorator listed as its own member: {text}"
        );
    }

    #[test]
    fn headings_inside_a_fenced_block_are_not_headings() {
        // A `#` in a shell snippet is a comment. Treating it as a heading also
        // poisoned the breadcrumb of every real heading after it.
        let src = "# Real\n\n```bash\n# not a heading\necho hi\n```\n\n## Second\n";
        let text = outline_of(src, "d.md", ChunkerKind::Markdown);
        assert!(!text.contains("not a heading"), "fenced comment became a heading: {text}");
        assert!(text.contains("Real :: Second"), "breadcrumb poisoned: {text}");
    }

    #[test]
    fn a_tilde_fence_also_hides_headings() {
        let src = "# Real\n\n~~~\n# not a heading\n~~~\n\n## Second\n";
        let text = outline_of(src, "d.md", ChunkerKind::Markdown);
        assert!(!text.contains("not a heading"), "tilde fence ignored: {text}");
    }

    #[test]
    fn a_very_long_signature_is_truncated() {
        let params: String = (0..200).map(|i| format!("arg{i:03}: usize, ")).collect();
        let src = format!("pub fn wide({params}last: usize) {{}}\n");
        let text = outline_of(&src, "b.rs", ChunkerKind::AstGrep(SupportLang::Rust));
        for line in text.lines().skip(1) {
            assert!(line.len() <= MAX_SIGNATURE_CHARS + 32, "signature not capped: {}", line.len());
        }
    }

    #[test]
    fn setext_headings_are_headings_too() {
        let src = "Top level\n=========\n\ntext\n\nSecond\n------\n\n### Deep\n";
        let text = outline_of(src, "g.md", ChunkerKind::Markdown);
        assert!(text.contains("Top level"), "setext h1 missed: {text}");
        assert!(text.contains("Second"), "setext h2 missed: {text}");
        assert!(text.contains("Top level > Second :: Deep"), "nesting wrong: {text}");
    }

    #[test]
    fn front_matter_dashes_do_not_promote_nothing() {
        // A `---` with no paragraph line above it is a thematic break or front
        // matter, not an underline.
        let src = "---\ntitle: x\n---\n\n# Real\n";
        let text = outline_of(src, "g.md", ChunkerKind::Markdown);
        assert!(text.contains("Real"));
        assert!(!text.contains("title: x"), "front matter became a heading: {text}");
    }

    #[test]
    fn a_hash_indented_four_spaces_is_code_not_a_heading() {
        // CommonMark allows at most three spaces before the marker; four makes
        // it an indented code block.
        let src = "# Real\n\n    # indented, so code\n    echo hi\n\n## Second\n";
        let text = outline_of(src, "g.md", ChunkerKind::Markdown);
        assert!(!text.contains("indented"), "indented code became a heading: {text}");
        assert!(text.contains("Real :: Second"), "breadcrumb poisoned: {text}");
    }

    #[test]
    fn a_declaration_without_a_body_keeps_its_whole_signature() {
        // Interface methods have no body child, so the fallback decides. Cutting
        // at the first newline lost the parameters of any wrapped one.
        let src = "public interface Repo {\n  List<Row> find(\n      String name,\n      Status status) throws SQLException;\n}\n";
        let text = outline_of(src, "h.java", ChunkerKind::AstGrep(SupportLang::Java));
        assert!(text.contains("String name"), "parameters lost: {text}");
        assert!(text.contains("throws SQLException"), "tail lost: {text}");
    }

    #[test]
    fn an_absurdly_long_path_does_not_underflow_the_budget() {
        // Nothing forbids a repo-relative path longer than the outline budget,
        // and `OUTLINE_MAX_CHARS - path.len()` panics in debug when it happens.
        let path = format!("{}/x.rs", "nested".repeat(700));
        assert!(path.len() > OUTLINE_MAX_CHARS);
        let src = "fn alpha() {}\nfn beta() {}\n";
        let chunks = chunk_source(src, &path, ChunkerKind::AstGrep(SupportLang::Rust));
        let outlines = of_kind(&chunks, ChunkKind::Outline);
        assert!(!outlines.is_empty(), "outline dropped entirely");
        for o in &outlines {
            assert!(o.content.len() <= OUTLINE_MAX_CHARS, "over budget: {}", o.content.len());
            assert_eq!(o.file_path, path, "the chunk still carries the full path");
        }
        let text = outlines[0].content.clone();
        assert!(text.contains("alpha") && text.contains("beta"), "members lost: {text}");
    }

    #[test]
    fn is_indexable_accepts_plain_text_formats() {
        assert_eq!(
            is_indexable(&PathBuf::from("Cargo.toml")),
            Some(ChunkerKind::Plain("toml"))
        );
        for ext in ["ps1", "psm1", "psd1"] {
            assert_eq!(
                is_indexable(&PathBuf::from(format!("script.{ext}"))),
                Some(ChunkerKind::Plain("powershell")),
                "expected .{ext} to map to the plain powershell chunker"
            );
        }
    }

    #[test]
    fn paragraph_split_detects_lf_blank_lines() {
        let src = "function A {}\n\nfunction B {}\n\nfunction C {}\n";
        let points = paragraph_split_points(src);
        // 0, after each blank-line run, and EOF → 4 points / 3 regions.
        assert_eq!(points.len(), 4, "points: {points:?}");
        assert_eq!(points[0], 0);
        assert_eq!(*points.last().unwrap(), src.len());
    }

    #[test]
    fn paragraph_split_detects_crlf_blank_lines() {
        // Regression for upstream PR #29: its splitter only matched bare
        // "\n\n", so CRLF files — the norm for PowerShell — never split.
        let src = "function A {}\r\n\r\nfunction B {}\r\n\r\nfunction C {}\r\n";
        let points = paragraph_split_points(src);
        assert_eq!(points.len(), 4, "points: {points:?}");
        assert_eq!(points[0], 0);
        assert_eq!(*points.last().unwrap(), src.len());
    }

    #[test]
    fn paragraph_split_treats_whitespace_only_lines_as_blank() {
        // A "blank" line holding spaces or tabs still separates paragraphs,
        // for both LF and CRLF files — and coverage stays byte-exact.
        for src in [
            "function A {}\n \nfunction B {}\n\t\nfunction C {}\n",
            "function A {}\r\n \t \r\nfunction B {}\r\n\t\r\nfunction C {}\r\n",
        ] {
            let points = paragraph_split_points(src);
            assert_eq!(points.len(), 4, "src: {src:?}, points: {points:?}");
            assert_eq!(points[0], 0);
            assert_eq!(*points.last().unwrap(), src.len());
            let chunks = chunk_source(src, "f", ChunkerKind::Plain("toml"));
            let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
            assert_eq!(joined, src, "chunks must cover the source exactly");
        }
        // Indentation on a *non-blank* line stays with that line: no split.
        let src = "a = 1\n    indented = 2\nb = 3\n";
        assert_eq!(paragraph_split_points(src).len(), 2, "no blank line, no split");
    }

    #[test]
    fn plain_splits_into_multiple_chunks_when_over_max() {
        let para = format!("[section]\nkey = \"{}\"\n", "x".repeat(1200));
        let src = format!("{para}\n{para}\n{para}");
        let chunks = chunk_source(&src, "big.toml", ChunkerKind::Plain("toml"));
        assert!(chunks.len() >= 2, "expected splitting, got {}", chunks.len());
        assert_eq!(chunks[0].language, "toml");
    }

    #[test]
    fn plain_chunks_cover_source() {
        for src in [
            "[package]\nname = \"demo\"\n\n[dependencies]\nserde = \"1\"\n",
            "function A {}\r\n\r\nfunction B {}\r\n\r\nfunction C {}\r\n",
        ] {
            let chunks = chunk_source(src, "f", ChunkerKind::Plain("toml"));
            let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
            assert_eq!(joined, src);
        }
    }

    #[test]
    fn plain_no_blank_lines_is_one_chunk() {
        let src = "a = 1\nb = 2\nc = 3\n";
        let chunks = chunk_source(src, "f.toml", ChunkerKind::Plain("toml"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
    }

    #[test]
    fn tiling_still_covers_every_byte_exactly_once() {
        let src = format!(
            "public class Outer {{\n  void big() {{\n{}  }}\n  void small() {{}}\n}}\n",
            filler(30, "f")
        );
        let mut tiles: Vec<Chunk> = java(&src)
            .into_iter()
            .filter(|c| matches!(c.kind, ChunkKind::Source | ChunkKind::Enclosing))
            .collect();
        tiles.sort_by_key(|c| c.start_byte);
        let joined: String = tiles.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(joined, src);
    }
}
