//! BFS traversal of a `CallGraph` for `callers` and `callees`.
//!
//! Mirrors `src/deps/traverse.rs` shape so the rendering code stays
//! symmetric. Both directions traverse over `CallEdge`s.

use crate::calls::graph::{CallEdge, CallGraph, CallTarget, Qn};
use crate::UNLIMITED;
use std::collections::{BTreeMap, HashSet, VecDeque};

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
    callees_info(graph, start, max_depth, true).hits
}

/// `callees` + frontier reporting. `include_external` is the query's own
/// `--hide-external` setting, applied *here* rather than by filtering the
/// hits afterwards: an external callee that the caller will not display is
/// not evidence of a truncated frontier either, and filtering after the walk
/// leaves the flag claiming there is more to see when raising `--depth` would
/// show nothing.
pub fn callees_info(
    graph: &CallGraph,
    start: &Qn,
    max_depth: usize,
    include_external: bool,
) -> TraversalInfo {
    let edges_at = |qn: &Qn| graph.forward.get(qn).cloned().unwrap_or_default();
    bfs(
        start,
        max_depth,
        UNLIMITED,
        |qn| {
            edges_at(qn)
                .into_iter()
                .map(|e| {
                    // External/bare edges are emitted but carry no node to
                    // recurse into (`None`); resolved targets are both
                    // emitted and expanded.
                    let next = match &e.target {
                        CallTarget::Resolved(t) => Some(t.clone()),
                        _ => None,
                    };
                    (next, e)
                })
                .collect()
        },
        move |e| include_external || matches!(e.target, CallTarget::Resolved(_)),
    )
}

/// Does a one-hop callee list leave a frontier behind? `--depth 1` stopped a
/// walk that had somewhere to go only when a resolved direct callee has an
/// outgoing edge that would be *visible*: with `--hide-external`, external
/// and bare continuations are not "more to see", so claiming a frontier
/// would make the raise-`--depth` hint a lie. Shared by the CLI and MCP
/// one-hop fast paths so the two can't drift.
pub fn one_hop_frontier(graph: &CallGraph, edges: &[CallEdge], include_external: bool) -> bool {
    edges.iter().any(|e| match &e.target {
        CallTarget::Resolved(t) => graph.forward.get(t).is_some_and(|out| {
            out.iter()
                .any(|next| include_external || matches!(next.target, CallTarget::Resolved(_)))
        }),
        _ => false,
    })
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
    // Depth buckets, not one FIFO. A single queue is only depth-monotonic
    // when every entry in it is within one depth of the next, which sorted
    // seeds alone don't give: seeds at depth 2 and depth 5 make the queue
    // pop the 5 before the 3s discovered under the 2, so a node reachable
    // both ways gets `seen` (and reported) at 6 instead of 3 — the wrong
    // depth, and dropped entirely if that exceeds `max_total_depth`. A map
    // keyed by depth pops in nondecreasing order whatever the spread, which
    // is what makes "first visit = minimum total depth" true.
    let mut seen: HashSet<Qn> = HashSet::new();
    let mut reported: HashSet<Qn> = HashSet::new();
    let mut pending: BTreeMap<usize, Vec<Qn>> = BTreeMap::new();
    let mut sorted: Vec<&(Qn, usize)> = seeds.iter().collect();
    // Sorted so a qn seeded twice keeps its lower depth (`seen` admits the
    // first one it sees).
    sorted.sort_by_key(|(_, d)| *d);
    for (qn, depth) in sorted {
        if *depth < max_total_depth && seen.insert(qn.clone()) {
            pending.entry(*depth).or_default().push(qn.clone());
        }
        reported.insert(qn.clone());
    }
    let mut out = Vec::new();
    while let Some((depth, batch)) = pending.pop_first() {
        if depth >= max_total_depth {
            continue;
        }
        for cur in batch {
            for e in graph.reverse.get(&cur).cloned().unwrap_or_default() {
                let next = e.source.clone();
                let first_visit = seen.insert(next.clone());
                // `reported` dedups output separately from traversal, so a
                // node first reached via a predicate-rejected edge can still
                // be reported when a later qualifying edge points at it.
                if predicate(&e) && !reported.contains(&next) {
                    reported.insert(next.clone());
                    out.push(CallHit {
                        depth: depth + 1,
                        edge: e,
                    });
                }
                if first_visit {
                    pending.entry(depth + 1).or_default().push(next);
                }
            }
        }
    }
    out
}

/// How many project symbols declare the terminal name of `targets` — the
/// breadth an unattributed edge naming that name is ambiguous across. One
/// means the name discriminates: an edge naming it can only have meant this
/// symbol (or something outside the project). Nine (`new` in this repo) or
/// thirty-nine (`close` in pgjdbc) means it names all of them equally, i.e.
/// none of them in particular.
pub fn name_declarers(graph: &CallGraph, targets: &[Qn]) -> usize {
    targets
        .iter()
        .map(|q| {
            graph
                .symbol_table
                .get(q.name())
                .map_or(1, |qns| qns.len().max(1))
        })
        .max()
        .unwrap_or(0)
}

