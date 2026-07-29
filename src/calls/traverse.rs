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

/// A traversal plus the fact an agent can't otherwise see: whether the
/// walk stopped because the graph ended, or because `--depth` cut it off
/// while reachable edges remained (issue #32).
#[derive(Debug, Clone)]
pub struct TraversalInfo {
    pub hits: Vec<CallHit>,
    /// True when at least one node at the depth cutoff still had
    /// unexplored outgoing edges.
    pub frontier_truncated: bool,
}

/// Forward — what does `start` call (transitively, deduped by target qn).
pub fn callees(graph: &CallGraph, start: &Qn, max_depth: usize) -> Vec<CallHit> {
    callees_info(graph, start, max_depth).hits
}

/// `callees` + frontier reporting.
pub fn callees_info(graph: &CallGraph, start: &Qn, max_depth: usize) -> TraversalInfo {
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
    callers_info(graph, start, max_depth, limit, predicate).hits
}

/// `callers` + frontier reporting.
pub fn callers_info<F: Fn(&CallEdge) -> bool>(
    graph: &CallGraph,
    start: &Qn,
    max_depth: usize,
    limit: usize,
    predicate: F,
) -> TraversalInfo {
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

/// Multi-source reverse BFS: one pass over the graph for a batch of
/// `(seed, base_depth)` pairs, reporting each caller once at its minimum
/// total depth (`base_depth` + distance from that seed). Equivalent to
/// running `callers` from every seed and keeping the min-depth hit per
/// node — but without re-walking the shared upstream cone once per seed,
/// which made `impact` on widely-constructed types quadratic-ish. Seeds
/// themselves are never reported; `max_total_depth` caps the walk.
pub fn callers_multi<F: Fn(&CallEdge) -> bool>(
    graph: &CallGraph,
    seeds: &[(Qn, usize)],
    max_total_depth: usize,
    predicate: F,
) -> Vec<CallHit> {
    // Sorted seeds + FIFO expansion keep the queue depth-monotonic, so the
    // first visit to a node is at its minimum total depth (the plain BFS
    // invariant, extended to non-uniform starting depths).
    let mut sorted: Vec<&(Qn, usize)> = seeds.iter().collect();
    sorted.sort_by_key(|(_, d)| *d);
    let mut seen: HashSet<Qn> = HashSet::new();
    let mut reported: HashSet<Qn> = HashSet::new();
    let mut q: VecDeque<(Qn, usize)> = VecDeque::new();
    for (qn, depth) in sorted {
        if *depth < max_total_depth && seen.insert(qn.clone()) {
            q.push_back((qn.clone(), *depth));
        }
        reported.insert(qn.clone());
    }
    let mut out = Vec::new();
    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max_total_depth {
            continue;
        }
        for e in graph.reverse.get(&cur).cloned().unwrap_or_default() {
            let next = e.source.clone();
            let first_visit = seen.insert(next.clone());
            // `reported` dedups output separately from traversal, so a node
            // first reached via a predicate-rejected edge can still be
            // reported when a later qualifying edge points at it.
            if predicate(&e) && !reported.contains(&next) {
                reported.insert(next.clone());
                out.push(CallHit {
                    depth: depth + 1,
                    edge: e,
                });
            }
            if first_visit {
                q.push_back((next, depth + 1));
            }
        }
    }
    out
}

/// Bare edges that *name* one of `targets` but were never attributed to a
/// node, so the reverse index (and therefore the `callers` BFS) can't see
/// them. An edge counts when a target qn survives in its candidate set,
/// or — for candidate-less bare edges — when the raw callee name matches a
/// target's terminal name. Dropping these silently makes `callers` report
/// a rename as cheaper than it is (issue #31).
pub fn unattributed_callers(graph: &CallGraph, targets: &[Qn]) -> Vec<CallEdge> {
    use crate::calls::graph::CallTarget;
    let target_set: HashSet<&Qn> = targets.iter().collect();
    let names: HashSet<&str> = targets.iter().map(|q| q.name()).collect();
    let mut out: Vec<CallEdge> = Vec::new();
    for edges in graph.forward.values() {
        for e in edges {
            let CallTarget::Bare(name) = &e.target else { continue };
            let hit = if e.candidates.is_empty() {
                names.contains(name.as_str())
            } else {
                e.candidates.iter().any(|c| target_set.contains(c))
            };
            if hit {
                out.push(e.clone());
            }
        }
    }
    out.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.source.0.cmp(&b.source.0))
    });
    out.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.source == b.source);
    out
}

/// All resolved + external + bare edges originating at `start`. Useful for
/// the `callees` text renderer where we want to surface unresolved targets
/// even when traversal can't recurse into them.
pub fn callees_one_hop(graph: &CallGraph, start: &Qn) -> Vec<CallEdge> {
    graph.forward.get(start).cloned().unwrap_or_default()
}

fn bfs<F, P>(
    start: &Qn,
    max_depth: usize,
    limit: usize,
    edges_at: F,
    predicate: P,
) -> TraversalInfo
where
    F: Fn(&Qn) -> Vec<(Option<Qn>, CallEdge)>,
    P: Fn(&CallEdge) -> bool,
{
    let mut out = Vec::new();
    let mut frontier_truncated = false;
    if limit == 0 {
        return TraversalInfo {
            hits: out,
            frontier_truncated,
        };
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
            // The walk stopped here, not the graph: distinguish "ends at
            // the depth cap with edges left" from "the graph ends" (#32).
            // Only claim a truncated frontier when something genuinely
            // unexplored lies beyond the cap — edges pointing back at
            // already-visited nodes (or already-reported externals) are
            // not "more", and the raise---depth hint would be a lie.
            if !frontier_truncated
                && edges_at(&cur).into_iter().any(|(next, edge)| match next {
                    Some(n) => !seen.contains(&n),
                    None => !reported_ext.contains(&edge.target.name_or_raw()),
                })
            {
                frontier_truncated = true;
            }
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
                        return TraversalInfo {
                            hits: out,
                            frontier_truncated,
                        };
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
                    return TraversalInfo {
                        hits: out,
                        frontier_truncated,
                    };
                }
            }
            if first_visit {
                q.push_back((next, depth + 1));
            }
        }
    }
    TraversalInfo {
        hits: out,
        frontier_truncated,
    }
}
