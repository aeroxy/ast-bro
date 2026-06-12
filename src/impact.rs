//! `impact` — cross-file impact analysis for a symbol.
//!
//! Wraps `callers`, `callees`, file-level `reverse-deps`, and test-detection
//! into one output so an agent sees the blast radius in a single command.
//!
//! Four output modes: `Deps` (what it calls/imports), `Dependents` (who
//! calls/imports it), `Tests` (affected tests only), `All` (default — all
//! three sections plus transitive depth grouping).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use colored::Colorize;
use serde::Serialize;
use serde_json::json;

use crate::calls::build::build_call_graph;
use crate::calls::cli_helpers::{resolve_target_full, ResolvedTarget, SymbolKind};
use crate::calls::graph::{CallEdge, CallGraph, CallTarget, Confidence};
use crate::calls::traverse::{self, CallHit};
use crate::deps::traverse as dep_traverse;
use crate::deps::DepGraph;
use crate::file_filter::is_test_file;
use crate::graph_cache::{self, UnifiedGraph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactMode {
    Deps,
    Dependents,
    Tests,
    All,
}

impl ImpactMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "deps" | "depend" | "dependencies" => Some(Self::Deps),
            "dependents" | "dependent" | "reverse" | "callers" => Some(Self::Dependents),
            "tests" | "test" => Some(Self::Tests),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

pub struct ImpactOptions {
    pub depth: usize,
    pub limit: usize,
    pub mode: ImpactMode,
    pub include_ambiguous: bool,
    pub tests: bool,
    pub exclude_tests: bool,
    pub json: bool,
    pub pretty: bool,
}

fn ensure_graph(
    root: &Path,
    force_rebuild: bool,
) -> std::io::Result<Arc<UnifiedGraph>> {
    let unified = if force_rebuild {
        graph_cache::shared::rebuild(root)?
    } else {
        graph_cache::get_or_init(root)?
    };
    if unified.calls.is_some() {
        return Ok(unified);
    }
    graph_cache::promote_calls(root, |g| build_call_graph(root, &g.deps))
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactSection {
    pub title: String,
    pub entries: Vec<ImpactEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactEntry {
    pub qn: String,
    pub file: String,
    pub line: u32,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub target: String,
    pub target_qn: String,
    pub target_file: String,
    pub target_line: u32,
    pub target_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<ImpactSection>>,
    pub transitive_count: usize,
    pub test_count: usize,
}

pub fn run_impact(
    target: &str,
    path: &Path,
    opts: &ImpactOptions,
    rebuild: bool,
) -> i32 {
    let root = match crate::project_root::find_root_for(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("# note: {}", e);
            return 2;
        }
    };
    let graph = match ensure_graph(&root, rebuild) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("# note: {}", e);
            return 1;
        }
    };
    let calls = match &graph.calls {
        Some(c) => c,
        None => {
            eprintln!("# note: call graph is empty");
            return 1;
        }
    };
    let candidates = resolve_target_full(calls, target);
    if candidates.is_empty() {
        eprintln!(
            "# note: no symbol matches '{}' (try a more specific suffix like 'Type.method').",
            target
        );
        return 2;
    }

    let mut reports = Vec::new();
    for c in &candidates {
        reports.push(compute_impact(c, calls, &graph.deps, &root, opts));
    }

    if opts.json {
        println!(
            "{}",
            render_json(target, &reports, opts.pretty, candidates.len())
        );
    } else {
        print!("{}", render_text(target, &reports, opts, candidates.len()));
    }
    0
}

