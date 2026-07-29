//! Text + JSON renderers for `callers` and `callees`.
//!
//! Colour palette mirrors `src/core.rs` and `src/surface/render.rs`:
//!   - file paths   → cyan + bold
//!   - line numbers → dimmed grey (truecolor 150,150,150 — same as `lines_suffix`)
//!   - symbol qns   → yellow (terminal name) on a dimmed scope prefix
//!   - confidence   → green=Exact, yellow=Inferred, red=Ambiguous
//!   - external/unresolved tags → cyan/red
//!   - `colored` honours `NO_COLOR=1` automatically so the e2e tests stay
//!   - whitespace-stable.

use crate::calls::graph::{CallEdge, CallGraph, CallTarget, Confidence, Qn};
use crate::calls::traverse::CallHit;
use crate::core::{JSON_SCHEMA_CALLEES, JSON_SCHEMA_CALLERS};
use colored::Colorize;
use serde::Serialize;
use serde_json::json;

/// Truncation facts for a rendered list (issue #32): `total` is the true
/// pre-`--limit` count, `frontier_truncated` says the depth cap stopped
/// the walk while edges remained.
pub struct Truncation {
    pub total: usize,
    pub frontier_truncated: bool,
}

impl Truncation {
    /// `" (showing N; raise --limit to see the rest)"` when the display
    /// was cut, empty otherwise.
    fn header_suffix(&self, shown: usize) -> String {
        section_suffix(self.total, shown)
    }
}

/// Shared "(showing N; …)" suffix for any section whose display count fell
/// short of its true total.
fn section_suffix(total: usize, shown: usize) -> String {
    if total > shown {
        format!(" (showing {}; raise --limit to see the rest)", shown)
    } else {
        String::new()
    }
}

pub fn render_callers_text(
    target: &str,
    hits: &[CallHit],
    unattributed: &[CallEdge],
    unattributed_total: usize,
    trunc: &Truncation,
) -> String {
    let mut out = String::new();
    out.push_str(&header_line_suffixed(
        "caller",
        trunc.total,
        target,
        &trunc.header_suffix(hits.len()),
    ));
    for h in hits {
        out.push_str(&format_caller_line(h));
        out.push('\n');
    }
    out.push_str(&unattributed_section(target, unattributed, unattributed_total));
    out
}

/// Call sites the resolver could not attribute (bare/ambiguous edges naming
/// the target). Rendered below the resolved hits so a caller count is never
/// silently low (issue #31). The section shares the `--limit` display
/// budget, so its header carries its own true total.
fn unattributed_section(target: &str, edges: &[CallEdge], total: usize) -> String {
    let mut out = String::new();
    if total == 0 {
        return out;
    }
    let suffix = section_suffix(total, edges.len());
    out.push_str(&format!(
        "\n{} {} unresolved call site(s) naming '{}' (receiver not resolvable; possible additional callers){}:\n",
        "##".dimmed(),
        total.to_string().bold(),
        target.yellow(),
        suffix.dimmed(),
    ));
    for e in edges {
        out.push_str(&format!(
            "{}{} {} {}  {}\n",
            colorize_file(&file_str(&e.file)),
            colorize_line(e.line),
            "in".dimmed(),
            colorize_qn(&e.source),
            colorize_confidence(e.confidence),
        ));
    }
    out
}

