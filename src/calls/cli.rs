//! CLI handlers for `ast-bro callers` and `ast-bro callees`.
//!
//! Two resolution paths under one command surface:
//!   1. Callable targets → walk the call-edge graph (today's behaviour).
//!   2. Type targets (`trait`/`class`/`struct`/`interface`/`enum`/`record`)
//!      → return implementations + constructions (`new T()`, `T::new()`).
//!      `callees` on a type is meaningless and errors with a hint pointing
//!      at the `Type.method` form.
//!
//! When the same name resolves both ways (rare — would need a callable and
//! a type sharing a name and file), both halves are returned.

use std::path::{Path, PathBuf};

use crate::calls::cli_helpers::{resolve_target_full, SymbolKind};
use crate::calls::graph::{CallEdge, CallGraph, CallKindCompat, CallTarget, Confidence, Qn};
use crate::calls::{render, traverse};
use crate::graph_cache;

#[allow(clippy::too_many_arguments)]
pub fn run_callers(
    target: &str,
    path: &Path,
    depth: usize,
    limit: usize,
    include_ambiguous: bool,
    tests: bool,
    exclude_tests: bool,
    rebuild: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    let (root, graph) = match graph_cache::load_for_symbol_query("callers", path, rebuild, json) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    let calls = graph.calls.as_ref().expect("calls half present");

    let candidates = resolve_target_full(calls, target);
    if candidates.is_empty() {
        return crate::cli_error::symbol_not_found("callers", target, json);
    }

    let mut hits = Vec::new();
    let mut frontier_truncated = false;
    let mut type_groups: Vec<TypeCallersGroup> = Vec::new();
    for c in &candidates {
        match c.kind {
            SymbolKind::Callable => {
                // Walk unbounded so the header can report the true total;
                // `--limit` trims the *display* below (issue #32).
                let info = traverse::callers_info(
                    calls,
                    &c.qn,
                    depth.max(1),
                    usize::MAX,
                    |edge| {
                        if !include_ambiguous && matches!(edge.confidence, Confidence::Ambiguous) {
                            return false;
                        }
                        if tests || exclude_tests {
                            let is_test = crate::file_filter::is_test_file(
                                &root.join(&edge.file),
                                &root,
                            );
                            if exclude_tests {
                                if is_test {
                                    return false;
                                }
                            } else if !is_test {
                                return false;
                            }
                        }
                        true
                    },
                );
                frontier_truncated |= info.frontier_truncated;
                hits.extend(info.hits);
            }
            SymbolKind::Type => {
                let mut group = collect_type_callers(calls, &c.qn);
                // Same --tests / --exclude-tests semantics as the callable
                // hits above — implementations and constructions both
                // carry repo-relative file paths.
                if tests || exclude_tests {
                    let keep = |file: &Path| {
                        let is_test =
                            crate::file_filter::is_test_file(&root.join(file), &root);
                        if exclude_tests {
                            !is_test
                        } else {
                            is_test
                        }
                    };
                    group.implementations.retain(|i| keep(&i.file));
                    group.constructions.retain(|e| keep(&e.file));
                    // The filter narrows the query itself; totals report
                    // the post-filter truth (display truncation below
                    // leaves them untouched).
                    group.implementations_total = group.implementations.len();
                    group.constructions_total = group.constructions.len();
                }
                type_groups.push(group);
            }
        }
    }

    // Combined true total across every section — callable hits plus type
    // groups — so `callers <Type> --limit N` cannot report a small total
    // while printing unbounded implementation/construction lists.
    let group_total: usize = type_groups
        .iter()
        .map(|g| g.implementations.len() + g.constructions.len())
        .sum();
    let total = hits.len() + group_total;
    if hits.len() > limit {
        hits.truncate(limit);
    }
    // Type groups share the --limit display budget; their true totals stay
    // on the group (implementations_total / constructions_total).
    let mut remaining = limit.saturating_sub(hits.len());
    for g in &mut type_groups {
        if g.implementations.len() > remaining {
            g.implementations.truncate(remaining);
        }
        remaining -= g.implementations.len();
        if g.constructions.len() > remaining {
            g.constructions.truncate(remaining);
        }
        remaining -= g.constructions.len();
    }
    // At the default depth 1, "the callers have callers" is expected, not a
    // qualification — a note that fires on most queries trains the reader
    // to skip it. The JSON `frontier_truncated` field is always set; the
    // stderr line only accompanies an explicitly deepened walk.
    if frontier_truncated && depth > 1 {
        eprintln!(
            "# note: --depth {} reached with unexplored edges beyond it; raise --depth to walk further",
            depth
        );
    }

    // Bare edges naming the target never enter the reverse index, so the
    // BFS above cannot count them. Surface them (or say they were hidden)
    // so the caller count is never silently low — issue #31.
    let callable_qns: Vec<Qn> = candidates
        .iter()
        .filter(|c| matches!(c.kind, SymbolKind::Callable))
        .map(|c| c.qn.clone())
        .collect();
    let mut unattributed = traverse::unattributed_callers(calls, &callable_qns);
    if tests || exclude_tests {
        unattributed.retain(|e| {
            let is_test = crate::file_filter::is_test_file(&root.join(&e.file), &root);
            if exclude_tests {
                !is_test
            } else {
                is_test
            }
        });
    }
    let mut unattributed_hidden = 0usize;
    if !include_ambiguous && !unattributed.is_empty() {
        unattributed_hidden = unattributed.len();
        eprintln!(
            "# note: {} unresolved call site(s) naming '{}' hidden (--hide-ambiguous); they may be additional callers",
            unattributed_hidden,
            target
        );
        unattributed.clear();
    }
    let unattributed_total = unattributed.len();
    let declarers = traverse::name_declarers(calls, &callable_qns);
    // The section carries its own cap rather than inheriting the resolved
    // list's leftover budget: the worse the resolver did, the *larger* the
    // leftover, so `--limit` would hand the least trustworthy rows the most
    // screen. `--limit` can still shrink the sample, never grow it.
    let sample = if declarers > render::MAX_DECLARERS_TO_LIST {
        0
    } else {
        UNATTRIBUTED_SAMPLE.min(limit)
    };
    if unattributed.len() > sample {
        unattributed.truncate(sample);
    }
    let unattributed = render::Unattributed {
        edges: &unattributed,
        total: unattributed_total,
        hidden: unattributed_hidden,
        declarers,
    };

    let trunc = render::Truncation {
        total,
        frontier_truncated,
    };
    if json {
        println!(
            "{}",
            render::render_callers_json_extended(
                target,
                depth.max(1),
                &hits,
                &type_groups,
                &unattributed,
                &trunc,
                pretty,
            )
        );
    } else {
        print!(
            "{}",
            render::render_callers_text_extended(
                target,
                &hits,
                &type_groups,
                &unattributed,
                &trunc,
            )
        );
    }
    0
}