fn compute_impact(
    c: &ResolvedTarget,
    calls: &CallGraph,
    deps: &DepGraph,
    root: &Path,
    opts: &ImpactOptions,
) -> ImpactReport {
    let (file_path, target_line, target_kind) = match c.kind {
        SymbolKind::Callable => {
            let meta = calls.callable_meta.get(&c.qn);
            let file = meta.map(|m| m.file.clone()).unwrap_or_else(|| PathBuf::from(c.qn.file()));
            let line = meta.map(|m| m.line).unwrap_or(0);
            let kind = meta.map(|m| m.kind.as_str()).unwrap_or("function").to_string();
            (file, line, kind)
        }
        SymbolKind::Type => {
            let tmeta = calls.types.get(&c.qn);
            let file = tmeta.map(|m| m.file.clone()).unwrap_or_else(|| PathBuf::from(c.qn.file()));
            let line = tmeta.map(|m| m.line).unwrap_or(0);
            let kind = tmeta.map(|m| m.kind.as_str()).unwrap_or("type").to_string();
            (file, line, kind)
        }
    };

    let mut report = ImpactReport {
        target: c.qn.as_str().to_string(),
        target_qn: c.qn.as_str().to_string(),
        target_file: crate::project_root::relative_posix(&file_path, root)
            .unwrap_or_else(|| file_path.display().to_string()),
        target_line,
        target_kind: target_kind.clone(),
        sections: Some(Vec::new()),
        transitive_count: 0,
        test_count: 0,
    };

    let sections = report.sections.as_mut().unwrap();

    if matches!(opts.mode, ImpactMode::Deps | ImpactMode::All) {
        sections.push(build_callees_section(c, calls, opts));
        sections.push(build_file_deps_section(&file_path, deps, root));
    }

    if matches!(opts.mode, ImpactMode::Dependents | ImpactMode::All) {
        sections.push(build_callers_section(c, calls, opts, root));
        sections.push(build_file_reverse_deps_section(&file_path, deps, root, opts));
    }

    let mut transitive = BTreeMap::new();
    let mut test_calls: Vec<CallHit> = Vec::new();

    let all_callers = traverse::callers(calls, &c.qn, opts.depth.max(1), opts.limit);
    for h in &all_callers {
        let abs = root.join(&h.edge.file);
        let is_test = is_test_file(&abs, root);
        if is_test && !opts.exclude_tests {
            test_calls.push(h.clone());
        }
        if h.depth > 1 {
            if opts.exclude_tests && is_test {
                continue;
            }
            if opts.tests && !is_test {
                continue;
            }
            let entry = ImpactEntry {
                qn: h.edge.source.as_str().to_string(),
                file: h.edge.file.display().to_string(),
                line: h.edge.line,
                kind: if h.edge.kind == crate::calls::graph::CallKindCompat::Implement {
                    calls
                        .types
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "type".into())
                } else {
                    calls
                        .callable_meta
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "function".into())
                },
                confidence: Some(h.edge.confidence.as_str().to_string()),
                depth: Some(h.depth),
            };
            transitive
                .entry(h.depth)
                .or_insert_with(Vec::new)
                .push(entry);
        }
    }

    if c.kind == SymbolKind::Type {
        if let Some(impls) = calls.implementors.get(c.qn.name()) {
            for qn in impls {
                let meta = calls.types.get(qn);
                let file = meta.map(|m| m.file.clone()).unwrap_or_else(|| PathBuf::from(qn.file()));
                let line = meta.map(|m| m.line).unwrap_or(0);
                let abs = root.join(&file);
                let is_test = is_test_file(&abs, root);
                if is_test && !opts.exclude_tests {
                    test_calls.push(CallHit {
                        depth: 1,
                        edge: CallEdge {
                            source: qn.clone(),
                            target: CallTarget::Resolved(c.qn.clone()),
                            kind: crate::calls::graph::CallKindCompat::Implement,
                            line,
                            file: file.clone(),
                            confidence: Confidence::Exact,
                            receiver: None,
                            candidates: Vec::new(),
                        },
                    });
                }
                // Implementors (depth 1) are shown in Section 1 (build_callers_section).
                // We don't add them to the transitive map to avoid duplication.

                // Transitive dependents: anyone who calls the implementor is a 
                // dependent of the base type at depth 2+.
                if opts.depth > 1 {
                    let sub_callers = traverse::callers(calls, qn, opts.depth - 1, opts.limit);
                    for h in sub_callers {
                        let total_depth = h.depth + 1;
                        let abs = root.join(&h.edge.file);
                        let is_test = is_test_file(&abs, root);
                        if is_test && !opts.exclude_tests {
                            let mut h2 = h.clone();
                            h2.depth = total_depth;
                            test_calls.push(h2);
                        }
                        if opts.exclude_tests && is_test {
                            continue;
                        }
                        if opts.tests && !is_test {
                            continue;
                        }
                        let entry = ImpactEntry {
                            qn: h.edge.source.as_str().to_string(),
                            file: h.edge.file.display().to_string(),
                            line: h.edge.line,
                            kind: calls
                                .callable_meta
                                .get(&h.edge.source)
                                .map(|m| m.kind.clone())
                                .unwrap_or_else(|| "function".into()),
                            confidence: Some(h.edge.confidence.as_str().to_string()),
                            depth: Some(total_depth),
                        };
                        transitive
                            .entry(total_depth)
                            .or_insert_with(Vec::new)
                            .push(entry);
                    }
                }
            }
        }
    }

    if matches!(opts.mode, ImpactMode::All) && !transitive.is_empty() {
        let total: usize = transitive.values().map(|v| v.len()).sum();
        report.transitive_count = total;
        let mut section = ImpactSection {
            title: format!("! {} entities transitively affected (depth {})", total, opts.depth),
            entries: Vec::new(),
        };
        for (depth, entries) in &transitive {
            for e in entries {
                let mut e = e.clone();
                e.depth = Some(*depth);
                section.entries.push(e);
            }
        }
        sections.push(section);
    }

    if opts.tests || matches!(opts.mode, ImpactMode::Tests | ImpactMode::All) {
        let excluded = opts.exclude_tests;
        let count = test_calls.len();
        report.test_count = count;
        let (display, entries) = if excluded {
            (
                "affected tests (0, excluded by --exclude-tests)".to_string(),
                Vec::new(),
            )
        } else {
            (
                format!("affected tests ({})", count),
                test_calls
                    .iter()
                    .map(|h| ImpactEntry {
                        qn: h.edge.source.as_str().to_string(),
                        file: h.edge.file.display().to_string(),
                        line: h.edge.line,
                        kind: calls
                            .callable_meta
                            .get(&h.edge.source)
                            .map(|m| m.kind.clone())
                            .unwrap_or_else(|| "function".into()),
                        confidence: Some(h.edge.confidence.as_str().to_string()),
                        depth: Some(h.depth),
                    })
                    .collect(),
            )
        };
        sections.push(ImpactSection { title: display, entries });
    }

    report
}