/// Like `render_callers_text` but interleaves type-aware groups
/// (implementations + constructions) below the call-edge hits. Used when
/// the same target name resolves both as a callable and as a type, or when
/// it's purely a type.
pub fn render_callers_text_extended(
    target: &str,
    hits: &[CallHit],
    type_groups: &[crate::calls::cli::TypeCallersGroup],
    unattributed: &[CallEdge],
    unattributed_total: usize,
    trunc: &Truncation,
) -> String {
    let mut out = String::new();
    // `trunc.total` is the combined true total (callable hits + type
    // groups, pre-truncation) computed by the caller; compare it against
    // the combined shown count so a capped section can never read as
    // complete just because another section inflated `shown`.
    let shown = hits.len()
        + type_groups
            .iter()
            .map(|g| g.implementations.len() + g.constructions.len())
            .sum::<usize>();
    out.push_str(&header_line_suffixed(
        "caller",
        trunc.total,
        target,
        &trunc.header_suffix(shown),
    ));

    for h in hits {
        out.push_str(&format_caller_line(h));
        out.push('\n');
    }

    for g in type_groups {
        // Headers carry the section's true total; a `--limit`-trimmed
        // section says so instead of passing its shortened list off as
        // complete (the header renders even when trimmed to zero rows).
        if g.implementations_total > 0 {
            let suffix = section_suffix(g.implementations_total, g.implementations.len());
            out.push_str(&format!(
                "\n{} {} {} of {}{}:\n",
                "##".dimmed(),
                g.implementations_total.to_string().bold(),
                "implementation(s)",
                format!("{} {}", g.kind, g.target_qn.name()).yellow(),
                suffix.dimmed(),
            ));
            for i in &g.implementations {
                out.push_str(&format!(
                    "{}{} {} {}\n",
                    colorize_file(&file_str(&i.file)),
                    colorize_line(i.line),
                    i.kind.dimmed(),
                    i.qn.name().yellow(),
                ));
            }
        }
        if g.constructions_total > 0 {
            let suffix = section_suffix(g.constructions_total, g.constructions.len());
            out.push_str(&format!(
                "\n{} {} {} of {}{}:\n",
                "##".dimmed(),
                g.constructions_total.to_string().bold(),
                "construction(s)",
                format!("{} {}", g.kind, g.target_qn.name()).yellow(),
                suffix.dimmed(),
            ));
            for e in &g.constructions {
                out.push_str(&format!(
                    "{}{} {} {}  {}\n",
                    colorize_file(&file_str(&e.file)),
                    colorize_line(e.line),
                    "in".dimmed(),
                    colorize_qn(&e.source),
                    colorize_confidence(e.confidence),
                ));
            }
        }
    }
    out.push_str(&unattributed_section(target, unattributed, unattributed_total));
    out
}

/// Text renderer for `callees` that handles both plain function targets
/// and type-target ancestor walks. Falls back to the simple flat output
/// when no type groups are present.
pub fn render_callees_text_extended(
    graph: &CallGraph,
    target: &Qn,
    edges: &[CallEdge],
    type_groups: &[crate::calls::cli::TypeCalleesGroup],
    include_external: bool,
) -> String {
    if type_groups.is_empty() {
        return render_callees_text(graph, target, edges, include_external);
    }

    let mut out = String::new();
    let total_ancestors: usize = type_groups.iter().map(|g| g.ancestors.len()).sum();
    let kept_flat: Vec<&CallEdge> = edges
        .iter()
        .filter(|e| include_external || matches!(e.target, CallTarget::Resolved(_)))
        .collect();

    out.push_str(&header_line(
        "ancestor",
        total_ancestors + kept_flat.len(),
        target.as_str(),
    ));

    // Function-target callee edges (only present when the target name
    // resolved to *both* a callable and a type — rare).
    for e in &kept_flat {
        out.push_str(&format!(
            "{}{} {}  {}\n",
            colorize_file(&file_str(&e.file)),
            colorize_line(e.line),
            colorize_target(&e.target),
            colorize_confidence(e.confidence),
        ));
    }

    for g in type_groups {
        if g.ancestors.is_empty() && (!include_external || g.unresolved_bases.is_empty()) {
            out.push_str(&format!(
                "\n{} {} {} has no ancestors in this project.\n",
                "##".dimmed(),
                g.kind.dimmed(),
                g.target_qn.name().yellow(),
            ));
            continue;
        }

        out.push_str(&format!(
            "\n{} {} ancestor(s) of {}:\n",
            "##".dimmed(),
            g.ancestors.len().to_string().bold(),
            format!("{} {}", g.kind, g.target_qn.name()).yellow(),
        ));

        for a in &g.ancestors {
            let depth_tag = if a.depth > 1 {
                format!(" depth={}", a.depth).dimmed().to_string()
            } else {
                String::new()
            };
            out.push_str(&format!(
                "\n{} {} {} ({}{}{}):\n",
                "###".dimmed(),
                a.kind.dimmed(),
                a.qn.name().yellow(),
                colorize_file(&file_str(&a.file)),
                colorize_line(a.line),
                depth_tag,
            ));
            if a.methods.is_empty() {
                out.push_str(&format!("  {}\n", "(no methods)".dimmed()));
            }
            for m in &a.methods {
                out.push_str(&format!(
                    "{}{} {} {}\n",
                    colorize_file(&file_str(&m.file)),
                    colorize_line(m.line),
                    "fn".dimmed(),
                    m.qn.name().yellow(),
                ));
            }
        }

        if include_external && !g.unresolved_bases.is_empty() {
            out.push_str(&format!(
                "\n{} {} external/unresolved base(s) of {}:\n",
                "##".dimmed(),
                g.unresolved_bases.len().to_string().bold(),
                g.target_qn.name().yellow(),
            ));
            for b in &g.unresolved_bases {
                out.push_str(&format!("{} {}\n", "[external]".cyan(), b.dimmed()));
            }
        }
    }
    out
}

