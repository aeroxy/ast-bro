//! MCP tool catalogue and dispatch — wraps the existing CLI render functions.

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::core::{
    self, DigestOptions, MapOptions,
};

/// Static descriptors returned to clients via `tools/list`.
pub fn list() -> Value {
    json!({
        "tools": [
            {
                "name": "map",
                "description": "AST-based structural map of source files — signatures with line ranges, no method bodies. Returns text by default (5–10× smaller than reading the file). Set `json: true` for the machine-readable schema `ast-bro.map.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files or directories to map.",
                            "minItems": 1
                        },
                        "no_private": { "type": "boolean", "description": "Hide private declarations." },
                        "no_fields":  { "type": "boolean", "description": "Hide field declarations." },
                        "no_docs":    { "type": "boolean", "description": "Hide doc comments." },
                        "no_attrs":   { "type": "boolean", "description": "Hide attributes / decorators." },
                        "no_lines":   { "type": "boolean", "description": "Hide line-range suffixes." },
                        "glob":       { "type": "string",  "description": "Glob filter applied during directory walk." },
                        "json":       { "type": "boolean", "description": "Return JSON (schema `ast-bro.map.v1`) instead of text." }
                    },
                    "required": ["paths"]
                }
            },
            {
                "name": "digest",
                "description": "One-page module map for an unfamiliar directory: every file's types and public methods. Returns text by default; set `json: true` for `ast-bro.map.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "paths": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files or directories to digest.",
                            "minItems": 1
                        },
                        "include_private": { "type": "boolean" },
                        "include_fields":  { "type": "boolean" },
                        "max_members":     { "type": "integer", "description": "Cap members per type (default 50)." },
                        "json":            { "type": "boolean" }
                    },
                    "required": ["paths"]
                }
            },
            {
                "name": "show",
                "description": "Extract source of one or more symbols from a single file. Suffix matching: `TakeDamage`, or `Player.TakeDamage` when ambiguous. For markdown the symbol is a heading. Returns text by default; set `json: true` for `ast-bro.show.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path":    { "type": "string", "description": "File to search." },
                        "symbols": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "One or more symbol names to extract.",
                            "minItems": 1
                        },
                        "json":    { "type": "boolean" }
                    },
                    "required": ["path", "symbols"]
                }
            },
            {
                "name": "implements",
                "description": "Find subclasses / implementations of a type using AST matching. Transitive by default — set `direct: true` for level-1 only. Returns text by default; set `json: true` for `ast-bro.implements.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "description": "Type name to look up." },
                        "paths":  {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Files or directories to search.",
                            "minItems": 1
                        },
                        "direct": { "type": "boolean", "description": "Direct subtypes only (skip transitive)." },
                        "json":   { "type": "boolean" }
                    },
                    "required": ["target", "paths"]
                }
            },
            {
                "name": "surface",
                "description": "True public API surface — resolves `pub use` re-exports (Rust) and `__all__` (Python) to compute exactly what a downstream user sees, not just every `pub`/non-underscore item per file. Falls back to visibility-filtered output for Java/C#/Go/Kotlin (no real re-export concept). Returns text by default; set `json: true` for `ast-bro.surface.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path":            { "type": "string",  "description": "Crate root file, package init, or directory to auto-detect (default \".\")." },
                        "tree":            { "type": "boolean", "description": "Render as a hierarchical tree grouped by module." },
                        "include_chain":   { "type": "boolean", "description": "Append the via-chain on each entry (text mode only)." },
                        "max_depth":       { "type": "integer", "description": "Recursion guard for re-export chains (default 16)." },
                        "include_private": { "type": "boolean", "description": "Include private items — only meaningful for the fallback resolver." },
                        "lang":            { "type": "string",  "description": "Force a resolver: `rust`, `python`, or `fallback`." },
                        "json":            { "type": "boolean" }
                    }
                }
            },
            {
                "name": "deps",
                "description": "Forward import-graph traversal: what does this file import (transitively)? Builds a per-repo dep graph at `.ast-bro/deps/graph.bin` on first call, then reuses it. Returns text by default; set `json: true` for `ast-bro.deps.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file":    { "type": "string",  "description": "Path to the file whose imports to follow." },
                        "depth":   { "type": "integer", "description": "Max BFS depth (default 3).", "minimum": 1 },
                        "external": { "type": "boolean", "description": "Include unresolved (external) imports." },
                        "rebuild": { "type": "boolean", "description": "Drop the cached graph and rebuild." },
                        "json":    { "type": "boolean" }
                    },
                    "required": ["file"]
                }
            },
            {
                "name": "reverse_deps",
                "description": "Reverse import-graph: who imports this file (transitively)? Useful for refactor blast-radius assessment. Returns text by default; set `json: true` for `ast-bro.reverse-deps.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file":    { "type": "string",  "description": "Path to the file whose importers to find." },
                        "depth":   { "type": "integer", "description": "Max BFS depth (default 3).", "minimum": 1 },
                        "limit":   { "type": "integer", "description": "Cap result count (default 200).", "minimum": 1 },
                        "rebuild": { "type": "boolean" },
                        "json":    { "type": "boolean" }
                    },
                    "required": ["file"]
                }
            },
            {
                "name": "cycles",
                "description": "Find import cycles via Tarjan SCC. Returns the list of strongly-connected components with `len > 1` (or singletons with self-edges). Returns text by default; set `json: true` for `ast-bro.cycles.v1`. Exits non-zero when cycles exist (useful for CI gates).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path":     { "type": "string",  "description": "Repo root (default \".\")." },
                        "min_size": { "type": "integer", "description": "Drop SCCs smaller than this (default 2).", "minimum": 1 },
                        "rebuild":  { "type": "boolean" },
                        "json":     { "type": "boolean" }
                    }
                }
            },
            {
                "name": "graph",
                "description": "Emit the file-level dependency graph. Returns text by default; set `json: true` for `ast-bro.graph.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path":             { "type": "string",  "description": "Repo root (default \".\")." },
                        "json":             { "type": "boolean", "description": "Return JSON (schema `ast-bro.graph.v1`) instead of text." },
                        "include_external": { "type": "boolean", "description": "Include unresolved imports in JSON output." },
                        "rebuild":          { "type": "boolean" }
                    }
                }
            },
            {
                "name": "search",
                "description": "Hybrid BM25 + dense semantic search over the repo. First call builds a per-repo index at `.ast-bro/index/` (one-time, ~seconds for typical repos). Returns text by default; set `json: true` for `ast-bro.search.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query":     { "type": "string",  "description": "Search query (free-form text or symbol name)." },
                        "path":      { "type": "string",  "description": "Repo root to search in (default \".\")." },
                        "top_k":     { "type": "integer", "description": "Max results to return (default 10).", "minimum": 1 },
                        "alpha":     { "type": "number",  "description": "Override semantic-vs-BM25 weight (0.0=pure BM25, 1.0=pure semantic). Default auto-detects from query type." },
                        "languages": { "type": "array", "items": { "type": "string" }, "description": "Restrict to chunks of these languages (e.g. [\"rust\", \"python\"])." },
                        "json":      { "type": "boolean", "description": "Return JSON (schema `ast-bro.search.v1`) instead of text." }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "find_related",
                "description": "Find chunks semantically similar to a given file:line. Useful for navigating to related code. Returns text by default; set `json: true` for `ast-bro.related.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path":  { "type": "string",  "description": "Repo-relative path of the source chunk." },
                        "line":  { "type": "integer", "description": "1-indexed line within `path`.", "minimum": 1 },
                        "root":  { "type": "string",  "description": "Repo root containing the index (default \".\")." },
                        "top_k": { "type": "integer", "description": "Max results (default 10).", "minimum": 1 },
                        "json":  { "type": "boolean" }
                    },
                    "required": ["path", "line"]
                }
            },
            {
                "name": "index",
                "description": "Build, refresh, or inspect the per-repo search index. With `stats: true` returns index stats. With `rebuild: true` drops the cache and rebuilds. Otherwise just opens (and incrementally refreshes if files changed). Returns text by default; set `json: true` for `ast-bro.index-stats.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path":    { "type": "string",  "description": "Repo root (default \".\")." },
                        "rebuild": { "type": "boolean", "description": "Drop existing cache and rebuild." },
                        "stats":   { "type": "boolean", "description": "Print index stats and return." },
                        "json":    { "type": "boolean" }
                    }
                }
            },
            {
                "name": "callers",
                "description": "Find callers of a symbol — AST-accurate, no grep noise. Suffix-matches the target like `show`/`implements`: `TakeDamage`, or `Type.method` when ambiguous. Builds a unified deps+calls cache at `.ast-bro/deps/graph.bin` on first call (the call half is built lazily). Returns text by default; set `json: true` for `ast-bro.callers.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target":            { "type": "string",  "description": "Symbol name to look up." },
                        "path":              { "type": "string",  "description": "Repo root (default \".\")." },
                        "depth":             { "type": "integer", "description": "Max BFS depth (default 1).", "minimum": 1 },
                        "limit":             { "type": "integer", "description": "Cap result count (default 200).", "minimum": 1 },
                        "include_ambiguous": { "type": "boolean", "description": "Keep callers whose target is unresolved." },
                        "rebuild":           { "type": "boolean" },
                        "json":              { "type": "boolean" }
                    },
                    "required": ["target"]
                }
            },
            {
                "name": "callees",
                "description": "What does this symbol call? — AST-accurate forward call traversal. Suffix-matches the target like `callers`. Returns text by default; set `json: true` for `ast-bro.callees.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target":   { "type": "string",  "description": "Symbol name to look up." },
                        "path":     { "type": "string",  "description": "Repo root (default \".\")." },
                        "depth":    { "type": "integer", "description": "Max BFS depth (default 1).", "minimum": 1 },
                        "external": { "type": "boolean", "description": "Include unresolved/external callees in output." },
                        "rebuild":  { "type": "boolean" },
                        "json":     { "type": "boolean" }
                    },
                    "required": ["target"]
                }
            },
            {
                "name": "run",
                "description": "AST-aware pattern search and rewrite. Use metavariables like $FUNC, $ARG, $$$BODY for structural matching. Search-only without rewrite; transform code with rewrite and write. WARNING: `write: true` mutates files on disk — a broad pattern can touch many files at once (capped at 50 per call). Always preview with the default dry-run first and confirm the diff before re-running with `write: true`. Returns text by default; set `json: true` for `ast-bro.run.v1`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pattern":  { "type": "string",  "description": "AST pattern with metavariables (e.g. '$FUNC($$$)')." },
                        "rewrite":  { "type": "string",  "description": "Replacement template (e.g. 'bar($A)'). Omit for search-only." },
                        "lang":     { "type": "string",  "description": "Language (auto-detected from file paths if omitted)." },
                        "paths":    { "type": "array", "items": { "type": "string" }, "description": "Files or directories to search.", "minItems": 1 },
                        "glob":     { "type": "string",  "description": "Glob pattern to filter files, e.g. '**/*.rs'." },
                        "write":    { "type": "boolean", "description": "Write changes to disk. Default: false (dry-run). DANGEROUS: mutates files; preview the dry-run diff first and confirm before flipping to true." },
                        "json":     { "type": "boolean", "description": "Return results as JSON instead of text." }
                    },
                    "required": ["pattern"]
                }
            }
        ]
    })
}

