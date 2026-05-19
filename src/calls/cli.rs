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

use crate::calls::build::build_call_graph;
use crate::calls::cli_helpers::{resolve_target_full, SymbolKind};
use crate::calls::graph::{CallEdge, CallGraph, CallKindCompat, CallTarget, Confidence, Qn};
use crate::calls::{render, traverse};
use crate::deps::cli::find_root_for;
use crate::graph_cache;

pub fn run_callers(
    target: &str,
    path: &Path,
    depth: usize,
    limit: usize,
    include_ambiguous: bool,
    rebuild: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    let root = match resolve_root(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("# note: {}", e);
            return 2;
        }
    };
    let graph = match ensure_graph(&root, rebuild) {
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

    // Split callables vs types and gather hits from each.
    let mut hits = Vec::new();
    let mut type_groups: Vec<TypeCallersGroup> = Vec::new();
    for c in &candidates {
        match c.kind {
            SymbolKind::Callable => {
                hits.extend(traverse::callers(calls, &c.qn, depth.max(1), limit));
            }
            SymbolKind::Type => {
                type_groups.push(collect_type_callers(calls, &c.qn));
            }
        }
    }

    if !include_ambiguous {
        hits.retain(|h| !matches!(h.edge.confidence, Confidence::Ambiguous));
    }
    if hits.len() > limit {
        hits.truncate(limit);
    }

    if json {
        println!(
            "{}",
            render::render_callers_json_extended(
                target,
                depth.max(1),
                &hits,
                &type_groups,
                pretty,
            )
        );
    } else {
        print!(
            "{}",
            render::render_callers_text_extended(target, &hits, &type_groups)
        );
    }
    0
}

pub fn run_callees(
    target: &str,
    path: &Path,
    depth: usize,
    external: bool,
    rebuild: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    let root = match resolve_root(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("# note: {}", e);
            return 2;
        }
    };
    let graph = match ensure_graph(&root, rebuild) {
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

    // Split the candidates: callables go through normal call-edge traversal;
    // types expand into their member methods (each method becomes its own
    // callable target, grouped under the type in the output).
    let mut all_edges = Vec::new();
    let mut type_groups: Vec<TypeCalleesGroup> = Vec::new();
    for c in &candidates {
        match c.kind {
            SymbolKind::Callable => {
                if depth <= 1 {
                    all_edges.extend(traverse::callees_one_hop(calls, &c.qn));
                } else {
                    for h in traverse::callees(calls, &c.qn, depth.max(1)) {
                        all_edges.push(h.edge);
                    }
                }
            }
            SymbolKind::Type => {
                type_groups.push(collect_type_callees(calls, &c.qn, depth));
            }
        }
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

fn collect_type_callers(calls: &CallGraph, type_qn: &Qn) -> TypeCallersGroup {
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

    TypeCallersGroup {
        target_qn: type_qn.clone(),
        kind,
        implementations,
        constructions,
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

/// Walk up from `path` (file or directory) looking for a project manifest
/// (`Cargo.toml`, `pyproject.toml`, `package.json`, …) and use that as the
/// graph root. Stops at the cwd. Without this, `ast-bro callers X ./src`
/// would treat `./src` itself as the root — qns would then look like
/// `main_helpers.rs::…` instead of `src/main_helpers.rs::…`, and any
/// `<file>:<symbol>` filter the user wrote against the project-relative path
/// would silently miss.
fn resolve_root(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("path not found: {}", path.display()));
    }
    find_root_for(path)
}

fn ensure_graph(
    root: &Path,
    force_rebuild: bool,
) -> std::io::Result<std::sync::Arc<crate::graph_cache::UnifiedGraph>> {
    let unified = if force_rebuild {
        graph_cache::shared::rebuild(root)?
    } else {
        graph_cache::get_or_init(root)?
    };
    if unified.calls.is_some() {
        return Ok(unified);
    }
    let promoted = graph_cache::promote_calls(root, |g| build_call_graph(root, &g.deps))?;
    Ok(promoted)
}