/// Terminal segment of a receiver as written at the call site, with the
/// language's separators normalized the way pass A normalizes them
/// (`Foo\Bar::m()`, `a.b.C.m()`, `a::b::C::m()` all yield `C`).
fn receiver_tail(recv: &str) -> &str {
    let recv = recv.trim();
    recv.rsplit(['\\', '.', ':']).next().unwrap_or(recv)
}

/// Enclosing-scope segment of a qn (`a/b.rs::Foo::method` → `Foo`), or
/// `None` for a free function whose only scope is the file.
fn enclosing_scope(qn: &Qn) -> Option<&str> {
    let s = qn.as_str();
    let name_start = s.rfind("::")?;
    let scope = &s[..name_start];
    match scope.rfind("::") {
        Some(i) => Some(&scope[i + 2..]),
        // Only the file segment is left — a free function has no type scope.
        None => None,
    }
}

/// Bare edges that *name* one of `targets` but were never attributed to a
/// node, so the reverse index (and therefore the `callers` BFS) can't see
/// them. An edge counts when a target qn survives in its candidate set,
/// or — for candidate-less bare edges — when the raw callee name matches a
/// target's terminal name. Dropping these silently makes `callers` report
/// a rename as cheaper than it is (issue #31).
///
/// The name test alone is far too generous: candidate sets are keyed on the
/// bare name, so every `Vec::new()` in the tree carries `CliError::new`
/// among its candidates. Two things narrow it without inventing type
/// inference:
///
///  1. **Receiver check** (here) — reuse pass A's rule: a receiver naming a
///     project type other than the target's own says the call site meant
///     that other type, so it is not evidence about the target.
///  2. **Discrimination gate** (`name_declarers`, applied by this
///     function's callers) — when many project symbols share the name, the
///     whole list is evidence about all of them and the display degrades to
///     a one-line count.
///
/// Results are ordered by evidence strength (narrowest candidate set first),
/// so a capped sample keeps the rows worth triaging.
pub fn unattributed_callers(graph: &CallGraph, targets: &[Qn]) -> Vec<CallEdge> {
    use crate::calls::graph::CallTarget;
    let target_set: HashSet<&Qn> = targets.iter().collect();
    let names: HashSet<&str> = targets.iter().map(|q| q.name()).collect();
    let scopes: HashSet<&str> = targets.iter().filter_map(enclosing_scope).collect();
    let mut out: Vec<CallEdge> = Vec::new();
    for edges in graph.forward.values() {
        for e in edges {
            let CallTarget::Bare(name) = &e.target else { continue };
            let hit = if e.candidates.is_empty() {
                names.contains(name.as_str())
            } else {
                e.candidates.iter().any(|c| target_set.contains(c))
            };
            if hit && receiver_admits(graph, e, &scopes) {
                out.push(e.clone());
            }
        }
    }
    out.sort_by(|a, b| {
        ambiguity_breadth(graph, a)
            .cmp(&ambiguity_breadth(graph, b))
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
            .then(a.source.0.cmp(&b.source.0))
    });
    out.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.source == b.source);
    out
}

/// Can this call site's receiver still be one of the targets' enclosing
/// types? `true` whenever the receiver leaves that open — self-like
/// receivers, absent receivers, and receivers whose type we cannot know (a
/// local, a parameter, an external type) all stay in. Only a receiver that
/// *names a project type* different from the target's scope is rejected,
/// which is pass A's rule applied one layer later. Rejecting on any
/// unmatched receiver name would drop the real `connection.close()` sites
/// along with the noise.
fn receiver_admits(graph: &CallGraph, e: &CallEdge, target_scopes: &HashSet<&str>) -> bool {
    use crate::calls::resolve::receiver_is_self_like;
    let Some(recv) = e.receiver.as_deref() else {
        return true;
    };
    if receiver_is_self_like(Some(recv)) {
        return true;
    }
    let tail = receiver_tail(recv);
    target_scopes.contains(tail) || !graph.type_by_name.contains_key(tail)
}

/// How many project symbols an unattributed edge is spread across: its
/// candidate count when pass C left one, otherwise the number of project
/// symbols sharing the callee name. Lower is stronger evidence.
fn ambiguity_breadth(graph: &CallGraph, e: &CallEdge) -> usize {
    if !e.candidates.is_empty() {
        return e.candidates.len();
    }
    graph
        .symbol_table
        .get(e.target.name_or_raw().as_str())
        .map_or(1, |qns| qns.len().max(1))
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
            //
            // Expandable edges count whatever the predicate says: a rejected
            // edge can still lead to a qualifying node deeper (that's why
            // `callers --tests` walks through non-test intermediates). An
            // emit-only edge has no deeper side, so it only counts when it
            // would actually be emitted.
            if !frontier_truncated
                && edges_at(&cur).into_iter().any(|(next, edge)| match next {
                    Some(n) => !seen.contains(&n),
                    None => predicate(&edge) && !reported_ext.contains(&edge.target.name_or_raw()),
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