/// Result of dispatching a tool — either textual content or an error message
/// surfaced as `isError: true` on the MCP response.
pub enum CallResult {
    Text(String),
    Error(String),
}

pub fn call(name: &str, args: Value) -> CallResult {
    match name {
        "map"          => run_map(args),
        "digest"       => run_digest(args),
        "show"         => run_show(args),
        "implements"   => run_implements(args),
        "surface"      => run_surface(args),
        "deps"         => run_deps(args),
        "reverse_deps" => run_reverse_deps(args),
        "cycles"       => run_cycles(args),
        "graph"        => run_graph(args),
        "search"       => crate::search::mcp::run_search(args),
        "find_related" => crate::search::mcp::run_find_related(args),
        "index"        => crate::search::mcp::run_index(args),
        "callers"      => run_callers(args),
        "callees"      => run_callees(args),
        "run"          => run_run(args),
        other => CallResult::Error(format!("unknown tool: {}", other)),
    }
}

// ---------- callers / callees ----------

#[derive(serde::Deserialize)]
struct CallersArgs {
    target: String,
    #[serde(default = "default_dot")]
    path: PathBuf,
    #[serde(default = "default_one")]
    depth: usize,
    #[serde(default = "default_two_hundred")]
    limit: usize,
    #[serde(default)]
    include_ambiguous: bool,
    #[serde(default)]
    json: bool,
}

