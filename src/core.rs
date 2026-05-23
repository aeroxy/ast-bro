use colored::Colorize;
use serde::{Serialize, Serializer};
use std::path::{Path, PathBuf};

// Stable JSON schema identifiers — bump on breaking changes.
pub const JSON_SCHEMA_MAP: &str = "ast-bro.map.v1";
pub const JSON_SCHEMA_SHOW: &str = "ast-bro.show.v1";
pub const JSON_SCHEMA_IMPLEMENTS: &str = "ast-bro.implements.v1";
pub const JSON_SCHEMA_SURFACE: &str = "ast-bro.surface.v1";
pub const JSON_SCHEMA_DEPS: &str = "ast-bro.deps.v1";
pub const JSON_SCHEMA_REVERSE_DEPS: &str = "ast-bro.reverse-deps.v1";
pub const JSON_SCHEMA_CYCLES: &str = "ast-bro.cycles.v1";
pub const JSON_SCHEMA_GRAPH: &str = "ast-bro.graph.v1";
pub const JSON_SCHEMA_DEPS_INDEX: &str = "ast-bro.deps-index.v1";
pub const JSON_SCHEMA_CALLERS: &str = "ast-bro.callers.v1";
pub const JSON_SCHEMA_CALLEES: &str = "ast-bro.callees.v1";
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
            max_doc_lines: 6,
            max_members: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DigestOptions {
    pub include_private: bool,
    pub include_fields: bool,
    pub max_members_per_type: usize,
    pub max_heading_depth: usize,
}

impl Default for DigestOptions {
    fn default() -> Self {
        Self {
            include_private: false,
            include_fields: false,
            max_members_per_type: 50,
            max_heading_depth: 3,
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
    let mut lines = vec![_format_file_header(
        &format!("# {}", result.path.display()),
        result,
    )];
    if let Some(warn) = _format_error_warning(result) {
        lines.push(warn);
    }
    for decl in &result.declarations {
        _render_decl(decl, opts, 0, &mut lines);
    }
    lines.join("\n")
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
        let order = [("headings", "headings"), ("code_blocks", "code blocks")];
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
            _ => {}
        }
        for child in &d.children {
            stack.push(child);
        }
    }
    out
}

fn _render_decl(decl: &Declaration, opts: &MapOptions, indent: usize, out: &mut Vec<String>) {
    use DeclarationKind::*;

    let is_field = matches!(decl.kind, Field | Property | Event | Indexer);
    if is_field && !opts.include_fields {
        return;
    }
    if decl.visibility == "private" && !opts.include_private {
        return;
    }

    let prefix = "    ".repeat(indent);

    if opts.include_docs && !decl.docs.is_empty() && !decl.docs_inside {
        for d in _clip_docs(&decl.docs, opts.max_doc_lines) {
            out.push(format!("{}{}", prefix, d));
        }
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
        out.push(format!(
            "{}namespace {}",
            prefix,
            decl.name.magenta().bold()
        ));
    } else {
        out.push(format!(
            "{}{}{}{}",
            prefix, attrs_prefix, decl.signature, suffix
        ));
    }

    if opts.include_docs && !decl.docs.is_empty() && decl.docs_inside {
        let inner_prefix = "    ".repeat(indent + 1);
        for d in _clip_docs(&decl.docs, opts.max_doc_lines) {
            out.push(format!("{}{}", inner_prefix, d));
        }
    }

    for child in &decl.children {
        _render_decl(child, opts, indent + 1, out);
    }

    if indent == 0
        || matches!(
            decl.kind,
            Class | Struct | Interface | Record | Enum | Namespace
        )
    {
        out.push(String::new());
    }
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

    let types = _flatten_types(&result.declarations, "");
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
        let attrs_inline = _format_inline_attrs(&t.attrs);
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
        header.push_str(&t.lines_suffix());
        lines.push(header);

        let members = _digest_members(&t, opts);
        if !members.is_empty() {
            let collapsed = _collapse_overloads(members.iter().map(|d| (*d).clone()).collect());
            let shown = &collapsed[..std::cmp::min(collapsed.len(), opts.max_members_per_type)];
            let tokens: Vec<String> = shown.iter().map(_format_member_token).collect();
            lines.extend(_wrap_tokens(&tokens, 100, "      "));
            if collapsed.len() > shown.len() {
                lines.push(
                    format!("      ... +{} more", collapsed.len() - shown.len())
                        .dimmed()
                        .to_string(),
                );
            }
        }
    }

    if !free_functions.is_empty() {
        let collapsed = _collapse_overloads(free_functions.iter().map(|d| (*d).clone()).collect());
        let shown =
            &collapsed[..std::cmp::min(collapsed.len(), opts.max_members_per_type)];
        let tokens: Vec<String> = shown.iter().map(_format_member_token).collect();
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

fn _flatten_types(decls: &[Declaration], prefix: &str) -> Vec<Declaration> {
    use DeclarationKind::*;
    let mut out = Vec::new();
    for d in decls {
        if d.kind == Namespace {
            let new_prefix = if prefix.is_empty() {
                format!("{}.", d.name)
            } else {
                format!("{}{}.", prefix, d.name)
            };
            out.extend(_flatten_types(&d.children, &new_prefix));
        } else if matches!(d.kind, Class | Struct | Interface | Record | Enum) {
            let mut qualified = d.clone();
            if !prefix.is_empty() {
                qualified.name = format!("{}{}", prefix, d.name);
            }
            let new_prefix = format!("{}.", qualified.name);
            out.push(qualified);
            out.extend(_flatten_types(&d.children, &new_prefix));
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
        if matches!(
            c.kind,
            Class | Struct | Interface | Record | Enum | Namespace | EnumMember
        ) {
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

/// Respect MapOptions when serialising the declaration tree.
fn _filter_decls(decls: &[Declaration], opts: &MapOptions) -> Vec<Declaration> {
    use DeclarationKind::*;
    if decls.is_empty() {
        return Vec::new();
    }
    decls
        .iter()
        .filter_map(|d| {
            let is_field = matches!(d.kind, Field | Property | Event | Indexer);
            if is_field && !opts.include_fields {
                return None;
            }
            if d.visibility == "private" && !opts.include_private {
                return None;
            }
            if matches!(d.kind, Heading | CodeBlock) && !opts.include_docs {
                return None;
            }
            let mut clone = d.clone();
            let mut children = _filter_decls(&d.children, opts);
            if let Some(cap) = opts.max_members {
                children.truncate(cap);
            }
            clone.children = children;
            Some(clone)
        })
        .collect()
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
    declarations: Vec<Declaration>,
}

#[derive(Serialize)]
struct JsonShowDoc<'a> {
    schema: &'static str,
    path: String,
    language: &'static str,
    matches: Vec<&'a SymbolMatch>,
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
        .map(|(r, path)| JsonFile {
            path,
            language: r.language,
            line_count: r.line_count,
            error_count: r.error_count,
            warning: if r.error_count > 0 {
                Some("output may be incomplete")
            } else {
                None
            },
            declarations: _filter_decls(&r.declarations, opts),
        })
        .collect();

    let doc = JsonMapDoc {
        schema: JSON_SCHEMA_MAP,
        files,
    };
    _to_json(&doc, pretty)
}

/// Render `show --json`.
pub fn render_json_show(result: &ParseResult, matches: &[SymbolMatch], pretty: bool) -> String {
    let doc = JsonShowDoc {
        schema: JSON_SCHEMA_SHOW,
        path: result.path.to_string_lossy().into_owned(),
        language: result.language,
        matches: matches.iter().collect(),
    };
    _to_json(&doc, pretty)
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