pub fn render_callees_text(
    _graph: &CallGraph,
    target: &Qn,
    edges: &[CallEdge],
    include_external: bool,
) -> String {
    let mut out = String::new();
    let kept: Vec<&CallEdge> = edges
        .iter()
        .filter(|e| include_external || matches!(e.target, CallTarget::Resolved(_)))
        .collect();
    out.push_str(&header_line("callee", kept.len(), target.as_str()));
    for e in kept {
        out.push_str(&format!(
            "{}{} {}  {}\n",
            colorize_file(&file_str(&e.file)),
            colorize_line(e.line),
            colorize_target(&e.target),
            colorize_confidence(e.confidence),
        ));
    }
    out
}

fn format_caller_line(h: &CallHit) -> String {
    let e = &h.edge;
    let depth = format!("depth={}", h.depth).dimmed().to_string();
    format!(
        "{}{} {} {}  {}",
        colorize_file(&file_str(&e.file)),
        colorize_line(e.line),
        depth,
        colorize_qn(&e.source),
        colorize_confidence(e.confidence),
    )
}

fn header_line(label: &str, count: usize, target: &str) -> String {
    header_line_suffixed(label, count, target, "")
}

/// `# 113 caller(s) for 'x' (showing 3; raise --limit to see the rest):`
/// — the suffix carries truncation facts so a capped list is never
/// mistaken for a complete one (issue #32).
fn header_line_suffixed(label: &str, count: usize, target: &str, suffix: &str) -> String {
    format!(
        "{} {} {}(s) for {}{}:\n",
        "#".dimmed(),
        count.to_string().bold(),
        label,
        format!("'{}'", target).yellow(),
        suffix.dimmed(),
    )
}

fn colorize_file(p: &str) -> String {
    p.cyan().bold().to_string()
}

fn colorize_line(line: u32) -> String {
    format!(":{:<5}", line).truecolor(150, 150, 150).to_string()
}

/// `path/to/file::Mod::method` → file dimmed, terminal name yellow,
/// scope segments between in default colour.
fn colorize_qn(qn: &Qn) -> String {
    let s = qn.as_str();
    if let Some(idx) = s.rfind("::") {
        let head = &s[..idx];
        let tail = &s[idx + 2..];
        format!(
            "{}{}{}",
            head.dimmed(),
            "::".dimmed(),
            tail.yellow()
        )
    } else {
        s.yellow().to_string()
    }
}

fn colorize_target(t: &CallTarget) -> String {
    match t {
        CallTarget::Resolved(qn) => colorize_qn(qn),
        CallTarget::External(s) => format!("{} {}", "[external]".cyan(), s.dimmed()),
        CallTarget::Bare(s) => format!("{} {}", "[unresolved]".red(), s.yellow()),
    }
}

fn colorize_confidence(c: Confidence) -> String {
    let label = format!("({})", c.as_str());
    match c {
        Confidence::Exact => label.green().to_string(),
        Confidence::Inferred => label.yellow().to_string(),
        Confidence::Ambiguous => label.red().dimmed().to_string(),
    }
}