#[derive(serde::Deserialize)]
struct CalleesArgs {
    target: String,
    #[serde(default = "default_dot")]
    path: PathBuf,
    #[serde(default = "default_one")]
    depth: usize,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    json: bool,
}

fn default_one() -> usize { 1 }
fn default_two_hundred() -> usize { 200 }
fn default_dot() -> PathBuf { PathBuf::from(".") }

fn run_callers(args: Value) -> CallResult {
    let a: CallersArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return CallResult::Error(format!("bad args: {}", e)),
    };
    let root = match resolve_root(&a.path) {
        Ok(r) => r,
        Err(e) => return CallResult::Error(e),
    };
    let out = crate::calls::mcp::run_callers_text(
        &a.target,
        &root,
        a.depth,
        a.limit,
        a.include_ambiguous,
        a.json,
    );
    CallResult::Text(out)
}

fn run_callees(args: Value) -> CallResult {
    let a: CalleesArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return CallResult::Error(format!("bad args: {}", e)),
    };
    let root = match resolve_root(&a.path) {
        Ok(r) => r,
        Err(e) => return CallResult::Error(e),
    };
    let out = crate::calls::mcp::run_callees_text(&a.target, &root, a.depth, a.external, a.json);
    CallResult::Text(out)
}

fn resolve_root(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("path not found: {}", path.display()));
    }
    // Walk up from `path` to the nearest project manifest so qns are
    // project-relative (`src/foo.rs::bar`, not `foo.rs::bar`). Matches the
    // CLI's resolve_root.
    crate::deps::cli::find_root_for(path)
}

