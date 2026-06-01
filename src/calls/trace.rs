//! `ast-bro trace <FROM> <TO>` — the static call path between two symbols.
//!
//! "How does `<from>` reach `<to>`?" A multi-source BFS over the call graph's
//! `forward` edges (the callees direction) finds the shortest chain from any
//! qn matching `<from>` to any qn matching `<to>`, then inlines each hop's
//! source body so a flow question is answered in ONE call instead of the agent
//! manually chaining `callees`. Output is size-capped; when no static path
//! exists (the chain broke at a dynamic-dispatch / framework boundary) we
//! degrade gracefully — inlining both endpoints plus the target file's sibling
//! callables so the agent still has somewhere to look.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use crate::calls::cli_helpers::resolve_target_qns;
use crate::calls::graph::{CallEdge, CallGraph, CallTarget, Qn};
use crate::core::{ParseResult, JSON_SCHEMA_TRACE};

/// Total inlined-body budget for one trace response. Beyond this, remaining
/// hops are listed header-only with a note.
const MAX_TOTAL_CHARS: usize = 24_000;
/// Per-symbol body cap. A larger body is truncated with a marker (use `show`
/// for the full text).
const MAX_BODY_CHARS: usize = 2_400;
/// Cap on the sibling-callable list shown on the graceful no-path response.
const MAX_SIBLINGS: usize = 30;

/// Result class of a trace, so the CLI can choose an exit code: a found path
/// and a no-path-but-endpoints-resolved are both exit 0 (informative); only a
/// `<from>`/`<to>` that matches no symbol is a non-zero "bad input".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceOutcome {
    Found,
    NoPath,
    Unresolved,
}

/// One hop on a resolved path: the qn reached and the edge taken into it.
struct Hop {
    qn: Qn,
    via: CallEdge,
}

/// A found path: the starting qn plus the ordered hops to the target.
struct Found {
    start: Qn,
    hops: Vec<Hop>,
}

/// Multi-source / multi-target BFS over `forward` (callees) edges. Returns the
/// shortest path from any `froms` qn to any `tos` qn, or `None` when the
/// target is unreachable within `max_depth` hops.
fn find_path(calls: &CallGraph, froms: &[Qn], tos: &[Qn], max_depth: usize) -> Option<Found> {
    use std::collections::HashSet;
    let to_set: HashSet<&Qn> = tos.iter().collect();

    // from == to: a zero-hop "path".
    for f in froms {
        if to_set.contains(f) {
            return Some(Found { start: f.clone(), hops: Vec::new() });
        }
    }

    let mut visited: HashSet<Qn> = HashSet::new();
    // child qn -> (parent qn, edge taken parent->child)
    let mut parent: HashMap<Qn, (Qn, CallEdge)> = HashMap::new();
    let mut queue: VecDeque<(Qn, usize)> = VecDeque::new();
    for f in froms {
        if visited.insert(f.clone()) {
            queue.push_back((f.clone(), 0));
        }
    }

    while let Some((cur, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let Some(edges) = calls.forward.get(&cur) else { continue };
        for e in edges {
            let CallTarget::Resolved(next) = &e.target else { continue };
            if visited.contains(next) {
                continue;
            }
            parent.insert(next.clone(), (cur.clone(), e.clone()));
            if to_set.contains(next) {
                return Some(reconstruct(next, &parent));
            }
            visited.insert(next.clone());
            queue.push_back((next.clone(), depth + 1));
        }
    }
    None
}

/// Walk the parent chain back from `target` to its source `from` qn.
fn reconstruct(target: &Qn, parent: &HashMap<Qn, (Qn, CallEdge)>) -> Found {
    let mut hops: Vec<Hop> = Vec::new();
    let mut cur = target.clone();
    while let Some((prev, edge)) = parent.get(&cur) {
        hops.push(Hop { qn: cur.clone(), via: edge.clone() });
        cur = prev.clone();
    }
    hops.reverse();
    Found { start: cur, hops }
}

/// Symbol suffix to feed `find_symbols`: the qn's scope chain after the file,
/// `::`-joined → dotted. `src/a.rs::Foo::bar` → `Foo.bar`; a module-free fn
/// falls back to its terminal name.
fn qn_symbol(qn: &Qn) -> String {
    match qn.as_str().find("::") {
        Some(i) => qn.as_str()[i + 2..].replace("::", "."),
        None => qn.name().to_string(),
    }
}

/// Cache of parsed files so a multi-hop path through one file parses it once.
struct BodyCache<'a> {
    root: &'a Path,
    parsed: HashMap<String, Option<ParseResult>>,
}