/// Display cap for the unresolved-call-site section. Small on purpose: the
/// section is a triage sample ordered by evidence strength, not a list to
/// page through — its header always carries the true total.
pub const UNATTRIBUTED_SAMPLE: usize = 25;

#[allow(clippy::too_many_arguments)]
pub fn run_callees(
    target: &str,
    path: &Path,
    depth: usize,
    limit: usize,
    external: bool,
    rebuild: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    let (_root, graph) = match graph_cache::load_for_symbol_query("callees", path, rebuild, json) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    let calls = graph.calls.as_ref().expect("calls half present");

    let candidates = resolve_target_full(calls, target);
    if candidates.is_empty() {
        return crate::cli_error::symbol_not_found("callees", target, json);
    }

    // Split the candidates: callables go through normal call-edge traversal;
    // types expand into their member methods (each method becomes its own
    // callable target, grouped under the type in the output).
    let mut all_edges = Vec::new();
    let mut frontier_truncated = false;
    let mut type_groups: Vec<TypeCalleesGroup> = Vec::new();
    for c in &candidates {
        match c.kind {
            SymbolKind::Callable => {
                if depth <= 1 {
                    let edges = traverse::callees_one_hop(calls, &c.qn);
                    // The one-hop fast path must still report the frontier:
                    // a resolved direct callee with outgoing calls of its
                    // own means --depth 1 stopped a walk that had edges
                    // left. JSON consumers read this flag at any depth.
                    frontier_truncated |= edges.iter().any(|e| match &e.target {
                        CallTarget::Resolved(qn) => {
                            calls.forward.get(qn).is_some_and(|out| !out.is_empty())
                        }
                        _ => false,
                    });
                    all_edges.extend(edges);
                } else {
                    let info = traverse::callees_info(calls, &c.qn, depth.max(1));
                    frontier_truncated |= info.frontier_truncated;
                    for h in info.hits {
                        all_edges.push(h.edge);
                    }
                }
            }
            SymbolKind::Type => {
                type_groups.push(collect_type_callees(calls, &c.qn, depth));
            }
        }
    }
    // `--hide-external` narrows the *query*, so drop those edges before any
    // counting: otherwise the text header, the stderr note and the JSON
    // `total` each describe a slightly different population (PR #38 review).
    if !external {
        all_edges.retain(|e| matches!(e.target, CallTarget::Resolved(_)));
    }
    // Ancestor groups are part of the answer, so they count toward the total
    // and share the display budget — the same rule `callers <Type>` follows,
    // otherwise `truncated` in `ast-bro.callees.v1` describes only `matches`.
    let group_total: usize = type_groups.iter().map(|g| g.ancestors.len()).sum();
    let total = all_edges.len() + group_total;
    if all_edges.len() > limit {
        all_edges.truncate(limit);
    }
    let mut remaining = limit.saturating_sub(all_edges.len());
    for g in &mut type_groups {
        if g.ancestors.len() > remaining {
            g.ancestors.truncate(remaining);
        }
        remaining -= g.ancestors.len();
    }
    let shown = all_edges.len() + type_groups.iter().map(|g| g.ancestors.len()).sum::<usize>();
    // Same rationale as run_callers: the frontier note only accompanies an
    // explicitly deepened walk; JSON always carries the flag.
    if frontier_truncated && depth > 1 {
        eprintln!(
            "# note: --depth {} reached with unexplored edges beyond it; raise --depth to walk further",
            depth
        );
    }
    if total > shown {
        eprintln!(
            "# note: {} callee(s) total; showing {} (raise --limit to see the rest)",
            total, shown
        );
    }

    let first = candidates
        .first()
        .map(|c| c.qn.clone())
        .unwrap_or_else(|| Qn::new(target.to_string()));
    if json {
        println!(
            "{}",
            render::render_callees_json_extended(
                &first,
                depth.max(1),
                &all_edges,
                &type_groups,
                total,
                shown,
                frontier_truncated,
                pretty,
            )
        );
    } else {
        print!(
            "{}",
            render::render_callees_text_extended(
                calls,
                &first,
                &all_edges,
                &type_groups,
                total,
                shown,
                external,
            )
        );
    }
    0
}