#[derive(Deserialize, Default)]
struct MapArgs {
    paths: Vec<PathBuf>,
    #[serde(default)] no_private: bool,
    #[serde(default)] no_fields: bool,
    #[serde(default)] no_docs: bool,
    #[serde(default)] no_attrs: bool,
    #[serde(default)] no_lines: bool,
    #[serde(default)] glob: Option<String>,
    #[serde(default)] json: bool,
}

fn run_map(args: Value) -> CallResult {
    let a: MapArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    if a.paths.is_empty() {
        return CallResult::Error("`paths` must not be empty".into());
    }
    let results = crate::walk_and_parse(&a.paths, a.glob.as_deref());
    let opts = MapOptions {
        include_private: !a.no_private,
        include_fields: !a.no_fields,
        include_docs: !a.no_docs,
        include_attributes: !a.no_attrs,
        include_line_numbers: !a.no_lines,
        max_doc_lines: 6,
        max_members: None,
    };
    if a.json {
        CallResult::Text(core::render_json_map(&results, &opts, true))
    } else {
        let mut out = String::new();
        for res in &results {
            out.push_str(&core::render_map(res, &opts));
            out.push('\n');
        }
        CallResult::Text(out)
    }
}

#[derive(Deserialize, Default)]
struct DigestArgs {
    paths: Vec<PathBuf>,
    #[serde(default)] include_private: bool,
    #[serde(default)] include_fields: bool,
    #[serde(default = "default_max_members")] max_members: usize,
    #[serde(default)] json: bool,
}

fn default_max_members() -> usize { 50 }