impl<'a> BodyCache<'a> {
    fn new(root: &'a Path) -> Self {
        Self { root, parsed: HashMap::new() }
    }

    fn parse(&mut self, file: &str) -> Option<&ParseResult> {
        self.parsed
            .entry(file.to_string())
            .or_insert_with(|| crate::parse_file(&self.root.join(file)))
            .as_ref()
    }

    /// Source body for `qn`, preferring the match whose start line equals the
    /// graph's recorded callable line (disambiguates overloads). Returns the
    /// (possibly truncated) body, or `None` when the file/symbol can't be read.
    fn body(&mut self, qn: &Qn, meta_line: Option<u32>) -> Option<String> {
        let symbol = qn_symbol(qn);
        let res = self.parse(qn.file())?;
        let matches = crate::core::find_symbols(res, &symbol);
        if matches.is_empty() {
            return None;
        }
        let chosen = meta_line
            .and_then(|ln| matches.iter().find(|m| m.start_line as u32 == ln))
            .unwrap_or(&matches[0]);
        Some(truncate_body(&chosen.source))
    }
}

fn truncate_body(src: &str) -> String {
    if src.len() <= MAX_BODY_CHARS {
        return src.to_string();
    }
    // Cut on a char boundary at the last newline before the cap.
    let mut cut = MAX_BODY_CHARS.min(src.len());
    while !src.is_char_boundary(cut) {
        cut -= 1;
    }
    let slice = &src[..cut];
    let cut = slice.rfind('\n').map(|i| i + 1).unwrap_or(slice.len());
    format!("{}… (body truncated — use `show` for the full text)\n", &src[..cut])
}

fn meta_line(calls: &CallGraph, qn: &Qn) -> Option<u32> {
    calls.callable_meta.get(qn).map(|m| m.line)
}

fn meta_loc(calls: &CallGraph, qn: &Qn) -> (String, u32, String) {
    match calls.callable_meta.get(qn) {
        Some(m) => (
            m.file.display().to_string(),
            m.line,
            m.kind.clone(),
        ),
        None => (qn.file().to_string(), 0, String::new()),
    }
}

/// Render a trace. Returns `(output, outcome)`. On `NoPath` the output still
/// carries the graceful endpoint + sibling context; on `Unresolved` it is a
/// note (or a JSON error envelope) and the caller exits non-zero.
pub fn render_trace(
    calls: &CallGraph,
    root: &Path,
    from: &str,
    to: &str,
    max_depth: usize,
    json: bool,
    pretty: bool,
) -> (String, TraceOutcome) {
    let froms = resolve_target_qns(calls, from);
    let tos = resolve_target_qns(calls, to);

    if froms.is_empty() || tos.is_empty() {
        let missing = if froms.is_empty() { from } else { to };
        if json {
            let v = serde_json::json!({
                "schema": JSON_SCHEMA_TRACE,
                "from": from, "to": to, "found": false,
                "error": format!("no callable symbol matches '{}'", missing),
                "hops": [],
            });
            return (to_json(&v, pretty), TraceOutcome::Unresolved);
        }
        let msg = format!(
            "# note: no callable symbol matches '{}' (try a more specific suffix like 'Type.method').\n",
            missing
        );
        return (msg, TraceOutcome::Unresolved);
    }

    let max_depth = max_depth.max(1);
    match find_path(calls, &froms, &tos, max_depth) {
        Some(found) => {
            let mut cache = BodyCache::new(root);
            let out = if json {
                render_found_json(calls, &found, from, to, &mut cache, pretty)
            } else {
                render_found_text(calls, &found, from, to, &mut cache)
            };
            (out, TraceOutcome::Found)
        }
        None => {
            let mut cache = BodyCache::new(root);
            // Graceful: show both endpoints + the target file's siblings.
            let from_qn = &froms[0];
            let to_qn = &tos[0];
            let out = if json {
                render_nopath_json(calls, from, to, from_qn, to_qn, &mut cache, pretty)
            } else {
                render_nopath_text(calls, from, to, from_qn, to_qn, &mut cache)
            };
            (out, TraceOutcome::NoPath)
        }
    }
}

