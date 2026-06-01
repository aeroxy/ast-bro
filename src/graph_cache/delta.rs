//! Per-file invalidation for the unified graph cache.
//!
//! Replaces the prior "any-delta = full rebuild" behaviour: when files are
//! added / modified / removed, we drop just the affected entries from the
//! in-memory graph, re-extract the changed files, and merge the result
//! back. The deps half always patches; the calls half patches when present.
//!
//! Stale-edge tradeoff (calls half, v1): edges originating from *unchanged*
//! files that point to a qn defined in a *changed* file may keep their
//! pre-update target. For deletions this means the edge points to a qn
//! that no longer exists; for renames it means the edge points to the old
//! name. Both are recovered by `--rebuild`. The plan called for a
//! bare-name re-resolution pass to fix this; that's a v2 add — current
//! call sites observed in real edits are dominated by intra-file changes,
//! and the lazy promotion path means a fresh `callers`/`callees` query
//! after `--rebuild` is no slower than today's behaviour.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::calls::build::extract_file;
use crate::calls::graph::{CallGraph, CallTarget};
use crate::calls::pass::{file_rel, FilePass};
use crate::calls::resolve;
use crate::deps::extract::extract;
use crate::deps::graph::{self as dep_graph, DepEdge};
use crate::deps::manifest::detect_aliases;
use crate::deps::resolver::{build_suffix_index, resolve as resolve_spec, ResolveCtx};
use crate::deps::{DepError, DepGraph};
use crate::search::cache::{hash_file, Delta, FileRecord};

/// Drop entries for changed files from `deps` and re-extract+resolve any
/// added/modified file. Suffix index is rebuilt fresh (one walk) since
/// add/remove changes file membership; cost is bounded by the project size
/// and is the same walk the full-rebuild path performs once.
pub fn apply_delta_to_deps(
    deps: &mut DepGraph,
    root: &Path,
    delta: &Delta,
) -> Result<(), DepError> {
    let to_process = changed_abs_paths(root, delta);
    let removed_abs: Vec<PathBuf> = delta
        .removed
        .iter()
        .map(|rel| root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)))
        .collect();

    for abs in &removed_abs {
        deps.forward.remove(abs);
        deps.external.remove(abs);
    }
    for abs in &to_process {
        deps.forward.remove(abs);
        deps.external.remove(abs);
    }

    if !to_process.is_empty() {
        let aliases = detect_aliases(root);
        let idx = build_suffix_index(root);

        let resolved: Vec<(PathBuf, Vec<DepEdge>, Vec<String>)> = to_process
            .par_iter()
            .map(|file| {
                let info = match idx.by_file.get(file) {
                    Some(i) => i,
                    None => return (file.clone(), Vec::new(), Vec::new()),
                };
                let raw_imports = extract(file, info.language);
                let mut edges = Vec::new();
                let mut external = Vec::new();
                let ctx = ResolveCtx {
                    from_file: file,
                    lang: info.language,
                    alias_prefix: aliases.go_module.as_deref(),
                    path_aliases: &aliases.ts_path_aliases,
                    php_psr4: &aliases.php_psr4,
                };
                for ri in raw_imports {
                    match resolve_spec(&ri.spec, &ctx, &idx) {
                        Some(target) => {
                            if target == *file {
                                continue;
                            }
                            edges.push(DepEdge {
                                target,
                                kind: ri.kind,
                                line: ri.line,
                                local_name: ri.local_name,
                                raw_path: ri.raw_path,
                            });
                        }
                        None => external.push(ri.raw_path.unwrap_or(ri.spec)),
                    }
                }
                (file.clone(), edges, external)
            })
            .collect();

        for (file, edges, external) in resolved {
            // Always insert into `forward` so the file is known to the graph,
            // even when it has zero outbound edges. Mirrors `build_graph`.
            deps.forward.insert(file.clone(), edges);
            if !external.is_empty() {
                deps.external.insert(file, external);
            }
        }
    }

    dep_graph::dedup_edges(deps);
    let edge_count: usize = deps.forward.values().map(|v| v.len()).sum();
    let external_count: usize = deps.external.values().map(|v| v.len()).sum();
    deps.stats.file_count = deps.forward.len();
    deps.stats.edge_count = edge_count;
    deps.stats.external_count = external_count;
    Ok(())
}