fn run_digest(args: Value) -> CallResult {
    let a: DigestArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    if a.paths.is_empty() {
        return CallResult::Error("`paths` must not be empty".into());
    }
    let results = crate::walk_and_parse(&a.paths, None);
    if a.json {
        let opts = MapOptions {
            include_private: a.include_private,
            include_fields: a.include_fields,
            include_docs: true,
            include_attributes: true,
            include_line_numbers: true,
            max_doc_lines: 6,
            max_members: Some(a.max_members),
        };
        CallResult::Text(core::render_json_map(&results, &opts, true))
    } else {
        let opts = DigestOptions {
            include_private: a.include_private,
            include_fields: a.include_fields,
            max_members_per_type: a.max_members,
            max_heading_depth: 3,
        };
        let root = if a.paths.len() == 1 && a.paths[0].is_dir() {
            Some(a.paths[0].as_path())
        } else {
            None
        };
        CallResult::Text(core::render_digest(&results, &opts, root))
    }
}

#[derive(Deserialize)]
struct ShowArgs {
    path: PathBuf,
    symbols: Vec<String>,
    #[serde(default)] json: bool,
}

fn run_show(args: Value) -> CallResult {
    let a: ShowArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    if a.symbols.is_empty() {
        return CallResult::Error("`symbols` must not be empty".into());
    }
    let res = match crate::parse_file(&a.path) {
        Some(r) => r,
        None => return CallResult::Error(format!("could not parse file: {}", a.path.display())),
    };

    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::new();
    for sym in &a.symbols {
        for m in core::find_symbols(&res, sym) {
            let key = (m.start_line, m.end_line, m.qualified_name.clone());
            if seen.insert(key) {
                all.push(m);
            }
        }
    }

    if a.json {
        CallResult::Text(core::render_json_show(&res, &all, true))
    } else {
        let mut out = String::new();
        for m in &all {
            out.push_str(&format!(
                "# {}:{}-{} {} ({})\n",
                res.path.display(), m.start_line, m.end_line, m.qualified_name, m.kind
            ));
            if !m.ancestor_signatures.is_empty() {
                out.push_str(&format!("# in: {}\n", m.ancestor_signatures.join(" → ")));
            }
            out.push_str(&m.source);
            out.push('\n');
        }
        CallResult::Text(out)
    }
}

#[derive(Deserialize)]
struct ImplementsArgs {
    target: String,
    paths: Vec<PathBuf>,
    #[serde(default)] direct: bool,
    #[serde(default)] json: bool,
}

#[derive(Deserialize, Default)]
struct SurfaceArgs {
    #[serde(default = "default_surface_path")]
    path: PathBuf,
    #[serde(default)] tree: bool,
    #[serde(default)] include_chain: bool,
    #[serde(default = "default_surface_max_depth")] max_depth: usize,
    #[serde(default)] include_private: bool,
    #[serde(default)] lang: Option<String>,
    #[serde(default)] json: bool,
}

fn default_surface_path() -> PathBuf {
    PathBuf::from(".")
}
fn default_surface_max_depth() -> usize {
    16
}

fn run_surface(args: Value) -> CallResult {
    let a: SurfaceArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    let lang_override = match a.lang {
        Some(s) => match crate::surface::LangOverride::parse(&s) {
            Some(l) => Some(l),
            None => return CallResult::Error(format!("unknown lang: {}", s)),
        },
        None => None,
    };
    let output = if a.json {
        crate::surface::OutputMode::Json { compact: false }
    } else if a.tree {
        crate::surface::OutputMode::Tree
    } else {
        crate::surface::OutputMode::Flat
    };
    let opts = crate::surface::SurfaceOptions {
        output,
        include_private: a.include_private,
        max_depth: a.max_depth,
        include_chain: a.include_chain,
        lang_override,
    };
    match crate::surface::resolve_surface(&a.path, &opts) {
        Ok(entries) => {
            CallResult::Text(crate::surface::render::render(&entries, output, a.include_chain))
        }
        Err(e) => CallResult::Error(format!("{e}")),
    }
}