fn build_callees_section(
    c: &ResolvedTarget,
    calls: &CallGraph,
    opts: &ImpactOptions,
) -> ImpactSection {
    let mut edges: Vec<CallEdge> = Vec::new();
    if c.kind == SymbolKind::Callable {
        let one_hop = traverse::callees_one_hop(calls, &c.qn);
        for e in one_hop {
            if !opts.include_ambiguous
                && matches!(e.confidence, Confidence::Ambiguous)
            {
                continue;
            }
            edges.push(e);
        }
    }
    ImpactSection {
        title: format!("→ calls ({})", edges.len()),
        entries: edges
            .into_iter()
            .map(|e| {
                let (qn, file, line) = match &e.target {
                    CallTarget::Resolved(q) => {
                        let meta = calls.callable_meta.get(q);
                        (
                            q.as_str().to_string(),
                            meta.map(|m| m.file.clone()).unwrap_or_else(|| e.file.clone()),
                            meta.map(|m| m.line).unwrap_or(e.line),
                        )
                    }
                    CallTarget::External(s) => {
                        (format!("[external] {s}"), e.file.clone(), e.line)
                    }
                    CallTarget::Bare(s) => {
                        (format!("[unresolved] {s}"), e.file.clone(), e.line)
                    }
                };
                ImpactEntry {
                    qn,
                    file: file.display().to_string(),
                    line,
                    kind: e.kind.as_str().to_string(),
                    confidence: Some(e.confidence.as_str().to_string()),
                    depth: None,
                }
            })
            .collect(),
    }
}

fn build_callers_section(
    c: &ResolvedTarget,
    calls: &CallGraph,
    opts: &ImpactOptions,
    root: &Path,
) -> ImpactSection {
    let mut hits = traverse::callers(calls, &c.qn, 1, opts.limit);
    if c.kind == SymbolKind::Type {
        if let Some(impls) = calls.implementors.get(c.qn.name()) {
            for qn in impls {
                if let Some(meta) = calls.types.get(qn) {
                    hits.push(CallHit {
                        depth: 1,
                        edge: CallEdge {
                            source: qn.clone(),
                            target: CallTarget::Resolved(c.qn.clone()),
                            kind: crate::calls::graph::CallKindCompat::Implement,
                            line: meta.line,
                            file: meta.file.clone(),
                            confidence: Confidence::Exact,
                            receiver: None,
                            candidates: Vec::new(),
                        },
                    });
                }
            }
        }
    }
    if !opts.include_ambiguous {
        hits.retain(|h| !matches!(h.edge.confidence, Confidence::Ambiguous));
    }
    hits.retain(|h| {
        let abs = root.join(&h.edge.file);
        let is_test = is_test_file(&abs, root);
        if opts.exclude_tests { !is_test }
        else if opts.tests { is_test }
        else { true }
    });
    ImpactSection {
        title: if c.kind == SymbolKind::Type {
            format!("← implemented / called by ({})", hits.len())
        } else {
            format!("← called by ({})", hits.len())
        },
        entries: hits
            .into_iter()
            .map(|h| ImpactEntry {
                qn: h.edge.source.as_str().to_string(),
                file: h.edge.file.display().to_string(),
                line: h.edge.line,
                kind: if h.edge.kind == crate::calls::graph::CallKindCompat::Implement {
                    calls
                        .types
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "type".into())
                } else {
                    calls
                        .callable_meta
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "function".into())
                },
                confidence: Some(h.edge.confidence.as_str().to_string()),
                depth: None,
            })
            .collect(),
    }
}

