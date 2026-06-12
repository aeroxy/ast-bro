//! `context` — token-budgeted context pack for a symbol.
//!
//! Greedy knapsack: target body (full) → direct callees (bodies) → direct
//! callers (signatures) → transitive callees/callers (signatures only) →
//! file reverse-deps (signatures). Fits the most relevant context for the
//! target into a caller-supplied token budget (~4 bytes per token).
//!
//! An LLM agent asking `context handleRequest --budget 2000` gets a single
//! payload of everything relevant to the symbol it's working on, instead of
//! chaining 4-5 separate `show`/`callers`/`callees` calls.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use colored::Colorize;
use serde::Serialize;
use serde_json::json;

use crate::calls::build::build_call_graph;
use crate::calls::cli_helpers::{resolve_target_full, ResolvedTarget, SymbolKind};
use crate::calls::graph::{CallGraph, CallTarget, Confidence};
use crate::calls::traverse;
use crate::graph_cache::{self, UnifiedGraph};

const BYTES_PER_TOKEN: usize = 4;

type ParsedFileCache = std::collections::HashMap<PathBuf, Option<crate::core::ParseResult>>;

fn parse_file_cached<'a>(
    path: &Path,
    cache: &'a mut ParsedFileCache,
) -> &'a Option<crate::core::ParseResult> {
    cache
        .entry(path.to_path_buf())
        .or_insert_with(|| crate::parse_file(path))
}