/// Aggregate "callers" of a type: implementations + constructions. Designed
/// to feed both the text and JSON renderers without duplicate work.
#[derive(Debug, Clone)]
pub struct TypeCallersGroup {
    pub target_qn: Qn,
    pub kind: String,
    pub implementations: Vec<ImplHit>,
    pub constructions: Vec<CallEdge>,
    /// True counts before `--limit` trimmed the display; set alongside any
    /// truncation so a capped section is never mistaken for a complete one.
    pub implementations_total: usize,
    pub constructions_total: usize,
}

/// Inverse of `TypeCallersGroup` on the *type relationship* graph: walks
/// the inheritance chain upward from the target type, returning each
/// ancestor (parent trait/class/etc.) along with the methods it declares.
/// Mirrors the CLI semantic that "callers = downstream uses, callees =
/// upstream dependencies".
#[derive(Debug, Clone)]
pub struct TypeCalleesGroup {
    pub target_qn: Qn,
    pub kind: String,
    pub ancestors: Vec<Ancestor>,
    /// True ancestor count before `--limit` trimmed the display — so a group
    /// capped to zero still says "3 ancestor(s)" rather than claiming the
    /// type has none.
    pub ancestors_total: usize,
    /// Bases written on the type that we couldn't resolve to a project
    /// type (stdlib traits, third-party crates). Surfaced when `--external`.
    pub unresolved_bases: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Ancestor {
    /// 1 = direct parent, 2 = grandparent, etc. Lets the renderer indent
    /// or annotate; also lets the JSON consumer reconstruct the chain.
    pub depth: usize,
    pub qn: Qn,
    pub kind: String,
    pub file: PathBuf,
    pub line: u32,
    pub methods: Vec<MethodInfo>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub qn: Qn,
    pub file: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ImplHit {
    pub qn: Qn,
    pub kind: String,
    pub file: PathBuf,
    pub line: u32,
}

pub(crate) fn collect_type_callers(calls: &CallGraph, type_qn: &Qn) -> TypeCallersGroup {
    let bare = type_qn.name().to_string();
    let kind = calls
        .types
        .get(type_qn)
        .map(|m| m.kind.clone())
        .unwrap_or_else(|| "type".to_string());

    // Implementations — qns whose `bases` contained this type's normalized
    // name. Stable order: file path then line.
    let mut implementations: Vec<ImplHit> = calls
        .implementors
        .get(&bare)
        .into_iter()
        .flatten()
        .filter_map(|qn| {
            let meta = calls.types.get(qn)?;
            Some(ImplHit {
                qn: qn.clone(),
                kind: meta.kind.clone(),
                file: meta.file.clone(),
                line: meta.line,
            })
        })
        .collect();
    implementations.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));

