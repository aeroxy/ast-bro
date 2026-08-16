use colored::Colorize;
use serde::{Serialize, Serializer};
use std::path::{Path, PathBuf};

// Stable JSON schema identifiers — bump on breaking changes.
pub const JSON_SCHEMA_MAP: &str = "ast-bro.map.v1";
/// `show` became a multi-file command (it accepts a file list, a directory,
/// or a glob), so its payload is a `files` array like `map`'s rather than a
/// single top-level `path` / `language` / `matches`. v1 was the single-file
/// shape; the bump is how a consumer tells which one it received.
pub const JSON_SCHEMA_SHOW: &str = "ast-bro.show.v2";
pub const JSON_SCHEMA_IMPLEMENTS: &str = "ast-bro.implements.v1";
pub const JSON_SCHEMA_SURFACE: &str = "ast-bro.surface.v1";
pub const JSON_SCHEMA_DEPS: &str = "ast-bro.deps.v1";
pub const JSON_SCHEMA_REVERSE_DEPS: &str = "ast-bro.reverse-deps.v1";
pub const JSON_SCHEMA_CYCLES: &str = "ast-bro.cycles.v1";
pub const JSON_SCHEMA_GRAPH: &str = "ast-bro.graph.v1";
pub const JSON_SCHEMA_DEPS_INDEX: &str = "ast-bro.deps-index.v1";
pub const JSON_SCHEMA_CALLERS: &str = "ast-bro.callers.v1";
pub const JSON_SCHEMA_CALLEES: &str = "ast-bro.callees.v1";
pub const JSON_SCHEMA_TRACE: &str = "ast-bro.trace.v1";
pub const JSON_SCHEMA_RUN: &str = "ast-bro.run.v1";
/// Unified on-disk cache holding both the file-level dep graph and (lazily)
/// the symbol-level call graph. Bumped from `graph-index.v1` to `v2` after
/// the per-file-invalidation work removed `skip_serializing_if` from
/// `DepEdge` and `CallEdge` Option/Vec fields — bincode uses positional
/// encoding so a skipped field corrupts everything that follows. v1 caches
/// were silently un-decodable (the loader returned `None` and rebuilt
/// every time, masking the bug). The v2 bump ensures upgrading users get
/// a clean rebuild rather than continuing to hit the silent fallback.
pub const JSON_SCHEMA_GRAPH_INDEX: &str = "ast-bro.graph-index.v1";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy, Default)]
pub enum DeclarationKind {
    #[default]
    Namespace,
    Class,
    Struct,
    Interface,
    Record,
    Enum,
    EnumMember,
    Method,
    Function,
    Constructor,
    Destructor,
    Property,
    Indexer,
    Field,
    Event,
    Delegate,
    Operator,
    Heading,
    CodeBlock,
    /// A markdown file's leading `---…---` YAML metadata block. Structure,
    /// not prose: for a task card or a Jekyll / Hugo / Astro / Obsidian page
    /// the frontmatter often *is* the file's whole content, and without a
    /// declaration for it such a file maps as empty.
    Frontmatter,
}

impl DeclarationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Record => "record",
            Self::Enum => "enum",
            Self::EnumMember => "enum_member",
            Self::Method => "method",
            Self::Function => "function",
            Self::Constructor => "ctor",
            Self::Destructor => "dtor",
            Self::Property => "property",
            Self::Indexer => "indexer",
            Self::Field => "field",
            Self::Event => "event",
            Self::Delegate => "delegate",
            Self::Operator => "operator",
            Self::Heading => "heading",
            Self::CodeBlock => "code_block",
            Self::Frontmatter => "frontmatter",
        }
    }
}

impl std::fmt::Display for DeclarationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Serialize for DeclarationKind {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Declaration {
    pub kind: DeclarationKind,
    pub name: String,
    pub signature: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attrs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<String>,
    pub docs_inside: bool,
    pub visibility: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub doc_start_byte: usize,
    /// Source-true keyword the language uses for this declaration when
    /// it diverges from the canonical `kind` (e.g. Rust `trait` vs the
    /// canonical `interface`, Scala `case class`, Kotlin `data class`,
    /// C# `record`). Renderers prefer this over `kind.to_string()` when
    /// present. Adapters populate as needed; default `None` → no change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_kind: Option<String>,
    /// Type/method modifiers worth surfacing in compact renderings:
    /// `async`, `static`, `abstract`, `override`, `unsafe`, `const`,
    /// `suspend`, `sealed`, `final`, `open`, `partial`, `inline`, …
    /// The renderer shows these as `[mod]` markers next to the symbol
    /// in digest output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<String>,
    /// True when the adapter saw a deprecation marker (`#[deprecated]`,
    /// `@Deprecated`, `@deprecated`, `[Obsolete]`). Renderers append a
    /// `[deprecated]` tag.
    #[serde(default, skip_serializing_if = "_is_false")]
    pub deprecated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Declaration>,
    /// Raw call-sites that appear *directly* inside this declaration's body.
    /// Does not include nested-declaration bodies — those have their own
    /// `calls` lists. Adapters populate during their tree walk; left empty
    /// when the adapter doesn't yet implement call-site extraction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<CallSite>,
}

fn _is_false(b: &bool) -> bool {
    !*b
}

/// The declaration kinds whose children `--max-members` caps — the one
/// definition of "member" shared by the text renderer and the JSON filter,
/// so the two can't drift apart (PR #38 review).
fn _is_capped_type(kind: &DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Class
            | DeclarationKind::Struct
            | DeclarationKind::Interface
            | DeclarationKind::Record
            | DeclarationKind::Enum
    )
}

/// Whether a child declaration counts as a *member* for `--max-members`.
/// Nested types are structure, not members — the digest renderer lists
/// them as their own flattened entries (`_digest_members` skips them) —
/// and enum variants are the type's shape, not its API surface (the names
/// renderer never lists them at all). Neither is cut or counted by the
/// cap, in text and JSON alike, so `+N more` and `dropped_members` agree.
fn _counts_toward_member_cap(kind: &DeclarationKind) -> bool {
    !matches!(
        kind,
        DeclarationKind::Class
            | DeclarationKind::Struct
            | DeclarationKind::Interface
            | DeclarationKind::Record
            | DeclarationKind::Enum
            | DeclarationKind::Namespace
            | DeclarationKind::EnumMember
    )
}

/// One unresolved call-site as observed during AST walking. Resolution to
/// a project-qualified name happens later in `src/calls/resolve.rs`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CallSite {
    /// Bare callee name as written: `foo()` → "foo"; `obj.bar()` → "bar";
    /// `Foo::baz()` → "baz".
    pub name: String,
    /// Receiver expression text when the callee is method-like
    /// (`obj.bar()` → "obj"; `self.x()` → "self"; `Foo::baz()` → "Foo").
    /// `None` for free-function calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// 1-indexed line of the call site.
    pub line: u32,
    /// Call kind — distinguishes a regular call from `new T()`, macro, etc.
    #[serde(default, skip_serializing_if = "_call_kind_is_default")]
    pub kind: CallKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallKind {
    #[default]
    Call,
    /// `new Foo()`, struct literal, Python class call, etc.
    Construct,
    /// Rust `macro!()`, C/C++ macro invocation.
    Macro,
    /// `super.foo()`, Rust `super::foo()`.
    Super,
}