pub struct ContextOptions {
    pub budget: usize,
    pub json: bool,
    pub pretty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextEntry {
    pub label: String,
    pub qn: String,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub signature: Option<String>,
    pub tokens: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextReport {
    pub symbol: String,
    pub budget: usize,
    pub used: usize,
    pub entries: Vec<ContextEntry>,
    pub truncated: bool,
    pub target_omitted: bool,
    pub body_unavailable: bool,
}

fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

pub fn run_context(
    target: &str,
    path: &Path,
    opts: &ContextOptions,
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

    let c = &candidates[0];
    let report = build_context(c, calls, &root, opts);

    if opts.json {
        let doc = json!({
            "schema": "ast-bro.context.v1",
            "report": report,
        });
        let json_str = if opts.pretty {
            serde_json::to_string_pretty(&doc)
        } else {
            serde_json::to_string(&doc)
        };
        println!("{}", json_str.unwrap_or_default());
    } else {
        print!("{}", render_text(&report));
    }
    0
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

fn build_context(
    c: &ResolvedTarget,
    calls: &CallGraph,
    root: &Path,
    opts: &ContextOptions,
) -> ContextReport {
    let budget_tokens = opts.budget;
    let mut used: usize = 0;
    let mut entries: Vec<ContextEntry> = Vec::new();
    let mut truncated = false;
    let mut target_omitted = false;
    let mut body_unavailable = false;

    let mut seen_qns: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen_qns.insert(c.qn.as_str().to_string());

    let mut parse_cache: ParsedFileCache = std::collections::HashMap::new();

    if c.kind == SymbolKind::Callable {
        let (body, sig, file_abs, line, kind) =
            resolve_qn_source(&c.qn, calls, root, &mut parse_cache);

        match (&body, &sig) {
            (Some(body_text), sig_opt) => {
                let tok = estimate_tokens(body_text.len());
                if tok <= budget_tokens.saturating_sub(used) {
                    used += tok;
                    entries.push(ContextEntry {
                        label: "target".into(),
                        qn: c.qn.as_str().to_string(),
                        file: file_abs.clone(),
                        line,
                        kind: Some(kind.clone()),
                        body: Some(body_text.clone()),
                        signature: sig_opt.clone(),
                        tokens: tok,
                    });
                } else {
                    target_omitted = true;
                    let sig_tok = sig_opt.as_ref().map(|s| estimate_tokens(s.len())).unwrap_or(0);
                    if sig_tok <= budget_tokens.saturating_sub(used) {
                        used += sig_tok;
                        entries.push(ContextEntry {
                            label: "target (signature only — budget)".into(),
                            qn: c.qn.as_str().to_string(),
                            file: file_abs.clone(),
                            line,
                            kind: Some(kind.clone()),
                            body: None,
                            signature: sig_opt.clone(),
                            tokens: sig_tok,
                        });
                    }
                }
            }
            (None, Some(sig_text)) => {
                body_unavailable = true;
                let sig_tok = estimate_tokens(sig_text.len());
                if sig_tok <= budget_tokens.saturating_sub(used) {
                    used += sig_tok;
                    entries.push(ContextEntry {
                        label: "target (signature only — unresolved)".into(),
                        qn: c.qn.as_str().to_string(),
                        file: file_abs.clone(),
                        line,
                        kind: Some(kind.clone()),
                        body: None,
                        signature: Some(sig_text.clone()),
                        tokens: sig_tok,
                    });
                }
            }
            (None, None) => {
                body_unavailable = true;
                entries.push(ContextEntry {
                    label: "target (metadata only — unresolved)".into(),
                    qn: c.qn.as_str().to_string(),
                    file: file_abs.clone(),
                    line,
                    kind: Some(kind.clone()),
                    body: None,
                    signature: None,
                    tokens: 0,
                });
            }
        }

        let direct_callees = traverse::callees_one_hop(calls, &c.qn);
        for e in &direct_callees {
            if matches!(e.confidence, Confidence::Ambiguous) {
                continue;
            }
            let CallTarget::Resolved(callee_qn) = &e.target else {
                continue;
            };
            if !seen_qns.insert(callee_qn.as_str().to_string()) {
                continue;
            }
            let (body, sig, file_str, line, kind) =
                resolve_qn_source(callee_qn, calls, root, &mut parse_cache);
            if let Some(b) = body {
                let tok = estimate_tokens(b.len());
                if tok <= budget_tokens.saturating_sub(used) {
                    used += tok;
                    entries.push(ContextEntry {
                        label: "direct dependency (body)".into(),
                        qn: callee_qn.as_str().to_string(),
                        file: file_str,
                        line,
                        kind: Some(kind),
                        body: Some(b),
                        signature: sig,
                        tokens: tok,
                    });
                    continue;
                }
            }
            let Some(ref sig_text) = sig else {
                continue;
            };
            let sig_tok = estimate_tokens(sig_text.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                entries.push(ContextEntry {
                    label: "direct dependency (signature only)".into(),
                    qn: callee_qn.as_str().to_string(),
                    file: file_str,
                    line,
                    kind: Some(kind),
                    body: None,
                    signature: sig,
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                break;
            }
        }

        let direct_callers = traverse::callers(calls, &c.qn, 1, 50);
        for h in &direct_callers {
            if matches!(h.edge.confidence, Confidence::Ambiguous) {
                continue;
            }
            if !seen_qns.insert(h.edge.source.as_str().to_string()) {
                continue;
            }
            let Some(sig) = signature_from_meta(calls, &h.edge.source, &mut parse_cache) else {
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                let meta = calls.callable_meta.get(&h.edge.source);
                entries.push(ContextEntry {
                    label: "direct dependent (signature)".into(),
                    qn: h.edge.source.as_str().to_string(),
                    file: h.edge.file.display().to_string(),
                    line: h.edge.line,
                    kind: meta.map(|m| m.kind.clone()),
                    body: None,
                    signature: Some(sig),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                break;
            }
        }

        let trans_callees = traverse::callees(calls, &c.qn, 2);
        for h in &trans_callees {
            if matches!(h.edge.confidence, Confidence::Ambiguous) || h.depth < 2 {
                continue;
            }
            let CallTarget::Resolved(qn) = &h.edge.target else {
                continue;
            };
            if !seen_qns.insert(qn.as_str().to_string()) {
                continue;
            }
            let Some(sig) = signature_from_meta(calls, qn, &mut parse_cache) else {
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                let meta = calls.callable_meta.get(qn);
                entries.push(ContextEntry {
                    label: "transitive dependency (signature)".into(),
                    qn: qn.as_str().to_string(),
                    file: h.edge.file.display().to_string(),
                    line: h.edge.line,
                    kind: meta.map(|m| m.kind.clone()),
                    body: None,
                    signature: Some(sig),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                break;
            }
        }

        let trans_callers = traverse::callers(calls, &c.qn, 2, 50);
        for h in &trans_callers {
            if matches!(h.edge.confidence, Confidence::Ambiguous) || h.depth < 2 {
                continue;
            }
            if !seen_qns.insert(h.edge.source.as_str().to_string()) {
                continue;
            }
            let Some(sig) = signature_from_meta(calls, &h.edge.source, &mut parse_cache) else {
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                let meta = calls.callable_meta.get(&h.edge.source);
                entries.push(ContextEntry {
                    label: "transitive dependent (signature)".into(),
                    qn: h.edge.source.as_str().to_string(),
                    file: h.edge.file.display().to_string(),
                    line: h.edge.line,
                    kind: meta.map(|m| m.kind.clone()),
                    body: None,
                    signature: Some(sig),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                break;
            }
        }
    }

    ContextReport {
        symbol: c.qn.as_str().to_string(),
        budget: budget_tokens,
        used,
        entries,
        truncated,
        target_omitted,
        body_unavailable,
    }
}

fn resolve_qn_source(
    qn: &crate::calls::graph::Qn,
    calls: &CallGraph,
    root: &Path,
    cache: &mut ParsedFileCache,
) -> (Option<String>, Option<String>, String, u32, String) {
    let file_abs = match calls.callable_meta.get(qn) {
        Some(m) => root.join(&m.file),
        None => root.join(qn.file()),
    };
    let line = calls.callable_meta.get(qn).map(|m| m.line).unwrap_or(0);
    let kind = calls
        .callable_meta
        .get(qn)
        .map(|m| m.kind.as_str())
        .unwrap_or("function")
        .to_string();
    let rel = crate::project_root::relative_posix(&file_abs, root)
        .unwrap_or_else(|| file_abs.display().to_string());
    let name = qn.as_str().split("::").last().unwrap_or(qn.as_str());

    if let Some(pr) = parse_file_cached(&file_abs, cache) {
        let matches = crate::core::find_symbols(pr, name);
        for m in &matches {
            if m.start_line == line as usize || m.start_line.abs_diff(line as usize) <= 1 {
                return (
                    Some(m.source.trim_end().to_string()),
                    Some(first_line(&m.source).to_string()),
                    rel,
                    m.start_line as u32,
                    kind,
                );
            }
        }
        if let Some(m) = matches.into_iter().next() {
            return (
                Some(m.source.trim_end().to_string()),
                Some(first_line(&m.source).to_string()),
                rel,
                m.start_line as u32,
                kind,
            );
        }
    }
    (None, None, rel, line, kind)
}

fn signature_from_meta(
    calls: &CallGraph,
    qn: &crate::calls::graph::Qn,
    cache: &mut ParsedFileCache,
) -> Option<String> {
    let file_abs = match calls.callable_meta.get(qn) {
        Some(m) => calls.root.join(&m.file),
        None => calls.root.join(qn.file()),
    };
    let name = qn.as_str().split("::").last().unwrap_or(qn.as_str());
    let pr = match parse_file_cached(&file_abs, cache) {
        Some(p) => p,
        None => return None,
    };
    let matches = crate::core::find_symbols(pr, name);
    matches.into_iter().next().map(|m| first_line(&m.source).to_string())
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

fn render_text(report: &ContextReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {} (budget: {}, used: {})\n",
        "context for".bold(),
        report.symbol.yellow(),
        format!("{} tokens", report.budget).truecolor(150, 150, 150),
        format!("{} tokens", report.used).green().bold(),
    ));
    if report.target_omitted {
        out.push_str(&format!(
            "{}\n",
            "# note: target body omitted to fit budget (show only signature)".dimmed()
        ));
    }
    if report.body_unavailable {
        out.push_str(&format!(
            "{}\n",
            "# note: target body could not be resolved (parse failure or unsupported language)".dimmed()
        ));
    }
    if report.truncated {
        out.push_str(&format!(
            "{}\n",
            "# warning: budget exhausted before all transitive context was included".dimmed()
        ));
    }
    let mut last_label = String::new();
    for e in &report.entries {
        if e.label != last_label {
            out.push_str(&format!("\n  {}:\n", e.label.bold().underline()));
            last_label = e.label.clone();
        }
        out.push_str(&format!(
            "    {} {} ({}:{}, ~{} tokens)\n",
            e.kind.as_deref().unwrap_or("symbol").dimmed(),
            e.qn.split("::").last().unwrap_or(&e.qn).yellow(),
            e.file.cyan(),
            e.line.to_string().truecolor(150, 150, 150),
            e.tokens,
        ));
        if let Some(body) = &e.body {
            for line in body.lines() {
                out.push_str(&format!("      {}\n", line));
            }
        } else if let Some(sig) = &e.signature {
            out.push_str(&format!("      {}\n", sig.truecolor(180, 180, 180)));
        }
    }
    out
}

pub mod mcp {
    use super::*;
    use serde_json::Value;

    pub fn run_context(args: Value) -> crate::mcp::tools::CallResult {
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Args {
            target: String,
            #[serde(default = "default_dot")]
            path: PathBuf,
            #[serde(default = "default_budget")]
            budget: usize,
            #[serde(default)]
            json: bool,
        }
        fn default_dot() -> PathBuf { PathBuf::from(".") }
        fn default_budget() -> usize { 8000 }

        let a: Args = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return crate::mcp::tools::CallResult::Error(format!("bad args: {e}"))
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
                "# note: no symbol matches '{}'.",
                a.target,
            ));
        }
        let opts = ContextOptions {
            budget: a.budget,
            json: true,
            pretty: true,
        };
        let report = build_context(&candidates[0], calls, &root, &opts);
        let doc = json!({
            "schema": "ast-bro.context.v1",
            "report": report,
        });
        crate::mcp::tools::CallResult::Text(
            serde_json::to_string_pretty(&doc).unwrap_or_default(),
        )
    }
}