fn file_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

#[derive(Serialize)]
struct JsonCaller<'a> {
    source: String,
    target: String,
    kind: &'static str,
    file: String,
    line: u32,
    depth: usize,
    confidence: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<&'a Qn>,
}

/// A bare call site naming the target that the resolver could not attribute
/// to a node. `callee` is the raw name as written; `candidates` are the qns
/// pass C could not choose between (empty when none were found at all).
#[derive(Serialize)]
struct JsonUnattributed<'a> {
    source: String,
    callee: String,
    kind: &'static str,
    file: String,
    line: u32,
    confidence: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<&'a Qn>,
}

fn json_unattributed(edges: &[CallEdge]) -> Vec<JsonUnattributed<'_>> {
    edges
        .iter()
        .map(|e| JsonUnattributed {
            source: e.source.to_string(),
            callee: e.target.name_or_raw(),
            kind: e.kind.as_str(),
            file: file_str(&e.file),
            line: e.line,
            confidence: e.confidence.as_str(),
            candidates: e.candidates.iter().collect(),
        })
        .collect()
}

#[derive(Serialize)]
struct JsonCallee<'a> {
    source: String,
    target: String,
    kind: &'static str,
    file: String,
    line: u32,
    depth: Option<usize>,
    confidence: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<&'a Qn>,
}

