//! Per-file invalidation for the unified graph cache.
//!
//! Replaces the prior "any-delta = full rebuild" behaviour: when files are
//! added / modified / removed, we drop just the affected entries from the
//! in-memory graph, re-extract the changed files, and merge the result
//! back. The deps half always patches; the calls half patches when present.
//!
//! The deps half parses only the changed files but resolves every file
//! again, from the imports the cache keeps per file: a change to one file
//! can move where an unchanged file's import resolves (#93).
//!
//! Stale-edge handling (calls half): edges originating from *unchanged*
//! files that point to a qn defined in a *changed* file are validated after
//! the re-extract (step 8) and demoted when their target no longer exists,
//! then re-resolved by name (step 9).
//!
//! The invariant step 9 has to hold is **parity**: a partial update and a
//! cold build of the same content must produce the same graph, or an
//! unrelated edit silently changes answers elsewhere in the project. That
//! means reusing the cold build's own rules — `resolve::disambiguate` for
//! pass C — rather than approximating them here.

use std::collections::{HashMap, HashSet};

use crate::graph_cache::cache::{existence_record, mtime_nanos, rel_posix, tracks_contents};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::calls::build::extract_file;
use crate::calls::graph::{add_callable, type_key, type_key_of, CallGraph, CallTarget};
use crate::calls::pass::{file_rel, FilePass};
use crate::calls::resolve;
use crate::deps::extract::{extract, RawImport};
use crate::deps::manifest::detect_aliases;
use crate::deps::resolver::build_suffix_index;
use crate::deps::{resolve_all, DepError, DepGraph};
use crate::search::cache::{hash_file, Delta, FileRecord};

/// Parse the added and modified files again, keep the cached imports of
/// every other file, and resolve every file against a fresh suffix index.
///
/// Every file resolves again, not only the changed ones: an added,
/// removed, or edited file can change where an unchanged file's import
/// lands — a deleted target, a new file that satisfies an external import
/// or is nearer than the old target, an Elixir module that moved between
/// two files (#93). Resolving is lookups against the index; parsing is
/// the cost, and only changed files pay it.
pub fn apply_delta_to_deps(
  deps: &mut DepGraph,
  root: &Path,
  delta: &Delta,
) -> Result<(), DepError> {
  let changed: HashSet<PathBuf> = changed_abs_paths(root, delta).into_iter().collect();
  let aliases = detect_aliases(root);
  let idx = build_suffix_index(root);

  // The index decides which files the graph holds, as in a cold build, so
  // a removed file drops out here.
  let mut cached = std::mem::take(&mut deps.imports);
  let mut imports = HashMap::with_capacity(idx.by_file.len());
  let mut to_extract = Vec::new();
  for (file, info) in &idx.by_file {
    match cached.remove(file) {
      Some(kept) if !changed.contains(file) => {
        imports.insert(file.clone(), kept);
      }
      _ => to_extract.push((file, info.language)),
    }
  }
  let fresh: Vec<(PathBuf, Vec<RawImport>)> = to_extract
    .par_iter()
    .map(|(file, lang)| ((*file).clone(), extract(file, *lang)))
    .collect();
  imports.extend(fresh);
  deps.imports = imports;

  // `build_ms` keeps the cold build's time.
  resolve_all(deps, &idx, &aliases);
  Ok(())
}

