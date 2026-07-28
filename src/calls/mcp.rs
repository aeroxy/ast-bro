//! MCP-side wrappers reused by `src/mcp/tools.rs` for `callers` / `callees`.
//!
//! Returns `String` so the MCP layer can wrap with its own `CallResult`.

use std::path::Path;

pub fn run_callers_text(target: &str, root: &Path, depth: usize, limit: usize, include_ambiguous: bool, json: bool) -> String {
    use crate::calls::build::build_call_graph;
    use crate::calls::{render, traverse};
    use crate::graph_cache;

    let unified = match graph_cache::get_or_init(root) {
        Ok(u) => u,
        Err(e) => return format!("# error: {}", e),
    };
    let promoted = if unified.calls.is_some() {
        unified
    } else {
        match graph_cache::promote_calls(root, |g| build_call_graph(root, &g.deps)) {
            Ok(p) => p,
            Err(e) => return format!("# error: {}", e),
        }
    };
    let calls = match &promoted.calls {
        Some(c) => c,
        None => return "# error: call graph unavailable".to_string(),
    };
    let qns = crate::calls::cli_helpers::resolve_target_qns(calls, target);
    let mut hits = Vec::new();
    let mut frontier_truncated = false;
    for qn in &qns {
        // Walk unbounded so the header carries the true total; `--limit`
        // trims the display (issue #32). Filter inside the traversal so
        // dropped ambiguous edges don't consume the limit.
        let info = traverse::callers_info(calls, qn, depth.max(1), usize::MAX, |e| {
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
    // Same unattributed-edge surfacing as the CLI path (issue #31). MCP has
    // no stderr channel, so when `include_ambiguous` is off the hidden count
    // is prepended to the response text instead of printed as a note —
    // silently dropping it would make "0 callers" read as authoritative.
    let mut unattributed = traverse::unattributed_callers(calls, &qns);
    let mut hidden_note = String::new();
    let mut unattributed_hidden = 0usize;
    if !include_ambiguous && !unattributed.is_empty() {
        unattributed_hidden = unattributed.len();
        hidden_note = format!(
            "# note: {} unresolved call site(s) naming '{}' hidden (include_ambiguous: false); they may be additional callers\n",
            unattributed_hidden,
            target
        );
        unattributed.clear();
    }
    // Share the limit display budget, as in the CLI path.
    let unattributed_total = unattributed.len();
    let remaining = limit.saturating_sub(hits.len());
    if unattributed.len() > remaining {
        unattributed.truncate(remaining);
    }
    let trunc = render::Truncation {
        total,
        frontier_truncated,
    };
    if json {
        render::render_callers_json(
            target,
            depth.max(1),
            &hits,
            &unattributed,
            unattributed_total,
            unattributed_hidden,
            &trunc,
            true,
        )
    } else {
        format!(
            "{}{}",
            hidden_note,
            render::render_callers_text(target, &hits, &unattributed, unattributed_total, &trunc)
        )
    }
}

pub fn run_trace_text(from: &str, to: &str, root: &Path, depth: usize, json: bool) -> String {
    use crate::calls::build::build_call_graph;
    use crate::calls::trace::render_trace;
    use crate::graph_cache;

    let unified = match graph_cache::get_or_init(root) {
        Ok(u) => u,
        Err(e) => return format!("# error: {}", e),
    };
    let promoted = if unified.calls.is_some() {
        unified
    } else {
        match graph_cache::promote_calls(root, |g| build_call_graph(root, &g.deps)) {
            Ok(p) => p,
            Err(e) => return format!("# error: {}", e),
        }
    };
    let calls = match &promoted.calls {
        Some(c) => c,
        None => return "# error: call graph unavailable".to_string(),
    };
    let (out, outcome) = render_trace(calls, root, from, to, depth.max(1), json, false);
    match outcome {
        // MCP has no stderr channel, so the diagnostic must be the response
        // itself — as a valid `ast-bro.trace.v1` doc under `json` so the
        // schema promise holds even for a rejected call.
        crate::calls::trace::TraceOutcome::Unresolved(missing) => {
            if json {
                serde_json::json!({
                    "schema": crate::core::JSON_SCHEMA_TRACE,
                    "from": from,
                    "to": to,
                    "found": false,
                    "error": format!("no callable symbol matches '{}'", missing),
                    "hops": [],
                })
                .to_string()
            } else {
                format!(
                    "# note: no callable symbol matches '{}' (try a more specific suffix like 'Type.method').\n",
                    missing
                )
            }
        }
        _ => out,
    }
}

pub fn run_callees_text(target: &str, root: &Path, depth: usize, external: bool, json: bool) -> String {
    use crate::calls::build::build_call_graph;
    use crate::calls::graph::Qn;
    use crate::calls::{render, traverse};
    use crate::graph_cache;

    let unified = match graph_cache::get_or_init(root) {
        Ok(u) => u,
        Err(e) => return format!("# error: {}", e),
    };
    let promoted = if unified.calls.is_some() {
        unified
    } else {
        match graph_cache::promote_calls(root, |g| build_call_graph(root, &g.deps)) {
            Ok(p) => p,
            Err(e) => return format!("# error: {}", e),
        }
    };
    let calls = match &promoted.calls {
        Some(c) => c,
        None => return "# error: call graph unavailable".to_string(),
    };
    let qns = crate::calls::cli_helpers::resolve_target_qns(calls, target);
    let mut all_edges = Vec::new();
    for qn in &qns {
        if depth <= 1 {
            all_edges.extend(traverse::callees_one_hop(calls, qn));
        } else {
            for h in traverse::callees(calls, qn, depth.max(1)) {
                all_edges.push(h.edge);
            }
        }
    }
    let first = qns.first().cloned().unwrap_or_else(|| Qn::new(target.to_string()));
    if json {
        render::render_callees_json(&first, depth.max(1), &all_edges, true)
    } else {
        render::render_callees_text(calls, &first, &all_edges, external)
    }
}