    // Constructions — call edges with kind == Construct whose bare callee
    // name matches the type. Caller-side metadata (source qn, file, line)
    // already lives on the edge.
    let mut constructions: Vec<CallEdge> = Vec::new();
    for edges in calls.forward.values() {
        for e in edges {
            if !matches!(e.kind, CallKindCompat::Construct) {
                continue;
            }
            let target_name = match &e.target {
                CallTarget::Resolved(qn) => qn.name().to_string(),
                CallTarget::External(s) | CallTarget::Bare(s) => trailing_segment(s),
            };
            if target_name == bare {
                constructions.push(e.clone());
            }
        }
    }
    // Also catch any call whose *receiver* is exactly this type name.
    // Covers:
    //   - `Foo::new()` / `Foo::default()` (Rust associated-fn calls)
    //   - `Foo.parse()` (unit-struct value used as a receiver)
    //   - `Foo.staticMethod()` (static method calls in TS/Java)
    // The receiver is preserved on the edge, so this is a single string
    // comparison per edge — no additional parsing.
    for edges in calls.forward.values() {
        for e in edges {
            if matches!(e.kind, CallKindCompat::Construct) {
                continue; // already counted above
            }
            let recv_matches = e
                .receiver
                .as_deref()
                .map(|r| trailing_segment(r) == bare)
                .unwrap_or(false);
            if recv_matches {
                constructions.push(e.clone());
            }
        }
    }

    constructions.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    constructions.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.source == b.source);

    let implementations_total = implementations.len();
    let constructions_total = constructions.len();
    TypeCallersGroup {
        target_qn: type_qn.clone(),
        kind,
        implementations,
        constructions,
        implementations_total,
        constructions_total,
    }
}

fn trailing_segment(s: &str) -> String {
    s.rsplit("::").next().unwrap_or(s).to_string()
}

/// Walk the inheritance chain upward from `type_qn`, returning each
/// resolved ancestor with the methods it declares. Direct parents have
/// `depth = 1`, grandparents `depth = 2`, etc. Cycles (diamond inheritance
/// in C++/Scala/Python) are broken by a `visited` set keyed on qn.
///
/// Bases that don't resolve to a project-internal type are returned in
/// `unresolved_bases` so callers can surface them under `--external`.
fn collect_type_callees(calls: &CallGraph, type_qn: &Qn, max_depth: usize) -> TypeCalleesGroup {
    let kind = calls
        .types
        .get(type_qn)
        .map(|m| m.kind.clone())
        .unwrap_or_else(|| "type".to_string());

    let mut ancestors: Vec<Ancestor> = Vec::new();
    let mut unresolved_bases: Vec<String> = Vec::new();
    let mut visited: std::collections::HashSet<Qn> = std::collections::HashSet::new();
    visited.insert(type_qn.clone());

    let mut frontier: Vec<Qn> = vec![type_qn.clone()];
    let depth_cap = max_depth.max(1);

    for d in 1..=depth_cap {
        let mut next_frontier: Vec<Qn> = Vec::new();
        for cur_qn in &frontier {
            let cur_meta = match calls.types.get(cur_qn) {
                Some(m) => m,
                None => continue,
            };
            for base in &cur_meta.bases {
                let normalised = normalise_type_name(base);
                let resolved = calls.type_by_name.get(&normalised);
                match resolved {
                    Some(qns) => {
                        for parent_qn in qns {
                            if !visited.insert(parent_qn.clone()) {
                                continue; // diamond — already visited
                            }
                            if let Some(parent_meta) = calls.types.get(parent_qn) {
                                ancestors.push(Ancestor {
                                    depth: d,
                                    qn: parent_qn.clone(),
                                    kind: parent_meta.kind.clone(),
                                    file: parent_meta.file.clone(),
                                    line: parent_meta.line,
                                    methods: methods_of_type(calls, parent_qn),
                                });
                                next_frontier.push(parent_qn.clone());
                            }
                        }
                    }
                    None => {
                        // Only collect at depth 1 — propagating "unresolved
                        // grandparents" is meaningless since we can't walk
                        // them anyway.
                        if d == 1 && !unresolved_bases.contains(&normalised) {
                            unresolved_bases.push(normalised);
                        }
                    }
                }
            }
        }
        if next_frontier.is_empty() {
            break;
        }
        frontier = next_frontier;
    }

    ancestors.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.qn.0.cmp(&b.qn.0)));
    unresolved_bases.sort();

    TypeCalleesGroup {
        target_qn: type_qn.clone(),
        kind,
        ancestors_total: ancestors.len(),
        ancestors,
        unresolved_bases,
    }
}