// ---------- text ----------

fn render_found_text(
    calls: &CallGraph,
    found: &Found,
    from: &str,
    to: &str,
    cache: &mut BodyCache,
) -> String {
    let hop_count = found.hops.len();
    let mut out = format!(
        "# trace: {} → {}   ({} hop{})\n",
        from,
        to,
        hop_count,
        if hop_count == 1 { "" } else { "s" },
    );

    let mut budget = MAX_TOTAL_CHARS;
    let mut step = 1usize;

    // Node 0 — the start.
    push_node_text(&mut out, calls, &found.start, step, None, cache, &mut budget);
    for hop in &found.hops {
        step += 1;
        out.push_str(&format!(
            "   ↓ {} (line {})\n",
            hop.via.kind.as_str(),
            hop.via.line,
        ));
        push_node_text(&mut out, calls, &hop.qn, step, Some(&hop.via), cache, &mut budget);
    }
    out
}

fn push_node_text(
    out: &mut String,
    calls: &CallGraph,
    qn: &Qn,
    step: usize,
    via: Option<&CallEdge>,
    cache: &mut BodyCache,
    budget: &mut usize,
) {
    let (file, line, kind) = meta_loc(calls, qn);
    let conf = via
        .map(|e| format!("  [{}]", e.confidence.as_str()))
        .unwrap_or_default();
    let kind_tag = if kind.is_empty() { String::new() } else { format!("  [{}]", kind) };
    out.push_str(&format!("{}. {}  {}:{}{}{}\n", step, qn, file, line, kind_tag, conf));

    if *budget == 0 {
        out.push_str("       … (body budget reached — use `show` for this symbol)\n");
        return;
    }
    if let Some(body) = cache.body(qn, meta_line(calls, qn)) {
        for l in body.lines() {
            out.push_str("       ");
            out.push_str(l);
            out.push('\n');
        }
        *budget = budget.saturating_sub(body.len());
    }
    out.push('\n');
}

fn render_nopath_text(
    calls: &CallGraph,
    from: &str,
    to: &str,
    from_qn: &Qn,
    to_qn: &Qn,
    cache: &mut BodyCache,
) -> String {
    let mut out = format!(
        "# trace: {} → {}   no static call path found\n\
         # the chain likely breaks at a dynamic-dispatch / framework boundary \
         (callback, trait object, route handler). Inlining both endpoints below.\n\n",
        from, to,
    );
    let mut budget = MAX_TOTAL_CHARS;
    out.push_str("## from:\n");
    push_node_text(&mut out, calls, from_qn, 1, None, cache, &mut budget);
    out.push_str("## to:\n");
    push_node_text(&mut out, calls, to_qn, 1, None, cache, &mut budget);

    let sibs = siblings(calls, to_qn);
    if !sibs.is_empty() {
        out.push_str(&format!(
            "## other callables in {} (siblings of the target):\n",
            to_qn.file()
        ));
        for qn in &sibs {
            let (_f, line, kind) = meta_loc(calls, qn);
            let kind_tag = if kind.is_empty() { String::new() } else { format!("  [{}]", kind) };
            out.push_str(&format!("   - {}  :{}{}\n", qn.name(), line, kind_tag));
        }
    }
    out
}

/// Callables defined in the same file as `target`, excluding it. Sorted +
/// capped so the no-path response stays bounded.
fn siblings(calls: &CallGraph, target: &Qn) -> Vec<Qn> {
    let file = target.file();
    let mut out: Vec<Qn> = calls
        .callable_meta
        .keys()
        .filter(|qn| qn.file() == file && *qn != target)
        .cloned()
        .collect();
    out.sort();
    out.truncate(MAX_SIBLINGS);
    out
}

// ---------- json ----------

fn node_json(calls: &CallGraph, qn: &Qn, via: Option<&CallEdge>, cache: &mut BodyCache) -> serde_json::Value {
    let (file, line, kind) = meta_loc(calls, qn);
    let body = cache.body(qn, meta_line(calls, qn));
    serde_json::json!({
        "qn": qn.as_str(),
        "file": file,
        "line": line,
        "kind": kind,
        "via": via.map(|e| e.kind.as_str()),
        "via_line": via.map(|e| e.line),
        "confidence": via.map(|e| e.confidence.as_str()),
        "body": body,
    })
}

