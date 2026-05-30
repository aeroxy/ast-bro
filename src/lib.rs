use clap::{Parser, Subcommand};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

mod adapters;
mod calls;
mod core;
mod deps;
mod file_filter;
mod graph_cache;
mod prompt;
mod installers;
mod hook;
mod main_helpers;
mod mcp;
mod project_root;
mod search;
mod run;
mod squeeze;
mod surface;

use crate::core::{DigestOptions, MapOptions, ParseResult};

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
    Map {
        /// Files or directories to map.
        #[arg(num_args = 1..)]
        paths: Vec<PathBuf>,

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
        #[arg(long)]
        glob: Option<String>,
        /// Emit output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON instead of pretty-printed
        #[arg(long)]
        compact: bool,
    },
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
    /// One-page module map
    Digest {
        #[arg(num_args = 1..)]
        paths: Vec<PathBuf>,

        #[arg(long)]
        include_private: bool,
        #[arg(long)]
        include_fields: bool,
        #[arg(long, default_value_t = 50)]
        max_members: usize,
        /// Emit output as JSON instead of text
        #[arg(long)]
        json: bool,
        /// With --json: emit compact (single-line) JSON
        #[arg(long)]
        compact: bool,
    },
    /// Find subclasses / implementations
    Implements {
        target: String,
        #[arg(num_args = 1..)]
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
        /// Install ast-bro as an MCP server entry instead of the CLAUDE.md prompt.
        /// Combine with `--skills` to install both.
        #[arg(long)]
        mcp: bool,
        /// Install ast-bro as a Claude Code skill instead of the CLAUDE.md prompt.
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
        #[arg(long, default_value_t = 200)]
        limit: usize,
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
        #[arg(long)]
        include_external: bool,
        #[arg(long)]
        rebuild: bool,
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
        /// Cap result count (mirrors reverse-deps).
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Include callers whose target is `Ambiguous` (off by default — noisy).
        #[arg(long)]
        include_ambiguous: bool,
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
        /// Include unresolved callees (the `Bare`/`External` bucket).
        #[arg(long)]
        external: bool,
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
    /// with line 1 / `usize::MAX` (the slicer clamps `end` to the real EOF).
    fn resolve(&self) -> (usize, usize) {
        (self.start.unwrap_or(1), self.end.unwrap_or(usize::MAX))
    }
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
fn build_filtered_walker(paths: &[PathBuf], glob_str: Option<&str>) -> Option<(WalkBuilder, Vec<PathBuf>)> {
    if paths.is_empty() {
        return None;
    }

    let existing: Vec<PathBuf> = paths
        .iter()
        .filter(|p| {
            if p.exists() {
                true
            } else {
                println!("# note: path not found: {}", p.display());
                false
            }
        })
        .cloned()
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

pub fn run() {
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    // Agent-friendly arg handling: instead of dying on a typo or unknown
    // flag, print the help text so the calling agent can self-correct
    // without a separate `--help` round-trip. `--help` / `--version` keep
    // their normal exit-0 behaviour; everything else prints help to stdout
    // and exits 0 too (agents see "output" rather than "error").
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                e.exit();
            }
            _ => {
                let mut cmd = Cli::command();
                let _ = cmd.print_help();
                println!();
                println!("# note: could not parse args ({}). Showing help instead.", e.kind());
                std::process::exit(0);
            }
        },
    };

    match &cli.command {
            Commands::Map {
                paths,
                no_private,
                no_fields,
                no_docs,
                no_attrs,
                no_lines,
                glob,
                json,
                compact,
            } => {
                let results = walk_and_parse(paths, glob.as_deref());
                let opts = MapOptions {
                    include_private: !(*no_private),
                    include_fields: !(*no_fields),
                    include_docs: !(*no_docs),
                    include_attributes: !(*no_attrs),
                    include_line_numbers: !(*no_lines),
                    max_doc_lines: 6,
                    max_members: None,
                };
                let json_on = *json;
                let pretty = !(*compact);
                if json_on {
                    println!("{}", crate::core::render_json_map(&results, &opts, pretty));
                } else {
                    for res in results {
                        println!("{}", crate::core::render_map(&res, &opts));
                        println!();
                    }
                }
            }
            Commands::Show {
                path,
                symbol,
                others,
                json,
                compact,
            } => {
                if !path.exists() {
                    println!("# note: path not found: {}", path.display());
                } else if let Some(res) = parse_file(path) {
                    let mut symbols = vec![symbol.as_str()];
                    symbols.extend(others.iter().map(|s| s.as_str()));
                    if *json {
                        let mut seen = std::collections::HashSet::new();
                        let mut all_matches = Vec::new();
                        for sym in &symbols {
                            for m in crate::core::find_symbols(&res, sym) {
                                let key = (m.start_line, m.end_line, m.qualified_name.clone());
                                if seen.insert(key) {
                                    all_matches.push(m);
                                }
                            }
                        }
                        println!(
                            "{}",
                            crate::core::render_json_show(&res, &all_matches, !(*compact))
                        );
                        if all_matches.is_empty() {
                            // JSON consumers see [] in the payload; humans/agents
                            // glancing at stderr-free output get a hint too.
                            println!("# note: no symbol matching {:?} in {}", symbol, path.display());
                        }
                    } else {
                        let mut any_match = false;
                        for sym in &symbols {
                            let matches = crate::core::find_symbols(&res, sym);
                            for m in matches {
                                any_match = true;
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
                        if !any_match {
                            let joined = symbols.join(", ");
                            println!(
                                "# note: no symbol matching '{}' in {}",
                                joined,
                                path.display()
                            );
                        }
                    }
                } else {
                    println!(
                        "# note: unsupported file type for `show`: {}",
                        path.display()
                    );
                }
            }
            Commands::Squeeze {
                path,
                range,
                raw,
                json,
                compact,
            } => {
                if !path.exists() {
                    println!("# note: path not found: {}", path.display());
                    return;
                }
                let text = match std::fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(_) => {
                        println!("# note: not valid UTF-8: {}", path.display());
                        return;
                    }
                };
                let resolved: Option<(usize, usize)> = range.as_ref().map(LineRange::resolve);
                let sliced = crate::squeeze::render::slice_lines(&text, resolved);
                let path_str = path.display().to_string();
                let report = crate::squeeze::render::SqueezeReport {
                    path: &path_str,
                    range: resolved,
                    raw: &sliced,
                    raw_requested: *raw,
                };
                if *json {
                    println!("{}", crate::squeeze::render::render_json(&report, !(*compact)));
                } else {
                    println!("{}", crate::squeeze::render::render_text(&report));
                }
            }
            Commands::Digest {
                paths,
                include_private,
                include_fields,
                max_members,
                json,
                compact,
            } => {
                let results = walk_and_parse(paths, None);
                if *json {
                    let opts = MapOptions {
                        include_private: *include_private,
                        include_fields: *include_fields,
                        include_docs: true,
                        include_attributes: true,
                        include_line_numbers: true,
                        max_doc_lines: 6,
                        max_members: Some(*max_members),
                    };
                    println!(
                        "{}",
                        crate::core::render_json_map(&results, &opts, !(*compact))
                    );
                } else {
                    let opts = DigestOptions {
                        include_private: *include_private,
                        include_fields: *include_fields,
                        max_members_per_type: *max_members,
                        max_heading_depth: 3,
                    };
                    let root = if paths.len() == 1 && paths[0].is_dir() {
                        Some(paths[0].as_path())
                    } else {
                        None
                    };
                    println!("{}", crate::core::render_digest(&results, &opts, root));
                }
            }
            Commands::Implements {
                target,
                paths,
                direct,
                json,
                compact,
            } => {
                let results = walk_and_parse(paths, None);
                let transitive = !direct;
                let matches = crate::core::find_implementations(&results, target, transitive);
                if *json {
                    println!(
                        "{}",
                        crate::core::render_json_implements(
                            target,
                            &matches,
                            transitive,
                            !(*compact),
                        )
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
                println!("{}", crate::prompt::AGENT_PROMPT);
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
                if *rebuild {
                    let cwd = std::env::current_dir()
                        .unwrap_or_else(|_| std::path::PathBuf::from("."));
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
                            println!(
                                "# note: expected <FILE>:<LINE>, got {t:?} \
                                 (or use --file FILE --line N instead)"
                            );
                            return;
                        }
                    },
                    (None, Some(f), Some(l)) => (f.clone(), *l),
                    _ => unreachable!("clap should have rejected this argument combination"),
                };
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
                let lang_override = match lang {
                    Some(s) => match crate::surface::LangOverride::parse(s) {
                        Some(l) => Some(l),
                        None => {
                            println!("# note: unknown --lang value '{}'. Expected rust|python|fallback.", s);
                            return;
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
                        println!("# note: {e}");
                    }
                }
            }
            Commands::Deps {
                file,
                depth,
                rebuild,
                json,
                compact,
            } => {
                let exit = crate::deps::cli::run_deps(
                    file,
                    *depth,
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
                rebuild,
                json,
                compact,
            } => {
                let exit = crate::deps::cli::run_reverse_deps(
                    file,
                    *depth,
                    *limit,
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
                let exit = crate::deps::cli::run_cycles(
                    path,
                    *min_size,
                    *json,
                    !(*compact),
                    *rebuild,
                );
                std::process::exit(exit);
            }
            Commands::Graph {
                path,
                json,
                include_external,
                rebuild,
                compact,
            } => {
                let exit = crate::deps::cli::run_graph(
                    path,
                    *json,
                    *include_external,
                    !(*compact),
                    *rebuild,
                );
                std::process::exit(exit);
            }
            Commands::Index {
                path,
                rebuild,
                stats,
                json,
                compact,
            } => {
                let exit = crate::search::cli::run_index(
                    path,
                    *rebuild,
                    *stats,
                    *json,
                    !(*compact),
                );
                std::process::exit(exit);
            }
            Commands::Callers {
                target,
                path,
                file,
                symbol,
                depth,
                limit,
                include_ambiguous,
                rebuild,
                json,
                compact,
            } => {
                let resolved = compose_target(target.as_deref(), file.as_deref(), symbol.as_deref());
                let exit = crate::calls::cli::run_callers(
                    &resolved,
                    path,
                    *depth,
                    *limit,
                    *include_ambiguous,
                    *rebuild,
                    *json,
                    !(*compact),
                );
                std::process::exit(exit);
            }
            Commands::Callees {
                target,
                path,
                file,
                symbol,
                depth,
                external,
                rebuild,
                json,
                compact,
            } => {
                let resolved = compose_target(target.as_deref(), file.as_deref(), symbol.as_deref());
                let exit = crate::calls::cli::run_callees(
                    &resolved,
                    path,
                    *depth,
                    *external,
                    *rebuild,
                    *json,
                    !(*compact),
                );
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
                let exit = crate::run::cli::run(
                    pattern,
                    rewrite.as_deref(),
                    lang.as_deref(),
                    paths,
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
                eprintln!(
                    "unknown --target '{}'. Known: {}",
                    name,
                    names(&registry)
                );
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
                eprintln!(
                    "unknown --target '{}'. Known: {}",
                    name,
                    names(&registry)
                );
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
        let hook = if s.hook_installed { "hook ✓" } else { "hook -" };
        let mcp = if s.mcp_installed { "mcp ✓" } else { "mcp -" };
        let skills = if s.skills_installed { "skills ✓" } else { "skills -" };
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
                println!("{:<14} {:<10} skipped  (not detected on this system)", inst.name(), "detect");
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
