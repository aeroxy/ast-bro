//! Output formatting for `deps`, `reverse-deps`, `cycles`, `graph`.
//! Text + JSON for everything.
//!
//! Text mode uses `colored::Colorize` which auto-detects TTY and
//! respects `NO_COLOR=1` (so integration tests stay byte-stable).
//! JSON output is always plain.

use colored::Colorize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::core::{
    JSON_SCHEMA_CYCLES, JSON_SCHEMA_DEPS, JSON_SCHEMA_GRAPH, JSON_SCHEMA_REVERSE_DEPS,
};
use crate::deps::graph::{DepEdge, DepGraph, ImportKind};
use crate::deps::scc::Cycle;
use crate::deps::traverse::DepHit;

// ---- Text rendering ----

/// The depth to name in a stdout header, or `None` when the walk reached the
/// end of the graph and the count needs no qualification.
pub fn depth_cutoff(depth: usize, frontier_truncated: bool) -> Option<usize> {
    frontier_truncated.then_some(depth)
}

/// The one wording for "`--depth` cut this walk short" — the difference
/// between "the import cone ends here" and "the walk stopped here" (issue
/// #32). Shared so the CLI (which prints it on stderr) and MCP (which has one
/// channel and prepends it to the response) cannot drift.
///
/// Unlike `callers` / `callees`, which suppress their note at the default
/// depth of 1 because "the callers have callers" is the norm rather than a
/// qualification, `deps` and `reverse-deps` default to 3: an import cone
/// running deeper than three hops is a fact about *this* file, so the note
/// earns its line at every depth. JSON carries `frontier_truncated` either way.
pub fn frontier_note(command: &str, depth: usize, frontier_truncated: bool) -> Option<String> {
    frontier_truncated.then(|| {
        format!(
            "# note: {} stopped at --depth {} with unvisited files beyond it; raise --depth to walk further",
            command, depth
        )
    })
}

pub fn render_deps_text(
    graph: &DepGraph,
    start: &Path,
    hits: &[DepHit],
    include_external: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", graph.rel(start).cyan().bold());
    if hits.is_empty()
        && (!include_external || graph.external.get(start).is_none_or(|v| v.is_empty()))
    {
        let _ = writeln!(out, "  {}", "(no imports)".dimmed());
        return out;
    }
    for h in hits {
        let prefix = "  ".repeat(h.depth);
        let alias = h
            .local_name
            .as_ref()
            .map(|a| format!(" {}", format!("[as {}]", a).cyan()))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{}{} {}{}",
            prefix,
            graph.rel(&h.file).green(),
            format!("({})", h.kind.label()).dimmed(),
            alias
        );
    }
    if include_external {
        if let Some(externals) = graph.external.get(start) {
            if !externals.is_empty() {
                let _ = writeln!(out);
                let _ = writeln!(
                    out,
                    "# {} {}",
                    externals.len().to_string().bold(),
                    if externals.len() == 1 {
                        "unresolved import:".dimmed()
                    } else {
                        "unresolved imports:".dimmed()
                    }
                );
                for ext in externals {
                    let _ = writeln!(out, "  {} {}", "[external]".cyan(), ext.dimmed());
                }
            }
        }
    }
    out
}

pub fn render_reverse_deps_text(
    graph: &DepGraph,
    start: &Path,
    hits: &[DepHit],
    total: usize,
    depth_cutoff: Option<usize>,
) -> String {
    let mut out = String::new();
    // Carry the true pre-`--limit` count in the header so a capped list is
    // never mistaken for a complete one (issue #32) — and qualify the count
    // itself when `--depth` stopped the walk, because that is the number a
    // reader quotes. The stderr note says the same thing; this one has to be
    // on stdout, where the result is.
    let counted = match depth_cutoff {
        Some(depth) => format!("{} total within --depth {}", total, depth),
        None => format!("{} total", total),
    };
    let suffix = if total > hits.len() {
        format!(
            " ({}; showing {} — raise --limit to see the rest)",
            counted,
            hits.len()
        )
    } else {
        format!(" ({})", counted)
    };
    let _ = writeln!(
        out,
        "{} {}",
        graph.rel(start).cyan().bold(),
        format!("← imported by:{}", suffix).dimmed()
    );
    if hits.is_empty() {
        let _ = writeln!(out, "  {}", "(no importers)".dimmed());
        return out;
    }
    for h in hits {
        let prefix = "  ".repeat(h.depth);
        let _ = writeln!(out, "{}{}", prefix, graph.rel(&h.file).yellow());
    }
    out
}

pub fn render_cycles_text(graph: &DepGraph, cycles: &[Cycle]) -> String {
    let mut out = String::new();
    if cycles.is_empty() {
        let _ = writeln!(out, "{}", "no cycles found".green());
        return out;
    }
    let _ = writeln!(
        out,
        "{}",
        format!("{} cycle(s):", cycles.len()).red().bold()
    );
    for (i, c) in cycles.iter().enumerate() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{} {}",
            format!("[{}]", i + 1).red().bold(),
            format!("cycle of {} files:", c.members.len()).dimmed()
        );
        for m in &c.members {
            let _ = writeln!(out, "  {}", graph.rel(m).yellow());
        }
    }
    out
}

