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

pub fn render_deps_text(graph: &DepGraph, start: &Path, hits: &[DepHit]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", graph.rel(start).cyan().bold());
    if hits.is_empty() && graph.external.get(start).map_or(true, |v| v.is_empty()) {
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
    out
}

pub fn render_reverse_deps_text(graph: &DepGraph, start: &Path, hits: &[DepHit]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {}",
        graph.rel(start).cyan().bold(),
        "← imported by:".dimmed()
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

pub fn render_graph_text(graph: &DepGraph) -> String {
    let mut out = String::new();
    let edges = graph.sorted_edges();
    let _ = writeln!(
        out,
        "{}",
        format!("{} files, {} edges", graph.stats.file_count, edges.len())
            .dimmed()
    );
    let mut grouped: BTreeMap<String, Vec<(String, ImportKind)>> = BTreeMap::new();
    for (s, t, k) in edges {
        grouped
            .entry(graph.rel(&s))
            .or_default()
            .push((graph.rel(&t), k));
    }
    for (s, ts) in grouped {
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", s.cyan().bold());
        for (t, k) in ts {
            let _ = writeln!(
                out,
                "  {} {} {}",
                "→".dimmed(),
                t.green(),
                format!("({})", k.label()).dimmed()
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

pub fn render_deps_json(graph: &DepGraph, start: &Path, hits: &[DepHit], pretty: bool) -> String {
    let external = graph
        .external
        .get(start)
        .cloned()
        .unwrap_or_default();
    let doc = DepsDoc {
        schema: JSON_SCHEMA_DEPS,
        file: graph.rel(start),
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
    pretty: bool,
) -> String {
    #[derive(Serialize)]
    struct Doc<'a> {
        schema: &'static str,
        file: String,
        importers: Vec<JsonHit<'a>>,
    }
    let doc = Doc {
        schema: JSON_SCHEMA_REVERSE_DEPS,
        file: graph.rel(start),
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