/// Patch the call graph for the same delta: drop everything anchored to
/// changed files, re-extract added/modified files, splice the new qns +
/// edges back in. `deps` is the *already-patched* deps graph so pass C's
/// dep-closure filter sees post-update state.
pub fn apply_delta_to_calls(
    calls: &mut CallGraph,
    deps: &DepGraph,
    root: &Path,
    delta: &Delta,
) {
    // Build the set of relative POSIX paths whose qn-bearing entries we
    // need to invalidate. `Qn::file()` returns this same form, so the
    // membership check is a single string lookup.
    let mut changed: HashSet<String> = HashSet::new();
    for rel in &delta.removed {
        changed.insert(rel.clone());
    }
    for path in delta.added.iter().chain(delta.modified.iter()) {
        changed.insert(file_rel(root, path));
    }
    if changed.is_empty() {
        return;
    }

    // 1. Drop forward edges originating from changed files. Edges from
    //    *other* files that point INTO changed files are left alone for
    //    now — they're either still valid (the qn survived the edit) or
    //    will be detected as stale after step 6 once we know which qns
    //    actually exist post-update. Demoting eagerly would cost the
    //    `Exact` confidence tag for edges whose target wasn't renamed.
    calls
        .forward
        .retain(|qn, _| !changed.contains(qn.file()));

    // 3. Drop changed-file qns from per-callable + per-type metadata.
    calls
        .callable_meta
        .retain(|qn, _| !changed.contains(qn.file()));
    calls
        .types
        .retain(|qn, _| !changed.contains(qn.file()));

    // 4. Drop changed-file entries from the inverted indices and prune
    //    empty buckets so subsequent callers don't see ghost keys.
    for v in calls.symbol_table.values_mut() {
        v.retain(|qn| !changed.contains(qn.file()));
    }
    calls.symbol_table.retain(|_, v| !v.is_empty());

    for v in calls.type_by_name.values_mut() {
        v.retain(|qn| !changed.contains(qn.file()));
    }
    calls.type_by_name.retain(|_, v| !v.is_empty());

    for v in calls.implementors.values_mut() {
        v.retain(|qn| !changed.contains(qn.file()));
    }
    calls.implementors.retain(|_, v| !v.is_empty());

    // 5. Re-extract added + modified files. We only re-parse the changed
    //    set (not the unchanged ones) — that's the whole point.
    let to_process = changed_abs_paths(root, delta);
    let new_passes: Vec<FilePass> = to_process
        .par_iter()
        .filter_map(|file| extract_file(root, file))
        .collect();

    // 6. Splice new qns into the live indices before resolving — so when
    //    pass A/B looks up the global symbol_table for a new edge, the
    //    targets newly defined in this same delta are visible.
    for fp in &new_passes {
        for (qn, meta) in fp.defined.iter().zip(fp.callable_locations.iter()) {
            calls.callable_meta.insert(qn.clone(), meta.clone());
            calls
                .symbol_table
                .entry(qn.name().to_string())
                .or_default()
                .push(qn.clone());
        }
        for (qn, meta) in &fp.types {
            calls.types.insert(qn.clone(), meta.clone());
            calls
                .type_by_name
                .entry(qn.name().to_string())
                .or_default()
                .push(qn.clone());
            for base in &meta.bases {
                let normalised = normalise_type_name(base);
                calls
                    .implementors
                    .entry(normalised)
                    .or_default()
                    .push(qn.clone());
            }
        }
    }
    sort_dedup(calls);

    // 7. Resolve edges for the new passes only, against the updated global
    //    symbol_table. Edges in unchanged files that already have a valid
    //    target (still in callable_meta after step 3) keep their original
    //    confidence — that's the whole point of skipping step 2's eager
    //    demote.
    let table = calls.symbol_table.clone();
    let resolved = resolve::run_with_table(root, deps, new_passes, table);
    for (qn, edges) in resolved.forward {
        calls.forward.entry(qn).or_default().extend(edges);
    }

    // 8. Validate Resolved edges against the post-update qn population.
    //    Any whose target qn no longer exists (deleted or renamed) gets
    //    demoted to Bare with the original callee name preserved. The
    //    next pass tries to re-resolve those bare names.
    let known_callable: HashSet<crate::calls::graph::Qn> =
        calls.callable_meta.keys().cloned().collect();
    for edges in calls.forward.values_mut() {
        for e in edges.iter_mut() {
            let demote = matches!(&e.target, CallTarget::Resolved(qn) if !known_callable.contains(qn));
            if demote {
                let bare = e.target.name_or_raw();
                e.target = CallTarget::Bare(bare);
                e.confidence = crate::calls::graph::Confidence::Ambiguous;
                e.candidates.clear();
            }
        }
    }

    // 9. Bare-name re-resolution: same precision rule as Pass B in
    //    `resolve.rs` — a single global match promotes to
    //    Resolved/Inferred, but receiver-bearing calls (`obj.method()`)
    //    require dep-graph confirmation we don't run here, so they stay
    //    Bare. Mirrors Pass B's receiver suppression so cold builds and
    //    incremental updates produce the same edge resolution; without
    //    this, partial updates would over-promote receiver-bearing calls
    //    that the cold build deliberately leaves Bare.
    for edges in calls.forward.values_mut() {
        for e in edges.iter_mut() {
            let CallTarget::Bare(name) = &e.target else { continue };
            let Some(cands) = calls.symbol_table.get(name) else { continue };
            let has_receiver = e
                .receiver
                .as_deref()
                .is_some_and(|r| !matches!(r, "self" | "Self" | "crate" | "super"));
            if cands.len() == 1 && !has_receiver {
                e.target = CallTarget::Resolved(cands[0].clone());
                e.confidence = crate::calls::graph::Confidence::Inferred;
                e.candidates.clear();
            } else if !cands.is_empty() {
                e.candidates = cands.clone();
            }
        }
    }

    // 10. Reverse adjacency + stats are derived; rebuild fresh.
    calls.rebuild_reverse();
    recompute_call_stats(calls);
}

