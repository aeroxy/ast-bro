//! BFS traversal of a `CallGraph` for `callers` and `callees`.
//!
//! Mirrors `src/deps/traverse.rs` shape so the rendering code stays
//! symmetric. Both directions traverse over `CallEdge`s.

use crate::calls::graph::{CallEdge, CallGraph, Qn};
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct CallHit {
    pub depth: usize,
    pub edge: CallEdge,
}

/// Forward — what does `start` call (transitively, deduped by target qn).
pub fn callees(graph: &CallGraph, start: &Qn, max_depth: usize) -> Vec<CallHit> {
    let edges_at = |qn: &Qn| graph.forward.get(qn).cloned().unwrap_or_default();
    bfs(
        start,
        max_depth,
        usize::MAX,
        |qn| {
            edges_at(qn)
                .into_iter()
                .map(|e| {
                    // External/bare edges are emitted but carry no node to
                    // recurse into (`None`); resolved targets are both
                    // emitted and expanded.
                    let next = match &e.target {
                        crate::calls::graph::CallTarget::Resolved(t) => Some(t.clone()),
                        _ => None,
                    };
                    (next, e)
                })
                .collect()
        },
        |_| true,
    )
}

/// Reverse — who calls `start` (transitively).
pub fn callers<F: Fn(&CallEdge) -> bool>(
    graph: &CallGraph,
    start: &Qn,
    max_depth: usize,
    limit: usize,
    predicate: F,
) -> Vec<CallHit> {
    let edges_at = |qn: &Qn| graph.reverse.get(qn).cloned().unwrap_or_default();
    bfs(
        start,
        max_depth,
        limit,
        |qn| {
            edges_at(qn)
                .into_iter()
                .map(|e| (Some(e.source.clone()), e))
                .collect()
        },
        predicate,
    )
}

/// All resolved + external + bare edges originating at `start`. Useful for
/// the `callees` text renderer where we want to surface unresolved targets
/// even when traversal can't recurse into them.
pub fn callees_one_hop(graph: &CallGraph, start: &Qn) -> Vec<CallEdge> {
    graph.forward.get(start).cloned().unwrap_or_default()
}

fn bfs<F, P>(start: &Qn, max_depth: usize, limit: usize, edges_at: F, predicate: P) -> Vec<CallHit>
where
    F: Fn(&Qn) -> Vec<(Option<Qn>, CallEdge)>,
    P: Fn(&CallEdge) -> bool,
{
    let mut out = Vec::new();
    if limit == 0 {
        return out;
    }
    // Two sets with different jobs: `seen` dedups *traversal* (every node is
    // expanded once, including ones whose edge the predicate rejects — a
    // test caller at depth 2 must still be reachable through a non-test
    // intermediate when filtering with --tests). `reported` dedups *output*,
    // so a node first reached via a rejected edge can still be reported
    // when a later qualifying edge points at it.
    let mut seen: HashSet<Qn> = HashSet::new();
    let mut reported: HashSet<Qn> = HashSet::new();
    // External/bare edges have no qn; dedup them by their raw callee text
    // so the same external symbol called from many nodes appears once.
    let mut reported_ext: HashSet<String> = HashSet::new();
    let mut q: VecDeque<(Qn, usize)> = VecDeque::new();
    q.push_back((start.clone(), 0));
    seen.insert(start.clone());
    reported.insert(start.clone());
    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for (next, edge) in edges_at(&cur) {
            let Some(next) = next else {
                // Emit-only edge (external/bare callee) — nothing to expand.
                if predicate(&edge) && reported_ext.insert(edge.target.name_or_raw()) {
                    out.push(CallHit {
                        depth: depth + 1,
                        edge,
                    });
                    if out.len() >= limit {
                        return out;
                    }
                }
                continue;
            };
            let first_visit = seen.insert(next.clone());
            if predicate(&edge) && !reported.contains(&next) {
                reported.insert(next.clone());
                out.push(CallHit {
                    depth: depth + 1,
                    edge,
                });
                if out.len() >= limit {
                    return out;
                }
            }
            if first_visit {
                q.push_back((next, depth + 1));
            }
        }
    }
    out
}