fn render_found_json(
    calls: &CallGraph,
    found: &Found,
    from: &str,
    to: &str,
    cache: &mut BodyCache,
    pretty: bool,
) -> String {
    let mut hops = vec![node_json(calls, &found.start, None, cache)];
    for hop in &found.hops {
        hops.push(node_json(calls, &hop.qn, Some(&hop.via), cache));
    }
    let v = serde_json::json!({
        "schema": JSON_SCHEMA_TRACE,
        "from": from,
        "to": to,
        "found": true,
        "hop_count": found.hops.len(),
        "hops": hops,
    });
    to_json(&v, pretty)
}

fn render_nopath_json(
    calls: &CallGraph,
    from: &str,
    to: &str,
    from_qn: &Qn,
    to_qn: &Qn,
    cache: &mut BodyCache,
    pretty: bool,
) -> String {
    let sibs: Vec<serde_json::Value> = siblings(calls, to_qn)
        .iter()
        .map(|qn| {
            let (file, line, kind) = meta_loc(calls, qn);
            serde_json::json!({ "qn": qn.as_str(), "file": file, "line": line, "kind": kind })
        })
        .collect();
    let v = serde_json::json!({
        "schema": JSON_SCHEMA_TRACE,
        "from": from,
        "to": to,
        "found": false,
        "endpoints": [
            node_json(calls, from_qn, None, cache),
            node_json(calls, to_qn, None, cache),
        ],
        "siblings": sibs,
    });
    to_json(&v, pretty)
}

fn to_json(v: &serde_json::Value, pretty: bool) -> String {
    if pretty {
        serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("<json error: {e}>"))
    } else {
        serde_json::to_string(v).unwrap_or_else(|e| format!("<json error: {e}>"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::graph::{CallEdge, CallKindCompat, CallTarget, Confidence};
    use std::path::PathBuf;

    fn edge(src: &str, dst: &str) -> CallEdge {
        CallEdge {
            source: Qn::new(src),
            target: CallTarget::Resolved(Qn::new(dst)),
            kind: CallKindCompat::Call,
            line: 1,
            file: PathBuf::from("f"),
            confidence: Confidence::Exact,
            receiver: None,
            candidates: Vec::new(),
        }
    }

    fn graph_with(edges: Vec<CallEdge>) -> CallGraph {
        let mut g = CallGraph::empty(PathBuf::from("."));
        for e in edges {
            g.forward.entry(e.source.clone()).or_default().push(e);
        }
        g
    }

    #[test]
    fn finds_multi_hop_path() {
        // a -> b -> c
        let g = graph_with(vec![edge("f::a", "f::b"), edge("f::b", "f::c")]);
        let found = find_path(&g, &[Qn::new("f::a")], &[Qn::new("f::c")], 12)
            .expect("path a->c exists");
        assert_eq!(found.start, Qn::new("f::a"));
        let chain: Vec<&str> = found.hops.iter().map(|h| h.qn.as_str()).collect();
        assert_eq!(chain, vec!["f::b", "f::c"]);
    }

    #[test]
    fn no_path_returns_none() {
        // a -> b ; isolated c
        let g = graph_with(vec![edge("f::a", "f::b")]);
        assert!(find_path(&g, &[Qn::new("f::a")], &[Qn::new("f::c")], 12).is_none());
    }

    #[test]
    fn depth_cap_blocks_far_target() {
        // a -> b -> c, but max_depth 1 only reaches b.
        let g = graph_with(vec![edge("f::a", "f::b"), edge("f::b", "f::c")]);
        assert!(find_path(&g, &[Qn::new("f::a")], &[Qn::new("f::c")], 1).is_none());
        assert!(find_path(&g, &[Qn::new("f::a")], &[Qn::new("f::b")], 1).is_some());
    }

    #[test]
    fn from_equals_to_is_zero_hops() {
        let g = graph_with(vec![edge("f::a", "f::b")]);
        let found = find_path(&g, &[Qn::new("f::a")], &[Qn::new("f::a")], 12).unwrap();
        assert!(found.hops.is_empty());
        assert_eq!(found.start, Qn::new("f::a"));
    }

    #[test]
    fn qn_symbol_dots_the_scope() {
        assert_eq!(qn_symbol(&Qn::new("src/a.rs::Foo::bar")), "Foo.bar");
        assert_eq!(qn_symbol(&Qn::new("src/a.rs::helper")), "helper");
    }
}
