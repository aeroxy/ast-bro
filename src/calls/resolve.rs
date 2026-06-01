//! Three-pass resolver: bare call names → qualified targets.
//!
//! Pass A — same-file: a call site `foo()` resolves if `foo` is defined in
//! the same file (a `Qn` we already collected) or if it appears in the
//! file's `ImportBinding`s and the import's module spec resolves to a
//! project file via the existing `src/deps/resolver` suffix index.
//!
//! Pass B — global symbol-table: any remaining bare name with exactly one
//! global qn match promotes to `Resolved(qn)`. Multiple matches defer.
//!
//! Pass C — dep-graph disambiguation: ambiguous names filter to candidates
//! whose file appears in the source file's transitive forward-dep closure.
//! Single survivor → `Inferred`; otherwise → `Ambiguous` with all
//! candidates kept under `CallEdge::candidates`.

use crate::calls::graph::{CallEdge, CallTarget, Confidence, Qn};
use crate::calls::pass::{file_rel, raw_to_edge, FilePass, RawEdge};
use crate::deps::manifest::detect_aliases;
use crate::deps::resolver::{build_suffix_index, resolve as resolve_spec, Lang, ResolveCtx};
use crate::deps::traverse;
use crate::deps::DepGraph;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Resolved {
    pub forward: HashMap<Qn, Vec<CallEdge>>,
    pub symbol_table: HashMap<String, Vec<Qn>>,
}

/// Build the global `bare-name → Vec<Qn>` table from a slice of passes.
/// Lifted out of `run` so incremental updates can rebuild the table from
/// (cached + new) passes without going through the full resolver.
pub fn build_symbol_table(passes: &[FilePass]) -> HashMap<String, Vec<Qn>> {
    let mut symbol_table: HashMap<String, Vec<Qn>> = HashMap::new();
    for fp in passes {
        for qn in &fp.defined {
            symbol_table
                .entry(qn.name().to_string())
                .or_default()
                .push(qn.clone());
        }
    }
    for v in symbol_table.values_mut() {
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
    }
    symbol_table
}

pub fn run(root: &Path, deps: &DepGraph, passes: Vec<FilePass>) -> Resolved {
    let symbol_table = build_symbol_table(&passes);
    run_with_table(root, deps, passes, symbol_table)
}