/// Update the on-disk fingerprint table to reflect the new state of disk
/// after a partial update. Entries for removed files are dropped; added
/// and modified entries are re-stat-and-hashed; mtime-only entries get
/// their mtime refreshed without a re-hash; unchanged entries pass through.
pub fn refresh_records(
    prev: Vec<FileRecord>,
    root: &Path,
    delta: &Delta,
) -> Vec<FileRecord> {
    let removed: HashSet<&str> = delta.removed.iter().map(|s| s.as_str()).collect();
    let touched: HashSet<String> = delta
        .added
        .iter()
        .chain(delta.modified.iter())
        .chain(delta.mtime_only.iter())
        .map(|p| rel_posix(root, p))
        .collect();

    let mut out: Vec<FileRecord> = prev
        .into_iter()
        .filter(|r| !removed.contains(r.path.as_str()) && !touched.contains(&r.path))
        .collect();

    for path in delta
        .added
        .iter()
        .chain(delta.modified.iter())
        .chain(delta.mtime_only.iter())
    {
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        let mtime_ns = mtime_nanos(&meta);
        let hash = hash_file(path).unwrap_or(0);
        let rel = rel_posix(root, path);
        out.push(FileRecord {
            path: rel,
            mtime_ns,
            size: meta.len(),
            content_hash: hash,
            chunk_start: 0,
            chunk_end: 0,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn changed_abs_paths(_root: &Path, delta: &Delta) -> Vec<PathBuf> {
    delta
        .added
        .iter()
        .chain(delta.modified.iter())
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect()
}

fn rel_posix(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|r| {
            r.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_else(|_| path.display().to_string())
}

fn mtime_nanos(meta: &std::fs::Metadata) -> i128 {
    let mtime = meta
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    match mtime.duration_since(std::time::SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

/// Mirror of `build::normalise_type_name` (kept private there). Strips
/// generics, brackets, dotted/`::`-prefixed scope so different forms of
/// the same base name hash to one bucket.
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

fn sort_dedup(calls: &mut CallGraph) {
    for v in calls.symbol_table.values_mut() {
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
    }
    for v in calls.type_by_name.values_mut() {
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
    }
    for v in calls.implementors.values_mut() {
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
    }
}

fn recompute_call_stats(calls: &mut CallGraph) {
    use crate::calls::graph::{CallTarget, GraphStats};
    let mut stats = GraphStats {
        function_count: calls.forward.len(),
        type_count: calls.types.len(),
        // Preserve the original build_ms — incremental updates don't reset it.
        build_ms: calls.stats.build_ms,
        ..Default::default()
    };
    for edges in calls.forward.values() {
        for e in edges {
            stats.edge_count += 1;
            match &e.target {
                CallTarget::Resolved(_) => stats.resolved_edge_count += 1,
                CallTarget::External(_) => stats.external_edge_count += 1,
                CallTarget::Bare(_) => stats.ambiguous_edge_count += 1,
            }
        }
    }
    calls.stats = stats;
}