/// Find every callable qn that lives one `::`-segment under `type_qn`.
/// Returns `MethodInfo` (file/line for each method) from the graph's
/// per-callable metadata so the renderer can jump straight to source.
fn methods_of_type(calls: &CallGraph, type_qn: &Qn) -> Vec<MethodInfo> {
    let prefix = format!("{}::", type_qn.as_str());
    let mut out: Vec<MethodInfo> = calls
        .forward
        .keys()
        .filter(|qn| {
            qn.as_str().starts_with(&prefix)
                && !qn.as_str()[prefix.len()..].contains("::")
        })
        .map(|m_qn| {
            let (file, line) = calls
                .callable_meta
                .get(m_qn)
                .map(|m| (m.file.clone(), m.line))
                .unwrap_or_else(|| {
                    // Last-resort fallback for stale caches without metadata.
                    calls
                        .types
                        .get(type_qn)
                        .map(|m| (m.file.clone(), m.line))
                        .unwrap_or((PathBuf::new(), 0))
                });
            MethodInfo {
                qn: m_qn.clone(),
                file,
                line,
            }
        })
        .collect();
    out.sort_by(|a, b| a.qn.0.cmp(&b.qn.0));
    out
}

/// Strip generics / scope prefix so `crate::base::LanguageAdapter<T>` and
/// `LanguageAdapter` hash to the same key — mirrors the build-side helper.
fn normalise_type_name(name: &str) -> String {
    let mut name = name.trim();
    if let Some(i) = name.find('<') {
        name = &name[..i];
    }
    if let Some(i) = name.find('[') {
        name = &name[..i];
    }
    if let Some(i) = name.rfind('.') {
        name = &name[i + 1..];
    }
    if let Some(i) = name.rfind("::") {
        name = &name[i + 2..];
    }
    name.to_string()
}

/// `ast-bro trace <FROM> <TO>` — shortest static call path between two
/// symbols, with each hop's body inlined. Reuses the same root resolution +
/// lazy call-graph build as `callers`/`callees`.
pub fn run_trace(
    from: &str,
    to: &str,
    path: &Path,
    depth: usize,
    rebuild: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    use crate::calls::trace::{render_trace, TraceOutcome};
    let (root, graph) = match graph_cache::load_for_symbol_query("trace", path, rebuild, json) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    let calls = graph.calls.as_ref().expect("calls half present");
    let (out, outcome) = render_trace(calls, &root, from, to, depth, json, pretty);
    match outcome {
        // Found a path, or both symbols resolved but no static path exists
        // (the graceful response is the answer) → success.
        TraceOutcome::Found | TraceOutcome::NoPath => {
            print!("{}", out);
            if !out.ends_with('\n') {
                println!();
            }
            0
        }
        // `<from>` or `<to>` matched no symbol → rejected call (#36:
        // nothing on stdout).
        TraceOutcome::Unresolved(missing) => {
            crate::cli_error::symbol_not_found("trace", &missing, json)
        }
    }
}