impl CallKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Construct => "construct",
            Self::Macro => "macro",
            Self::Super => "super",
        }
    }
}

impl Serialize for CallKind {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

fn _call_kind_is_default(k: &CallKind) -> bool {
    matches!(k, CallKind::Call)
}

/// One `import` / `use` / `using` binding from a source file. Populated by
/// adapters; consumed by the call-graph resolver in pass A and useful as
/// extra signal for downstream consumers.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ImportBinding {
    /// Local name visible in this file: `Foo`, `bar`, `pd` (in
    /// `import pandas as pd`).
    pub local: String,
    /// Module specifier as written: `crate::net::Bar`, `numpy.linalg`,
    /// `./helpers`, `com.foo.Bar`.
    pub module: String,
    /// 1-indexed line of the import statement.
    pub line: u32,
}

impl Declaration {
    pub fn lines_suffix(&self) -> String {
        if self.start_line == 0 {
            String::new()
        } else if self.start_line == self.end_line {
            format!("  L{}", self.start_line)
                .truecolor(150, 150, 150)
                .to_string()
        } else {
            format!("  L{}-{}", self.start_line, self.end_line)
                .truecolor(150, 150, 150)
                .to_string()
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ParseResult {
    #[serde(serialize_with = "_serialize_path")]
    pub path: PathBuf,
    pub language: &'static str,
    #[serde(skip)]
    pub source: Vec<u8>,
    pub line_count: usize,
    pub error_count: usize,
    pub declarations: Vec<Declaration>,
    /// Local-binding → module map for this file, populated by adapters from
    /// `use` / `import` / `using` statements. Consumed by the call-graph
    /// resolver in pass A.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<ImportBinding>,
}

#[derive(Debug, Clone)]
pub struct MapOptions {
    pub include_private: bool,
    pub include_fields: bool,
    pub include_docs: bool,
    pub include_attributes: bool,
    pub include_line_numbers: bool,
    pub max_doc_lines: usize,
    /// Cap members per type in JSON digest output (mirrors DigestOptions::max_members_per_type).
    pub max_members: Option<usize>,
}

impl Default for MapOptions {
    fn default() -> Self {
        Self {
            include_private: true,
            include_fields: true,
            include_docs: true,
            include_attributes: true,
            include_line_numbers: true,
            max_doc_lines: crate::defaults::MAX_DOC_LINES,
            max_members: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DigestOptions {
    pub include_private: bool,
    pub include_fields: bool,
    pub include_attributes: bool,
    pub include_line_numbers: bool,
    pub max_members_per_type: usize,
    pub max_heading_depth: usize,
}

impl Default for DigestOptions {
    fn default() -> Self {
        Self {
            include_private: false,
            include_fields: false,
            include_attributes: true,
            include_line_numbers: true,
            max_members_per_type: crate::defaults::MAX_MEMBERS,
            max_heading_depth: crate::defaults::MAX_HEADING_DEPTH,
        }
    }
}

// --- Marker post-processing ---
//
// Each adapter emits a Declaration with the basics (kind, signature,
// attrs, docs, visibility). The richer presentation fields —
// `native_kind`, `modifiers`, `deprecated` — are derived from those
// basics in one centralised pass keyed off the language string. That
// keeps adapters dumb and fast (one tree walk only) and lets us add a
// new marker by editing one file.

/// Walk every declaration tree and fill in `native_kind`, `modifiers`,
/// and `deprecated` based on the language conventions. Idempotent —
/// values an adapter already set are preserved.
pub fn populate_markers(decls: &mut [Declaration], language: &str) {
    for d in decls.iter_mut() {
        _populate_one(d, language);
        if !d.children.is_empty() {
            populate_markers(&mut d.children, language);
        }
    }
}

fn _populate_one(d: &mut Declaration, lang: &str) {
    if d.native_kind.is_none() {
        d.native_kind = _native_kind(d, lang);
    }
    if d.modifiers.is_empty() {
        d.modifiers = _modifiers(d, lang);
    }
    if !d.deprecated {
        d.deprecated = _deprecated(d, lang);
    }
}

fn _native_kind(d: &Declaration, lang: &str) -> Option<String> {
    let sig = d.signature.as_str();
    // Skip the visibility prefix when looking for a leading keyword.
    let starts_with_kw = |kw: &str| -> bool {
        sig.split_whitespace()
            .find(|t| !matches!(*t, "pub" | "private" | "public" | "internal" | "protected"))
            .map(|t| t == kw)
            .unwrap_or(false)
    };
    let contains_kw_pair = |first: &str, second: &str| -> bool {
        let toks: Vec<&str> = sig.split_whitespace().collect();
        toks.windows(2).any(|w| w[0] == first && w[1] == second)
    };

    match lang {
        "rust" => {
            if matches!(d.kind, DeclarationKind::Interface) && starts_with_kw("trait") {
                Some("trait".to_string())
            } else {
                None
            }
        }
        "scala" => {
            if contains_kw_pair("case", "class") {
                Some("case class".to_string())
            } else if contains_kw_pair("case", "object") {
                Some("case object".to_string())
            } else if starts_with_kw("object") {
                Some("object".to_string())
            } else if starts_with_kw("trait") {
                Some("trait".to_string())
            } else {
                None
            }
        }
        "kotlin" => {
            if contains_kw_pair("data", "class") {
                Some("data class".to_string())
            } else if contains_kw_pair("enum", "class") {
                Some("enum class".to_string())
            } else if contains_kw_pair("sealed", "class") {
                Some("sealed class".to_string())
            } else if contains_kw_pair("companion", "object") {
                Some("companion object".to_string())
            } else if starts_with_kw("object") {
                Some("object".to_string())
            } else {
                None
            }
        }
        "java" => {
            if starts_with_kw("record") {
                Some("record".to_string())
            } else if starts_with_kw("enum") {
                Some("enum".to_string())
            } else if starts_with_kw("interface") {
                Some("interface".to_string())
            } else {
                None
            }
        }
        "csharp" => {
            if contains_kw_pair("record", "struct") {
                Some("record struct".to_string())
            } else if starts_with_kw("record") {
                Some("record".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn _modifiers(d: &Declaration, lang: &str) -> Vec<String> {
    let want: &[&str] = match lang {
        "rust" => &["async", "unsafe", "const", "extern"],
        "python" => &["async"],
        "typescript" => &["async", "static", "abstract", "readonly", "override"],
        "java" => &["static", "abstract", "final", "synchronized", "default", "native"],
        "kotlin" => &[
            "suspend", "open", "inner", "value", "inline", "infix", "tailrec", "operator",
            "abstract", "override", "sealed", "final",
        ],
        "scala" => &["sealed", "final", "abstract", "implicit", "inline", "lazy", "override"],
        "csharp" => &["partial", "sealed", "static", "abstract", "virtual", "override", "async"],
        _ => &[],
    };
    if want.is_empty() {
        return Vec::new();
    }
    // Stop scanning once we hit the actual declaration keyword — anything
    // beyond is the name/parameter list, not a modifier.
    let stop_words: &[&str] = match lang {
        "rust" => &["fn", "trait", "struct", "enum", "impl", "type", "mod"],
        "python" => &["def", "class"],
        "typescript" => &[
            "function", "class", "interface", "enum", "type", "const", "let", "var",
        ],
        "java" => &["class", "interface", "enum", "record", "void"],
        "kotlin" => &["fun", "class", "interface", "object", "enum", "val", "var"],
        "scala" => &["def", "class", "trait", "object", "val", "var", "type"],
        "csharp" => &["class", "interface", "struct", "record", "enum", "void"],
        _ => &[],
    };

    let mut out = Vec::new();
    for tok in d.signature.split_whitespace() {
        if stop_words.contains(&tok) {
            break;
        }
        if want.contains(&tok) {
            out.push(tok.to_string());
        }
    }

    // Python decorators land in `attrs`, not the signature line.
    if lang == "python" {
        for a in &d.attrs {
            let trimmed = a.trim_start_matches('@');
            let name = trimmed.split('(').next().unwrap_or(trimmed).trim();
            match name {
                "classmethod" => out.push("classmethod".to_string()),
                "staticmethod" => out.push("static".to_string()),
                "abstractmethod" => out.push("abstract".to_string()),
                "property" => out.push("property".to_string()),
                _ => {}
            }
        }
    }

    out
}

fn _deprecated(d: &Declaration, lang: &str) -> bool {
    let attr_has = |needle: &str| d.attrs.iter().any(|a| a.contains(needle));
    let doc_has = |needle: &str| d.docs.iter().any(|x| x.contains(needle));
    match lang {
        "rust" => attr_has("#[deprecated") || attr_has("#[ deprecated"),
        "python" => {
            attr_has("@deprecated")
                || attr_has("@typing.deprecated")
                || attr_has("@warnings.deprecated")
        }
        "typescript" => doc_has("@deprecated") || attr_has("@deprecated"),
        "java" | "kotlin" => attr_has("@Deprecated"),
        "scala" => attr_has("@deprecated"),
        "csharp" => attr_has("[Obsolete") || attr_has("[ Obsolete"),
        // Go convention: a `Deprecated:` paragraph in the doc comment.
        "go" => doc_has("Deprecated:"),
        _ => false,
    }
}

// --- Renderers ---

/// Generate a dynamic legend based on what token types are actually
/// present in the digest output. Avoids showing tokens that never appear.
fn _generate_digest_legend(results: &[ParseResult]) -> String {
    let mut has_callable = false;
    let mut has_overloads = false;
    let mut has_modifiers = false;
    let mut has_deprecated = false;

    for r in results {
        _scan_siblings_for_legend(
            &r.declarations,
            &mut has_callable,
            &mut has_overloads,
            &mut has_modifiers,
            &mut has_deprecated,
        );
    }

    let mut entries: Vec<&'static str> = Vec::new();
    if has_callable {
        entries.push("name() = callable");
    }
    if has_overloads {
        entries.push("[N×] = N overloads");
    }
    if has_modifiers {
        entries.push("[m] = modifier");
    }
    if has_deprecated {
        entries.push("[deprecated] = deprecated");
    }

    if entries.is_empty() {
        return String::new();
    }

    format!("# legend: {}\n", entries.join(" · "))
}

/// Walk a list of sibling declarations looking for legend-relevant signals.
/// `has_overloads` mirrors `_collapse_overloads`'s actual collapse rule
/// (adjacent same-kind same-name callables in source order) rather than
/// scanning the rendered name for `[N×]` — the legend runs on the raw IR
/// before `_collapse_overloads` rewrites any names, so a substring check
/// would never match.
fn _scan_siblings_for_legend(
    siblings: &[Declaration],
    has_callable: &mut bool,
    has_overloads: &mut bool,
    has_modifiers: &mut bool,
    has_deprecated: &mut bool,
) {
    use DeclarationKind::*;
    if !*has_overloads {
        for w in siblings.windows(2) {
            if w[0].kind == w[1].kind
                && w[0].name == w[1].name
                && matches!(w[0].kind, Method | Function | Constructor | Destructor | Operator)
            {
                *has_overloads = true;
                break;
            }
        }
    }
    for d in siblings {
        if !*has_callable
            && matches!(d.kind, Method | Function | Constructor | Destructor | Operator)
        {
            *has_callable = true;
        }
        if !*has_modifiers && !d.modifiers.is_empty() {
            *has_modifiers = true;
        }
        if !*has_deprecated && d.deprecated {
            *has_deprecated = true;
        }
        _scan_siblings_for_legend(
            &d.children,
            has_callable,
            has_overloads,
            has_modifiers,
            has_deprecated,
        );
    }
}

/// Map a line count to a coarse size bucket. Lets agents pick whether
/// to read the file in full or just the digest.
fn _size_label(line_count: usize) -> &'static str {
    match line_count {
        0..=50 => "tiny",
        51..=200 => "small",
        201..=600 => "medium",
        601..=1500 => "large",
        _ => "xlarge",
    }
}


pub fn render_map(result: &ParseResult, opts: &MapOptions) -> String {
    render_map_units(result, opts)
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a map is assembled from: one piece of text plus the structure that text
/// does not carry.
///
/// [`render_map`] joins the text and drops the rest, which is all a reader needs.
/// A caller that has to *shorten* a map needs the rest: which declaration a piece
/// renders, and which declaration encloses it. Rendered text answers neither
/// question. Indentation looks like an answer and is not one — a doc comment
/// keeps the indentation it had in the source, so its continuation lines land at
/// arbitrary columns, and a separator has no indentation at all.
#[derive(Debug, Clone)]
pub struct MapUnit {
    /// The text [`render_map`] writes for this piece.
    pub text: String,
    /// What this piece is, and for a declaration, which one.
    pub kind: MapUnitKind,
    /// The declaration this piece nests under, `None` at the top level.
    pub parent: Option<usize>,
}

/// What a [`MapUnit`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapUnitKind {
    /// The file header or a parse warning: not a declaration, and a map missing
    /// one reads as a map of a different file.
    Preamble,
    /// One declaration together with its doc comment.
    ///
    /// `id` numbers the declaration in pre-order over the whole tree, counting
    /// the declarations [`MapOptions`] filtered out as well, so the number means
    /// the same thing in every render of one file and two renders can be matched
    /// against each other.
    Declaration { id: usize },
    /// A `... +N more member(s)` marker: a statement about the enclosing type
    /// rather than a declaration of its own.
    MoreMembers,
    /// The blank line between items, which carries no structure.
    Separator,
}

impl MapUnit {
    fn preamble(text: String) -> Self {
        Self {
            text,
            kind: MapUnitKind::Preamble,
            parent: None,
        }
    }

    fn separator() -> Self {
        Self {
            text: String::new(),
            kind: MapUnitKind::Separator,
            parent: None,
        }
    }
}

/// The map as the units it is assembled from, rather than as one string.
pub fn render_map_units(result: &ParseResult, opts: &MapOptions) -> Vec<MapUnit> {
    let mut units = vec![MapUnit::preamble(_format_file_header(
        &format!("# {}", result.path.display()),
        result,
    ))];
    if let Some(warn) = _format_error_warning(result) {
        units.push(MapUnit::preamble(warn));
    }
    let mut next_id = 0usize;
    for decl in &result.declarations {
        _render_decl(decl, opts, 0, None, &mut next_id, &mut units);
    }
    units
}
fn _format_file_header(prefix: &str, result: &ParseResult) -> String {
    let counts = _collect_counts(&result.declarations);
    // Lead with size bucket + raw counts. Char count is intentionally
    // raw rather than a token estimate — tokenizers vary, and a single
    // chars/4 number printed authoritatively would mislead. Agents that
    // care can divide by their tokenizer's empirical ratio.
    let mut parts = vec![
        format!("[{}]", _size_label(result.line_count)),
        format!("{} lines", result.line_count),
        format!("{} chars", result.source.len()),
    ];

    if result.language == "markdown" {
        // Frontmatter first: it leads the file and leads the outline, so the
        // header should read in the same order the body does.
        let order = [
            ("frontmatter", "frontmatter"),
            ("headings", "headings"),
            ("code_blocks", "code blocks"),
        ];
        for (key, label) in order {
            let n = counts.get(key).copied().unwrap_or(0);
            if n > 0 {
                parts.push(format!("{} {}", n, label));
            }
        }
    } else {
        let order = [
            ("types", "types"),
            ("methods", "methods"),
            ("fields", "fields"),
        ];
        for (key, label) in order {
            let n = counts.get(key).copied().unwrap_or(0);
            if n > 0 {
                parts.push(format!("{} {}", n, label));
            }
        }
    }
    format!("{} ({})", prefix.blue().bold(), parts.join(", ").dimmed())
}

fn _format_error_warning(result: &ParseResult) -> Option<String> {
    if result.error_count == 0 {
        return None;
    }
    let plural = if result.error_count != 1 { "s" } else { "" };
    Some(
        format!(
            "# WARNING: {} parse error{} — output may be incomplete",
            result.error_count, plural
        )
        .red()
        .to_string(),
    )
}

fn _collect_counts(decls: &[Declaration]) -> std::collections::HashMap<&'static str, usize> {
    use DeclarationKind::*;
    let mut out = std::collections::HashMap::new();
    out.insert("types", 0);
    out.insert("methods", 0);
    out.insert("fields", 0);
    out.insert("headings", 0);
    out.insert("code_blocks", 0);
    out.insert("frontmatter", 0);

    let mut stack: Vec<&Declaration> = decls.iter().collect();
    while let Some(d) = stack.pop() {
        let k = &d.kind;
        match k {
            Class | Struct | Interface | Record | Enum => *out.get_mut("types").unwrap() += 1,
            Method | Function | Constructor | Destructor | Operator => {
                *out.get_mut("methods").unwrap() += 1
            }
            Field | Property | Event | Indexer => *out.get_mut("fields").unwrap() += 1,
            Heading => *out.get_mut("headings").unwrap() += 1,
            CodeBlock => *out.get_mut("code_blocks").unwrap() += 1,
            Frontmatter => *out.get_mut("frontmatter").unwrap() += 1,
            _ => {}
        }
        for child in &d.children {
            stack.push(child);
        }
    }
    out
}

fn _render_decl(
    decl: &Declaration,
    opts: &MapOptions,
    indent: usize,
    parent: Option<usize>,
    next_id: &mut usize,
    out: &mut Vec<MapUnit>,
) {
    use DeclarationKind::*;

    let push_docs = |prefix: &str, lines_out: &mut Vec<String>| {
        for d in _clip_docs(&decl.docs, opts.max_doc_lines) {
            lines_out.push(format!(
                "{}{}",
                prefix,
                d.trim_end_matches(&['\r', '\n'][..])
            ));
        }
    };

    let id = *next_id;
    *next_id += 1;

    if !_map_eligible(decl, opts) {
        // Number the subtree anyway: a declaration's number has to name the same
        // declaration whatever the options dropped, or two renders of one file
        // cannot be matched against each other.
        *next_id += _decl_count(decl);
        return;
    }

    let prefix = "    ".repeat(indent);

    // The declaration and its doc comment go out as one unit, so a consumer that
    // drops the declaration cannot leave the comment behind to read as
    // documentation of whatever follows it.
    let mut unit: Vec<String> = Vec::new();
    if opts.include_docs && !decl.docs.is_empty() && !decl.docs_inside {
        push_docs(&prefix, &mut unit);
    }

    let attrs_prefix = if opts.include_attributes && !decl.attrs.is_empty() {
        format!("{} ", decl.attrs.join(" "))
    } else {
        String::new()
    };

    let suffix = if opts.include_line_numbers {
        decl.lines_suffix()
    } else {
        String::new()
    };

    if decl.kind == Namespace {
        unit.push(format!(
            "{}namespace {}",
            prefix,
            decl.name.magenta().bold()
        ));
    } else {
        unit.push(format!(
            "{}{}{}{}",
            prefix, attrs_prefix, decl.signature, suffix
        ));
    }

    if opts.include_docs && !decl.docs.is_empty() && decl.docs_inside {
        let inner_prefix = "    ".repeat(indent + 1);
        push_docs(&inner_prefix, &mut unit);
    }
    out.push(MapUnit {
        text: unit.join("\n"),
        kind: MapUnitKind::Declaration { id },
        parent,
    });

    // `--max-members` caps how many members a type renders; the remainder
    // is reported, never silently dropped (issues #37 / #32).
    let cap = if _is_capped_type(&decl.kind) {
        opts.max_members.unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    let visible = |d: &Declaration| _map_eligible(d, opts);
    let mut shown = 0usize;
    let mut hidden = 0usize;
    for child in &decl.children {
        if visible(child) && _counts_toward_member_cap(&child.kind) {
            if shown >= cap {
                hidden += 1;
                *next_id += 1 + _decl_count(child);
                continue;
            }
            shown += 1;
        }
        _render_decl(child, opts, indent + 1, Some(id), next_id, out);
    }
    if hidden > 0 {
        out.push(MapUnit {
            text: format!(
                "{}    ... +{} more member(s) (raise --max-members)",
                prefix, hidden
            )
            .dimmed()
            .to_string(),
            kind: MapUnitKind::MoreMembers,
            parent: Some(id),
        });
    }

    if indent == 0
        || matches!(
            decl.kind,
            Class | Struct | Interface | Record | Enum | Namespace
        )
    {
        out.push(MapUnit::separator());
    }
}

/// Declarations under `decl`, `decl` itself excluded.
fn _decl_count(decl: &Declaration) -> usize {
    decl.children.iter().map(|c| 1 + _decl_count(c)).sum()
}

fn _clip_docs(docs: &[String], limit: usize) -> Vec<String> {
    if docs.len() <= limit {
        docs.to_vec()
    } else {
        let mut clipped = docs[..limit].to_vec();
        clipped.push("...".to_string());
        clipped
    }
}

pub fn render_digest(
    results: &[ParseResult],
    opts: &DigestOptions,
    root: Option<&std::path::Path>,
) -> String {
    if results.is_empty() {
        return "# no files\n".to_string();
    }

    let default_root = results[0].path.parent().unwrap_or(std::path::Path::new(""));
    let root = root.unwrap_or(default_root);

    let mut grouped: std::collections::BTreeMap<&std::path::Path, Vec<&ParseResult>> =
        std::collections::BTreeMap::new();
    for r in results {
        let parent = r.path.parent().unwrap_or(std::path::Path::new(""));
        grouped.entry(parent).or_default().push(r);
    }

    // Check if all files failed to parse — surface a batch warning.
    let all_failed = !results.is_empty() && results.iter().all(|r| r.error_count > 0);
    let batch_warning = if all_failed {
        Some(format!(
            "# BATCH WARNING: All {} files failed to parse — output may be incomplete",
            results.len()
        ))
    } else {
        None
    };

    // Top-of-output legend so an agent reading the digest cold knows
    // what the compact tokens mean.
    let legend = _generate_digest_legend(results);
    let mut lines = if legend.is_empty() {
        vec![]
    } else {
        vec![legend.dimmed().to_string()]
    };

    // Batch warning comes after legend but before content.
    if let Some(warn) = batch_warning {
        lines.push(warn.red().to_string());
    }
    for (dir, res) in grouped {
        let rel = dir.strip_prefix(root).unwrap_or(dir);
        lines.push(format!("{}/", rel.display().to_string().cyan().bold()));
        for r in res {
            lines.extend(_digest_one(r, opts));
        }
        lines.push(String::new());
    }

    let mut out = lines.join("\n");
    out = out.trim_end().to_string();
    out.push('\n');
    out
}

fn _digest_one(result: &ParseResult, opts: &DigestOptions) -> Vec<String> {
    let name = result
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let mut lines = vec![_format_file_header(&format!("  {}", name), result)];
    if let Some(warn) = _format_error_warning(result) {
        lines.push(format!("  {}", warn));
    }

    if result.language == "markdown" {
        let toc = _digest_markdown(&result.declarations, opts, 4, 1);
        if toc.is_empty() {
            if let Some(last) = lines.last_mut() {
                last.push_str("  # empty");
            }
            return lines;
        }
        lines.extend(toc);
        return lines;
    }

    let types = _flatten_types(&result.declarations, "", opts);
    let free_functions = _flatten_free_functions(&result.declarations, opts);

    if types.is_empty() && free_functions.is_empty() {
        if let Some(last) = lines.last_mut() {
            last.push_str("  # no declarations");
        }
        return lines;
    }

    for t in types {
        let mut header = String::from("    ");
        // Inline attrs (e.g. `#[derive(Debug)]`, `@dataclass`) before the
        // kind keyword. The adapter populated `attrs` already; we just
        // surface a short joined form (skip overly long attr lists).
        let attrs_inline = if opts.include_attributes {
            _format_inline_attrs(&t.attrs)
        } else {
            String::new()
        };
        if !attrs_inline.is_empty() {
            header.push_str(&attrs_inline);
            header.push(' ');
        }
        // Prefer `native_kind` (`trait`, `case class`, `data class`, …)
        // when the adapter set it; fall back to the canonical kind.
        let kind_str = t
            .native_kind
            .as_deref()
            .unwrap_or(t.kind.as_str());
        // Modifiers (abstract / sealed / final / partial / …) — but skip
        // ones the native_kind already names, so we don't render
        // "sealed sealed class" for `sealed class Foo`.
        for m in &t.modifiers {
            if !kind_str.split_whitespace().any(|w| w == m) {
                header.push_str(&format!("{} ", m.dimmed()));
            }
        }
        header.push_str(&kind_str.italic().to_string());
        header.push(' ');
        header.push_str(&t.name.green().bold().to_string());
        if !t.bases.is_empty() {
            header.push_str(" : ");
            header.push_str(&t.bases.join(", "));
        }
        if t.deprecated {
            header.push_str(&format!(" {}", "[deprecated]".red()));
        }
        if opts.include_line_numbers {
            header.push_str(&t.lines_suffix());
        }
        lines.push(header);

        let members = _digest_members(&t, opts);
        if !members.is_empty() {
            // Cap the *raw* member list, then collapse survivors for
            // display — so the "+N more" count and the JSON payload's
            // `dropped_members` count the same declarations even when
            // overloads collapse into one token.
            let cap = std::cmp::min(members.len(), opts.max_members_per_type);
            let dropped = members.len() - cap;
            let collapsed =
                _collapse_overloads(members[..cap].iter().map(|d| (*d).clone()).collect());
            let tokens: Vec<String> = collapsed.iter().map(_format_member_token).collect();
            lines.extend(_wrap_tokens(&tokens, 100, "      "));
            if dropped > 0 {
                lines.push(format!("      ... +{} more", dropped).dimmed().to_string());
            }
        }
    }

    if !free_functions.is_empty() {
        // Free functions are not members of a type, so `--max-members`
        // does not apply — matching the JSON payload, which never caps
        // them. Capping here without a marker was silent truncation.
        let collapsed = _collapse_overloads(free_functions.iter().map(|d| (*d).clone()).collect());
        let tokens: Vec<String> = collapsed.iter().map(_format_member_token).collect();
        lines.extend(_wrap_tokens(&tokens, 100, "    "));
    }

    lines
}

/// Render a single member as a compact token for the digest list.
fn _format_member_token(m: &Declaration) -> String {
    let mut s = String::new();
    for mod_ in &m.modifiers {
        s.push_str(&format!("[{}] ", mod_.dimmed()));
    }
    let is_callable = matches!(
        m.kind,
        DeclarationKind::Method
            | DeclarationKind::Function
            | DeclarationKind::Constructor
            | DeclarationKind::Destructor
            | DeclarationKind::Operator
    );
    if is_callable {
        // Display name might already carry an overload-count suffix
        // ("foo [3×]") that `_collapse_overloads` attached — keep it
        // outside the parens.
        if let Some((base, suffix)) = m.name.rsplit_once(' ') {
            if suffix.starts_with('[') {
                s.push_str(&format!("{}() {}", base.yellow(), suffix.dimmed()));
            } else {
                s.push_str(&format!("{}()", m.name.yellow()));
            }
        } else {
            s.push_str(&format!("{}()", m.name.yellow()));
        }
    } else {
        s.push_str(&format!(
            "{} [{}]",
            m.name.yellow(),
            m.kind.as_str().dimmed()
        ));
    }
    if m.deprecated {
        s.push_str(&format!(" {}", "[deprecated]".red()));
    }
    s
}

/// Collapse adjacent same-name callables into one token with a `[N×]`
/// suffix. Saves a noisy line on overload-heavy languages (Java/C# in
/// particular). Operates on owned `Declaration`s so the renderer can
/// rewrite the `name`.
fn _collapse_overloads(members: Vec<Declaration>) -> Vec<Declaration> {
    let mut out: Vec<Declaration> = Vec::with_capacity(members.len());
    for m in members {
        let is_callable = matches!(
            m.kind,
            DeclarationKind::Method
                | DeclarationKind::Function
                | DeclarationKind::Constructor
                | DeclarationKind::Destructor
                | DeclarationKind::Operator
        );
        if is_callable {
            if let Some(prev) = out.last_mut() {
                if prev.kind == m.kind {
                    let prev_base = prev
                        .name
                        .rsplit_once(' ')
                        .filter(|(_, s)| s.starts_with('['))
                        .map(|(b, _)| b)
                        .unwrap_or(prev.name.as_str());
                    if prev_base == m.name {
                        let count = prev
                            .name
                            .rsplit_once(' ')
                            .and_then(|(_, s)| {
                                s.strip_prefix('[')
                                    .and_then(|s| s.strip_suffix("×]"))
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(1)
                            + 1;
                        prev.name = format!("{} [{}×]", prev_base, count);
                        continue;
                    }
                }
            }
        }
        out.push(m);
    }
    out
}

/// Pick a small subset of attrs worth showing inline before the kind
/// keyword. We surface short attrs (under 40 chars combined) to avoid
/// noise. Long ones (full `#[derive(Debug, Clone, PartialEq, Eq, …)]`)
/// stay in the outline-only attrs prefix.
fn _format_inline_attrs(attrs: &[String]) -> String {
    if attrs.is_empty() {
        return String::new();
    }
    let joined = attrs.join(" ");
    if joined.len() > 40 {
        // Fall back to a short marker so the reader knows attrs exist
        // without flooding the line.
        return format!("[{}attr{}]", attrs.len(), if attrs.len() == 1 { "" } else { "s" })
            .dimmed()
            .to_string();
    }
    joined.dimmed().to_string()
}

fn _flatten_types(decls: &[Declaration], prefix: &str, opts: &DigestOptions) -> Vec<Declaration> {
    use DeclarationKind::*;
    let mut out = Vec::new();
    for d in decls {
        if d.kind == Namespace {
            // Private namespaces (Rust `mod tests`, private inline mods)
            // are dropped subtree-and-all under the public-only view, the
            // same way the JSON filter treats them.
            if d.visibility == "private" && !opts.include_private {
                continue;
            }
            let new_prefix = if prefix.is_empty() {
                format!("{}.", d.name)
            } else {
                format!("{}{}.", prefix, d.name)
            };
            out.extend(_flatten_types(&d.children, &new_prefix, opts));
        } else if matches!(d.kind, Class | Struct | Interface | Record | Enum) {
            if d.visibility == "private" && !opts.include_private {
                continue;
            }
            let mut qualified = d.clone();
            if !prefix.is_empty() {
                qualified.name = format!("{}{}", prefix, d.name);
            }
            let new_prefix = format!("{}.", qualified.name);
            out.push(qualified);
            out.extend(_flatten_types(&d.children, &new_prefix, opts));
        }
    }
    out
}

fn _flatten_free_functions<'a>(
    decls: &'a [Declaration],
    opts: &DigestOptions,
) -> Vec<&'a Declaration> {
    use DeclarationKind::*;
    let mut out = Vec::new();
    for d in decls {
        if d.kind == Namespace {
            // Same private-namespace gate as `_flatten_types`: a private
            // `mod` hides its subtree from the public-only view.
            if d.visibility == "private" && !opts.include_private {
                continue;
            }
            out.extend(_flatten_free_functions(&d.children, opts));
        } else if matches!(d.kind, Class | Struct | Interface | Record | Enum) {
            continue;
        } else {
            if d.kind == Field && !opts.include_fields {
                continue;
            }
            if d.visibility == "private" && !opts.include_private {
                continue;
            }
            out.push(d);
        }
    }
    out
}

fn _digest_members<'a>(type_decl: &'a Declaration, opts: &DigestOptions) -> Vec<&'a Declaration> {
    use DeclarationKind::*;
    let mut members = Vec::new();
    for c in &type_decl.children {
        // Nested types and enum variants via the shared predicate: neither
        // is a member for display or capping purposes (types render as
        // their own flattened entries; variants are never listed here).
        if !_counts_toward_member_cap(&c.kind) {
            continue;
        }
        if c.kind == Field && !opts.include_fields {
            continue;
        }
        if c.visibility == "private" && !opts.include_private {
            continue;
        }
        members.push(c);
    }
    members
}

fn _wrap_tokens(tokens: &[String], width: usize, indent: &str) -> Vec<String> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = indent.to_string();
    for tok in tokens {
        let piece = if cur == indent {
            tok.clone()
        } else {
            format!("  {}", tok)
        };
        if cur.len() + piece.len() > width && cur != indent {
            out.push(cur);
            cur = format!("{}{}", indent, tok);
        } else {
            cur.push_str(&piece);
        }
    }
    if cur != indent {
        out.push(cur);
    }
    out
}

#[derive(Debug, Serialize)]
pub struct SymbolMatch {
    pub qualified_name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub source: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ancestor_signatures: Vec<String>,
}

pub fn find_symbols(result: &ParseResult, symbol: &str) -> Vec<SymbolMatch> {
    let parts: Vec<&str> = symbol.split('.').collect();
    let mut matches = Vec::new();
    _search_walk(
        &result.declarations,
        &result.source,
        Vec::new(),
        Vec::new(),
        &parts,
        &mut matches,
    );
    matches
}

fn _search_walk(
    decls: &[Declaration],
    src: &[u8],
    trail: Vec<String>,
    ancestors: Vec<&Declaration>,
    parts: &[&str],
    out: &mut Vec<SymbolMatch>,
) {
    for d in decls {
        let mut new_trail = trail.clone();
        if !d.name.is_empty() {
            new_trail.push(d.name.clone());
        }

        // For markdown headings, fall back to case-insensitive substring
        // per dotted part so `show README.md install` matches `## Installation`.
        // Code symbols stay on exact suffix-equality — substring there would
        // collide too easily (`new` matching `is_new`, `renew`, …).
        let heading_relax = matches!(d.kind, DeclarationKind::Heading);
        if !d.name.is_empty() && _trail_matches(&new_trail, parts, heading_relax) {
            let start = if d.doc_start_byte > 0 {
                std::cmp::max(d.doc_start_byte, d.start_byte)
            } else {
                d.start_byte
            };
            let end = d.end_byte;
            let source = String::from_utf8_lossy(&src[start..end]).to_string();

            out.push(SymbolMatch {
                qualified_name: new_trail.join("."),
                kind: d.kind.to_string(),
                start_line: d.start_line,
                end_line: d.end_line,
                source,
                ancestor_signatures: ancestors.iter().map(|a| a.signature.clone()).collect(),
            });
        }

        if !d.children.is_empty() {
            let mut new_ancestors = ancestors.clone();
            new_ancestors.push(d);
            _search_walk(&d.children, src, new_trail, new_ancestors, parts, out);
        }
    }
}

fn _trail_matches(trail: &[String], parts: &[&str], substring: bool) -> bool {
    if parts.len() > trail.len() {
        return false;
    }
    let start = trail.len() - parts.len();
    for (i, p) in parts.iter().enumerate() {
        let segment = &trail[start + i];
        let hit = if substring {
            segment.to_lowercase().contains(&p.to_lowercase())
        } else {
            segment == p
        };
        if !hit {
            return false;
        }
    }
    true
}

/// Whether any *type* declaration in `results` matches `target` — the gate
/// that distinguishes "this type has no implementations" (a real answer)
/// from "no such type anywhere" (a rejected query, issue #36). Shared by
/// the CLI and MCP `implements` surfaces so the two cannot drift. Only
/// type-shaped kinds count as proof: a function named `helper` must not
/// validate `implements helper` — and a C# `delegate` is a declared type,
/// so a 0-match answer on one is legitimate.
pub fn implements_target_exists(results: &[ParseResult], target: &str) -> bool {
    results.iter().any(|r| {
        find_symbols(r, target).iter().any(|m| {
            matches!(
                m.kind.as_str(),
                "class" | "struct" | "interface" | "record" | "enum" | "delegate"
            )
        })
    })
}

#[derive(Clone, Serialize)]
pub struct ImplMatch {
    pub path: String,
    pub start_line: usize,
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
}

pub fn find_implementations(
    results: &[ParseResult],
    type_name: &str,
    transitive: bool,
) -> Vec<ImplMatch> {
    let target = _normalize_type_name(type_name);
    let mut all_types: Vec<(&std::path::Path, &Declaration)> = Vec::new();
    for r in results {
        _collect_candidate_types(&r.declarations, &r.path, &mut all_types);
    }

    let mut direct = Vec::new();
    for (path, d) in &all_types {
        for b in &d.bases {
            if _normalize_type_name(b) == target {
                direct.push(_impl_match(path, d, Vec::new()));
                break;
            }
        }
    }

    if !transitive {
        return direct;
    }

    let mut out = direct.clone();
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    for m in &direct {
        seen.insert((m.path.clone(), m.start_line));
    }

    let mut frontier = direct;
    while !frontier.is_empty() {
        let mut next_frontier = Vec::new();
        for parent in frontier {
            let parent_name = _normalize_type_name(&parent.name);
            for (path, d) in &all_types {
                let key = (path.to_string_lossy().to_string(), d.start_line);
                if seen.contains(&key) {
                    continue;
                }
                for b in &d.bases {
                    if _normalize_type_name(b) == parent_name {
                        let mut chain = parent.via.clone();
                        chain.push(parent.name.clone());
                        let m = _impl_match(path, d, chain);
                        seen.insert(key.clone());
                        out.push(_impl_match(path, d, m.via.clone()));
                        next_frontier.push(m);
                        break;
                    }
                }
            }
        }
        frontier = next_frontier;
    }
    out
}

fn _collect_candidate_types<'a>(
    decls: &'a [Declaration],
    path: &'a std::path::Path,
    out: &mut Vec<(&'a std::path::Path, &'a Declaration)>,
) {
    use DeclarationKind::*;
    for d in decls {
        if matches!(d.kind, Class | Struct | Interface | Record) {
            out.push((path, d));
        }
        if !d.children.is_empty() {
            _collect_candidate_types(&d.children, path, out);
        }
    }
}

fn _impl_match(path: &std::path::Path, d: &Declaration, via: Vec<String>) -> ImplMatch {
    ImplMatch {
        path: path.to_string_lossy().to_string(),
        start_line: d.start_line,
        kind: d.kind.to_string(),
        name: d.name.clone(),
        via,
    }
}

fn _normalize_type_name(name: &str) -> String {
    let mut name = name.trim();
    if let Some(i) = name.find('<') {
        name = &name[..i];
    }
    if let Some(i) = name.find('[') {
        name = &name[..i];
    }
    if let Some(i) = name.rfind('.') {
        name = &name[i + 1..];
    }
    if let Some(i) = name.rfind("::") {
        name = &name[i + 2..];
    }
    name.to_string()
}

fn _digest_markdown(
    decls: &[Declaration],
    opts: &DigestOptions,
    indent: usize,
    depth: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    if depth > opts.max_heading_depth {
        return out;
    }
    let pad = " ".repeat(indent);
    for d in decls {
        if matches!(d.kind, DeclarationKind::Heading) {
            out.push(format!("{}{}{}", pad, d.signature, d.lines_suffix()));
            out.extend(_digest_markdown(&d.children, opts, indent + 2, depth + 1));
        } else if matches!(d.kind, DeclarationKind::CodeBlock) && opts.include_fields {
            out.push(format!("{}{}{}", pad, d.signature, d.lines_suffix()));
        } else if matches!(d.kind, DeclarationKind::Frontmatter) {
            // Unconditional: for a file that is *only* frontmatter, dropping
            // this line under a preset would render the file as empty, which
            // is the miss this declaration exists to fix.
            out.push(format!("{}{}{}", pad, d.signature, d.lines_suffix()));
        }
    }
    out
}

fn _serialize_path<S: Serializer>(p: &Path, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&p.to_string_lossy())
}

// ---------------------------------------------------------------------------
// JSON rendering
//
// JSON is another view over the same Declaration graph that powers the
// terminal formatters.  The schema is versioned via the JSON_SCHEMA_*
// constants; bump those on breaking changes.
// ---------------------------------------------------------------------------

/// Respect MapOptions when serialising the declaration tree. `dropped`
/// accumulates how many members `max_members` cut, so the JSON output can
/// say a cap was hit instead of silently truncating (issue #32). With
/// `include_docs` off, doc comments are shed from the payload too — they
/// are routinely a third of its weight (issue #37).
fn _filter_decls(
    decls: &[Declaration],
    opts: &MapOptions,
    dropped: &mut usize,
) -> Vec<Declaration> {
    decls
        .iter()
        .filter(|d| _map_eligible(d, opts))
        .map(|d| _filter_one(d, opts, dropped))
        .collect()
}

/// Project one (already-eligible) declaration through MapOptions. Same
/// member definition as both text renderers: the cap applies to *type*
/// members only, and nested types neither consume nor count toward it
/// (`_counts_toward_member_cap`). A child cut by the cap is not recursed
/// into, so its subtree contributes nothing to `dropped` — keeping JSON
/// `dropped_members` equal to the sum of the text renderers' `+N more`
/// lines even when a capped type is itself cut by an ancestor's cap.
fn _filter_one(d: &Declaration, opts: &MapOptions, dropped: &mut usize) -> Declaration {
    let cap = if _is_capped_type(&d.kind) {
        opts.max_members.unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    let mut members_kept = 0usize;
    let mut children = Vec::new();
    for c in d.children.iter().filter(|c| _map_eligible(c, opts)) {
        if _counts_toward_member_cap(&c.kind) {
            if members_kept >= cap {
                *dropped += 1;
                continue;
            }
            members_kept += 1;
        }
        children.push(_filter_one(c, opts, dropped));
    }
    let mut clone = d.clone();
    if !opts.include_docs {
        clone.docs = Vec::new();
    }
    clone.children = children;
    clone
}

/// Whether a declaration survives the MapOptions projection — the one
/// predicate shared by the text renderer (both its early-return and its
/// member-cap counting) and the JSON filter, so the two cannot drift.
fn _map_eligible(d: &Declaration, opts: &MapOptions) -> bool {
    use DeclarationKind::*;
    let is_field = matches!(d.kind, Field | Property | Event | Indexer);
    if is_field && !opts.include_fields {
        return false;
    }
    if d.visibility == "private" && !opts.include_private {
        return false;
    }
    // Markdown headings and fenced blocks are the *structure* of a
    // markdown file (their `docs` are empty), not doc comments —
    // `--no-docs` strips documentation fields via the JSON projection but
    // must not delete the declarations themselves; the text renderer
    // never did.
    true
}

#[derive(Serialize)]
struct JsonMapDoc<'a> {
    schema: &'static str,
    files: Vec<JsonFile<'a>>,
}

#[derive(Serialize)]
struct JsonFile<'a> {
    path: &'a str,
    language: &'static str,
    line_count: usize,
    error_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<&'static str>,
    /// Present (true) when `--max-members` cut members out of
    /// `declarations` — a capped payload must be distinguishable from a
    /// complete one (issue #32).
    #[serde(skip_serializing_if = "_is_false")]
    truncated: bool,
    /// How many members the cap dropped (0 when not truncated).
    #[serde(skip_serializing_if = "_is_zero")]
    dropped_members: usize,
    declarations: Vec<Declaration>,
}

fn _is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Serialize)]
struct JsonImplementsDoc<'a> {
    schema: &'static str,
    target: &'a str,
    transitive: bool,
    matches: &'a [ImplMatch],
}

/// Render `map` (or `map --json`) — one entry per file.
pub fn render_json_map(results: &[ParseResult], opts: &MapOptions, pretty: bool) -> String {
    let mut paths: Vec<String> = results
        .iter()
        .map(|r| r.path.to_string_lossy().into_owned())
        .collect();

    let files: Vec<JsonFile> = results
        .iter()
        .zip(paths.iter_mut())
        .map(|(r, path)| {
            let mut dropped = 0usize;
            let declarations = _filter_decls(&r.declarations, opts, &mut dropped);
            JsonFile {
                path,
                language: r.language,
                line_count: r.line_count,
                error_count: r.error_count,
                warning: if r.error_count > 0 {
                    Some("output may be incomplete")
                } else {
                    None
                },
                truncated: dropped > 0,
                dropped_members: dropped,
                declarations,
            }
        })
        .collect();

    let doc = JsonMapDoc {
        schema: JSON_SCHEMA_MAP,
        files,
    };

    // Projection flags apply to the JSON payload exactly as they apply to
    // text (issue #39): a caller that passes `--no-docs` has said it does
    // not want the field. Omission is opt-in per call — with no projection
    // flags the payload is serialized directly and stays byte-identical.
    let strip_docs = !opts.include_docs;
    let strip_lines = !opts.include_line_numbers;
    let strip_attrs = !opts.include_attributes;
    if !(strip_docs || strip_lines || strip_attrs) {
        return _to_json(&doc, pretty);
    }
    // Same failure envelope as the unprojected `_to_json` path: a
    // serialization error must not degrade into a schema-less
    // `{"projected": ...}` stub that reads as a valid (empty) answer.
    let mut val = match serde_json::to_value(&doc) {
        Ok(v) => v,
        Err(e) => return format!("{{\"error\":\"{}\"}}", e),
    };
    if let Some(files) = val.get_mut("files").and_then(|f| f.as_array_mut()) {
        for file in files {
            if let Some(decls) = file.get_mut("declarations") {
                _strip_projected_keys(decls, strip_docs, strip_lines, strip_attrs);
            }
        }
    }
    // Say which keys are gone. A projection is not always something the
    // caller asked for directly — `digest --json` inherits `--detail names`
    // from its preset and sheds `docs` with it — so a consumer that indexes
    // `docs` needs a field to guard on rather than an absence to guess at
    // (PR #38 review). Present only when something *was* stripped, which
    // keeps the unprojected payload byte-identical.
    val["projected"] = serde_json::json!({
        "docs": !strip_docs,
        "line_numbers": !strip_lines,
        "attributes": !strip_attrs,
    });
    if pretty {
        serde_json::to_string_pretty(&val)
    } else {
        serde_json::to_string(&val)
    }
    .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
}

/// Remove the keys a projection flag opted out of, recursively through
/// `children`. `doc_start_byte` rides with either group — it is a doc
/// artifact *and* a byte offset — so either `--no-docs` or `--no-lines`
/// drops it.
fn _strip_projected_keys(decls: &mut serde_json::Value, docs: bool, lines: bool, attrs: bool) {
    let Some(arr) = decls.as_array_mut() else {
        return;
    };
    for d in arr {
        let Some(map) = d.as_object_mut() else { continue };
        if docs {
            map.remove("docs");
            map.remove("docs_inside");
            map.remove("doc_start_byte");
        }
        if lines {
            map.remove("start_line");
            map.remove("end_line");
            map.remove("start_byte");
            map.remove("end_byte");
            map.remove("doc_start_byte");
        }
        if attrs {
            map.remove("attrs");
        }
        if let Some(children) = map.get_mut("children") {
            _strip_projected_keys(children, docs, lines, attrs);
        }
    }
}

/// Render `implements --json`.
pub fn render_json_implements(
    target: &str,
    matches: &[ImplMatch],
    transitive: bool,
    pretty: bool,
) -> String {
    let doc = JsonImplementsDoc {
        schema: JSON_SCHEMA_IMPLEMENTS,
        target,
        transitive,
        matches,
    };
    _to_json(&doc, pretty)
}

fn _to_json<T: Serialize>(value: &T, pretty: bool) -> String {
    if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
}