fn run_implements(args: Value) -> CallResult {
    let a: ImplementsArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    if a.paths.is_empty() {
        return CallResult::Error("`paths` must not be empty".into());
    }
    let results = crate::walk_and_parse(&a.paths, None);
    let transitive = !a.direct;
    let matches = core::find_implementations(&results, &a.target, transitive);

    if a.json {
        CallResult::Text(core::render_json_implements(&a.target, &matches, transitive, true))
    } else {
        let mut out = format!(
            "# {} match(es) for '{}' (incl. transitive):\n",
            matches.len(), a.target
        );
        for m in &matches {
            let via = if m.via.is_empty() {
                String::new()
            } else {
                format!(" [via {}]", m.via.last().unwrap())
            };
            out.push_str(&format!("{}:{}  {} {}{}\n", m.path, m.start_line, m.kind, m.name, via));
        }
        CallResult::Text(out)
    }
}

// ---- deps / reverse-deps / cycles / graph ----

#[derive(Deserialize, Default)]
struct DepsArgs {
    file: PathBuf,
    #[serde(default = "default_depth")] depth: usize,
    #[serde(default)] external: bool,
    #[serde(default)] json: bool,
}

#[derive(Deserialize, Default)]
struct ReverseDepsArgs {
    file: PathBuf,
    #[serde(default = "default_depth")] depth: usize,
    #[serde(default = "default_limit")] limit: usize,
    #[serde(default)] json: bool,
}

#[derive(Deserialize, Default)]
struct CyclesArgs {
    #[serde(default = "default_path")] path: PathBuf,
    #[serde(default = "default_min_size")] min_size: usize,
    #[serde(default)] json: bool,
}

#[derive(Deserialize, Default)]
struct GraphArgs {
    #[serde(default = "default_path")] path: PathBuf,
    #[serde(default)] json: bool,
    #[serde(default)] include_external: bool,
}

fn default_depth() -> usize { 3 }
fn default_limit() -> usize { 200 }
fn default_min_size() -> usize { 2 }
fn default_path() -> PathBuf { PathBuf::from(".") }

fn run_deps(args: Value) -> CallResult {
    let a: DepsArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    let root = match crate::deps::cli::find_root_for(&a.file) {
        Ok(r) => r,
        Err(e) => return CallResult::Error(e),
    };
    let graph = match crate::graph_cache::shared::get_or_init(&root).map(|u| u.deps.clone()) {
        Ok(g) => g,
        Err(e) => return CallResult::Error(e.to_string()),
    };
    let canon = match a.file.canonicalize() {
        Ok(c) => c,
        Err(e) => return CallResult::Error(format!("cannot resolve {}: {}", a.file.display(), e)),
    };
    let _ = a.external; // forwarded but only relevant to graph; deps text always shows what's resolved.
    let hits = crate::deps::traverse::forward(&graph, &canon, a.depth.max(1));
    if a.json {
        CallResult::Text(crate::deps::render::render_deps_json(&graph, &canon, &hits, true))
    } else {
        CallResult::Text(crate::deps::render::render_deps_text(&graph, &canon, &hits))
    }
}

fn run_reverse_deps(args: Value) -> CallResult {
    let a: ReverseDepsArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    let root = match crate::deps::cli::find_root_for(&a.file) {
        Ok(r) => r,
        Err(e) => return CallResult::Error(e),
    };
    let graph = match crate::graph_cache::shared::get_or_init(&root).map(|u| u.deps.clone()) {
        Ok(g) => g,
        Err(e) => return CallResult::Error(e.to_string()),
    };
    let canon = match a.file.canonicalize() {
        Ok(c) => c,
        Err(e) => return CallResult::Error(format!("cannot resolve {}: {}", a.file.display(), e)),
    };
    let hits = crate::deps::traverse::reverse(&graph, &canon, a.depth.max(1), a.limit);
    if a.json {
        CallResult::Text(crate::deps::render::render_reverse_deps_json(&graph, &canon, &hits, true))
    } else {
        CallResult::Text(crate::deps::render::render_reverse_deps_text(&graph, &canon, &hits))
    }
}

