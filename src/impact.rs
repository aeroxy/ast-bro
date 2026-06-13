//! `impact` — cross-file impact analysis for a symbol.
//!
//! Wraps `callers`, `callees`, file-level `reverse-deps`, and test-detection
//! into one output so an agent sees the blast radius in a single command.
//!
//! Four output modes: `Deps` (what it calls/imports), `Dependents` (who
//! calls/imports it), `Tests` (affected tests only), `All` (default — all
//! three sections plus transitive depth grouping).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use colored::Colorize;
use serde::Serialize;
use serde_json::json;

use crate::calls::cli::collect_type_callers;
use crate::calls::cli_helpers::{resolve_target_full, ResolvedTarget, SymbolKind};
use crate::calls::graph::{CallEdge, CallGraph, CallTarget, Confidence, Qn};
use crate::calls::traverse::{self, CallHit};
use crate::deps::traverse as dep_traverse;
use crate::deps::DepGraph;
use crate::file_filter::is_test_file;
use crate::graph_cache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactMode {
    Deps,
    Dependents,
    Tests,
    All,
}

impl ImpactMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "deps" | "depend" | "dependencies" => Some(Self::Deps),
            "dependents" | "dependent" | "reverse" | "callers" => Some(Self::Dependents),
            "tests" | "test" => Some(Self::Tests),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

pub struct ImpactOptions {
    pub depth: usize,
    pub limit: usize,
    pub mode: ImpactMode,
    pub include_ambiguous: bool,
    pub tests: bool,
    pub exclude_tests: bool,
    pub json: bool,
    pub pretty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactSection {
    pub title: String,
    pub entries: Vec<ImpactEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactEntry {
    pub qn: String,
    pub file: String,
    pub line: u32,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub target_qn: String,
    pub target_file: String,
    pub target_line: u32,
    pub target_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<ImpactSection>>,
    pub transitive_count: usize,
    pub test_count: usize,
}

pub fn run_impact(
    target: &str,
    path: &Path,
    opts: &ImpactOptions,
    rebuild: bool,
) -> i32 {
    let root = match crate::project_root::find_root_for(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("# note: {}", e);
            return 2;
        }
    };
    let graph = match graph_cache::ensure_with_calls(&root, rebuild) {
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

    if candidates.len() > 1 {
        eprintln!(
            "# note: '{}' matched {} symbols; showing all. Use a more specific suffix (e.g. 'Type.method') to narrow.",
            target,
            candidates.len(),
        );
    }

    let mut reports = Vec::new();
    for c in &candidates {
        reports.push(compute_impact(c, calls, &graph.deps, &root, opts));
    }

    if opts.json {
        println!(
            "{}",
            render_json(target, &reports, opts.pretty, candidates.len())
        );
    } else {
        print!("{}", render_text(&reports, candidates.len()));
    }
    0
}

fn compute_impact(
    c: &ResolvedTarget,
    calls: &CallGraph,
    deps: &DepGraph,
    root: &Path,
    opts: &ImpactOptions,
) -> ImpactReport {
    let (file_path, target_line, target_kind) = match c.kind {
        SymbolKind::Callable => {
            let meta = calls.callable_meta.get(&c.qn);
            let file = meta.map(|m| m.file.clone()).unwrap_or_else(|| PathBuf::from(c.qn.file()));
            let line = meta.map(|m| m.line).unwrap_or(0);
            let kind = meta.map(|m| m.kind.as_str()).unwrap_or("function").to_string();
            (file, line, kind)
        }
        SymbolKind::Type => {
            let tmeta = calls.types.get(&c.qn);
            let file = tmeta.map(|m| m.file.clone()).unwrap_or_else(|| PathBuf::from(c.qn.file()));
            let line = tmeta.map(|m| m.line).unwrap_or(0);
            let kind = tmeta.map(|m| m.kind.as_str()).unwrap_or("type").to_string();
            (file, line, kind)
        }
    };

    let mut report = ImpactReport {
        target_qn: c.qn.as_str().to_string(),
        target_file: crate::project_root::relative_posix(&file_path, root)
            .unwrap_or_else(|| file_path.display().to_string()),
        target_line,
        target_kind: target_kind.clone(),
        sections: Some(Vec::new()),
        transitive_count: 0,
        test_count: 0,
    };

    let sections = report.sections.as_mut().unwrap();

    if matches!(opts.mode, ImpactMode::Deps | ImpactMode::All) {
        sections.push(build_callees_section(c, calls, opts, root));
        sections.push(build_file_deps_section(&file_path, deps, root, opts));
    }

    if matches!(opts.mode, ImpactMode::Dependents | ImpactMode::All) {
        sections.push(build_callers_section(c, calls, opts, root));
        sections.push(build_file_reverse_deps_section(&file_path, deps, root, opts));
    }

    let mut transitive = BTreeMap::new();
    let mut test_calls: Vec<CallHit> = Vec::new();

    // Test filtering happens inside the traversal so excluded edges don't
    // consume --limit. Ambiguous edges are dropped here too when
    // --hide-ambiguous is set, matching the direct sections' retain.
    let all_callers = traverse::callers(calls, &c.qn, opts.depth.max(1), opts.limit, |e| {
        if !opts.include_ambiguous && matches!(e.confidence, Confidence::Ambiguous) {
            return false;
        }
        if !opts.tests && !opts.exclude_tests {
            return true;
        }
        let is_test = is_test_file(&root.join(&e.file), root);
        if opts.exclude_tests {
            !is_test
        } else {
            is_test
        }
    });
    for h in &all_callers {
        let abs = root.join(&h.edge.file);
        let is_test = is_test_file(&abs, root);
        if is_test && !opts.exclude_tests {
            test_calls.push(h.clone());
        }
        if h.depth > 1 {
            if opts.exclude_tests && is_test {
                continue;
            }
            if opts.tests && !is_test {
                continue;
            }
            let entry = ImpactEntry {
                qn: h.edge.source.as_str().to_string(),
                file: h.edge.file.display().to_string(),
                line: h.edge.line,
                kind: if h.edge.kind == crate::calls::graph::CallKindCompat::Implement {
                    calls
                        .types
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "type".into())
                } else {
                    calls
                        .callable_meta
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "function".into())
                },
                confidence: Some(h.edge.confidence.as_str().to_string()),
                depth: Some(h.depth),
            };
            transitive
                .entry(h.depth)
                .or_insert_with(Vec::new)
                .push(entry);
        }
    }