pub fn render_graph_text(graph: &DepGraph, include_external: bool) -> String {
    let mut out = String::new();
    let edges = graph.sorted_edges();
    let mut grouped: BTreeMap<String, Vec<(String, Option<ImportKind>)>> = BTreeMap::new();
    for (s, t, k) in edges {
        grouped
            .entry(graph.rel(&s))
            .or_default()
            .push((graph.rel(&t), Some(k)));
    }
    if include_external {
        for (file, externals) in &graph.external {
            let s = graph.rel(file);
            for ext in externals {
                grouped.entry(s.clone()).or_default().push((ext.clone(), None));
            }
        }
    }

    let total_edges: usize = grouped.values().map(|v| v.len()).sum();
    let _ = writeln!(
        out,
        "{}",
        format!("{} files, {} edges", graph.stats.file_count, total_edges).dimmed()
    );

    for (s, ts) in grouped {
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", s.cyan().bold());
        for (t, k) in ts {
            let kind_label = k
                .map(|kind| format!(" ({})", kind.label()))
                .unwrap_or_else(|| format!(" {}", "[external]".cyan()));
            let _ = writeln!(
                out,
                "  {} {} {}",
                "→".dimmed(),
                if k.is_some() { t.green() } else { t.dimmed() },
                kind_label.dimmed()
            );
        }
    }
    out
}

// ---- JSON rendering ----

#[derive(Serialize)]
struct DepsDoc<'a> {
    schema: &'static str,
    file: String,
    /// True when `--depth` stopped the walk while unvisited imports remained,
    /// so `hits` is a prefix of the import cone rather than all of it
    /// (issue #32).
    frontier_truncated: bool,
    hits: Vec<JsonHit<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    external: Vec<String>,
}

#[derive(Serialize)]
struct JsonHit<'a> {
    depth: usize,
    file: String,
    kind: &'static str,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_name: Option<&'a str>,
}

pub fn render_deps_json(
    graph: &DepGraph,
    start: &Path,
    hits: &[DepHit],
    frontier_truncated: bool,
    include_external: bool,
    pretty: bool,
) -> String {
    let external = if include_external {
        graph.external.get(start).cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let doc = DepsDoc {
        schema: JSON_SCHEMA_DEPS,
        file: graph.rel(start),
        frontier_truncated,
        hits: hits
            .iter()
            .map(|h| JsonHit {
                depth: h.depth,
                file: graph.rel(&h.file),
                kind: h.kind.label(),
                line: h.line,
                local_name: h.local_name.as_deref(),
            })
            .collect(),
        external,
    };
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

pub fn render_reverse_deps_json(
    graph: &DepGraph,
    start: &Path,
    hits: &[DepHit],
    total: usize,
    frontier_truncated: bool,
    pretty: bool,
) -> String {
    #[derive(Serialize)]
    struct Doc<'a> {
        schema: &'static str,
        file: String,
        /// True pre-`--limit` importer count (issue #32).
        total: usize,
        truncated: bool,
        /// True when `--depth` stopped the walk while unvisited importers
        /// remained. Orthogonal to `truncated`: that one says the *display*
        /// was cut, this one says `total` itself counts only part of the
        /// reverse cone (issue #32).
        frontier_truncated: bool,
        importers: Vec<JsonHit<'a>>,
    }
    let doc = Doc {
        schema: JSON_SCHEMA_REVERSE_DEPS,
        file: graph.rel(start),
        total,
        truncated: total > hits.len(),
        frontier_truncated,
        importers: hits
            .iter()
            .map(|h| JsonHit {
                depth: h.depth,
                file: graph.rel(&h.file),
                kind: h.kind.label(),
                line: h.line,
                local_name: h.local_name.as_deref(),
            })
            .collect(),
    };
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

pub fn render_cycles_json(graph: &DepGraph, cycles: &[Cycle], pretty: bool) -> String {
    #[derive(Serialize)]
    struct Doc {
        schema: &'static str,
        cycles: Vec<JsonCycle>,
    }
    #[derive(Serialize)]
    struct JsonCycle {
        size: usize,
        members: Vec<String>,
    }
    let doc = Doc {
        schema: JSON_SCHEMA_CYCLES,
        cycles: cycles
            .iter()
            .map(|c| JsonCycle {
                size: c.members.len(),
                members: c.members.iter().map(|p| graph.rel(p)).collect(),
            })
            .collect(),
    };
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

pub fn render_graph_json(graph: &DepGraph, include_external: bool, pretty: bool) -> String {
    #[derive(Serialize)]
    struct Doc<'a> {
        schema: &'static str,
        file_count: usize,
        edge_count: usize,
        edges: Vec<JsonEdge<'a>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        external: Vec<JsonExternal>,
    }
    #[derive(Serialize)]
    struct JsonEdge<'a> {
        from: String,
        to: String,
        kind: &'a str,
        line: u32,
    }
    #[derive(Serialize)]
    struct JsonExternal {
        from: String,
        spec: String,
    }
    let mut edges = Vec::new();
    let sorted = graph.sorted_edges();
    for (s, t, k) in &sorted {
        // Find the edge to grab its line.
        let line = graph
            .forward
            .get(s)
            .and_then(|es| es.iter().find(|e| e.target == *t))
            .map(|e| e.line)
            .unwrap_or(0);
        edges.push(JsonEdge {
            from: graph.rel(s),
            to: graph.rel(t),
            kind: k.label(),
            line,
        });
    }
    let mut external = Vec::new();
    if include_external {
        let mut keys: Vec<_> = graph.external.keys().collect();
        keys.sort();
        for k in keys {
            for spec in &graph.external[k] {
                external.push(JsonExternal {
                    from: graph.rel(k),
                    spec: spec.clone(),
                });
            }
        }
    }
    let doc = Doc {
        schema: JSON_SCHEMA_GRAPH,
        file_count: graph.stats.file_count,
        edge_count: edges.len(),
        edges,
        external,
    };
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

// Suppress warnings for fields that are filled but not yet read by every renderer.
#[allow(dead_code)]
fn _touch(_e: &DepEdge) {}