fn run_cycles(args: Value) -> CallResult {
    let a: CyclesArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    let root = match a.path.canonicalize() {
        Ok(r) => r,
        Err(e) => return CallResult::Error(format!("cannot resolve {}: {}", a.path.display(), e)),
    };
    let graph = match crate::graph_cache::shared::get_or_init(&root).map(|u| u.deps.clone()) {
        Ok(g) => g,
        Err(e) => return CallResult::Error(e.to_string()),
    };
    let cycles = crate::deps::scc::detect(&graph, a.min_size);
    if a.json {
        CallResult::Text(crate::deps::render::render_cycles_json(&graph, &cycles, true))
    } else {
        CallResult::Text(crate::deps::render::render_cycles_text(&graph, &cycles))
    }
}

fn run_graph(args: Value) -> CallResult {
    let a: GraphArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };
    let root = match a.path.canonicalize() {
        Ok(r) => r,
        Err(e) => return CallResult::Error(format!("cannot resolve {}: {}", a.path.display(), e)),
    };
    let graph = match crate::graph_cache::shared::get_or_init(&root).map(|u| u.deps.clone()) {
        Ok(g) => g,
        Err(e) => return CallResult::Error(e.to_string()),
    };
    let body = if a.json {
        crate::deps::render::render_graph_json(&graph, a.include_external, true)
    } else {
        crate::deps::render::render_graph_text(&graph)
    };
    CallResult::Text(body)
}

// ---- run (AST-aware pattern search + rewrite) ----

/// Safety cap: MCP rewrite can touch at most this many files in a single call.
/// Prevents a broad pattern from destroying an entire repo.
const MCP_REWRITE_MAX_FILES: usize = 50;

#[derive(Deserialize, Default)]
struct RunArgs {
    pattern: String,
    #[serde(default)]
    rewrite: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    write: bool,
    #[serde(default)]
    json: bool,
}