/// Patch the call graph for the same delta: drop everything anchored to
/// changed files, re-extract added/modified files, splice the new qns +
/// edges back in. `deps` is the *already-patched* deps graph so pass C's
/// dep-closure filter sees post-update state.
pub fn apply_delta_to_calls(calls: &mut CallGraph, deps: &DepGraph, root: &Path, delta: &Delta) {
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
  calls.forward.retain(|qn, _| !changed.contains(qn.file()));

  // 3. Drop changed-file qns from per-callable + per-type metadata.
  calls
    .callable_meta
    .retain(|qn, _| !changed.contains(qn.file()));
  calls.types.retain(|qn, _| !changed.contains(qn.file()));
  calls.namespaces.retain(|qn| !changed.contains(qn.file()));

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
  //    set (not the unchanged ones) — that's the whole point. Only a file
  //    the deps graph holds is parsed, as in a cold build: a delta also
  //    carries files whose existence alone counts (`data.json`, a README).
  let to_process: Vec<PathBuf> = changed_abs_paths(root, delta)
    .into_iter()
    .filter(|p| deps.forward.contains_key(p))
    .collect();
  let new_passes: Vec<FilePass> = to_process
    .par_iter()
    .filter_map(|file| extract_file(root, file))
    .collect();

  // 6. Splice new qns into the live indices before resolving — so when
  //    pass A/B looks up the global symbol_table for a new edge, the
  //    targets newly defined in this same delta are visible.
  for fp in &new_passes {
    calls.namespaces.extend(fp.namespaces.iter().cloned());
    for (qn, meta) in fp.defined.iter().zip(fp.callable_locations.iter()) {
      add_callable(&mut calls.callable_meta, qn, meta);
      calls
        .symbol_table
        .entry(qn.name().to_string())
        .or_default()
        .push(qn.clone());
    }
    for (qn, meta) in &fp.types {
      calls.types.insert(qn.clone(), meta.clone());
      let lang = resolve::qn_lang(qn);
      calls
        .type_by_name
        .entry(type_key_of(qn, &meta.kind))
        .or_default()
        .push(qn.clone());
      for base in &meta.bases {
        calls
          .implementors
          .entry(type_key(lang, base))
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
  let scopes = resolve::scope_paths(
    calls.callable_meta.keys(),
    calls.types.keys().chain(&calls.namespaces),
  );
  let resolved = resolve::run_with_table(
    root,
    deps,
    new_passes,
    table,
    &scopes,
    &calls.type_by_name,
    &calls.callable_meta,
  );
  for (qn, edges) in resolved.forward {
    calls.forward.entry(qn).or_default().extend(edges);
  }

  // 8. Validate Resolved edges against the post-update qn population.
  //    Any whose target qn no longer exists (deleted or renamed) gets
  //    demoted to Bare with the original callee name preserved. The
  //    next pass tries to re-resolve those bare names. A construction
  //    targets its type's qn, so types count as well as callables: with
  //    callables alone, every construction of a Rust struct or a TS class
  //    looked like a deleted target after any change (#128).
  let known_targets: HashSet<crate::calls::graph::Qn> = calls
    .callable_meta
    .keys()
    .chain(calls.types.keys())
    .cloned()
    .collect();
  //    An edge pass A resolved through an import resolves that import
  //    again, as a cold build does: the module may declare the name
  //    elsewhere, re-export it, or be gone, and then the call is to the
  //    name the file wrote (`alias`), not to another file's `real`.
  //    The index for it is built only when such an edge goes stale.
  let mut import_ctx = None;
  for edges in calls.forward.values_mut() {
    for e in edges.iter_mut() {
      // Step 9 re-resolves every Elixir edge from its whole target,
      // which a struct literal's module needs.
      if crate::deps::resolver::Lang::from_path(&e.file)
        == Some(crate::deps::resolver::Lang::Elixir)
      {
        continue;
      }
      let demote = matches!(&e.target, CallTarget::Resolved(qn) if !known_targets.contains(qn));
      if !demote {
        continue;
      }
      e.confidence = crate::calls::graph::Confidence::Ambiguous;
      e.candidates.clear();
      let Some(binding) = &e.import else {
        e.target = CallTarget::Bare(e.target.name_or_raw());
        continue;
      };
      let (aliases, idx) =
        import_ctx.get_or_insert_with(|| (detect_aliases(root), build_suffix_index(root)));
      let from_file = root.join(&e.file);
      e.target = match resolve::resolve_via_imports(
        binding,
        &from_file,
        crate::deps::resolver::Lang::from_path(&from_file),
        aliases,
        idx,
        root,
        &calls.symbol_table,
        &calls.type_by_name,
        &calls.callable_meta,
      ) {
        resolve::Imported::Declared(target) => {
          e.confidence = crate::calls::graph::Confidence::Exact;
          CallTarget::Resolved(target)
        }
        resolve::Imported::Undeclared(name) => CallTarget::Bare(name),
        // What a cold build records: the local name, and no binding.
        resolve::Imported::Outside => {
          let local = binding.local.clone();
          e.import = None;
          CallTarget::Bare(local)
        }
      };
    }
  }

  // 9. Bare-name re-resolution: the same rules the cold build applies, so
  //    a partial update and a cold build of the same content agree.
  //    Pass B's rule — a single global match promotes, receiver-bearing
  //    calls don't (without a type for `obj`, single-name matches are too
  //    noisy) — then pass C's rule for everything else: candidates are the
  //    ones the caller's file can actually *reach*, via
  //    `resolve::disambiguate`. Assigning the unfiltered symbol-table
  //    entry here instead (what this used to do) handed every bare edge in
  //    the project candidates the dep filter had ruled out, so one
  //    unrelated file edit changed answers everywhere.
  let mut closures = crate::calls::resolve::ClosureCache::default();
  for edges in calls.forward.values_mut() {
    for e in edges.iter_mut() {
      // A scope receiver resolves by the cold build's scope rule, on
      // every edge: a delta can define its scope, or a nearer one.
      if e.receiver_scope != crate::core::ReceiverScope::None {
        let receiver = e.receiver.clone().unwrap_or_default();
        let name = match &e.target {
          CallTarget::Resolved(qn) => qn.name().to_string(),
          CallTarget::Bare(name) => name.clone(),
          // `resolve_scoped` writes `External("{receiver}.{name}")`.
          CallTarget::External(raw) => raw
            .strip_prefix(receiver.as_str())
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(raw)
            .to_string(),
        };
        let src_file = root.join(&e.file);
        (e.target, e.confidence, e.candidates) = match resolve::resolve_scoped(
          &e.source,
          &receiver,
          e.receiver_scope,
          &name,
          e.arity,
          &e.nesting,
          &calls.symbol_table,
          &calls.callable_meta,
          &scopes,
        ) {
          resolve::Scoped::Done(target, confidence) => (target, confidence, Vec::new()),
          resolve::Scoped::Ambiguous(cands) => {
            resolve::disambiguate(deps, root, &src_file, &name, &cands, &mut closures)
          }
        };
        continue;
      }
      // Elixir's other edges re-resolve by module; see `calls::elixir`.
      let tables = (
        &calls.symbol_table,
        &calls.type_by_name,
        &calls.callable_meta,
      );
      if crate::calls::elixir::reresolve(e, tables) {
        continue;
      }
      let CallTarget::Bare(name) = &e.target else {
        continue;
      };
      // Pass B's language rule: only a function the caller's language
      // can call is a candidate (issue #96).
      let cands = crate::calls::resolve::reachable_from(
        crate::deps::resolver::Lang::from_path(&e.file),
        calls.symbol_table.get(name).map_or(&[][..], Vec::as_slice),
      );
      if cands.is_empty() {
        // What a cold build leaves: bare, with no candidates. Leaving
        // the edge as it was would keep candidates that may since
        // have been deleted.
        e.confidence = crate::calls::graph::Confidence::Ambiguous;
        e.candidates.clear();
        continue;
      }
      // Same gate as pass B on a cold build: scope-shifting keyword
      // receivers (`crate`/`super`) withhold single-match promotion —
      // the one match may sit in a scope the keyword rules out.
      let recv = e.receiver.as_deref();
      let has_receiver = !crate::calls::resolve::receiver_is_self_like(recv)
        || crate::calls::resolve::receiver_is_scope_shifting(recv);
      if cands.len() == 1 && !has_receiver {
        // Same tag pass B gives this case on a cold build
        // (`resolve.rs`, single global match, no receiver) — parity
        // is the whole point of this step, and confidence is part of
        // the answer `callers --json` reports.
        e.target = CallTarget::Resolved(cands[0].clone());
        e.confidence = crate::calls::graph::Confidence::Exact;
        e.candidates.clear();
        continue;
      }
      let src_file = root.join(&e.file);
      let (target, confidence, candidates) =
        crate::calls::resolve::disambiguate(deps, root, &src_file, name, &cands, &mut closures);
      e.target = target;
      e.confidence = confidence;
      e.candidates = candidates;
    }
  }

  // 10. Reverse adjacency + stats are derived; rebuild fresh.
  calls.rebuild_reverse();
  recompute_call_stats(calls);
}

/// Update the on-disk fingerprint table to reflect the new state of disk
/// after a partial update. Entries for removed files are dropped; added
/// and modified entries are re-stat-and-hashed; mtime-only entries keep
/// their hash and take the mtime and size the delta hashed them under
/// (`MtimeOnly`), as they are not parsed again; unchanged entries pass
/// through.
pub fn refresh_records(prev: Vec<FileRecord>, root: &Path, delta: &Delta) -> Vec<FileRecord> {
  let removed: HashSet<&str> = delta.removed.iter().map(|s| s.as_str()).collect();
  let touched: HashSet<String> = delta
    .added
    .iter()
    .chain(delta.modified.iter())
    .map(|p| rel_posix(root, p))
    .collect();
  let seen: HashMap<String, (i128, u64)> = delta
    .mtime_only
    .iter()
    .map(|t| (rel_posix(root, &t.path), (t.mtime_ns, t.size)))
    .collect();

  let sources = crate::deps::manifest::alias_sources(root);
  let mut out: Vec<FileRecord> = prev
    .into_iter()
    .filter(|r| !removed.contains(r.path.as_str()) && !touched.contains(&r.path))
    .map(|mut r| {
      if let Some(&(mtime_ns, size)) = seen.get(&r.path) {
        r.mtime_ns = mtime_ns;
        r.size = size;
      }
      r
    })
    .collect();

  for path in delta.added.iter().chain(delta.modified.iter()) {
    let rel = rel_posix(root, path);
    // A file whose existence alone counts is not hashed; its contents are
    // never compared.
    if !tracks_contents(path, root, &sources) {
      out.push(existence_record(rel));
      continue;
    }
    let Ok(meta) = std::fs::metadata(path) else {
      continue;
    };
    let mtime_ns = mtime_nanos(&meta);
    let hash = hash_file(path).unwrap_or(0);
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