fn build_file_deps_section(
    file: &Path,
    deps: &DepGraph,
    root: &Path,
) -> ImpactSection {
    let deps_file = root.join(file);
    let hits = dep_traverse::forward(deps, &deps_file, 1);
    ImpactSection {
        title: format!("→ imports (file, {})", hits.len()),
        entries: hits
            .into_iter()
            .map(|h| {
                let rel = crate::project_root::relative_posix(&h.file, root)
                    .unwrap_or_else(|| h.file.display().to_string());
                ImpactEntry {
                    qn: rel,
                    file: h.file.display().to_string(),
                    line: h.line,
                    kind: format!("{:?}", h.kind).to_lowercase(),
                    confidence: None,
                    depth: None,
                }
            })
            .collect(),
    }
}

fn build_file_reverse_deps_section(
    file: &Path,
    deps: &DepGraph,
    root: &Path,
    opts: &ImpactOptions,
) -> ImpactSection {
    let deps_file = root.join(file);
    let hits = dep_traverse::reverse(deps, &deps_file, 1, opts.limit);
    let hits: Vec<_> = hits
        .into_iter()
        .filter(|h| {
            if opts.exclude_tests {
                !is_test_file(&h.file, root)
            } else if opts.tests {
                is_test_file(&h.file, root)
            } else {
                true
            }
        })
        .collect();
    ImpactSection {
        title: format!("← imported by (file, {})", hits.len()),
        entries: hits
            .into_iter()
            .map(|h| {
                let rel = crate::project_root::relative_posix(&h.file, root)
                    .unwrap_or_else(|| h.file.display().to_string());
                ImpactEntry {
                    qn: rel,
                    file: h.file.display().to_string(),
                    line: h.line,
                    kind: format!("{:?}", h.kind).to_lowercase(),
                    confidence: None,
                    depth: None,
                }
            })
            .collect(),
    }
}

fn render_text(
    _target_raw: &str,
    reports: &[ImpactReport],
    opts: &ImpactOptions,
    candidate_count: usize,
) -> String {
    let mut out = String::new();
    for r in reports {
        out.push_str(&format!(
            "{} {} {} ({}:{})\n",
            "⊕".bold(),
            r.target_kind.dimmed(),
            r.target_qn.split("::").last().unwrap_or(&r.target_qn).yellow(),
            colorize_file(&r.target_file),
            colorize_line(r.target_line),
        ));
        if let Some(sections) = &r.sections {
            for s in sections {
                if s.entries.is_empty() {
                    continue;
                }
                out.push_str(&format!("\n  {}\n", s.title.bold()));
                for e in &s.entries {
                    let depth_tag = match e.depth {
                        Some(d) if d > 1 => format!(" depth={}", d).dimmed().to_string(),
                        _ => String::new(),
                    };
                    let conf_tag = match &e.confidence {
                        Some(c) if c != "Exact" => format!(" {}", colorize_confidence(c)),
                        _ => String::new(),
                    };
                    out.push_str(&format!(
                        "    {}{} {} {}{}{}\n",
                        if e.qn.starts_with('[') { "" } else { "→ " }.dimmed(),
                        e.kind.dimmed(),
                        e.qn.name_or_raw_segment().yellow(),
                        colorize_file_path(&e.qn, &e.file),
                        depth_tag,
                        conf_tag,
                    ));
                }
            }
        }
        if r.transitive_count > 0 {
            out.push_str(&format!(
                "\n{} {} symbols transitively affected{}{}\n",
                "!".bold(),
                r.transitive_count,
                if opts.tests {
                    format!(", {} affected tests", r.test_count)
                } else {
                    String::new()
                },
                if opts.exclude_tests {
                    " (tests excluded)"
                } else {
                    ""
                },
            ));
        }
        if candidate_count > 1 {
            out.push('\n');
        }
    }
    out
}