/// Resolve `passes`'s raw edges against a *prebuilt* symbol_table (which
/// must include every qn the resolver should be allowed to see). The
/// incremental path passes the full project's symbol_table here while
/// only handing in raw edges from changed files — so new edges in changed
/// files still resolve to qns defined elsewhere in the project.
pub fn run_with_table(
    root: &Path,
    deps: &DepGraph,
    passes: Vec<FilePass>,
    symbol_table: HashMap<String, Vec<Qn>>,
) -> Resolved {
    // ---------- Suffix index for import resolution (reused from deps). ----------
    let aliases = detect_aliases(root);
    let suffix_idx = build_suffix_index(root);

    // ---------- Pass A + B per-file, then pass C with the dep graph. ----------
    let mut forward: HashMap<Qn, Vec<CallEdge>> = HashMap::new();
    let mut ambiguous_buffer: Vec<(RawEdge, PathBuf, Vec<Qn>)> = Vec::new();

    for fp in passes {
        let file_qns: HashSet<String> = fp.defined.iter().map(|q| q.name().to_string()).collect();
        let local_qn_by_name: HashMap<String, Qn> = fp
            .defined
            .iter()
            .map(|q| (q.name().to_string(), q.clone()))
            .collect();

        // Build a quick `local_name -> module spec` lookup for pass A.
        let import_lookup: HashMap<String, String> = fp
            .imports
            .iter()
            .map(|b| (b.local.clone(), b.module.clone()))
            .collect();

        let lang = Lang::from_path(&fp.file);

        for raw in fp.raw_edges {
            // -------- Pass A: same-file -------- //
            if file_qns.contains(&raw.bare_name) {
                let target = local_qn_by_name
                    .get(&raw.bare_name)
                    .cloned()
                    .map(CallTarget::Resolved)
                    .unwrap_or(CallTarget::Bare(raw.bare_name.clone()));
                let edge = raw_to_edge(
                    raw.clone(),
                    target,
                    Confidence::Exact,
                    rel_path(root, &fp.file),
                    Vec::new(),
                );
                forward.entry(edge.source.clone()).or_default().push(edge);
                continue;
            }

            // -------- Pass A (cont): import resolution -------- //
            if let Some(spec) = import_lookup.get(&raw.bare_name) {
                if let Some(target) = resolve_via_imports(
                    spec,
                    &raw.bare_name,
                    &fp.file,
                    lang,
                    &aliases,
                    &suffix_idx,
                    root,
                    &symbol_table,
                ) {
                    let edge = raw_to_edge(
                        raw.clone(),
                        CallTarget::Resolved(target),
                        Confidence::Exact,
                        rel_path(root, &fp.file),
                        Vec::new(),
                    );
                    forward.entry(edge.source.clone()).or_default().push(edge);
                    continue;
                }
            }

            // -------- Pass B: global symbol table -------- //
            //
            // Receiver-bearing calls (`obj.method()`) only get pass B
            // promotion when the dep graph confirms a relationship — without
            // a resolved type for `obj`, single-name matches are too noisy
            // (e.g. `builder.hidden()` would resolve to any project method
            // happening to be called `hidden`). Defer them to pass C.
            let has_receiver = raw.receiver.is_some()
                && !matches!(raw.receiver.as_deref(), Some("self") | Some("Self") | Some("crate") | Some("super"));
            match symbol_table.get(&raw.bare_name) {
                Some(cands) if cands.len() == 1 && !has_receiver => {
                    let edge = raw_to_edge(
                        raw.clone(),
                        CallTarget::Resolved(cands[0].clone()),
                        Confidence::Exact,
                        rel_path(root, &fp.file),
                        Vec::new(),
                    );
                    forward.entry(edge.source.clone()).or_default().push(edge);
                }
                Some(cands) if !cands.is_empty() => {
                    // Defer to pass C — either ambiguous (>1) or
                    // receiver-bearing single match.
                    ambiguous_buffer.push((raw.clone(), fp.file.clone(), cands.clone()));
                }
                _ => {
                    // 0 candidates: keep as Bare. Could be external, could
                    // be dynamically dispatched. We don't try to distinguish
                    // External here (would require deeper import tracking).
                    let edge = raw_to_edge(
                        raw.clone(),
                        CallTarget::Bare(raw.bare_name.clone()),
                        Confidence::Ambiguous,
                        rel_path(root, &fp.file),
                        Vec::new(),
                    );
                    forward.entry(edge.source.clone()).or_default().push(edge);
                }
            }
        }

        // Make sure every defined function shows up in `forward`, even the
        // leaves that called nothing — `callees` should return a clean
        // "no callees" instead of "function not found".
        for qn in fp.defined {
            forward.entry(qn).or_default();
        }
    }

    // ---------- Pass C: dep-graph disambiguation for ambiguous bares. ----------
    for (raw, src_file, cands) in ambiguous_buffer {
        let closure = forward_closure_files(deps, &src_file);
        let filtered: Vec<Qn> = cands
            .iter()
            .filter(|qn| {
                let cand_file = root.join(qn.file().replace('/', std::path::MAIN_SEPARATOR_STR));
                closure.contains(&cand_file)
            })
            .cloned()
            .collect();

        let (target, confidence, candidates) = if filtered.len() == 1 {
            (CallTarget::Resolved(filtered[0].clone()), Confidence::Inferred, Vec::new())
        } else if filtered.is_empty() {
            // No dep-relationship — fall back to ambiguous.
            (CallTarget::Bare(raw.bare_name.clone()), Confidence::Ambiguous, cands)
        } else {
            // Multiple survivors — keep as ambiguous with surviving set.
            (CallTarget::Bare(raw.bare_name.clone()), Confidence::Ambiguous, filtered)
        };

        let edge = raw_to_edge(raw, target, confidence, rel_path(root, &src_file), candidates);
        forward.entry(edge.source.clone()).or_default().push(edge);
    }

    Resolved {
        forward,
        symbol_table,
    }
}

/// Resolve `from <module> import <name>` (or equivalent) by mapping `module`
/// to a project file via the suffix index, then qualifying `name` inside it.
/// Returns `None` when the module isn't resolvable to a project file
/// (likely an external dep — we leave that to pass-B/C).
#[allow(clippy::too_many_arguments)] // resolution context is genuinely wide
fn resolve_via_imports(
    spec: &str,
    bare_name: &str,
    from_file: &Path,
    lang: Option<Lang>,
    aliases: &crate::deps::manifest::ProjectAliases,
    idx: &crate::deps::resolver::SuffixIndex,
    root: &Path,
    symbol_table: &HashMap<String, Vec<Qn>>,
) -> Option<Qn> {
    let lang = lang?;
    let ctx = ResolveCtx {
        from_file,
        lang,
        alias_prefix: aliases.go_module.as_deref(),
        path_aliases: &aliases.ts_path_aliases,
        php_psr4: &aliases.php_psr4,
    };
    let target_file = resolve_spec(spec, &ctx, idx)?;
    let rel = file_rel(root, &target_file);

    // Prefer a qn whose file matches the resolved target.
    if let Some(cands) = symbol_table.get(bare_name) {
        for cand in cands {
            if cand.file() == rel {
                return Some(cand.clone());
            }
        }
    }
    // Fall back: synthesize a file-scope qn (not always correct for nested
    // declarations, but better than dropping the edge).
    Some(Qn::new(format!("{}::{}", rel, bare_name)))
}

fn forward_closure_files(deps: &DepGraph, from: &Path) -> HashSet<PathBuf> {
    let hits = traverse::forward(deps, from, 8);
    let mut out: HashSet<PathBuf> = hits.into_iter().map(|h| h.file).collect();
    out.insert(from.to_path_buf());
    out
}

fn rel_path(root: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(root).map(|p| p.to_path_buf()).unwrap_or_else(|_| file.to_path_buf())
}

