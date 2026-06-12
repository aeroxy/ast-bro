//! BFS traversal of a `DepGraph` for `deps` and `reverse-deps`.
//!
//! Both directions share one BFS routine parameterised by an
//! adjacency lookup closure.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::deps::graph::{DepEdge, DepGraph, ImportKind};

#[derive(Debug, Clone)]
pub struct DepHit {
    pub depth: usize,
    pub file: PathBuf,
    pub kind: ImportKind,
    pub line: u32,
    pub local_name: Option<String>,
}

/// Forward BFS — what does `start` import (transitively).
pub fn forward(graph: &DepGraph, start: &Path, max_depth: usize) -> Vec<DepHit> {
    let edges_at = |p: &Path| graph.forward.get(p).cloned().unwrap_or_default();
    bfs(start, max_depth, usize::MAX, edges_at, |_| true)
}

/// Reverse BFS — who imports `start` (transitively).
pub fn reverse<F: Fn(&DepEdge) -> bool>(
    graph: &DepGraph,
    start: &Path,
    max_depth: usize,
    limit: usize,
    predicate: F,
) -> Vec<DepHit> {
    let rev = graph.reverse_adjacency();
    let edges_at = |p: &Path| {
        rev.get(p)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|src| DepEdge {
                target: src,
                kind: ImportKind::Bare,
                line: 0,
                local_name: None,
                raw_path: None,
            })
            .collect::<Vec<_>>()
    };
    bfs(start, max_depth, limit, edges_at, predicate)
}

fn bfs<F, P>(
    start: &Path,
    max_depth: usize,
    limit: usize,
    edges_at: F,
    predicate: P,
) -> Vec<DepHit>
where
    F: Fn(&Path) -> Vec<DepEdge>,
    P: Fn(&DepEdge) -> bool,
{
    let mut out = Vec::new();
    // `seen` dedups traversal (filtered nodes are still expanded so deeper
    // qualifying hits stay reachable); `reported` dedups output, so a node
    // first reached via a rejected edge can still be reported when a later
    // qualifying edge points at it. Mirrors `calls::traverse::bfs`.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut reported: HashSet<PathBuf> = HashSet::new();
    let mut q: VecDeque<(PathBuf, usize)> = VecDeque::new();
    let start_buf = start.to_path_buf();
    q.push_back((start_buf.clone(), 0));
    seen.insert(start_buf.clone());
    reported.insert(start_buf);
    while let Some((cur, depth)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for e in edges_at(&cur) {
            let first_visit = seen.insert(e.target.clone());
            if predicate(&e) && !reported.contains(&e.target) {
                reported.insert(e.target.clone());
                out.push(DepHit {
                    depth: depth + 1,
                    file: e.target.clone(),
                    kind: e.kind,
                    line: e.line,
                    local_name: e.local_name,
                });
                if out.len() >= limit {
                    return out;
                }
            }
            if first_visit {
                q.push_back((e.target, depth + 1));
            }
        }
    }
    out
}

/// Per-file depth from `source` in the *combined* (forward + reverse)
/// graph — used by `find-related` for dep-aware boosting.
pub fn neighbourhood_depths(
    graph: &DepGraph,
    source: &Path,
    max_depth: usize,
) -> HashMap<PathBuf, usize> {
    let mut depths: HashMap<PathBuf, usize> = HashMap::new();
    let source_buf = source.to_path_buf();
    depths.insert(source_buf.clone(), 0);
    let rev = graph.reverse_adjacency();
    let mut q: VecDeque<(PathBuf, usize)> = VecDeque::new();
    q.push_back((source_buf, 0));
    while let Some((cur, d)) = q.pop_front() {
        if d >= max_depth {
            continue;
        }
        let mut neighbours: Vec<PathBuf> = graph
            .forward
            .get(&cur)
            .map(|edges| edges.iter().map(|e| e.target.clone()).collect())
            .unwrap_or_default();
        if let Some(reversers) = rev.get(&cur) {
            neighbours.extend(reversers.clone());
        }
        for n in neighbours {
            let new_d = d + 1;
            let prev = depths.get(&n).copied();
            if prev.is_none_or(|p| p > new_d) {
                depths.insert(n.clone(), new_d);
                q.push_back((n, new_d));
            }
        }
    }
    depths
}