#[allow(clippy::too_many_arguments)] // one arg per orthogonal output facet
pub fn render_callers_json_extended(
    target: &str,
    depth: usize,
    hits: &[CallHit],
    type_groups: &[crate::calls::cli::TypeCallersGroup],
    unattributed: &[CallEdge],
    unattributed_total: usize,
    unattributed_hidden: usize,
    trunc: &Truncation,
    pretty: bool,
) -> String {
    let matches: Vec<JsonCaller> = hits
        .iter()
        .map(|h| JsonCaller {
            source: h.edge.source.to_string(),
            target: h.edge.target.display(),
            kind: h.edge.kind.as_str(),
            file: file_str(&h.edge.file),
            line: h.edge.line,
            depth: h.depth,
            confidence: h.edge.confidence.as_str(),
            candidates: h.edge.candidates.iter().collect(),
        })
        .collect();

    let type_views: Vec<serde_json::Value> = type_groups
        .iter()
        .map(|g| {
            let impls: Vec<serde_json::Value> = g
                .implementations
                .iter()
                .map(|i| {
                    json!({
                        "qn": i.qn.as_str(),
                        "kind": i.kind,
                        "file": file_str(&i.file),
                        "line": i.line,
                    })
                })
                .collect();
            let ctors: Vec<serde_json::Value> = g
                .constructions
                .iter()
                .map(|e| {
                    json!({
                        "source": e.source.to_string(),
                        "target": e.target.display(),
                        "file": file_str(&e.file),
                        "line": e.line,
                        "kind": e.kind.as_str(),
                        "confidence": e.confidence.as_str(),
                    })
                })
                .collect();
            json!({
                "qn": g.target_qn.as_str(),
                "kind": g.kind,
                "implementations": impls,
                "constructions": ctors,
                "implementations_total": g.implementations_total,
                "constructions_total": g.constructions_total,
                "truncated": g.implementations_total > g.implementations.len()
                    || g.constructions_total > g.constructions.len(),
            })
        })
        .collect();

    let doc = json!({
        "schema": JSON_SCHEMA_CALLERS,
        "target": target,
        "depth": depth,
        "total": trunc.total,
        // Combined-shown vs combined-total, matching the text header: type
        // groups count toward both sides, so a truncated callable list is
        // reported even when groups pad the shown count (PR #38 review).
        "truncated": trunc.total > hits.len()
            + type_groups
                .iter()
                .map(|g| g.implementations.len() + g.constructions.len())
                .sum::<usize>(),
        "frontier_truncated": trunc.frontier_truncated,
        "matches": matches,
        "types": type_views,
        "unattributed": json_unattributed(unattributed),
        "unattributed_total": unattributed_total,
        "unattributed_truncated": unattributed_total > unattributed.len(),
        // Sites dropped by hide_ambiguous — kept in the body because JSON
        // consumers (MCP especially) have no stderr to read the note from.
        "unattributed_hidden": unattributed_hidden,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)] // one arg per orthogonal output facet
pub fn render_callers_json(
    target: &str,
    depth: usize,
    hits: &[CallHit],
    unattributed: &[CallEdge],
    unattributed_total: usize,
    unattributed_hidden: usize,
    trunc: &Truncation,
    pretty: bool,
) -> String {
    let matches: Vec<JsonCaller> = hits
        .iter()
        .map(|h| JsonCaller {
            source: h.edge.source.to_string(),
            target: h.edge.target.display(),
            kind: h.edge.kind.as_str(),
            file: file_str(&h.edge.file),
            line: h.edge.line,
            depth: h.depth,
            confidence: h.edge.confidence.as_str(),
            candidates: h.edge.candidates.iter().collect(),
        })
        .collect();
    let doc = json!({
        "schema": JSON_SCHEMA_CALLERS,
        "target": target,
        "depth": depth,
        "total": trunc.total,
        "truncated": trunc.total > hits.len(),
        "frontier_truncated": trunc.frontier_truncated,
        "matches": matches,
        "unattributed": json_unattributed(unattributed),
        "unattributed_total": unattributed_total,
        "unattributed_truncated": unattributed_total > unattributed.len(),
        "unattributed_hidden": unattributed_hidden,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

pub fn render_callees_json_extended(
    target: &Qn,
    depth: usize,
    edges: &[CallEdge],
    type_groups: &[crate::calls::cli::TypeCalleesGroup],
    total: usize,
    frontier_truncated: bool,
    pretty: bool,
) -> String {
    let matches: Vec<JsonCallee> = edges
        .iter()
        .map(|e| JsonCallee {
            source: e.source.to_string(),
            target: e.target.display(),
            kind: e.kind.as_str(),
            file: file_str(&e.file),
            line: e.line,
            depth: None,
            confidence: e.confidence.as_str(),
            candidates: e.candidates.iter().collect(),
        })
        .collect();

    let type_views: Vec<serde_json::Value> = type_groups
        .iter()
        .map(|g| {
            let ancestors: Vec<serde_json::Value> = g
                .ancestors
                .iter()
                .map(|a| {
                    let methods: Vec<serde_json::Value> = a
                        .methods
                        .iter()
                        .map(|m| {
                            json!({
                                "qn": m.qn.as_str(),
                                "file": file_str(&m.file),
                                "line": m.line,
                            })
                        })
                        .collect();
                    json!({
                        "qn": a.qn.as_str(),
                        "kind": a.kind,
                        "depth": a.depth,
                        "file": file_str(&a.file),
                        "line": a.line,
                        "methods": methods,
                    })
                })
                .collect();
            json!({
                "qn": g.target_qn.as_str(),
                "kind": g.kind,
                "ancestors": ancestors,
                "unresolved_bases": g.unresolved_bases,
            })
        })
        .collect();

    let doc = json!({
        "schema": JSON_SCHEMA_CALLEES,
        "target": target.as_str(),
        "depth": depth,
        "total": total,
        "truncated": total > edges.len(),
        "frontier_truncated": frontier_truncated,
        "matches": matches,
        "types": type_views,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

pub fn render_callees_json(target: &Qn, depth: usize, edges: &[CallEdge], pretty: bool) -> String {
    let matches: Vec<JsonCallee> = edges
        .iter()
        .map(|e| JsonCallee {
            source: e.source.to_string(),
            target: e.target.display(),
            kind: e.kind.as_str(),
            file: file_str(&e.file),
            line: e.line,
            depth: None,
            confidence: e.confidence.as_str(),
            candidates: e.candidates.iter().collect(),
        })
        .collect();
    let doc = json!({
        "schema": JSON_SCHEMA_CALLEES,
        "target": target.as_str(),
        "depth": depth,
        "matches": matches,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}
