use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

mod adapters;
mod calls;
mod cli_error;
mod context;
mod core;
mod deps;
mod file_filter;
mod graph_cache;
mod hook;
mod impact;
mod installers;
mod main_helpers;
mod mcp;
mod path_glob;
mod project_root;
mod prompt;
mod run;
mod search;
mod squeeze;
mod surface;

use crate::core::{DigestOptions, MapOptions, ParseResult};

/// Traversal `limit` meaning "collect the whole cone". The `limit`
/// arguments in `calls::traverse` / `deps::traverse` bound how many hits are
/// *collected*, and the callers that must report an exact total before
/// `--limit` trims the display (issue #32) may not stop early. One shared
/// constant so the intent is named at the call site instead of a bare
/// `usize::MAX` reading as "a very large cap".
pub(crate) const UNLIMITED: usize = usize::MAX;

#[derive(Parser)]
#[command(name = "ast-bro")]
#[command(version)]
#[command(about = "Fast, AST-based structural outline for source files", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Map files or directories — signatures with line ranges, no method bodies.
    ///
    /// Three orthogonal axes (issue #37): detail (`--detail names|signatures|full`),
    /// visibility (`--no-private`, `--no-fields`, …), and scope (`--glob`,
    /// `--max-members`). `--preset digest` = `--detail names --no-private
    /// --no-fields --max-members 50`; explicit flags override the preset.
    Map(MapArgs),
    /// Extract source of a symbol
    Show {
        path: PathBuf,
        symbol: String,
        #[arg(num_args = 0..)]
        others: Vec<String>,
        /// Emit output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON
        #[arg(long)]
        compact: bool,
    },
    /// Compress repetitive log/text into a smaller, reversible form with a legend.
    ///
    /// For **logs and text**, not code — for code use `map` / `digest` / `show`.
    /// Shrinks repeated lines, tags, timestamps and token sequences; prints a legend
    /// so the original is recoverable. Falls back to the raw input when squeezing
    /// would make it larger.
    Squeeze {
        /// Path to the log/text file to read.
        path: PathBuf,
        /// Optional 1-indexed inclusive line range: `N`, `A:B`, `A:`, or `:B`.
        #[arg(value_parser = parse_line_range)]
        range: Option<LineRange>,
        /// Skip compression — emit the raw text with a header (for diffing/inspecting).
        #[arg(long)]
        raw: bool,
        /// Emit output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON
        #[arg(long)]
        compact: bool,
    },
    /// One-page module map — alias for `map --preset digest`.
    Digest(MapArgs),
    /// Find subclasses / implementations
    Implements {
        target: String,
        /// Files or directories to search. Required — `implements` walks
        /// only what it is given, and an empty walk would answer "no
        /// implementations" for a type that has them (issue #33). Declared
        /// required so `--help` and the runtime agree, and so clap's own
        /// message names the missing argument.
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,

        #[arg(short, long)]
        direct: bool,
        /// Emit output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON
        #[arg(long)]
        compact: bool,
    },
    /// Print the agent prompt snippet
    Prompt,
    /// Install ast-bro into a coding-agent CLI
    Install {
        #[arg(long, conflicts_with = "all")]
        target: Option<String>,
        #[arg(long, conflicts_with = "target")]
        all: bool,
        #[arg(long)]
        local: bool,
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        always: bool,
        #[arg(long, default_value_t = 200)]
        min_lines: usize,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
        /// Install ast-bro as an MCP server entry instead of the agent prompt.
        /// Combine with `--skills` to install both.
        #[arg(long)]
        mcp: bool,
        /// Install ast-bro as an agent skill instead of the agent prompt.
        /// Combine with `--mcp` to install both.
        #[arg(long)]
        skills: bool,
    },
    /// Remove ast-bro from a coding-agent CLI
    Uninstall {
        #[arg(long, conflicts_with = "all")]
        target: Option<String>,
        #[arg(long, conflicts_with = "target")]
        all: bool,
        #[arg(long)]
        local: bool,
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Report what's installed where
    Status {
        #[arg(long)]
        local: bool,
        #[arg(long, conflicts_with = "local")]
        global: bool,
    },
    /// Internal: read a tool-call event from stdin and respond
    Hook {
        #[arg(long)]
        protocol: String,
        #[arg(long, default_value_t = 200)]
        min_lines: usize,
        #[arg(long)]
        always: bool,
    },
    /// Run as an MCP (Model Context Protocol) server over stdio
    Mcp,
    /// Hybrid BM25 + dense semantic search over the repo
    Search {
        /// Search query (free-form text or symbol name)
        query: String,
        /// Repository root to search in (default: ".")
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Number of results to return
        #[arg(short = 'k', long = "top-k", default_value_t = 10)]
        top_k: usize,
        /// Override auto alpha (semantic vs. BM25 weight, 0.0–1.0)
        #[arg(long)]
        alpha: Option<f32>,
        /// Filter by language (repeatable, e.g. `--lang rust --lang python`)
        #[arg(long = "lang")]
        languages: Vec<String>,
        /// Force a full rebuild of the index before searching
        #[arg(long)]
        rebuild: bool,
        /// Emit output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON
        #[arg(long)]
        compact: bool,
    },
    /// Find chunks semantically similar to a given file:line
    ///
    /// Pass the source location either as a positional `<FILE>:<LINE>`
    /// (matches grep / search-result output you can paste back) or via
    /// `--file <FILE> --line <LINE>` for scripting use.
    FindRelated {
        /// Source location as `<FILE>:<LINE>`. Optional when `--file` and
        /// `--line` are passed together.
        #[arg(required_unless_present_all = ["file", "line"], conflicts_with_all = ["file", "line"])]
        target: Option<String>,
        /// Repository root containing the index (default: ".")
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Alternative to the positional `<FILE>:<LINE>` form
        #[arg(long, requires = "line")]
        file: Option<String>,
        /// 1-indexed line number when using `--file`
        #[arg(long, requires = "file")]
        line: Option<u32>,
        #[arg(short = 'k', long = "top-k", default_value_t = 10)]
        top_k: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// True public API surface — resolves `pub use` / `__all__` re-exports.
    Surface {
        /// Crate root file, package init, or directory to auto-detect.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Render as a hierarchical tree grouped by module.
        #[arg(long)]
        tree: bool,
        /// Append the via-chain on each entry (text mode only).
        #[arg(long)]
        include_chain: bool,
        /// Recursion guard for re-export chains.
        #[arg(long, default_value_t = 16)]
        max_depth: usize,
        /// Include private items (only meaningful for fallback languages).
        #[arg(long)]
        include_private: bool,
        /// Force a specific resolver: `rust`, `python`, or `fallback`.
        #[arg(long)]
        lang: Option<String>,
        /// Emit output as JSON instead of text.
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON.
        #[arg(long)]
        compact: bool,
    },
    /// Forward import-graph traversal: what does this file import (transitively)?
    Deps {
        file: PathBuf,
        #[arg(long, default_value_t = 3)]
        depth: usize,
        /// Hide unresolved external imports from the footer.
        /// Shown by default (tagged `[external]`); set this flag to drop them.
        #[arg(long)]
        hide_external: bool,
        /// Force a fresh dep-graph build.
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Reverse import-graph: who imports this file (transitively)?
    ReverseDeps {
        file: PathBuf,
        #[arg(long, default_value_t = 3)]
        depth: usize,
        /// Cap how many importers are *displayed*. The walk is unbounded so
        /// the reported total is exact.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Show only importers from test files (path heuristic: tests/, __tests__/, *_test.*, *.spec.*, …).
        #[arg(long)]
        tests: bool,
        /// Exclude importers from test files (same path heuristic as --tests).
        #[arg(long, conflicts_with = "tests")]
        exclude_tests: bool,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Find import cycles via Tarjan SCC.
    Cycles {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 2)]
        min_size: usize,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Emit the dep graph (text or JSON).
    Graph {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        /// Hide unresolved external imports from the graph.
        /// Shown by default (tagged `[external]`); set this flag to drop them.
        #[arg(long)]
        hide_external: bool,
        /// (Deprecated — unresolved externals are shown by default now.)
        #[arg(long, hide = true)]
        include_external: bool,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Token-budgeted context for a symbol — target body + relevant deps/callers in one call.
    Context {
        /// Symbol to build context for (same form as callers/callees).
        #[arg(required_unless_present_all = ["file", "symbol"], conflicts_with_all = ["file", "symbol"])]
        target: Option<String>,
        /// Repository root (default: ".").
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Alternative to the positional target.
        #[arg(long, requires = "symbol")]
        file: Option<String>,
        /// Symbol name when using `--file`.
        #[arg(long, requires = "file")]
        symbol: Option<String>,
        /// Token budget (default 8000). Rough estimate: 1 token ≈ 4 bytes.
        #[arg(long, default_value_t = 8000)]
        budget: usize,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Find callers of a symbol — AST-accurate, no grep noise.
    ///
    /// Pass the symbol either as a positional `<TARGET>` (suffix-matched
    /// like `show`/`implements`: `TakeDamage`, `Player.TakeDamage`, or
    /// `src/Player.cs:TakeDamage` to scope to one file), or via
    /// `--file <FILE> --symbol <NAME>` for scripting use.
    Callers {
        /// Symbol to look up. Optional when `--file` and `--symbol` are passed.
        #[arg(required_unless_present_all = ["file", "symbol"], conflicts_with_all = ["file", "symbol"])]
        target: Option<String>,
        /// Repository root (default: ".").
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Alternative to the `<FILE>:<NAME>` positional form.
        #[arg(long, requires = "symbol")]
        file: Option<String>,
        /// Symbol name when using `--file`.
        #[arg(long, requires = "file")]
        symbol: Option<String>,
        /// Max BFS depth (1 = direct callers only).
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Cap how many callers are *displayed* (mirrors reverse-deps).
        /// The walk itself is unbounded so the reported total is exact —
        /// raising `--depth` costs work regardless of this cap.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Hide callers whose target is `Ambiguous` (multiple candidates).
        /// Shown by default (tagged red); set this flag to drop them.
        #[arg(long)]
        hide_ambiguous: bool,
        /// (Deprecated — ambiguous matches are shown by default now.)
        #[arg(long, hide = true)]
        include_ambiguous: bool,
        /// Show only callers in test files (path heuristic: tests/, __tests__/, *_test.*, *.spec.*, …).
        #[arg(long)]
        tests: bool,
        /// Exclude callers from test files (same path heuristic as --tests).
        #[arg(long, conflicts_with = "tests")]
        exclude_tests: bool,
        /// Force a fresh call-graph build.
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// What does this symbol call? — AST-accurate forward call traversal.
    ///
    /// Same target-spec rules as `callers`: positional `<TARGET>` (with
    /// optional `<FILE>:<NAME>` scoping) or `--file --symbol`.
    Callees {
        #[arg(required_unless_present_all = ["file", "symbol"], conflicts_with_all = ["file", "symbol"])]
        target: Option<String>,
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, requires = "symbol")]
        file: Option<String>,
        #[arg(long, requires = "file")]
        symbol: Option<String>,
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Cap how many callees are *displayed* (the header always reports
        /// the true total).
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Hide unresolved callees (the `Bare`/`External` bucket).
        /// Shown by default (tagged cyan/red); set this flag to drop them.
        #[arg(long)]
        hide_external: bool,
        /// (Deprecated — unresolved/external callees are shown by default now.)
        #[arg(long, hide = true)]
        external: bool,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Trace the static call path between two symbols — "how does <FROM> reach <TO>?"
    ///
    /// Shortest-path BFS over the call graph, with each hop's source body
    /// inlined so a flow question is answered in one call instead of chaining
    /// `callees`. When no static path exists (the chain broke at dynamic
    /// dispatch), both endpoints + the target file's siblings are shown.
    /// Targets are suffix-matched like `callers` (`run`, `Type.method`, or
    /// `src/f.rs:name`).
    Trace {
        /// Source symbol — where the call path starts.
        from: String,
        /// Destination symbol — where the call path should reach.
        to: String,
        /// Repository root (default: ".").
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Max path length in hops.
        #[arg(long, default_value_t = 12)]
        depth: usize,
        /// Force a fresh call-graph build.
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Cross-file impact analysis: callers + callees + file reverse-deps + test detection, one command.
    Impact {
        /// Symbol to analyse (same form as callers/callees: `TakeDamage`, `Player.TakeDamage`, `src/Player.cs:TakeDamage`).
        #[arg(required_unless_present_all = ["file", "symbol"], conflicts_with_all = ["file", "symbol"])]
        target: Option<String>,
        /// Repository root (default: ".").
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Alternative to the positional target.
        #[arg(long, requires = "symbol")]
        file: Option<String>,
        /// Symbol name when using `--file`.
        #[arg(long, requires = "file")]
        symbol: Option<String>,
        /// Transitive depth (default 2).
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Cap how many results are *displayed* per section. Sections are
        /// walked in full so their totals are exact.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Section to show: `deps`, `dependents`, `tests`, or `all` (default).
        #[arg(long, default_value = "all")]
        mode: String,
        /// Hide ambiguous call-edge matches from impact output.
        /// Shown by default (tagged red); set this flag to drop them.
        #[arg(long)]
        hide_ambiguous: bool,
        /// Show only test files (path heuristic: tests/, __tests__/, *_test.*, *.spec.*, …).
        #[arg(long)]
        tests: bool,
        /// Exclude test files from the output (same path heuristic as --tests).
        #[arg(long, conflicts_with = "tests")]
        exclude_tests: bool,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        compact: bool,
    },
    /// Build, refresh, or inspect the per-repo search index
    Index {
        /// Repository root (default: ".")
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Drop any existing cache and rebuild from scratch
        #[arg(long)]
        rebuild: bool,
        /// Print index stats and exit
        #[arg(long)]
        stats: bool,
        /// With --stats: emit output as JSON
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON
        #[arg(long)]
        compact: bool,
    },
    /// AST-aware search and rewrite using pattern matching with metavariables
    Run {
        /// Pattern to match (e.g. '$FUNC($$$)', 'if ($COND) { $$$BODY }')
        #[arg(short, long)]
        pattern: String,

        /// Replacement template (e.g. 'bar($A)'). Omit for search-only mode.
        #[arg(short, long)]
        rewrite: Option<String>,

        /// Language (auto-detected from file extension if omitted)
        #[arg(short, long)]
        lang: Option<String>,

        /// Paths to search (files or directories). Defaults to current directory.
        paths: Vec<PathBuf>,

        /// Filter files by glob pattern
        #[arg(long)]
        glob: Option<String>,

        /// Actually write changes. Without this flag, only shows matches/dry-run.
        #[arg(long)]
        write: bool,

        /// Emit output as JSON
        #[arg(long)]
        json: bool,

        /// With --json: compact single-line JSON
        #[arg(long)]
        compact: bool,
    },
}

/// Shared argument set for `map` and its `digest` alias (issue #37). The
/// two subcommands are one command internally — same walk, same
/// `Declaration` IR, byte-identical `--json` — differing only in which
/// preset applies.
#[derive(clap::Args)]
struct MapArgs {
    /// Files or directories to map.
    #[arg(num_args = 1..)]
    paths: Vec<PathBuf>,

    /// Detail level: `full` (signatures + docs), `signatures` (signatures,
    /// no docs), `names` (bare member names — the digest renderer).
    /// Defaults to `full` for `map`, `names` under `--preset digest` /
    /// the `digest` alias.
    #[arg(long, value_enum)]
    detail: Option<DetailLevel>,

    /// Named preset. `digest` = `--detail names --no-private --no-fields
    /// --max-members 50`. Explicit flags override the preset.
    #[arg(long, value_enum)]
    preset: Option<MapPreset>,

    #[arg(long)]
    no_private: bool,
    #[arg(long)]
    no_fields: bool,
    #[arg(long)]
    no_docs: bool,
    #[arg(long)]
    no_attrs: bool,
    #[arg(long)]
    no_lines: bool,

    /// Include private members (overrides a preset that hides them).
    #[arg(long, conflicts_with = "no_private")]
    include_private: bool,
    /// Include fields (overrides a preset that hides them).
    #[arg(long, conflicts_with = "no_fields")]
    include_fields: bool,

    /// Cap members shown per type, at any detail level.
    #[arg(long)]
    max_members: Option<usize>,
    /// Only walk files matching this glob (e.g. '*.java').
    #[arg(long)]
    glob: Option<String>,

    /// Emit output as JSON instead of text
    #[arg(long)]
    json: bool,
    /// With --json: emit compact (single-line) JSON instead of pretty-printed
    #[arg(long)]
    compact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DetailLevel {
    Names,
    Signatures,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum MapPreset {
    Digest,
}

pub(crate) fn parse_file(path: &Path) -> Option<ParseResult> {
    crate::main_helpers::parse_file_for_hook(path)
}

/// A 1-indexed, inclusive line range for `squeeze`. Either bound may be open:
/// `start` defaults to line 1, `end` defaults to EOF. Maps cleanly to the
/// `Option<(usize, usize)>` the squeeze renderer expects via [`LineRange::resolve`].
#[derive(Clone, Debug)]
pub struct LineRange {
    pub start: Option<usize>,
    pub end: Option<usize>,
}

impl LineRange {
    /// Collapse to the `(start, end)` pair the report uses, filling open bounds
    /// with line 1 / EOF and clamping `end` to the real line count so JSON
    /// consumers never see an out-of-range sentinel like `usize::MAX`.
    fn resolve(&self, line_count: usize) -> (usize, usize) {
        clamp_line_range(self.start.unwrap_or(1), self.end, line_count)
    }
}

/// Clamp a 1-indexed inclusive line range to a file's real line count.
pub(crate) fn clamp_line_range(
    start: usize,
    end: Option<usize>,
    line_count: usize,
) -> (usize, usize) {
    let end = end.unwrap_or(line_count).min(line_count).max(start);
    (start, end)
}

/// Clap value parser for `squeeze`'s optional range argument. Accepts:
/// `N` (single line), `A:B` (inclusive), `A:` (A to EOF), `:B` (start to B).
/// All bounds are 1-indexed; `0` and `A > B` are rejected. Clamping to the
/// file's real line count happens later in the slicer.
fn parse_line_range(s: &str) -> Result<LineRange, String> {
    let parse_bound = |part: &str| -> Result<Option<usize>, String> {
        if part.is_empty() {
            return Ok(None);
        }
        match part.parse::<usize>() {
            Ok(0) => Err("line numbers are 1-indexed (got 0)".to_string()),
            Ok(n) => Ok(Some(n)),
            Err(_) => Err(format!("invalid line number: {part:?}")),
        }
    };

    let range = match s.split_once(':') {
        // `A:B`, `A:`, `:B`, or `:`
        Some((a, b)) => LineRange {
            start: parse_bound(a)?,
            end: parse_bound(b)?,
        },
        // `N` — single line, both bounds equal
        None => {
            let n = parse_bound(s)?;
            LineRange { start: n, end: n }
        }
    };

    if let (Some(start), Some(end)) = (range.start, range.end) {
        if start > end {
            return Err(format!("range start {start} is after end {end}"));
        }
    }

    Ok(range)
}

/// Parse `<FILE>:<LINE>` into the two parts. Returns `None` if there's no
/// colon or the suffix doesn't parse as a u32. Used by `find-related`.
fn parse_file_line(s: &str) -> Option<(String, u32)> {
    let (file, line) = s.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    Some((file.to_string(), line.parse().ok()?))
}

/// Filter out non-existent paths, build a WalkBuilder with filters and glob overrides,
/// and return (builder, existing_paths). Returns None if no paths exist.
fn build_filtered_walker(
    paths: &[PathBuf],
    glob_str: Option<&str>,
) -> Option<(WalkBuilder, Vec<PathBuf>)> {
    if paths.is_empty() {
        return None;
    }

    let existing: Vec<PathBuf> = paths
        .iter()
        .flat_map(|p| {
            let expanded = path_glob::expand_existing(p);
            if expanded.is_empty() {
                // Diagnostics never go to stdout (issue #36); callers that
                // pre-validate via `require_paths` won't reach this.
                eprintln!("# note: path not found: {}", p.display());
            }
            expanded
        })
        .collect();
    if existing.is_empty() {
        return None;
    }

    let mut builder = WalkBuilder::new(&existing[0]);
    for p in existing.iter().skip(1) {
        builder.add(p);
    }

    builder.hidden(false);
    file_filter::add_filters(&mut builder, &existing[0]);

    if let Some(g) = glob_str {
        if let Ok(override_builder) = ignore::overrides::OverrideBuilder::new("").add(g) {
            if let Ok(over) = override_builder.build() {
                builder.overrides(over);
            }
        }
    }

    Some((builder, existing))
}

pub(crate) fn walk_paths(paths: &[PathBuf], glob_str: Option<&str>) -> Vec<PathBuf> {
    let (tx, rx) = std::sync::mpsc::channel();
    let Some((builder, existing)) = build_filtered_walker(paths, glob_str) else {
        return Vec::new();
    };
    let walker = builder.build_parallel();
    let root = existing[0].clone();

    walker.run(|| {
        let tx = tx.clone();
        let root = root.clone();
        Box::new(move |result| {
            if let Ok(entry) = result {
                if entry.file_type().is_some_and(|ft| ft.is_file())
                    && !file_filter::should_skip_path(entry.path(), &root)
                {
                    let _ = tx.send(entry.path().to_path_buf());
                }
            }
            ignore::WalkState::Continue
        })
    });

    drop(tx);
    let mut results: Vec<_> = rx.into_iter().collect();
    results.sort();
    results
}

pub(crate) fn walk_and_parse(paths: &[PathBuf], glob_str: Option<&str>) -> Vec<ParseResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    let Some((builder, existing)) = build_filtered_walker(paths, glob_str) else {
        return Vec::new();
    };
    let walker = builder.build_parallel();
    let root = existing[0].clone();

    walker.run(|| {
        let tx = tx.clone();
        let root = root.clone();
        Box::new(move |result| {
            if let Ok(entry) = result {
                if entry.file_type().is_some_and(|ft| ft.is_file())
                    && !file_filter::should_skip_path(entry.path(), &root)
                {
                    if let Some(parsed) = parse_file(entry.path()) {
                        let _ = tx.send(parsed);
                    }
                }
            }
            ignore::WalkState::Continue
        })
    });

    drop(tx);
    let mut results: Vec<_> = rx.into_iter().collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    results
}

/// Rejected argument list → stderr + exit 2 (issue #36). Keeps clap's own
/// rendering (it already includes usage and did-you-mean suggestions), adds
/// a cross-subcommand hint when the offending flag exists on a sibling
/// subcommand, and emits the `ast-bro.error.v1` envelope when `--json` was
/// among the raw args.
fn exit_with_parse_error(e: clap::Error) -> ! {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    use clap::CommandFactory;

    // The parse failed, so there is no `Cli` to read `--json` or the
    // subcommand from, and clap's error context carries neither — hence the
    // raw scan. It stops at `--`: everything past the separator is
    // positional data (a pattern, a path), so a literal `--json` there was
    // never a request for JSON output.
    //
    // args_os + lossy conversion: a non-UTF-8 argument must follow the
    // exit-2 contract, not panic the rejection path itself.
    let raw: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .take_while(|a| a != "--")
        .collect();
    let json_mode = raw.iter().any(|a| a == "--json");
    let subcommand = raw
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "ast-bro".to_string());

    let invalid_arg = e.get(ContextKind::InvalidArg).and_then(|v| match v {
        ContextValue::String(s) => Some(s.clone()),
        _ => None,
    });

    // Cross-subcommand hint: `--glob` on `digest` should say it's a `map`
    // flag, turning the failure into a fix.
    let mut hint: Option<String> = None;
    if e.kind() == ErrorKind::UnknownArgument {
        if let Some(flag) = invalid_arg.as_deref().and_then(|f| f.strip_prefix("--")) {
            let flag = flag.split('=').next().unwrap_or(flag);
            let cmd = Cli::command();
            let hosts: Vec<String> = cmd
                .get_subcommands()
                .filter(|sc| {
                    sc.get_name() != subcommand
                        && sc.get_arguments().any(|a| a.get_long() == Some(flag))
                })
                .map(|sc| sc.get_name().to_string())
                .collect();
            if !hosts.is_empty() {
                hint = Some(format!("`--{}` is a `{}` flag", flag, hosts.join("` / `")));
            }
        }
    }

    // clap prints parse errors (message + usage + suggestion) to stderr.
    let _ = e.print();
    if let Some(h) = &hint {
        eprintln!("  {}", h);
    }

    let kind = match e.kind() {
        ErrorKind::UnknownArgument => crate::cli_error::ErrorKind::UnknownFlag,
        _ => crate::cli_error::ErrorKind::BadArgument,
    };
    // A missing required argument must say *which* one: clap's first line
    // stops at "the following required arguments were not provided:" and the
    // names live in the context, so the envelope would otherwise describe the
    // shape of the error and not the error (PR #38 review).
    let missing: Vec<String> = match e.get(ContextKind::InvalidArg) {
        Some(ContextValue::Strings(v)) => v.clone(),
        Some(ContextValue::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    let detail = match (&invalid_arg, e.kind()) {
        (Some(flag), ErrorKind::UnknownArgument) => {
            format!("`{}` has no `{}` flag", subcommand, flag)
        }
        (_, ErrorKind::MissingRequiredArgument) if !missing.is_empty() => format!(
            "`{}` is missing required argument(s): {}",
            subcommand,
            missing.join(", ")
        ),
        _ => e
            .to_string()
            .lines()
            .next()
            .unwrap_or("invalid arguments")
            .trim_start_matches("error: ")
            .to_string(),
    };
    if json_mode {
        let mut err = crate::cli_error::CliError::new(subcommand, kind, detail);
        if let Some(h) = hint {
            err = err.hint(h);
        }
        // Human text already printed via clap above; emit only the envelope.
        err.emit_json_only();
    }
    std::process::exit(kind.exit_code());
}

/// Validate path arguments before walking (issue #33). An empty argument
/// list (a collapsed shell substitution) or a list where nothing resolves
/// rejects with exit 2 on stderr; partial misses proceed with a stderr
/// note. Returns the *original* arguments that resolved, not their
/// expansions: the walker expands what it is handed, so returning
/// expansions would run a second glob pass over names that may themselves
/// contain glob metacharacters. The cost is that a glob argument is
/// expanded twice — once here to test existence, once in the walker — which
/// is deliberate: correctness over one saved directory read.
fn require_paths(command: &str, paths: &[PathBuf], json_mode: bool) -> Vec<PathBuf> {
    use crate::cli_error::{CliError, ErrorKind};
    if paths.is_empty() {
        CliError::new(
            command,
            ErrorKind::NoInput,
            format!(
                "`{}` received no paths (0 arguments after the subcommand)",
                command
            ),
        )
        .hint(format!(
            "An empty argument list is not an empty codebase: nothing was inspected.\n\
             If the list came from a shell substitution such as `ast-bro {} $(...)`, the\n\
             substitution produced nothing. Resolve the files first, check the list is\n\
             non-empty, then pass the paths explicitly.",
            command
        ))
        .exit(json_mode);
    }
    let (existing, missing) = split_existing(paths);
    if existing.is_empty() {
        CliError::new(
            command,
            ErrorKind::PathNotFound,
            format!(
                "`{}` resolved 0 of {} path(s); nothing was inspected",
                command,
                paths.len()
            ),
        )
        .hint(format!(
            "missing: {}\n\
             An empty result here means the paths are wrong, not that the code has no declarations.",
            missing.join(", ")
        ))
        .extra("requested", serde_json::json!(paths.len()))
        .extra("resolved", serde_json::json!(0))
        .extra("missing", serde_json::json!(missing))
        .exit(json_mode);
    }
    for m in &missing {
        eprintln!("# note: path not found: {}", m);
    }
    existing
}

/// `require_paths` for subcommands whose path list is genuinely optional
/// (`run` walks the current directory when given none). An empty list is a
/// real default here; a *wrong* list is still a rejection, so anything
/// passed goes through the same existence check.
fn resolve_optional_paths(command: &str, paths: &[PathBuf], json_mode: bool) -> Vec<PathBuf> {
    if paths.is_empty() {
        return vec![PathBuf::from(".")];
    }
    require_paths(command, paths, json_mode)
}

/// The shared "this path does not exist" rejection for subcommands that take
/// a single root (`search`, `find-related`) instead of a path list. Without
/// it an unresolved root yields an empty result set on stdout with exit 0 —
/// "nothing matched" and "you named a directory that isn't there" must not
/// look the same (issue #33).
fn require_path(command: &str, path: &Path, json_mode: bool) {
    use crate::cli_error::{CliError, ErrorKind};
    if !path.exists() {
        CliError::new(
            command,
            ErrorKind::PathNotFound,
            format!("path not found: {}", path.display()),
        )
        .hint("An empty result here would mean the path is wrong, not that nothing matched.")
        .exit(json_mode);
    }
}

/// Partition path arguments into (resolvable originals, missing display
/// strings) — the shared core of `require_paths` (CLI) and
/// `resolve_paths_for_mcp` (MCP), which differ only in how they report.
fn split_existing(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut existing: Vec<PathBuf> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for p in paths {
        if path_glob::expand_existing(p).is_empty() {
            missing.push(p.display().to_string());
        } else {
            existing.push(p.clone());
        }
    }
    (existing, missing)
}

/// MCP-side counterpart of `require_paths` (#33): the server has no stderr
/// channel to the client and must not exit, so validation failures come
/// back as `Err(message)` for the tool to wrap in `CallResult::Error`.
/// `Ok` carries the original arguments that resolved plus the missing
/// originals — text responses prepend a note line, JSON responses inject
/// a `missing_paths` field, so partial resolution is never silent.
pub(crate) fn resolve_paths_for_mcp(
    command: &str,
    paths: &[PathBuf],
) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    if paths.is_empty() {
        return Err(format!(
            "`{}` received no paths (empty argument list): an empty list is not an empty codebase — nothing was inspected",
            command
        ));
    }
    let (existing, missing) = split_existing(paths);
    if existing.is_empty() {
        return Err(format!(
            "`{}` resolved 0 of {} path(s); nothing was inspected. missing: {}. An empty result here would mean the paths are wrong, not that the code has no declarations",
            command,
            paths.len(),
            missing.join(", ")
        ));
    }
    Ok((existing, missing))
}

/// One handler behind both `map` and `digest` (issue #37). Resolves the
/// preset first, then lets explicitly-passed flags override it, so
/// `digest --include-private` and `map --preset digest --max-members 8`
/// both mean what they say.
fn run_map_digest(a: &MapArgs, digest_alias: bool) {
    let command = if digest_alias { "digest" } else { "map" };
    let paths = require_paths(command, &a.paths, a.json);

    let preset_digest = digest_alias || matches!(a.preset, Some(MapPreset::Digest));
    let detail = a.detail.unwrap_or(if preset_digest {
        DetailLevel::Names
    } else {
        DetailLevel::Full
    });
    let include_private = if a.include_private {
        true
    } else if a.no_private {
        false
    } else {
        !preset_digest
    };
    let include_fields = if a.include_fields {
        true
    } else if a.no_fields {
        false
    } else {
        !preset_digest
    };
    let max_members = a.max_members.or(if preset_digest { Some(50) } else { None });
    // Docs ride only on the `full` detail level (and can still be dropped
    // there with --no-docs). This also governs the JSON payload, which is
    // what makes `digest --json` shed its doc-comment weight.
    let include_docs = matches!(detail, DetailLevel::Full) && !a.no_docs;

    let results = walk_and_parse(&paths, a.glob.as_deref());
    let pretty = !a.compact;
    let map_opts = MapOptions {
        include_private,
        include_fields,
        include_docs,
        include_attributes: !a.no_attrs,
        include_line_numbers: !a.no_lines,
        max_doc_lines: 6,
        max_members,
    };

    if a.json {
        println!(
            "{}",
            crate::core::render_json_map(&results, &map_opts, pretty)
        );
        return;
    }
    match detail {
        DetailLevel::Names => {
            let opts = DigestOptions {
                include_private,
                include_fields,
                include_attributes: !a.no_attrs,
                include_line_numbers: !a.no_lines,
                max_members_per_type: max_members.unwrap_or(usize::MAX),
                max_heading_depth: 3,
            };
            let root = if paths.len() == 1 && paths[0].is_dir() {
                Some(paths[0].as_path())
            } else {
                None
            };
            println!("{}", crate::core::render_digest(&results, &opts, root));
        }
        DetailLevel::Signatures | DetailLevel::Full => {
            if results.is_empty() {
                // The paths exist but contain nothing parseable — that's a
                // real (empty) answer, and stdout must say so rather than
                // stay silent (issue #33).
                println!("# 0 parseable file(s) in the given path(s)");
                return;
            }
            let mut out = String::new();
            for res in &results {
                out.push_str(&crate::core::render_map(res, &map_opts));
                out.push_str("\n\n");
            }
            println!("{}", out.trim_end());
            // A directory-wide `map` can balloon past what fits in one
            // read; point at the digest preset instead of letting the
            // caller reach for `head` and silently lose the middle
            // (issue #35). Never redirect — the choice stays with the
            // caller. Small outputs stay quiet.
            const HINT_THRESHOLD_BYTES: usize = 25_000;
            let dir_input = paths.iter().find(|p| p.is_dir());
            if let Some(dir) = dir_input {
                if out.len() > HINT_THRESHOLD_BYTES {
                    eprintln!(
                        "# hint: this was a directory ({} KB); `{} {} --preset digest --max-members 8` answers the same question in a fraction of the size",
                        out.len() / 1024,
                        command,
                        dir.display(),
                    );
                }
            }
        }
    }
}

pub fn run() {
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    // Error contract (issue #36): a rejected argument list is an error, not
    // a result. `--help` / `--version` keep their exit-0 behaviour, and a
    // bare `ast-bro` (no arguments at all) still prints help on stdout —
    // that's a request for orientation, not a failed query. Everything
    // else — unknown flag, unknown subcommand, bad value — goes to stderr
    // with exit 2, plus an `ast-bro.error.v1` envelope when `--json` was
    // among the raw args, so a mistyped flag is never mistaken for a
    // large successful answer.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                e.exit();
            }
            _ if std::env::args_os().len() <= 1 => {
                let mut cmd = Cli::command();
                let _ = cmd.print_help();
                println!();
                std::process::exit(0);
            }
            _ => {
                exit_with_parse_error(e);
            }
        },
    };

    match &cli.command {
        Commands::Map(args) => {
            run_map_digest(args, false);
        }
        Commands::Show {
            path,
            symbol,
            others,
            json,
            compact,
        } => {
            use crate::cli_error::{CliError, ErrorKind as CliErrorKind};
            if !path.exists() {
                CliError::new(
                    "show",
                    CliErrorKind::PathNotFound,
                    format!("path not found: {}", path.display()),
                )
                .exit(*json);
            }
            let Some(res) = parse_file(path) else {
                CliError::new(
                    "show",
                    CliErrorKind::BadArgument,
                    format!("unsupported file type for `show`: {}", path.display()),
                )
                .exit(*json);
            };
            let mut symbols = vec![symbol.as_str()];
            symbols.extend(others.iter().map(|s| s.as_str()));
            // Per-symbol matches, deduped across overlapping suffixes.
            let mut seen = std::collections::HashSet::new();
            let mut all_matches = Vec::new();
            let mut unmatched: Vec<&str> = Vec::new();
            for sym in &symbols {
                let found = crate::core::find_symbols(&res, sym);
                if found.is_empty() {
                    unmatched.push(sym);
                }
                for m in found {
                    let key = (m.start_line, m.end_line, m.qualified_name.clone());
                    if seen.insert(key) {
                        all_matches.push(m);
                    }
                }
            }
            if all_matches.is_empty() {
                // No requested symbol exists: the query could not run as
                // asked. An empty payload would read as "the file has no
                // such declarations", which is a different claim (#36).
                CliError::new(
                    "show",
                    CliErrorKind::SymbolNotFound,
                    format!(
                        "no symbol matching '{}' in {}",
                        symbols.join(", "),
                        path.display()
                    ),
                )
                .hint("Suffix matching is supported: try 'Type.method' or check `ast-bro map <file>` for the declared names.")
                .exit(*json);
            }
            if !unmatched.is_empty() {
                eprintln!(
                    "# note: no symbol matching '{}' in {} (other symbol(s) shown)",
                    unmatched.join(", "),
                    path.display()
                );
            }
            if *json {
                println!(
                    "{}",
                    crate::core::render_json_show(&res, &all_matches, !(*compact))
                );
            } else {
                for m in &all_matches {
                    println!(
                        "# {}:{}-{} {} ({})",
                        res.path.display(),
                        m.start_line,
                        m.end_line,
                        m.qualified_name,
                        m.kind
                    );
                    if !m.ancestor_signatures.is_empty() {
                        println!("# in: {}", m.ancestor_signatures.join(" → "));
                    }
                    println!("{}", m.source);
                }
            }
        }
        Commands::Squeeze {
            path,
            range,
            raw,
            json,
            compact,
        } => {
            use crate::cli_error::{CliError, ErrorKind as CliErrorKind};
            if !path.exists() {
                CliError::new(
                    "squeeze",
                    CliErrorKind::PathNotFound,
                    format!("path not found: {}", path.display()),
                )
                .exit(*json);
            }
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    // A directory is a wrong argument (exit 2); a read
                    // failure on an existing file is an internal failure
                    // (exit 1), matching the surface handler's Io mapping.
                    let (kind, detail) = if path.is_dir() {
                        (
                            CliErrorKind::BadArgument,
                            format!(
                                "`squeeze` expects a file, got a directory: {}",
                                path.display()
                            ),
                        )
                    } else {
                        (
                            CliErrorKind::IndexError,
                            format!("could not read {}: {}", path.display(), e),
                        )
                    };
                    CliError::new("squeeze", kind, detail).exit(*json);
                }
            };
            let line_count = text.lines().count();
            let resolved: Option<(usize, usize)> = range.as_ref().map(|r| r.resolve(line_count));
            let sliced = crate::squeeze::render::slice_lines(&text, resolved);
            let path_str = path.display().to_string();
            let report = crate::squeeze::render::SqueezeReport {
                path: &path_str,
                range: resolved,
                raw: &sliced,
                raw_requested: *raw,
            };
            if *json {
                println!(
                    "{}",
                    crate::squeeze::render::render_json(&report, !(*compact))
                );
            } else {
                println!("{}", crate::squeeze::render::render_text(&report));
            }
        }
        Commands::Digest(args) => {
            run_map_digest(args, true);
        }
        Commands::Implements {
            target,
            paths,
            direct,
            json,
            compact,
        } => {
            let paths = require_paths("implements", paths, *json);
            let results = walk_and_parse(&paths, None);
            let transitive = !direct;
            let matches = crate::core::find_implementations(&results, target, transitive);
            if matches.is_empty() {
                // Distinguish "this type has no implementations" (a real,
                // interesting answer: exit 0) from "no such type anywhere"
                // (the query could not run as asked: exit 2) — issue #36.
                // Only *type* declarations count as proof: a function named
                // `helper` must not validate `implements helper`.
                let target_exists = results.iter().any(|r| {
                    crate::core::find_symbols(r, target).iter().any(|m| {
                        matches!(
                            m.kind.as_str(),
                            "class" | "struct" | "interface" | "record" | "enum"
                        )
                    })
                });
                if !target_exists {
                    crate::cli_error::CliError::new(
                        "implements",
                        crate::cli_error::ErrorKind::SymbolNotFound,
                        format!("no type named '{}' in the given path(s)", target),
                    )
                    .hint("A 0-match answer is only reported for types that exist. Check the spelling or widen the search path.")
                    .exit(*json);
                }
            }
            if *json {
                println!(
                    "{}",
                    crate::core::render_json_implements(target, &matches, transitive, !(*compact),)
                );
            } else {
                println!(
                    "# {} match(es) for '{}' (incl. transitive):",
                    matches.len(),
                    target
                );
                for m in matches {
                    let via = if m.via.is_empty() {
                        String::new()
                    } else {
                        format!(" [via {}]", m.via.last().unwrap())
                    };
                    println!("{}:{}  {} {}{}", m.path, m.start_line, m.kind, m.name, via);
                }
            }
        }
        Commands::Prompt => {
            println!("{}", crate::prompt::agent_prompt());
        }
        Commands::Install {
            target,
            all,
            local,
            global,
            always,
            min_lines,
            dry_run,
            force,
            mcp,
            skills,
        } => {
            let scope = resolve_scope(*local, *global);
            let opts = installers::InstallOpts {
                min_lines: *min_lines,
                always: *always,
                dry_run: *dry_run,
                force: *force,
            };
            let exit = run_install(target.as_deref(), *all, *mcp, *skills, &scope, &opts);
            std::process::exit(exit);
        }
        Commands::Uninstall {
            target,
            all,
            local,
            global,
            dry_run,
        } => {
            let scope = resolve_scope(*local, *global);
            let opts = installers::InstallOpts {
                dry_run: *dry_run,
                ..installers::InstallOpts::default()
            };
            let exit = run_uninstall(target.as_deref(), *all, &scope, &opts);
            std::process::exit(exit);
        }
        Commands::Status { local, global } => {
            let scope = resolve_scope(*local, *global);
            run_status(&scope);
        }
        Commands::Hook {
            protocol,
            min_lines,
            always,
        } => {
            let exit = hook::run(protocol, *min_lines, *always);
            std::process::exit(exit);
        }
        Commands::Mcp => {
            let exit = mcp::run();
            std::process::exit(exit);
        }
        Commands::Search {
            query,
            path,
            top_k,
            alpha,
            languages,
            rebuild,
            json,
            compact,
        } => {
            require_path("search", path, *json);
            if *rebuild {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                if let Err(e) = crate::search::index::Index::build(path, &cwd) {
                    eprintln!("ast-bro: rebuild failed: {e}");
                    std::process::exit(1);
                }
            }
            let exit = crate::search::cli::run_search(
                query,
                path,
                *top_k,
                *alpha,
                languages.clone(),
                *json,
                !(*compact),
            );
            std::process::exit(exit);
        }
        Commands::FindRelated {
            target,
            path,
            file,
            line,
            top_k,
            json,
            compact,
        } => {
            // Clap guarantees one of: (target alone) or (file + line).
            let (file_path, line_num) = match (target, file, line) {
                (Some(t), _, _) => match parse_file_line(t) {
                    Some(parsed) => parsed,
                    None => {
                        crate::cli_error::CliError::new(
                            "find-related",
                            crate::cli_error::ErrorKind::BadArgument,
                            format!("expected <FILE>:<LINE>, got {t:?}"),
                        )
                        .hint("Use `find-related src/file.rs:42`, or `--file FILE --line N`.")
                        .exit(*json);
                    }
                },
                (None, Some(f), Some(l)) => (f.clone(), *l),
                _ => unreachable!("clap should have rejected this argument combination"),
            };
            require_path("find-related", path, *json);
            let exit = crate::search::cli::run_find_related(
                &file_path,
                line_num,
                path,
                *top_k,
                *json,
                !(*compact),
            );
            std::process::exit(exit);
        }
        Commands::Surface {
            path,
            tree,
            include_chain,
            max_depth,
            include_private,
            lang,
            json,
            compact,
        } => {
            if !path.exists() {
                crate::cli_error::CliError::new(
                    "surface",
                    crate::cli_error::ErrorKind::PathNotFound,
                    format!("path not found: {}", path.display()),
                )
                .exit(*json);
            }
            let lang_override = match lang {
                Some(s) => match crate::surface::LangOverride::parse(s) {
                    Some(l) => Some(l),
                    None => {
                        crate::cli_error::CliError::new(
                            "surface",
                            crate::cli_error::ErrorKind::BadArgument,
                            format!("unknown --lang value '{}'", s),
                        )
                        .hint("Expected rust|python|fallback.")
                        .exit(*json);
                    }
                },
                None => None,
            };
            let json_on = *json;
            let pretty = !(*compact);
            let output = if json_on {
                crate::surface::OutputMode::Json { compact: !pretty }
            } else if *tree {
                crate::surface::OutputMode::Tree
            } else {
                crate::surface::OutputMode::Flat
            };
            let opts = crate::surface::SurfaceOptions {
                output,
                include_private: *include_private,
                max_depth: *max_depth,
                include_chain: *include_chain,
                lang_override,
            };
            match crate::surface::resolve_surface(path, &opts) {
                Ok(entries) => {
                    let rendered =
                        crate::surface::render::render(&entries, opts.output, opts.include_chain);
                    print!("{}", rendered);
                }
                Err(e) => {
                    use crate::surface::SurfaceError;
                    let kind = match &e {
                        SurfaceError::NoEntryPoint { .. } => {
                            crate::cli_error::ErrorKind::PathNotFound
                        }
                        // The path was verified to exist above, so a
                        // surviving Io error is a read/permission failure —
                        // an internal failure (exit 1), not a wrong path an
                        // agent should retry differently.
                        SurfaceError::Io { .. } | SurfaceError::Parse { .. } => {
                            crate::cli_error::ErrorKind::IndexError
                        }
                        SurfaceError::BadOverride(_) => crate::cli_error::ErrorKind::BadArgument,
                    };
                    crate::cli_error::CliError::new("surface", kind, e.to_string()).exit(json_on);
                }
            }
        }
        Commands::Deps {
            file,
            depth,
            hide_external,
            rebuild,
            json,
            compact,
        } => {
            let exit = crate::deps::cli::run_deps(
                file,
                *depth,
                !(*hide_external),
                *json,
                !(*compact),
                *rebuild,
            );
            std::process::exit(exit);
        }
        Commands::ReverseDeps {
            file,
            depth,
            limit,
            tests,
            exclude_tests,
            rebuild,
            json,
            compact,
        } => {
            let exit = crate::deps::cli::run_reverse_deps(
                file,
                *depth,
                *limit,
                *tests,
                *exclude_tests,
                *json,
                !(*compact),
                *rebuild,
            );
            std::process::exit(exit);
        }
        Commands::Cycles {
            path,
            min_size,
            rebuild,
            json,
            compact,
        } => {
            let exit = crate::deps::cli::run_cycles(path, *min_size, *json, !(*compact), *rebuild);
            std::process::exit(exit);
        }
        Commands::Graph {
            path,
            json,
            hide_external,
            include_external,
            rebuild,
            compact,
        } => {
            if *include_external {
                eprintln!("# note: --include-external is deprecated; unresolved imports are shown by default now (use --hide-external to drop them)");
            }
            let exit =
                crate::deps::cli::run_graph(path, *json, !(*hide_external), !(*compact), *rebuild);
            std::process::exit(exit);
        }
        Commands::Index {
            path,
            rebuild,
            stats,
            json,
            compact,
        } => {
            let exit = crate::search::cli::run_index(path, *rebuild, *stats, *json, !(*compact));
            std::process::exit(exit);
        }
        Commands::Callers {
            target,
            path,
            file,
            symbol,
            depth,
            limit,
            hide_ambiguous,
            include_ambiguous,
            tests,
            exclude_tests,
            rebuild,
            json,
            compact,
        } => {
            if *include_ambiguous {
                eprintln!("# note: --include-ambiguous is deprecated; ambiguous callers are shown by default now (use --hide-ambiguous to drop them)");
            }
            let resolved = compose_target(target.as_deref(), file.as_deref(), symbol.as_deref());
            let exit = crate::calls::cli::run_callers(
                &resolved,
                path,
                *depth,
                *limit,
                !(*hide_ambiguous),
                *tests,
                *exclude_tests,
                *rebuild,
                *json,
                !(*compact),
            );
            std::process::exit(exit);
        }
        Commands::Context {
            target,
            path,
            file,
            symbol,
            budget,
            rebuild,
            json,
            compact,
        } => {
            let resolved = compose_target(target.as_deref(), file.as_deref(), symbol.as_deref());
            let exit = crate::context::run_context(
                &resolved,
                path,
                &crate::context::ContextOptions {
                    budget: *budget,
                    json: *json,
                    pretty: !(*compact),
                },
                *rebuild,
            );
            std::process::exit(exit);
        }
        Commands::Callees {
            target,
            path,
            file,
            symbol,
            depth,
            limit,
            hide_external,
            external,
            rebuild,
            json,
            compact,
        } => {
            if *external {
                eprintln!("# note: --external is deprecated; unresolved/external callees are shown by default now (use --hide-external to drop them)");
            }
            let resolved = compose_target(target.as_deref(), file.as_deref(), symbol.as_deref());
            let exit = crate::calls::cli::run_callees(
                &resolved,
                path,
                *depth,
                *limit,
                !(*hide_external),
                *rebuild,
                *json,
                !(*compact),
            );
            std::process::exit(exit);
        }
        Commands::Trace {
            from,
            to,
            path,
            depth,
            rebuild,
            json,
            compact,
        } => {
            let exit =
                crate::calls::cli::run_trace(from, to, path, *depth, *rebuild, *json, !(*compact));
            std::process::exit(exit);
        }
        Commands::Impact {
            target,
            path,
            file,
            symbol,
            depth,
            limit,
            mode,
            hide_ambiguous,
            tests,
            exclude_tests,
            rebuild,
            json,
            compact,
        } => {
            let impact_mode = match crate::impact::ImpactMode::parse(mode) {
                Some(m) => m,
                None => {
                    eprintln!(
                        "# note: unknown --mode '{}'. Expected: deps, dependents, tests, all",
                        mode
                    );
                    std::process::exit(2);
                }
            };
            let resolved = compose_target(target.as_deref(), file.as_deref(), symbol.as_deref());
            let opts = crate::impact::ImpactOptions {
                depth: *depth,
                limit: *limit,
                mode: impact_mode,
                include_ambiguous: !(*hide_ambiguous),
                tests: *tests,
                exclude_tests: *exclude_tests,
                json: *json,
                pretty: !(*compact),
            };
            let exit = crate::impact::run_impact(&resolved, path, &opts, *rebuild);
            std::process::exit(exit);
        }
        Commands::Run {
            pattern,
            rewrite,
            lang,
            paths,
            glob,
            write,
            json,
            compact,
        } => {
            // Same rejection rule as every other path-taking subcommand: a
            // path that doesn't resolve is a failed call (exit 2, empty
            // stdout), not a run that matched nothing (issue #33). The MCP
            // side already did this via `resolve_paths_for_mcp`.
            let paths = resolve_optional_paths("run", paths, *json);
            let exit = crate::run::cli::run(
                pattern,
                rewrite.as_deref(),
                lang.as_deref(),
                &paths,
                glob.as_deref(),
                *write,
                *json,
                !(*compact),
            );
            std::process::exit(exit);
        }
    }
}

/// Fold `--file <F> --symbol <S>` into the same `<file>:<symbol>` canonical
/// form the positional `<TARGET>` arg uses. Clap's `required_unless_present_all`
/// guarantees exactly one of the two arms is populated.
fn compose_target(target: Option<&str>, file: Option<&str>, symbol: Option<&str>) -> String {
    if let Some(t) = target {
        return t.to_string();
    }
    match (file, symbol) {
        (Some(f), Some(s)) => format!("{}:{}", f, s),
        _ => unreachable!("clap guarantees target XOR (file && symbol)"),
    }
}

fn resolve_scope(local: bool, _global: bool) -> installers::Scope {
    if local {
        installers::Scope::Local(std::env::current_dir().expect("cwd"))
    } else {
        installers::Scope::Global
    }
}

fn run_install(
    target: Option<&str>,
    all: bool,
    mcp: bool,
    skills: bool,
    scope: &installers::Scope,
    opts: &installers::InstallOpts,
) -> i32 {
    let registry = installers::registry();
    let chosen: Vec<&Box<dyn installers::Installer>> = if all {
        select_all(&registry, scope)
    } else if let Some(name) = target {
        match registry.iter().find(|i| i.name() == name) {
            Some(i) => vec![i],
            None => {
                eprintln!("unknown --target '{}'. Known: {}", name, names(&registry));
                return 2;
            }
        }
    } else {
        eprintln!(
            "must pass --target <name> or --all. Known: {}",
            names(&registry)
        );
        return 2;
    };

    let exclusive_mode = mcp || skills;
    let mut any_installed = false;
    let mut any_failed = false;
    for inst in chosen {
        let label = inst.name();
        if !exclusive_mode {
            match inst.install_prompt(scope, opts) {
                Ok(c) => {
                    print_change(label, "prompt", &c);
                    if !matches!(
                        c,
                        installers::Change::Skipped { .. } | installers::Change::NotApplicable
                    ) {
                        any_installed = true;
                    }
                }
                Err(e) => {
                    eprintln!("{}: prompt: {}", label, e);
                    any_failed = true;
                }
            }
            match inst.install_hook(scope, opts) {
                Ok(c) => {
                    print_change(label, "hook", &c);
                    if !matches!(
                        c,
                        installers::Change::Skipped { .. } | installers::Change::NotApplicable
                    ) {
                        any_installed = true;
                    }
                }
                Err(e) => {
                    eprintln!("{}: hook: {}", label, e);
                    any_failed = true;
                }
            }
            match inst.install_subagents(scope, opts) {
                Ok(changes) => {
                    for c in &changes {
                        print_change(label, "subagent", c);
                        if !matches!(
                            c,
                            installers::Change::Skipped { .. } | installers::Change::NotApplicable
                        ) {
                            any_installed = true;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: subagent: {}", label, e);
                    any_failed = true;
                }
            }
        } else {
            if mcp {
                match inst.install_mcp(scope, opts) {
                    Ok(c) => {
                        print_change(label, "mcp", &c);
                        if !matches!(
                            c,
                            installers::Change::Skipped { .. } | installers::Change::NotApplicable
                        ) {
                            any_installed = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: mcp: {}", label, e);
                        any_failed = true;
                    }
                }
            }
            if skills {
                match inst.install_skills(scope, opts) {
                    Ok(c) => {
                        print_change(label, "skills", &c);
                        if !matches!(
                            c,
                            installers::Change::Skipped { .. } | installers::Change::NotApplicable
                        ) {
                            any_installed = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: skills: {}", label, e);
                        any_failed = true;
                    }
                }
            }
        }
    }

    if any_failed && any_installed {
        1
    } else if any_failed {
        2
    } else {
        0
    }
}

fn run_uninstall(
    target: Option<&str>,
    all: bool,
    scope: &installers::Scope,
    opts: &installers::InstallOpts,
) -> i32 {
    let registry = installers::registry();
    let chosen: Vec<&Box<dyn installers::Installer>> = if all {
        select_all(&registry, scope)
    } else if let Some(name) = target {
        match registry.iter().find(|i| i.name() == name) {
            Some(i) => vec![i],
            None => {
                eprintln!("unknown --target '{}'. Known: {}", name, names(&registry));
                return 2;
            }
        }
    } else {
        eprintln!(
            "must pass --target <name> or --all. Known: {}",
            names(&registry)
        );
        return 2;
    };

    let mut any_failed = false;
    for inst in chosen {
        match inst.uninstall(scope, opts) {
            Ok(changes) => {
                for c in changes {
                    print_change(inst.name(), "uninstall", &c);
                }
            }
            Err(e) => {
                eprintln!("{}: {}", inst.name(), e);
                any_failed = true;
            }
        }
    }
    if any_failed {
        1
    } else {
        0
    }
}

fn run_status(scope: &installers::Scope) {
    for inst in installers::registry() {
        let s = inst.status(scope);
        let prompt = if s.prompt_installed {
            format!("prompt {}", s.prompt_version.unwrap_or_else(|| "?".into()))
        } else {
            "prompt -".to_string()
        };
        let hook = if s.hook_installed {
            "hook ✓"
        } else {
            "hook -"
        };
        let mcp = if s.mcp_installed { "mcp ✓" } else { "mcp -" };
        let skills = if s.skills_installed {
            "skills ✓"
        } else {
            "skills -"
        };
        println!(
            "{:<14} {:<14} {:<8} {:<8} {}",
            inst.name(),
            prompt,
            hook,
            mcp,
            skills
        );
    }
}

fn names(registry: &[Box<dyn installers::Installer>]) -> String {
    registry
        .iter()
        .map(|i| i.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Picks the adapters to act on for `--all`. For `Scope::Global`, we
/// skip targets whose `detect()` reports the CLI is absent (and print a
/// note). For `Scope::Local`, the user explicitly opted into this repo
/// so detection is bypassed.
#[allow(clippy::borrowed_box)]
fn select_all<'a>(
    registry: &'a [Box<dyn installers::Installer>],
    scope: &installers::Scope,
) -> Vec<&'a Box<dyn installers::Installer>> {
    let bypass_detection = matches!(scope, installers::Scope::Local(_));
    registry
        .iter()
        .filter(|inst| {
            if bypass_detection {
                return true;
            }
            let d = inst.detect(scope);
            if !d.present {
                println!(
                    "{:<14} {:<10} skipped  (not detected on this system)",
                    inst.name(),
                    "detect"
                );
            }
            d.present
        })
        .collect()
}

fn print_change(target: &str, phase: &str, change: &installers::Change) {
    use installers::Change::*;
    match change {
        Created(p) => println!("{:<14} {:<10} created  {}", target, phase, p.display()),
        Updated(p) => println!("{:<14} {:<10} updated  {}", target, phase, p.display()),
        Removed(p) => println!("{:<14} {:<10} removed  {}", target, phase, p.display()),
        Skipped { path, reason } => {
            println!(
                "{:<14} {:<10} skipped  {} ({})",
                target,
                phase,
                path.display(),
                reason
            )
        }
        NotApplicable => println!("{:<14} {:<10} n/a", target, phase),
    }
}