    if c.kind == SymbolKind::Type {
        // Construct/receiver edges never resolve to a type qn, so the
        // reverse index (and `traverse::callers` above) can't see them.
        // Type dependents are gathered from construction edges instead:
        //   depth 1: implementors + callables constructing the type itself
        //   depth 2: callers of those constructors; callables constructing
        //            an implementor
        //   depth 3+: `traverse::callers` from each construction site.
        let mut seen_transitive: std::collections::HashSet<Qn> =
            std::collections::HashSet::new();
        let add_transitive = |depth: usize,
                                  source: &Qn,
                                  file: &Path,
                                  line: u32,
                                  confidence: Confidence,
                                  is_test: bool,
                                  transitive: &mut BTreeMap<usize, Vec<ImpactEntry>>,
                                  test_calls: &mut Vec<CallHit>,
                                  seen: &mut std::collections::HashSet<Qn>| {
            if opts.exclude_tests && is_test {
                return;
            }
            if opts.tests && !is_test {
                return;
            }
            // Dedup before the test push too — the same source reached via
            // several construction sites must appear once in both sections.
            if !seen.insert(source.clone()) {
                return;
            }
            if is_test {
                test_calls.push(CallHit {
                    depth,
                    edge: CallEdge {
                        source: source.clone(),
                        target: CallTarget::Resolved(c.qn.clone()),
                        kind: crate::calls::graph::CallKindCompat::Construct,
                        line,
                        file: file.to_path_buf(),
                        confidence,
                        receiver: None,
                        candidates: Vec::new(),
                    },
                });
            }
            transitive.entry(depth).or_default().push(ImpactEntry {
                qn: source.as_str().to_string(),
                file: file.display().to_string(),
                line,
                kind: calls
                    .callable_meta
                    .get(source)
                    .map(|m| m.kind.clone())
                    .unwrap_or_else(|| "function".into()),
                confidence: Some(confidence.as_str().to_string()),
                depth: Some(depth),
            });
        };

        // Same predicate as the callable traversal above — filtering inside
        // the BFS keeps excluded edges from consuming --limit, and ambiguous
        // edges are dropped when --hide-ambiguous is set.
        let keep_by_test_flags = |e: &CallEdge| {
            if !opts.include_ambiguous && matches!(e.confidence, Confidence::Ambiguous) {
                return false;
            }
            if !opts.tests && !opts.exclude_tests {
                return true;
            }
            let is_test = is_test_file(&root.join(&e.file), root);
            if opts.exclude_tests {
                !is_test
            } else {
                is_test
            }
        };

        // Collect transitive dependents at their MINIMUM depth. Each
        // construction site spawns its own reverse BFS; emitting hits eagerly
        // into a shared `seen` set would pin a symbol to whichever site
        // reached it first, so a deeper site processed earlier could mask a
        // shallower path and report an inflated depth. Resolving the minimum
        // here makes the result independent of construction-site order.
        let mut cand: std::collections::HashMap<Qn, (usize, PathBuf, u32, Confidence)> =
            std::collections::HashMap::new();
        let consider =
            |cand: &mut std::collections::HashMap<Qn, (usize, PathBuf, u32, Confidence)>,
             depth: usize,
             source: &Qn,
             file: &Path,
             line: u32,
             conf: Confidence| {
                cand.entry(source.clone())
                    .and_modify(|slot| {
                        if depth < slot.0 {
                            *slot = (depth, file.to_path_buf(), line, conf);
                        }
                    })
                    .or_insert((depth, file.to_path_buf(), line, conf));
            };

        // Callables that construct the target type itself are depth-1
        // dependents: shown in the callers section, recorded here for the
        // affected-tests section, and used as seeds for the deeper walk. They
        // are marked seen so they never reappear as transitive entries.
        let group = collect_type_callers(calls, &c.qn);
        for e in &group.constructions {
            let is_test = is_test_file(&root.join(&e.file), root);
            // Insert unconditionally (mark seen) as before; only the test_calls
            // push respects --hide-ambiguous, matching build_callers_section's
            // retain so an ambiguous construction isn't hidden there yet counted here.
            let fresh = seen_transitive.insert(e.source.clone());
            let ambiguous_hidden =
                !opts.include_ambiguous && matches!(e.confidence, Confidence::Ambiguous);
            if fresh && is_test && !opts.exclude_tests && !ambiguous_hidden {
                test_calls.push(CallHit {
                    depth: 1,
                    edge: e.clone(),
                });
            }
            // Callers of the constructors are depth-2+ dependents.
            if opts.depth > 1 {
                for h in traverse::callers(
                    calls,
                    &e.source,
                    opts.depth - 1,
                    opts.limit,
                    keep_by_test_flags,
                ) {
                    consider(
                        &mut cand,
                        h.depth + 1,
                        &h.edge.source,
                        &h.edge.file,
                        h.edge.line,
                        h.edge.confidence,
                    );
                }
            }
        }

        if let Some(impls) = calls.implementors.get(c.qn.name()) {
            for qn in impls {
                let meta = calls.types.get(qn);
                let file = meta.map(|m| m.file.clone()).unwrap_or_else(|| PathBuf::from(qn.file()));
                let line = meta.map(|m| m.line).unwrap_or(0);
                let abs = root.join(&file);
                // Implementors (depth 1) are shown in the callers section and
                // never repeated as transitive entries.
                seen_transitive.insert(qn.clone());
                if is_test_file(&abs, root) && !opts.exclude_tests {
                    test_calls.push(CallHit {
                        depth: 1,
                        edge: CallEdge {
                            source: qn.clone(),
                            target: CallTarget::Resolved(c.qn.clone()),
                            kind: crate::calls::graph::CallKindCompat::Implement,
                            line,
                            file: file.clone(),
                            confidence: Confidence::Exact,
                            receiver: None,
                            candidates: Vec::new(),
                        },
                    });
                }
                // Whoever constructs an implementor depends on the base type
                // at depth 2; callers of those constructors at depth 3+.
                if opts.depth > 1 {
                    let group = collect_type_callers(calls, qn);
                    for e in &group.constructions {
                        // Skip ambiguous construction seeds when hidden, so
                        // they don't leak into depth-2 transitive/test output
                        // (matches build_callers_section's retain).
                        if !opts.include_ambiguous
                            && matches!(e.confidence, Confidence::Ambiguous)
                        {
                            continue;
                        }
                        consider(&mut cand, 2, &e.source, &e.file, e.line, e.confidence);
                        if opts.depth > 2 {
                            for h in traverse::callers(
                                calls,
                                &e.source,
                                opts.depth - 2,
                                opts.limit,
                                keep_by_test_flags,
                            ) {
                                consider(
                                    &mut cand,
                                    h.depth + 2,
                                    &h.edge.source,
                                    &h.edge.file,
                                    h.edge.line,
                                    h.edge.confidence,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Emit each transitive dependent once, at its resolved minimum depth.
        // Sorting by (depth, qn) keeps output deterministic regardless of the
        // HashMap's iteration order.
        let mut resolved: Vec<(Qn, usize, PathBuf, u32, Confidence)> = cand
            .into_iter()
            .map(|(qn, (depth, file, line, conf))| (qn, depth, file, line, conf))
            .collect();
        resolved.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.as_str().cmp(b.0.as_str())));
        for (source, depth, file, line, conf) in &resolved {
            let is_test = is_test_file(&root.join(file), root);
            add_transitive(
                *depth,
                source,
                file,
                *line,
                *conf,
                is_test,
                &mut transitive,
                &mut test_calls,
                &mut seen_transitive,
            );
        }
    }

    if matches!(opts.mode, ImpactMode::All) && !transitive.is_empty() {
        let total: usize = transitive.values().map(|v| v.len()).sum();
        report.transitive_count = total;
        let mut section = ImpactSection {
            title: format!("! {} symbols transitively affected (depth {})", total, opts.depth),
            entries: Vec::new(),
        };
        for (depth, entries) in &transitive {
            for e in entries {
                let mut e = e.clone();
                e.depth = Some(*depth);
                section.entries.push(e);
            }
        }
        sections.push(section);
    }

    if opts.tests || matches!(opts.mode, ImpactMode::Tests | ImpactMode::All) {
        let excluded = opts.exclude_tests;
        let count = test_calls.len();
        report.test_count = count;
        let (display, entries) = if excluded {
            (
                "affected tests (0, excluded by --exclude-tests)".to_string(),
                Vec::new(),
            )
        } else {
            (
                format!("affected tests ({})", count),
                test_calls
                    .iter()
                    .map(|h| ImpactEntry {
                        qn: h.edge.source.as_str().to_string(),
                        file: h.edge.file.display().to_string(),
                        line: h.edge.line,
                        // Implementor test entries carry a type qn (not in
                        // callable_meta), so resolve their kind from the type
                        // table — same as the transitive section above.
                        kind: if h.edge.kind == crate::calls::graph::CallKindCompat::Implement {
                            calls
                                .types
                                .get(&h.edge.source)
                                .map(|m| m.kind.clone())
                                .unwrap_or_else(|| "type".into())
                        } else {
                            calls
                                .callable_meta
                                .get(&h.edge.source)
                                .map(|m| m.kind.clone())
                                .unwrap_or_else(|| "function".into())
                        },
                        confidence: Some(h.edge.confidence.as_str().to_string()),
                        depth: Some(h.depth),
                    })
                    .collect(),
            )
        };
        sections.push(ImpactSection { title: display, entries });
    }

    report
}

/// `--tests` / `--exclude-tests` verdict for a repo-relative file path.
fn passes_test_flags(file: &Path, root: &Path, opts: &ImpactOptions) -> bool {
    if !opts.tests && !opts.exclude_tests {
        return true;
    }
    let is_test = is_test_file(&root.join(file), root);
    if opts.exclude_tests {
        !is_test
    } else {
        is_test
    }
}

fn build_callees_section(
    c: &ResolvedTarget,
    calls: &CallGraph,
    opts: &ImpactOptions,
    root: &Path,
) -> ImpactSection {
    let mut edges: Vec<CallEdge> = Vec::new();
    if c.kind == SymbolKind::Callable {
        let one_hop = traverse::callees_one_hop(calls, &c.qn);
        for e in one_hop {
            if !opts.include_ambiguous
                && matches!(e.confidence, Confidence::Ambiguous)
            {
                continue;
            }
            edges.push(e);
        }
    }
    let entries: Vec<ImpactEntry> = edges
        .into_iter()
        .filter_map(|e| {
                let (qn, file, line) = match &e.target {
                    CallTarget::Resolved(q) => {
                        let meta = calls.callable_meta.get(q);
                        (
                            q.as_str().to_string(),
                            meta.map(|m| m.file.clone()).unwrap_or_else(|| e.file.clone()),
                            meta.map(|m| m.line).unwrap_or(e.line),
                        )
                    }
                    CallTarget::External(s) => {
                        (format!("[external] {s}"), e.file.clone(), e.line)
                    }
                    CallTarget::Bare(s) => {
                        (format!("[unresolved] {s}"), e.file.clone(), e.line)
                    }
                };
                if !passes_test_flags(&file, root, opts) {
                    return None;
                }
                Some(ImpactEntry {
                    qn,
                    file: file.display().to_string(),
                    line,
                    kind: e.kind.as_str().to_string(),
                    confidence: Some(e.confidence.as_str().to_string()),
                    depth: None,
                })
            })
            .collect();
    ImpactSection {
        title: format!("→ calls ({})", entries.len()),
        entries,
    }
}

fn build_callers_section(
    c: &ResolvedTarget,
    calls: &CallGraph,
    opts: &ImpactOptions,
    root: &Path,
) -> ImpactSection {
    // Collect type-specific dependents first. For a type, implementors and
    // construction sites are the most relevant edges, so they go ahead of the
    // BFS callers and survive the per-section cap below (the BFS tail yields).
    let mut hits: Vec<CallHit> = Vec::new();
    if c.kind == SymbolKind::Type {
        if let Some(impls) = calls.implementors.get(c.qn.name()) {
            for qn in impls {
                if let Some(meta) = calls.types.get(qn) {
                    hits.push(CallHit {
                        depth: 1,
                        edge: CallEdge {
                            source: qn.clone(),
                            target: CallTarget::Resolved(c.qn.clone()),
                            kind: crate::calls::graph::CallKindCompat::Implement,
                            line: meta.line,
                            file: meta.file.clone(),
                            confidence: Confidence::Exact,
                            receiver: None,
                            candidates: Vec::new(),
                        },
                    });
                }
            }
        }
        // Construction sites (`Foo {}`, `Foo::new()`, `Foo.method()`) —
        // these edges target a bare name, never the type qn, so the
        // reverse-index lookup below can't return them.
        let group = collect_type_callers(calls, &c.qn);
        for e in group.constructions {
            hits.push(CallHit { depth: 1, edge: e });
        }
    }
    // Filter inside the traversal so dropped edges don't consume the limit.
    hits.extend(traverse::callers(calls, &c.qn, 1, opts.limit, |e| {
        if !opts.include_ambiguous && matches!(e.confidence, Confidence::Ambiguous) {
            return false;
        }
        if opts.tests || opts.exclude_tests {
            let is_test = is_test_file(&root.join(&e.file), root);
            if opts.exclude_tests {
                return !is_test;
            }
            return is_test;
        }
        true
    }));
    if !opts.include_ambiguous {
        hits.retain(|h| !matches!(h.edge.confidence, Confidence::Ambiguous));
    }
    hits.retain(|h| passes_test_flags(&h.edge.file, root, opts));
    // Type implementors/constructions were collected first, so when the merged
    // list exceeds the per-section limit the appended BFS callers are trimmed,
    // not the type-specific dependents. (No-op for callable targets.)
    if hits.len() > opts.limit {
        hits.truncate(opts.limit);
    }
    ImpactSection {
        title: if c.kind == SymbolKind::Type {
            format!("← implemented / called by ({})", hits.len())
        } else {
            format!("← called by ({})", hits.len())
        },
        entries: hits
            .into_iter()
            .map(|h| ImpactEntry {
                qn: h.edge.source.as_str().to_string(),
                file: h.edge.file.display().to_string(),
                line: h.edge.line,
                kind: if h.edge.kind == crate::calls::graph::CallKindCompat::Implement {
                    calls
                        .types
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "type".into())
                } else {
                    calls
                        .callable_meta
                        .get(&h.edge.source)
                        .map(|m| m.kind.clone())
                        .unwrap_or_else(|| "function".into())
                },
                confidence: Some(h.edge.confidence.as_str().to_string()),
                depth: None,
            })
            .collect(),
    }
}

fn build_file_deps_section(
    file: &Path,
    deps: &DepGraph,
    root: &Path,
    opts: &ImpactOptions,
) -> ImpactSection {
    let deps_file = root.join(file);
    let hits = dep_traverse::forward(deps, &deps_file, 1);
    let hits: Vec<_> = hits
        .into_iter()
        .filter(|h| passes_test_flags(&h.file, root, opts))
        .collect();
    ImpactSection {
        title: format!("→ imports (file, {})", hits.len()),
        entries: hits
            .into_iter()
            .map(|h| {
                let rel = crate::project_root::relative_posix(&h.file, root)
                    .unwrap_or_else(|| h.file.display().to_string());
                ImpactEntry {
                    qn: rel.clone(),
                    file: rel,
                    line: h.line,
                    kind: format!("{:?}", h.kind).to_lowercase(),
                    confidence: None,
                    depth: None,
                }
            })
            .collect(),
    }
}

fn build_file_reverse_deps_section(
    file: &Path,
    deps: &DepGraph,
    root: &Path,
    opts: &ImpactOptions,
) -> ImpactSection {
    let deps_file = root.join(file);
    // Test filtering happens inside the traversal so excluded importers
    // don't consume --limit (same pattern as the caller traversals).
    let hits = dep_traverse::reverse(deps, &deps_file, 1, opts.limit, |e| {
        passes_test_flags(&e.target, root, opts)
    });
    ImpactSection {
        title: format!("← imported by (file, {})", hits.len()),
        entries: hits
            .into_iter()
            .map(|h| {
                let rel = crate::project_root::relative_posix(&h.file, root)
                    .unwrap_or_else(|| h.file.display().to_string());
                ImpactEntry {
                    qn: rel.clone(),
                    file: rel,
                    line: h.line,
                    kind: format!("{:?}", h.kind).to_lowercase(),
                    confidence: None,
                    depth: None,
                }
            })
            .collect(),
    }
}

fn render_text(reports: &[ImpactReport], candidate_count: usize) -> String {
    let mut out = String::new();
    for r in reports {
        out.push_str(&format!(
            "{} {} {} ({}:{})\n",
            "⊕".bold(),
            r.target_kind.dimmed(),
            r.target_qn.split("::").last().unwrap_or(&r.target_qn).yellow(),
            colorize_file(&r.target_file),
            colorize_line(r.target_line),
        ));
        if let Some(sections) = &r.sections {
            for s in sections {
                if s.entries.is_empty() {
                    continue;
                }
                out.push_str(&format!("\n  {}\n", s.title.bold()));
                for e in &s.entries {
                    let depth_tag = match e.depth {
                        Some(d) if d > 1 => format!(" depth={}", d).dimmed().to_string(),
                        _ => String::new(),
                    };
                    let conf_tag = match &e.confidence {
                        Some(c) if c != "Exact" => format!(" {}", colorize_confidence(c)),
                        _ => String::new(),
                    };
                    out.push_str(&format!(
                        "    {}{} {} {}{}{}\n",
                        if e.qn.starts_with('[') { "" } else { "→ " }.dimmed(),
                        e.kind.dimmed(),
                        name_or_raw_segment(&e.qn).yellow(),
                        colorize_file_path(&e.qn, &e.file),
                        depth_tag,
                        conf_tag,
                    ));
                }
            }
        }
        if candidate_count > 1 {
            out.push('\n');
        }
    }
    out
}

fn render_json(
    target_raw: &str,
    reports: &[ImpactReport],
    pretty: bool,
    candidate_count: usize,
) -> String {
    let doc = json!({
        "schema": "ast-bro.impact.v1",
        "target": target_raw,
        "candidates": candidate_count,
        "impacts": reports,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    } else {
        serde_json::to_string(&doc).unwrap_or_default()
    }
}

fn colorize_file(p: &str) -> String {
    p.cyan().bold().to_string()
}

fn colorize_line(line: u32) -> String {
    if line == 0 {
        String::new()
    } else {
        format!(":{}", line).truecolor(150, 150, 150).to_string()
    }
}

fn colorize_confidence(c: &str) -> String {
    match c {
        "Exact" => "Exact".green().to_string(),
        "Inferred" => "Inferred".yellow().to_string(),
        "Ambiguous" => "Ambiguous".red().to_string(),
        other => other.to_string(),
    }
}

fn colorize_file_path(qn: &str, file: &str) -> String {
    let display = if qn.contains("::") {
        let parts: Vec<&str> = qn.splitn(2, "::").collect();
        if parts.len() == 2 { parts[0] } else { file }
    } else {
        file
    };
    format!(" ({})", display).truecolor(100, 100, 100).to_string()
}

/// Terminal `::` segment of a qn, or the string unchanged when it's a
/// tagged pseudo-entry like `[external] serde_json::to_string`.
fn name_or_raw_segment(qn: &str) -> &str {
    if qn.starts_with('[') {
        return qn;
    }
    match qn.rfind("::") {
        Some(i) => &qn[i + 2..],
        None => qn,
    }
}

pub mod mcp {
    use super::*;
    use serde_json::Value;

    pub fn run_impact(mut args: Value) -> crate::mcp::tools::CallResult {
        // Pre-rename clients sent `include_ambiguous` (true = show); the new
        // `hide_ambiguous` arg inverts the polarity. Translate so old clients
        // keep their behavior, matching the callers/callees/deps tools.
        crate::mcp::tools::translate_renamed_bool(&mut args, "include_ambiguous", "hide_ambiguous");
        #[derive(serde::Deserialize)]
        struct Args {
            target: String,
            #[serde(default = "default_dot")]
            path: PathBuf,
            #[serde(default = "default_two")]
            depth: usize,
            #[serde(default = "default_limit")]
            limit: usize,
            #[serde(default = "default_mode")]
            mode: String,
            /// Ambiguous edges are shown by default (CLI parity) — set this to
            /// drop them. They carry the construction sites of struct-literal usage.
            #[serde(default)]
            hide_ambiguous: bool,
            #[serde(default)]
            tests: bool,
            #[serde(default)]
            exclude_tests: bool,
            /// Text by default (MCP convention); `true` returns
            /// `ast-bro.impact.v1` JSON.
            #[serde(default)]
            json: bool,
        }
        fn default_dot() -> PathBuf { PathBuf::from(".") }
        fn default_two() -> usize { 2 }
        fn default_limit() -> usize { 200 }
        fn default_mode() -> String { "all".into() }

        let a: Args = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => {
                return crate::mcp::tools::CallResult::Error(format!("bad args: {e}"))
            }
        };
        let mode = match ImpactMode::parse(&a.mode) {
            Some(m) => m,
            None => {
                return crate::mcp::tools::CallResult::Error(format!(
                    "unknown --mode '{}'. Expected: deps, dependents, tests, all",
                    a.mode,
                ))
            }
        };
        let root = match crate::project_root::find_root_for(&a.path) {
            Ok(r) => r,
            Err(e) => return crate::mcp::tools::CallResult::Error(e),
        };
        let graph = match graph_cache::ensure_with_calls(&root, false) {
            Ok(g) => g,
            Err(e) => {
                return crate::mcp::tools::CallResult::Error(format!(
                    "# error: {}", e
                ))
            }
        };
        let calls = match &graph.calls {
            Some(c) => c,
            None => {
                return crate::mcp::tools::CallResult::Error(
                    "# error: call graph is empty".into(),
                )
            }
        };
        let candidates = resolve_target_full(calls, &a.target);
        if candidates.is_empty() {
            return crate::mcp::tools::CallResult::Error(format!(
                "# note: no symbol matches '{}' (try a more specific suffix like 'Type.method').",
                a.target,
            ));
        }
        let opts = ImpactOptions {
            depth: a.depth,
            limit: a.limit,
            mode,
            include_ambiguous: !a.hide_ambiguous,
            tests: a.tests,
            exclude_tests: a.exclude_tests,
            json: true,
            pretty: true,
        };
        let mut reports = Vec::new();
        for c in &candidates {
            reports.push(compute_impact(c, calls, &graph.deps, &root, &opts));
        }
        let body = if a.json {
            render_json(&a.target, &reports, true, candidates.len())
        } else {
            render_text(&reports, candidates.len())
        };
        crate::mcp::tools::CallResult::Text(body)
    }
}
