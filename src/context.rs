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

use colored::Colorize;
use serde::Serialize;
use serde_json::json;

use crate::calls::cli_helpers::{resolve_target_full, ResolvedTarget, SymbolKind};
use crate::calls::graph::{CallGraph, CallTarget, Confidence};
use crate::calls::traverse;
use crate::graph_cache;

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

/// Push the target's entry, degrading to fit the remaining budget:
/// full body → signature only → metadata only. Shared by the callable
/// and type branches of `build_context`.
///
/// Returns `(target_omitted, body_unavailable)`.
#[allow(clippy::too_many_arguments)]
fn push_target_entry(
    qn: &str,
    file: String,
    line: u32,
    kind: String,
    body: Option<String>,
    sig: Option<String>,
    budget: usize,
    used: &mut usize,
    entries: &mut Vec<ContextEntry>,
) -> (bool, bool) {
    let entry = |label: &str, body: Option<String>, sig: Option<String>, tok: usize| ContextEntry {
        label: label.into(),
        qn: qn.to_string(),
        file: file.clone(),
        line,
        kind: Some(kind.clone()),
        body,
        signature: sig,
        tokens: tok,
    };
    match (body, sig) {
        (Some(b), sig) => {
            let tok = estimate_tokens(b.len());
            if tok <= budget.saturating_sub(*used) {
                *used += tok;
                entries.push(entry("target", Some(b), sig, tok));
                (false, false)
            } else {
                let sig_tok = sig.as_ref().map(|s| estimate_tokens(s.len())).unwrap_or(0);
                if sig.is_some() && sig_tok <= budget.saturating_sub(*used) {
                    *used += sig_tok;
                    entries.push(entry("target (signature only — budget)", None, sig, sig_tok));
                } else {
                    entries.push(entry("target (metadata only — budget)", None, None, 0));
                }
                (true, false)
            }
        }
        (None, Some(s)) => {
            let sig_tok = estimate_tokens(s.len());
            if sig_tok <= budget.saturating_sub(*used) {
                *used += sig_tok;
                entries.push(entry(
                    "target (signature only — unresolved)",
                    None,
                    Some(s),
                    sig_tok,
                ));
            } else {
                // Signature won't fit either — still record a metadata-only
                // entry so the target isn't silently absent from the pack.
                entries.push(entry("target (metadata only — unresolved)", None, None, 0));
            }
            (false, true)
        }
        (None, None) => {
            entries.push(entry("target (metadata only — unresolved)", None, None, 0));
            (false, true)
        }
    }
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
    let graph = match graph_cache::ensure_with_calls(&root, rebuild) {
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
    if candidates.len() > 1 {
        eprintln!(
            "# note: '{}' matched {} symbols; showing '{}'. Use a more specific suffix (e.g. 'Type.method') to pick another.",
            target,
            candidates.len(),
            c.qn.as_str()
        );
    }
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

        let (omitted, unavailable) = push_target_entry(
            c.qn.as_str(),
            file_abs,
            line,
            kind,
            body,
            sig,
            budget_tokens,
            &mut used,
            &mut entries,
        );
        target_omitted |= omitted;
        body_unavailable |= unavailable;

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
            let had_body = body.is_some();
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
                // A body existed but exceeded the budget and there is no
                // signature to degrade to — that's truncation, not a
                // resolution failure.
                truncated |= had_body;
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
                continue;
            }
        }

        // Ambiguous edges are filtered inside the traversal so they don't
        // consume the result limit.
        let direct_callers = traverse::callers(calls, &c.qn, 1, 50, |e| {
            !matches!(e.confidence, Confidence::Ambiguous)
        });
        for h in &direct_callers {
            if !seen_qns.insert(h.edge.source.as_str().to_string()) {
                continue;
            }
            let Some(sig) = signature_from_meta(calls, &h.edge.source, root, &mut parse_cache) else {
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                let meta = calls.callable_meta.get(&h.edge.source);
                // Signature entries point at the declaration, not the call
                // site the traversal edge happens to carry.
                let (decl_file, decl_line) = meta
                    .map(|m| (m.file.display().to_string(), m.line))
                    .unwrap_or_else(|| (h.edge.file.display().to_string(), h.edge.line));
                entries.push(ContextEntry {
                    label: "direct dependent (signature)".into(),
                    qn: h.edge.source.as_str().to_string(),
                    file: decl_file,
                    line: decl_line,
                    kind: meta.map(|m| m.kind.clone()),
                    body: None,
                    signature: Some(sig),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                continue;
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
            let Some(sig) = signature_from_meta(calls, qn, root, &mut parse_cache) else {
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                let meta = calls.callable_meta.get(qn);
                let (decl_file, decl_line) = meta
                    .map(|m| (m.file.display().to_string(), m.line))
                    .unwrap_or_else(|| (h.edge.file.display().to_string(), h.edge.line));
                entries.push(ContextEntry {
                    label: "transitive dependency (signature)".into(),
                    qn: qn.as_str().to_string(),
                    file: decl_file,
                    line: decl_line,
                    kind: meta.map(|m| m.kind.clone()),
                    body: None,
                    signature: Some(sig),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                continue;
            }
        }

        let trans_callers = traverse::callers(calls, &c.qn, 2, 50, |e| {
            !matches!(e.confidence, Confidence::Ambiguous)
        });
        for h in &trans_callers {
            if h.depth < 2 {
                continue;
            }
            if !seen_qns.insert(h.edge.source.as_str().to_string()) {
                continue;
            }
            let Some(sig) = signature_from_meta(calls, &h.edge.source, root, &mut parse_cache) else {
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                let meta = calls.callable_meta.get(&h.edge.source);
                // Signature entries point at the declaration, not the call
                // site the traversal edge happens to carry.
                let (decl_file, decl_line) = meta
                    .map(|m| (m.file.display().to_string(), m.line))
                    .unwrap_or_else(|| (h.edge.file.display().to_string(), h.edge.line));
                entries.push(ContextEntry {
                    label: "transitive dependent (signature)".into(),
                    qn: h.edge.source.as_str().to_string(),
                    file: decl_file,
                    line: decl_line,
                    kind: meta.map(|m| m.kind.clone()),
                    body: None,
                    signature: Some(sig),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                continue;
            }
        }
    } else {
        // Type target: show the type definition, its implementors, and callers
        // of its methods / constructors.
        let type_meta = calls.types.get(&c.qn);
        let file_abs = type_meta
            .map(|m| root.join(&m.file))
            .unwrap_or_else(|| root.join(c.qn.file()));
        let line = type_meta.map(|m| m.line).unwrap_or(0);
        let kind = type_meta
            .map(|m| m.kind.as_str())
            .unwrap_or("type")
            .to_string();
        let name = c.qn.name();

        let mut body_text: Option<String> = None;
        let mut sig_text: Option<String> = None;
        if let Some(pr) = parse_file_cached(&file_abs, &mut parse_cache) {
            let matches = crate::core::find_symbols(pr, name);
            for m in &matches {
                if m.start_line.abs_diff(line as usize) <= 1 {
                    body_text = Some(m.source.trim_end().to_string());
                    sig_text = m
                        .source
                        .lines()
                        .next()
                        .map(|l| l.trim_end().to_string());
                    break;
                }
            }
            if body_text.is_none() {
                if let Some(m) = matches.into_iter().next() {
                    body_text = Some(m.source.trim_end().to_string());
                    sig_text = m
                        .source
                        .lines()
                        .next()
                        .map(|l| l.trim_end().to_string());
                }
            }
        }

        let file_rel = crate::project_root::relative_posix(&file_abs, root)
            .unwrap_or_else(|| file_abs.display().to_string());

        let (omitted, unavailable) = push_target_entry(
            c.qn.as_str(),
            file_rel,
            line,
            kind,
            body_text,
            sig_text,
            budget_tokens,
            &mut used,
            &mut entries,
        );
        target_omitted |= omitted;
        body_unavailable |= unavailable;

        // Implementors of this type (the "who implements it" dimension).
        if let Some(impls) = calls.implementors.get(c.qn.name()) {
            for impl_qn in impls {
                if !seen_qns.insert(impl_qn.as_str().to_string()) {
                    continue;
                }
                let (impl_body, impl_sig, impl_file, impl_line, _) =
                    resolve_qn_source(impl_qn, calls, root, &mut parse_cache);
                let impl_kind = calls
                    .types
                    .get(impl_qn)
                    .map(|m| m.kind.clone())
                    .unwrap_or_else(|| "type".into());
                // resolve_qn_source already returns a repo-relative path.
                let rel_file = impl_file;
                let had_body = impl_body.is_some();
                if let Some(b) = impl_body {
                    let tok = estimate_tokens(b.len());
                    if tok <= budget_tokens.saturating_sub(used) {
                        used += tok;
                        entries.push(ContextEntry {
                            label: "implementor (body)".into(),
                            qn: impl_qn.as_str().to_string(),
                            file: rel_file,
                            line: impl_line,
                            kind: Some(impl_kind),
                            body: Some(b),
                            signature: impl_sig,
                            tokens: tok,
                        });
                        continue;
                    }
                }
                let Some(ref sig) = impl_sig else {
                    truncated |= had_body;
                    continue;
                };
                let sig_tok = estimate_tokens(sig.len());
                if sig_tok <= budget_tokens.saturating_sub(used) {
                    used += sig_tok;
                    entries.push(ContextEntry {
                        label: "implementor (signature)".into(),
                        qn: impl_qn.as_str().to_string(),
                        file: rel_file,
                        line: impl_line,
                        kind: Some(impl_kind),
                        body: None,
                        signature: Some(sig.clone()),
                        tokens: sig_tok,
                    });
                } else {
                    truncated = true;
                    continue;
                }
            }
        }

        // Methods of this type: any callable whose QN is prefixed with
        // `<type_qn>::`. Shown as dependencies (the type is composed of
        // its methods).
        let type_prefix = format!("{}::", c.qn.as_str());
        let mut method_qns: Vec<_> = calls
            .callable_meta
            .keys()
            .filter(|q| q.as_str().starts_with(&type_prefix))
            .cloned()
            .collect();
        method_qns.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for method_qn in &method_qns {
            if !seen_qns.insert(method_qn.as_str().to_string()) {
                continue;
            }
            let (body, sig, meth_file, meth_line, meth_kind) =
                resolve_qn_source(method_qn, calls, root, &mut parse_cache);
            let had_body = body.is_some();
            let rel_file = meth_file;
            if let Some(b) = body {
                let tok = estimate_tokens(b.len());
                if tok <= budget_tokens.saturating_sub(used) {
                    used += tok;
                    entries.push(ContextEntry {
                        label: "method (body)".into(),
                        qn: method_qn.as_str().to_string(),
                        file: rel_file,
                        line: meth_line,
                        kind: Some(meth_kind),
                        body: Some(b),
                        signature: sig,
                        tokens: tok,
                    });
                    continue;
                }
            }
            let Some(ref sig) = sig else {
                truncated |= had_body;
                continue;
            };
            let sig_tok = estimate_tokens(sig.len());
            if sig_tok <= budget_tokens.saturating_sub(used) {
                used += sig_tok;
                entries.push(ContextEntry {
                    label: "method (signature)".into(),
                    qn: method_qn.as_str().to_string(),
                    file: rel_file,
                    line: meth_line,
                    kind: Some(meth_kind),
                    body: None,
                    signature: Some(sig.clone()),
                    tokens: sig_tok,
                });
            } else {
                truncated = true;
                continue;
            }
        }

        // Callers of each method — direct dependents of the type.
        for method_qn in &method_qns {
            let method_callers = traverse::callers(calls, method_qn, 1, 20, |e| {
                !matches!(e.confidence, Confidence::Ambiguous)
            });
            for h in &method_callers {
                if !seen_qns.insert(h.edge.source.as_str().to_string()) {
                    continue;
                }
                let Some(sig) = signature_from_meta(calls, &h.edge.source, root, &mut parse_cache)
                else {
                    continue;
                };
                let sig_tok = estimate_tokens(sig.len());
                if sig_tok <= budget_tokens.saturating_sub(used) {
                    used += sig_tok;
                    let meta = calls.callable_meta.get(&h.edge.source);
                    // Signature entries point at the declaration, not the call
                    // site the traversal edge happens to carry.
                    let (decl_file, decl_line) = meta
                        .map(|m| (m.file.display().to_string(), m.line))
                        .unwrap_or_else(|| (h.edge.file.display().to_string(), h.edge.line));
                    entries.push(ContextEntry {
                        label: "dependent (signature)".into(),
                        qn: h.edge.source.as_str().to_string(),
                        file: decl_file,
                        line: decl_line,
                        kind: meta.map(|m| m.kind.clone()),
                        body: None,
                        signature: Some(sig),
                        tokens: sig_tok,
                    });
                } else {
                    truncated = true;
                    continue;
                }
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

/// File/line/kind for a qn — callable table first, then the type table
/// (the implementor loop in `build_context` passes type qns straight from
/// `calls.implementors`), then the qn's own path component. The declaration
/// line feeds the ±1-line disambiguation in both source lookups below.
///
/// Joins against the freshly-resolved `root`, not `calls.root`: the latter
/// is an absolute path deserialised from graph.bin and goes stale when the
/// repository directory is moved or renamed.
fn meta_location(
    calls: &CallGraph,
    qn: &crate::calls::graph::Qn,
    root: &Path,
) -> (PathBuf, u32, String) {
    if let Some(m) = calls.callable_meta.get(qn) {
        (root.join(&m.file), m.line, m.kind.clone())
    } else if let Some(t) = calls.types.get(qn) {
        (root.join(&t.file), t.line, t.kind.clone())
    } else {
        (root.join(qn.file()), 0, "function".to_string())
    }
}

fn resolve_qn_source(
    qn: &crate::calls::graph::Qn,
    calls: &CallGraph,
    root: &Path,
    cache: &mut ParsedFileCache,
) -> (Option<String>, Option<String>, String, u32, String) {
    let (file_abs, line, kind) = meta_location(calls, qn, root);
    let rel = crate::project_root::relative_posix(&file_abs, root)
        .unwrap_or_else(|| file_abs.display().to_string());
    let name = qn.name();

    if let Some(pr) = parse_file_cached(&file_abs, cache) {
        let matches = crate::core::find_symbols(pr, name);
        for m in &matches {
            if m.start_line.abs_diff(line as usize) <= 1 {
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
    root: &Path,
    cache: &mut ParsedFileCache,
) -> Option<String> {
    let (file_abs, line, _) = meta_location(calls, qn, root);
    let name = qn.name();
    let pr = match parse_file_cached(&file_abs, cache) {
        Some(p) => p,
        None => return None,
    };
    let matches = crate::core::find_symbols(pr, name);
    for m in &matches {
        if m.start_line.abs_diff(line as usize) <= 1 {
            return Some(first_line(&m.source).to_string());
        }
    }
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
        struct Args {
            target: String,
            #[serde(default = "default_dot")]
            path: PathBuf,
            #[serde(default = "default_budget")]
            budget: usize,
            /// Text by default (MCP convention); `true` returns
            /// `ast-bro.context.v1` JSON.
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
        let graph = match graph_cache::ensure_with_calls(&root, false) {
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
        if candidates.len() > 1 {
            eprintln!(
                "# note: '{}' matched {} symbols; showing '{}'. Use a more specific suffix (e.g. 'Type.method') to pick another.",
                a.target,
                candidates.len(),
                candidates[0].qn.as_str()
            );
        }
        let report = build_context(&candidates[0], calls, &root, &opts);
        let body = if a.json {
            let doc = json!({
                "schema": "ast-bro.context.v1",
                "report": report,
            });
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        } else {
            render_text(&report)
        };
        crate::mcp::tools::CallResult::Text(body)
    }
}
