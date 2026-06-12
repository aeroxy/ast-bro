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
    for qn in &qns {
        // Filter inside the traversal so dropped ambiguous edges don't
        // consume the limit (mirrors the CLI's run_callers).
        hits.extend(traverse::callers(calls, qn, depth.max(1), limit, |e| {
            include_ambiguous
                || !matches!(e.confidence, crate::calls::graph::Confidence::Ambiguous)
        }));
    }
    if hits.len() > limit {
        hits.truncate(limit);
    }
    if json {
        render::render_callers_json(target, depth.max(1), &hits, true)
    } else {
        render::render_callers_text(target, &hits)
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
    let (out, _outcome) = render_trace(calls, root, from, to, depth.max(1), json, false);
    out
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
