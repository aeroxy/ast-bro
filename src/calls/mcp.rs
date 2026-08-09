//! MCP-side wrappers reused by `src/mcp/tools.rs` for `callers` / `callees`
//! / `trace`.
//!
//! Each returns `Result<String, String>`: `Ok` is the response text, `Err`
//! the diagnostic the MCP layer surfaces as `isError: true` — MCP has no
//! stderr channel, and a failure encoded as a successful-looking `# error:`
//! text would read as an answer (#33 on the MCP surface).

use std::path::Path;

/// Shared prologue: the unified graph with the calls half present, or the
/// diagnostic to surface as `isError`.
fn load_calls_graph(
    root: &Path,
) -> Result<std::sync::Arc<crate::graph_cache::UnifiedGraph>, String> {
    use crate::calls::build::build_call_graph;
    use crate::graph_cache;
    let unified = graph_cache::get_or_init(root).map_err(|e| e.to_string())?;
    if unified.calls.is_some() {
        Ok(unified)
    } else {
        graph_cache::promote_calls(root, |g| build_call_graph(root, &g.deps))
            .map_err(|e| e.to_string())
    }
}

pub fn run_callers_text(
    target: &str,
    root: &Path,
    depth: usize,
    limit: usize,
    include_ambiguous: bool,
    json: bool,
) -> Result<String, String> {
    use crate::calls::{render, traverse};

    let promoted = load_calls_graph(root)?;
    let calls = promoted
        .calls
        .as_ref()
        .ok_or_else(|| "call graph unavailable".to_string())?;
    let qns = crate::calls::cli_helpers::resolve_target_qns(calls, target);
    let mut hits = Vec::new();
    let mut frontier_truncated = false;
    for qn in &qns {
        // Walk unbounded so the header carries the true total; `--limit`
        // trims the display (issue #32). Filter inside the traversal so
        // dropped ambiguous edges don't consume the limit.
        let info = traverse::callers_info(calls, qn, depth.max(1), crate::UNLIMITED, |e| {
            include_ambiguous
                || !matches!(e.confidence, crate::calls::graph::Confidence::Ambiguous)
        });
        frontier_truncated |= info.frontier_truncated;
        hits.extend(info.hits);
    }
    let total = hits.len();
    if hits.len() > limit {
        hits.truncate(limit);
    }
    // Same unattributed-edge policy as the CLI path (issue #31), through
    // the one shared constructor so the two surfaces cannot drift. MCP has
    // no stderr channel, so when `include_ambiguous` is off the hidden
    // count is prepended to the response text instead of printed as a note
    // — silently dropping it would make "0 callers" read as authoritative.
    let facts =
        render::UnattributedFacts::collect(calls, &qns, include_ambiguous, limit, &|_| true);
    let hidden_note = facts
        .hidden_note(target, "include_ambiguous: false")
        .map(|n| format!("{n}\n"))
        .unwrap_or_default();
    let unattributed = facts.view();
    let trunc = render::Truncation {
        total,
        frontier_truncated,
    };
    if json {
        Ok(render::render_callers_json(
            target,
            depth.max(1),
            &hits,
            &unattributed,
            &trunc,
            true,
        ))
    } else {
        // Same note as the CLI's stderr line, through the same helper so the
        // two can't drift. MCP's one channel is the response text.
        let frontier_note = render::frontier_note(depth, frontier_truncated)
            .map(|n| format!("{n}\n"))
            .unwrap_or_default();
        Ok(format!(
            "{}{}{}",
            frontier_note,
            hidden_note,
            render::render_callers_text(target, &hits, &unattributed, &trunc)
        ))
    }
}

pub fn run_trace_text(
    from: &str,
    to: &str,
    root: &Path,
    depth: usize,
    json: bool,
) -> Result<String, String> {
    use crate::calls::trace::render_trace;

    let promoted = load_calls_graph(root)?;
    let calls = promoted
        .calls
        .as_ref()
        .ok_or_else(|| "call graph unavailable".to_string())?;
    let (out, outcome) = render_trace(calls, root, from, to, depth.max(1), json, false);
    match outcome {
        // An endpoint that matches no symbol is a rejected call, not an
        // answer — the CLI exits 2 (#36), and MCP's one error channel is
        // `isError`, same as `map`/`digest` missing-input handling.
        crate::calls::trace::TraceOutcome::Unresolved(missing) => Err(format!(
            "no callable symbol matches '{}' (try a more specific suffix like 'Type.method')",
            missing
        )),
        _ => Ok(out),
    }
}

pub fn run_callees_text(
    target: &str,
    root: &Path,
    depth: usize,
    limit: usize,
    external: bool,
    json: bool,
) -> Result<String, String> {
    use crate::calls::graph::Qn;
    use crate::calls::{render, traverse};

    let promoted = load_calls_graph(root)?;
    let calls = promoted
        .calls
        .as_ref()
        .ok_or_else(|| "call graph unavailable".to_string())?;
    let qns = crate::calls::cli_helpers::resolve_target_qns(calls, target);
    let mut all_edges = Vec::new();
    // Same frontier accounting as `cli::run_callees`: the one-hop fast path
    // reports a frontier when a resolved direct callee has outgoing calls of
    // its own, deeper walks read it off the traversal. `ast-bro.callees.v1`
    // promises the flag at any depth, on this surface too.
    let mut frontier_truncated = false;
    for qn in &qns {
        if depth <= 1 {
            let edges = traverse::callees_one_hop(calls, qn);
            frontier_truncated |= traverse::one_hop_frontier(calls, &edges, external);
            all_edges.extend(edges);
        } else {
            let info = traverse::callees_info(calls, qn, depth.max(1), external);
            frontier_truncated |= info.frontier_truncated;
            for h in info.hits {
                all_edges.push(h.edge);
            }
        }
    }
    // `hide_external` narrows the query, so apply it once, before counting,
    // and in both output modes — the JSON branch used to ignore the flag the
    // tool schema advertises (matches the CLI fix in `cli::run_callees`).
    if !external {
        all_edges.retain(|e| matches!(e.target, crate::calls::graph::CallTarget::Resolved(_)));
    }
    let first = qns.first().cloned().unwrap_or_else(|| Qn::new(target.to_string()));
    // `limit` bounds the payload, not the walk: the total stays exact so a
    // consumer can tell a capped answer from a complete one (issue #32).
    let total = all_edges.len();
    if all_edges.len() > limit {
        all_edges.truncate(limit);
    }
    if json {
        // Same renderer as the CLI, with no type groups: a consumer reading
        // `ast-bro.callees.v1` can't tell a missing key from a complete
        // answer, so `total` / `truncated` / `frontier_truncated` are always
        // present here too.
        Ok(render::render_callees_json_extended(
            &first,
            depth.max(1),
            &all_edges,
            &[],
            total,
            all_edges.len(),
            frontier_truncated,
            true,
        ))
    } else {
        Ok(render::render_callees_text(
            calls, &first, &all_edges, total, external,
        ))
    }
}
