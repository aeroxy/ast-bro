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

/// Everything the "unresolved call sites" section needs to describe itself
/// honestly (issue #31, PR #38 review): the rows to show, the true total
/// behind them, how many were dropped by `--hide-ambiguous`, and how many
/// project symbols declare the target's terminal name — the last being what
/// decides whether a list of these sites is evidence about *this* symbol or
/// about every symbol sharing its name.
pub struct Unattributed<'a> {
    pub edges: &'a [CallEdge],
    pub total: usize,
    pub hidden: usize,
    pub declarers: usize,
}

impl Unattributed<'_> {
    /// Does the target's terminal name still say something about *this*
    /// symbol? A name a handful of symbols declare does: the sites are worth
    /// triaging, and the ones that aren't callers are cheap to dismiss.
    /// A name many symbols declare does not — the list is then evidence
    /// about all of them and, worse, reads as this symbol's rename cost.
    /// Measured against the review's numbers: `registry` (2 declarers, 5
    /// sites, all real) and `getOid` (3, 112) stay listed, `new` in this repo
    /// (9, 1020 — every `Vec::new()` in the tree) and `close` in pgjdbc
    /// (39, 1124) collapse to a one-line count.
    fn discriminating(&self) -> bool {
        self.declarers <= MAX_DECLARERS_TO_LIST
    }
}

/// See `Unattributed::discriminating`.
pub const MAX_DECLARERS_TO_LIST: usize = 3;

pub fn render_callers_text(
    target: &str,
    hits: &[CallHit],
    unattributed: &Unattributed<'_>,
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
    out.push_str(&unattributed_section(target, unattributed));
    out
}

/// Call sites the resolver could not attribute (bare/ambiguous edges naming
/// the target). Rendered below the resolved hits so a caller count is never
/// silently low (issue #31), with its own display cap independent of
/// `--limit` so a badly-resolved target does not earn more screen than a
/// well-resolved one.
fn unattributed_section(target: &str, u: &Unattributed<'_>) -> String {
    let mut out = String::new();
    if u.total == 0 {
        return out;
    }
    if !u.discriminating() {
        // No list: with N declarers these sites are evidence about all N.
        // The reason travels with the count so the number can't be quoted
        // as this symbol's rename cost.
        out.push_str(&format!(
            "\n{} {} unresolved call site(s) name '{}' — receiver type unknown, and '{}' is declared by {} symbols in this project, so these sites are evidence about all of them, not this one (not listed)\n",
            "##".dimmed(),
            u.total.to_string().bold(),
            target.yellow(),
            terminal_name(target),
            u.declarers,
        ));
        return out;
    }
    let suffix = sample_suffix(u.total, u.edges.len());
    out.push_str(&format!(
        "\n{} {} unresolved call site(s) naming '{}' (receiver not resolvable; possible additional callers){}:\n",
        "##".dimmed(),
        u.total.to_string().bold(),
        target.yellow(),
        suffix.dimmed(),
    ));
    for e in u.edges {
        // The receiver as written is the one triage signal available with no
        // type inference: `os.close()` and `connection.close()` are the same
        // edge to the resolver and obviously different to a reader.
        let recv = e
            .receiver
            .as_deref()
            .map(|r| format!(" {}", format!("recv={}", r).dimmed()))
            .unwrap_or_default();
        out.push_str(&format!(
            "{}{} {} {}{}  {}\n",
            colorize_file(&file_str(&e.file)),
            colorize_line(e.line),
            "in".dimmed(),
            colorize_qn(&e.source),
            recv,
            colorize_confidence(e.confidence),
        ));
    }
    out
}

/// Suffix for the unattributed sample, whose cap is deliberately *not*
/// `--limit`: raising `--limit` widens the resolved list, not this sample,
/// so the text must not promise otherwise.
fn sample_suffix(total: usize, shown: usize) -> String {
    if total > shown {
        format!(" (showing the {} narrowest)", shown)
    } else {
        String::new()
    }
}

/// Last segment of a user-supplied target (`Type.method` → `method`), for
/// messages that talk about the name rather than the symbol.
fn terminal_name(target: &str) -> &str {
    target.rsplit(['.', ':']).next().unwrap_or(target)
}