fn render_json(
    target_raw: &str,
    reports: &[ImpactReport],
    pretty: bool,
    candidate_count: usize,
) -> String {
    let doc = json!({
        "schema": "ast-bro.impact.v1",
        "target": target_raw,
        "candidates": candidate_count,
        "impacts": reports,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

fn colorize_file(p: &str) -> String {
    p.cyan().bold().to_string()
}

fn colorize_line(line: u32) -> String {
    if line == 0 {
        String::new()
    } else {
        format!(":{}", line).truecolor(150, 150, 150).to_string()
    }
}

fn colorize_confidence(c: &str) -> String {
    match c {
        "Exact" => "Exact".green().to_string(),
        "Inferred" => "Inferred".yellow().to_string(),
        "Ambiguous" => "Ambiguous".red().to_string(),
        other => other.to_string(),
    }
}

fn colorize_file_path(qn: &str, file: &str) -> String {
    let display = if qn.contains("::") {
        let parts: Vec<&str> = qn.splitn(2, "::").collect();
        if parts.len() == 2 { parts[0] } else { file }
    } else {
        file
    };
    format!(" ({})", display).truecolor(100, 100, 100).to_string()
}

trait NameOrRaw {
    fn name_or_raw_segment(&self) -> String;
}

impl NameOrRaw for String {
    fn name_or_raw_segment(&self) -> String {
        if self.starts_with('[') {
            return self.clone();
        }
        match self.rfind("::") {
            Some(i) => self[i + 2..].to_string(),
            None => self.clone(),
        }
    }
}

pub mod mcp {
    use super::*;
    use serde_json::Value;

    pub fn run_impact(args: Value) -> crate::mcp::tools::CallResult {
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Args {
            target: String,
            #[serde(default = "default_dot")]
            path: PathBuf,
            #[serde(default = "default_two")]
            depth: usize,
            #[serde(default = "default_limit")]
            limit: usize,
            #[serde(default = "default_mode")]
            mode: String,
            #[serde(default)]
            include_ambiguous: bool,
            #[serde(default)]
            tests: bool,
            #[serde(default)]
            exclude_tests: bool,
            #[serde(default)]
            json: bool,
        }
        fn default_dot() -> PathBuf { PathBuf::from(".") }
        fn default_two() -> usize { 2 }
        fn default_limit() -> usize { 200 }
        fn default_mode() -> String { "all".into() }

        let a: Args = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return crate::mcp::tools::CallResult::Error(format!("bad args: {e}"))
            }
        };
        let mode = match ImpactMode::from_str(&a.mode) {
            Some(m) => m,
            None => {
                return crate::mcp::tools::CallResult::Error(format!(
                    "unknown --mode '{}'. Expected: deps, dependents, tests, all",
                    a.mode,
                ))
            }
        };
        let root = match crate::project_root::find_root_for(&a.path) {
            Ok(r) => r,
            Err(e) => return crate::mcp::tools::CallResult::Error(e),
        };
        let graph = match ensure_graph(&root, false) {
            Ok(g) => g,
            Err(e) => {
                return crate::mcp::tools::CallResult::Error(format!(
                    "# error: {}", e
                ))
            }
        };
        let calls = match &graph.calls {
            Some(c) => c,
            None => {
                return crate::mcp::tools::CallResult::Error(
                    "# error: call graph is empty".into(),
                )
            }
        };
        let candidates = resolve_target_full(calls, &a.target);
        if candidates.is_empty() {
            return crate::mcp::tools::CallResult::Error(format!(
                "# note: no symbol matches '{}' (try a more specific suffix like 'Type.method').",
                a.target,
            ));
        }
        let opts = ImpactOptions {
            depth: a.depth,
            limit: a.limit,
            mode,
            include_ambiguous: a.include_ambiguous,
            tests: a.tests,
            exclude_tests: a.exclude_tests,
            json: true,
            pretty: true,
        };
        let mut reports = Vec::new();
        for c in &candidates {
            reports.push(compute_impact(c, calls, &graph.deps, &root, &opts));
        }
        crate::mcp::tools::CallResult::Text(render_json(
            &a.target,
            &reports,
            true,
            candidates.len(),
        ))
    }
}