fn run_run(args: Value) -> CallResult {
    let a: RunArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return CallResult::Error(format!("invalid arguments: {}", e)),
    };

    // Validate pattern upfront when language is known, so an invalid
    // pattern fails fast instead of after walking every file.
    let (fixed_lang, compiled_pattern) = if let Some(ref l) = a.lang {
        let lang = match crate::run::cli::parse_lang(l) {
            Some(l) => l,
            None => return CallResult::Error(format!("unsupported language '{}'", l)),
        };
        let pat = match ast_grep_core::Pattern::try_new(&a.pattern, lang) {
            Ok(p) => p,
            Err(e) => return CallResult::Error(format!("invalid pattern: {}", e)),
        };
        (Some(lang), Some(pat))
    } else {
        (None, None)
    };

    let search_paths = if a.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        a.paths
    };
    let files = crate::walk_paths(&search_paths, a.glob.as_deref());
    
    #[derive(serde::Serialize)]
    struct RewriteRecord {
        file: String,
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    let mut all_matches = Vec::new();
    let mut output = String::new();
    // Search-mode errors are kept out of `output` so the final report can
    // group them in a clearly separated `--- errors ---` block after the
    // match list, rather than interleaving error lines with results.
    let mut search_errors: Vec<String> = Vec::new();
    let mut rewrite_records: Vec<RewriteRecord> = Vec::new();
    let mut rewrite_count: usize = 0;
    let mut error_count: usize = 0;
    let mut rewrite_capped = false;

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => {
                error_count += 1;
                continue;
            },
        };
        let lang = if let Some(l) = fixed_lang {
            l
        } else {
            match crate::run::detect_lang(path) {
                Some(l) => l,
                None => continue, // Silently skip non-source files in directory walk
            }
        };

        // Search-only mode (no rewrite template)
        if a.rewrite.is_none() {
            let result = if let Some(ref compiled) = compiled_pattern {
                crate::run::search_with_pattern(&source, lang, compiled)
            } else {
                crate::run::search(&source, lang, &a.pattern)
            };
            match result {
                Ok(mut matches) => {
                    if !matches.is_empty() {
                        let file_str = path.to_string_lossy().to_string();
                        for m in &mut matches {
                            m.file = file_str.clone();
                        }
                        all_matches.extend(matches);
                    }
                }
                Err(e) => {
                    search_errors.push(format!(
                        "search failed for pattern {:?} ({}) in {}: {}",
                        a.pattern,
                        lang,
                        path.display(),
                        e,
                    ));
                    error_count += 1;
                }
            }
            continue;
        }

        // Rewrite mode (dry-run or write)
        let replacement = a.rewrite.as_deref().unwrap_or("");
        match crate::run::rewrite(&source, lang, &a.pattern, replacement) {
            Ok(Some(new_source)) => {
                let file_str = path.to_string_lossy().to_string();
                if a.write {
                    if rewrite_count < MCP_REWRITE_MAX_FILES {
                        if let Err(e) = std::fs::write(path, &new_source) {
                            output.push_str(&format!("{}: write failed: {}\n", file_str, e));
                            error_count += 1;
                            rewrite_records.push(RewriteRecord {
                                file: file_str,
                                status: "write_failed",
                                diff: None,
                                error: Some(e.to_string()),
                            });
                        } else {
                            output.push_str(&format!("{}: rewritten\n", file_str));
                            rewrite_count += 1;
                            rewrite_records.push(RewriteRecord {
                                file: file_str,
                                status: "rewritten",
                                diff: None,
                                error: None,
                            });
                        }
                    } else {
                        rewrite_capped = true;
                        break;
                    }
                } else {
                    // Dry-run: show unified diff
                    let diff = crate::run::cli::line_change_report(path, &source, &new_source);
                    output.push_str(&diff);
                    rewrite_count += 1;
                    rewrite_records.push(RewriteRecord {
                        file: file_str,
                        status: "diff",
                        diff: Some(diff),
                        error: None,
                    });
                }
            }
            Ok(None) => {} // no matches in this file
            Err(e) => {
                let file_str = path.to_string_lossy().to_string();
                output.push_str(&format!("{}: {}\n", file_str, e));
                error_count += 1;
                rewrite_records.push(RewriteRecord {
                    file: file_str,
                    status: "rewrite_error",
                    diff: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    // Rewrite mode: output already contains diffs or write confirmations
    if a.rewrite.is_some() {
        if a.json {
            #[derive(serde::Serialize)]
            struct RewriteDoc<'a> {
                mode: &'static str,
                dry_run: bool,
                rewrite_count: usize,
                error_count: usize,
                capped: bool,
                cap_limit: usize,
                files: &'a [RewriteRecord],
            }
            let doc = RewriteDoc {
                mode: "rewrite",
                dry_run: !a.write,
                rewrite_count,
                error_count,
                capped: rewrite_capped,
                cap_limit: MCP_REWRITE_MAX_FILES,
                files: &rewrite_records,
            };
            return CallResult::Text(
                serde_json::to_string_pretty(&doc).unwrap_or_default(),
            );
        }
        if rewrite_count == 0 && !rewrite_capped && error_count == 0 {
            output.push_str("No matches found for rewrite.");
        }
        if rewrite_capped {
            output.push_str(&format!("\n# warning: reached safety cap of {} files; remaining files were not processed.", MCP_REWRITE_MAX_FILES));
        }
        if error_count > 0 {
            output.push_str(&format!("\n({} files had errors)", error_count));
        }
        return CallResult::Text(output);
    }

    // Search-only mode
    if a.json {
        CallResult::Text(serde_json::to_string_pretty(&all_matches).unwrap_or_default())
    } else {
        if all_matches.is_empty() {
            output.push_str("No matches found.");
        } else {
            output.push_str(&format!("Found {} matches in {} files:\n", all_matches.len(), files.len()));
            for m in all_matches {
                let first_line = m.matched_text.lines().next().unwrap_or("");
                output.push_str(&format!("{}:{}:{}-{}:{}: {}\n", m.file, m.start_line, m.start_col, m.end_line, m.end_col, first_line));
            }
        }
        if !search_errors.is_empty() {
            output.push_str("\n--- errors ---\n");
            for line in &search_errors {
                output.push_str(line);
                output.push('\n');
            }
        }
        if error_count > 0 {
            output.push_str(&format!("\n(Skipped {} files due to errors)", error_count));
        }
        CallResult::Text(output)
    }
}