/// Like `render_callers_text` but interleaves type-aware groups
/// (implementations + constructions) below the call-edge hits. Used when
/// the same target name resolves both as a callable and as a type, or when
/// it's purely a type.
pub fn render_callers_text_extended(
    target: &str,
    hits: &[CallHit],
    type_groups: &[crate::calls::cli::TypeCallersGroup],
    unattributed: &Unattributed<'_>,
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
    out.push_str(&unattributed_section(target, unattributed));
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
    total: usize,
    shown: usize,
    include_external: bool,
) -> String {
    if type_groups.is_empty() {
        return render_callees_text(graph, target, edges, total, include_external);
    }

    let mut out = String::new();
    // True total (flat edges + ancestor groups, pre-`--limit`) with the
    // truncation suffix, so stdout alone says whether anything was cut — a
    // reader who never sees stderr must not read a capped list as complete
    // (issue #32).
    out.push_str(&header_line_suffixed(
        "ancestor",
        total,
        target.as_str(),
        &section_suffix(total, shown),
    ));

    // Function-target callee edges (only present when the target name
    // resolved to *both* a callable and a type — rare).
    for e in edges {
        out.push_str(&format!(
            "{}{} {}  {}\n",
            colorize_file(&file_str(&e.file)),
            colorize_line(e.line),
            colorize_target(&e.target),
            colorize_confidence(e.confidence),
        ));
    }

    for g in type_groups {
        if g.ancestors_total == 0 && (!include_external || g.unresolved_bases.is_empty()) {
            out.push_str(&format!(
                "\n{} {} {} has no ancestors in this project.\n",
                "##".dimmed(),
                g.kind.dimmed(),
                g.target_qn.name().yellow(),
            ));
            continue;
        }

        out.push_str(&format!(
            "\n{} {} ancestor(s) of {}{}:\n",
            "##".dimmed(),
            g.ancestors_total.to_string().bold(),
            format!("{} {}", g.kind, g.target_qn.name()).yellow(),
            section_suffix(g.ancestors_total, g.ancestors.len()).dimmed(),
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
    total: usize,
    include_external: bool,
) -> String {
    let mut out = String::new();
    // The `include_external` filter is a safeguard for callers that hand over
    // an unfiltered list (MCP); the CLI applies it before counting so
    // `total`, the header and the stderr note all describe one population.
    let kept: Vec<&CallEdge> = edges
        .iter()
        .filter(|e| include_external || matches!(e.target, CallTarget::Resolved(_)))
        .collect();
    out.push_str(&header_line_suffixed(
        "callee",
        total,
        target.as_str(),
        &section_suffix(total, kept.len()),
    ));
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
    /// Receiver as written at the call site — the same triage signal the
    /// text rows carry, so a JSON consumer can tell `os.close()` from
    /// `connection.close()` without re-reading the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    receiver: Option<&'a str>,
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
            receiver: e.receiver.as_deref(),
            candidates: e.candidates.iter().collect(),
        })
        .collect()
}

/// The unattributed facts as JSON fields, shared by both `callers` payload
/// shapes. `unattributed_declarers` plus `unattributed_suppressed` are what
/// a machine consumer needs to avoid reading a big `unattributed_total` as a
/// caller count: with N declarers the total is spread across all N, and the
/// rows were withheld for that reason.
fn json_unattributed_fields(u: &Unattributed<'_>) -> Vec<(String, serde_json::Value)> {
    vec![
        (
            "unattributed".to_string(),
            serde_json::to_value(json_unattributed(u.edges)).unwrap_or_default(),
        ),
        ("unattributed_total".to_string(), json!(u.total)),
        (
            "unattributed_truncated".to_string(),
            json!(u.total > u.edges.len()),
        ),
        // Sites dropped by hide_ambiguous — kept in the body because JSON
        // consumers (MCP especially) have no stderr to read the note from.
        ("unattributed_hidden".to_string(), json!(u.hidden)),
        ("unattributed_declarers".to_string(), json!(u.declarers)),
        (
            "unattributed_suppressed".to_string(),
            json!(u.total > 0 && !u.discriminating()),
        ),
    ]
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
    unattributed: &Unattributed<'_>,
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

    let mut doc = json!({
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
    });
    for (k, v) in json_unattributed_fields(unattributed) {
        doc[k] = v;
    }
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

pub fn render_callers_json(
    target: &str,
    depth: usize,
    hits: &[CallHit],
    unattributed: &Unattributed<'_>,
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
    let mut doc = json!({
        "schema": JSON_SCHEMA_CALLERS,
        "target": target,
        "depth": depth,
        "total": trunc.total,
        "truncated": trunc.total > hits.len(),
        "frontier_truncated": trunc.frontier_truncated,
        "matches": matches,
    });
    for (k, v) in json_unattributed_fields(unattributed) {
        doc[k] = v;
    }
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)] // one arg per orthogonal output facet
pub fn render_callees_json_extended(
    target: &Qn,
    depth: usize,
    edges: &[CallEdge],
    type_groups: &[crate::calls::cli::TypeCalleesGroup],
    total: usize,
    shown: usize,
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
                "ancestors_total": g.ancestors_total,
                "truncated": g.ancestors_total > g.ancestors.len(),
                "unresolved_bases": g.unresolved_bases,
            })
        })
        .collect();

    let doc = json!({
        "schema": JSON_SCHEMA_CALLEES,
        "target": target.as_str(),
        "depth": depth,
        "total": total,
        // Combined-shown vs combined-total: ancestor groups count toward
        // both sides, so a capped group is reported even when the flat edge
        // list fit (PR #38 review).
        "truncated": total > shown,
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
