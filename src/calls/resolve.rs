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
            // An explicit receiver disqualifies same-file binding: a local
            // homonym must not shadow `connection.getCtx()` — the receiver
            // says the target lives on another object (issue #31). Self-like
            // receivers (`self`, `this`, …) and receiver-less calls still
            // bind locally, and a type-qualified call (`Foo::bar()`,
            // `Foo.bar()`) binds only to a local qn actually scoped under
            // that type.
            let self_like = receiver_is_self_like(raw.receiver.as_deref());

            // -------- Pass A: same-file -------- //
            if file_qns.contains(&raw.bare_name) {
                let local_target = if self_like {
                    // Prefer the sibling under the caller's own scope:
                    // `self.shared()` inside `Greeter::caller` is
                    // `Greeter::shared`, not a same-file homonym from
                    // another class.
                    raw.source
                        .0
                        .rfind("::")
                        .map(|i| format!("{}::{}", &raw.source.0[..i], raw.bare_name))
                        .and_then(|want| fp.defined.iter().find(|q| q.0 == want).cloned())
                        .or_else(|| local_qn_by_name.get(&raw.bare_name).cloned())
                } else {
                    raw.receiver.as_deref().and_then(|recv| {
                        // Receivers can arrive namespace-qualified
                        // (`Foo\Greeter::m()` in PHP, `a::b::Type::m()`
                        // elsewhere). Normalize the separators and require
                        // the *complete* receiver path to match a local qn.
                        // No terminal-segment fallback: `other::Type::m()`
                        // explicitly names another scope's `Type`, and
                        // discarding the qualifiers would hand the edge to
                        // an unrelated same-file homonym tagged Exact. A
                        // miss falls through to pass B/C, where the dep
                        // graph either confirms the target (`Inferred`) or
                        // the edge stays honestly `Ambiguous`.
                        let normalized = recv.replace(['\\', '.'], "::");
                        // Rust self-relative prefixes (`crate::`, `self::`,
                        // `super::…`) never appear inside qns — those start
                        // with the repo-relative file path. Each prefix
                        // picks its own anchor (see the match arms below);
                        // matching is always *anchored equality*, never a
                        // bare suffix, and a miss falls through to pass
                        // B/C — no terminal-segment fallback.
                        #[derive(Clone, Copy)]
                        enum SelfRel {
                            Crate,
                            SelfMod,
                            Super(usize),
                        }
                        let mut rest = normalized.as_str();
                        let mut rel: Option<SelfRel> = None;
                        loop {
                            if let Some(r) = rest.strip_prefix("crate::") {
                                rest = r;
                                rel = Some(SelfRel::Crate);
                            } else if let Some(r) = rest.strip_prefix("self::") {
                                rest = r;
                                rel.get_or_insert(SelfRel::SelfMod);
                            } else if let Some(r) = rest.strip_prefix("super::") {
                                rest = r;
                                rel = Some(match rel {
                                    Some(SelfRel::Super(n)) => SelfRel::Super(n + 1),
                                    _ => SelfRel::Super(1),
                                });
                            } else {
                                break;
                            }
                        }
                        if let Some(rel) = rel {
                            let want_tail = format!("::{}::{}", rest, raw.bare_name);
                            let caller = raw.source.0.as_str();
                            return match rel {
                                // `crate::P` anchors at the *crate root*
                                // (`src/lib.rs` / `src/main.rs`), which is
                                // the caller's own file only when the caller
                                // lives there. Anywhere else the two differ,
                                // and anchoring at the caller's file binds
                                // `crate::inner::Foo::method()` to a
                                // same-file `mod inner` decoy while the real
                                // `crate::inner` is another file — so the
                                // arm fires exactly where it would be wrong.
                                // Off the crate root the edge falls through
                                // to pass B/C, which resolve it against the
                                // whole project.
                                SelfRel::Crate => {
                                    let file = caller
                                        .split_once("::")
                                        .map_or(caller, |(f, _)| f);
                                    if !is_crate_root(file) {
                                        return None;
                                    }
                                    let want = format!("{}{}", file, want_tail);
                                    fp.defined.iter().find(|q| q.0 == want).cloned()
                                }
                                // `self::P` starts at the caller's
                                // enclosing scope; each `super::` skips one
                                // more level before the walk begins, so an
                                // ancestor decoy at the caller's own level
                                // cannot shadow the parent. (A method's qn
                                // carries type segments the resolver can't
                                // tell from modules, so the walk continues
                                // upward past the start — anchored equality
                                // keeps any hit real.)
                                SelfRel::SelfMod | SelfRel::Super(_) => {
                                    let skip = match rel {
                                        SelfRel::Super(n) => n,
                                        _ => 0,
                                    };
                                    let mut cur = caller;
                                    std::iter::from_fn(|| {
                                        let i = cur.rfind("::")?;
                                        cur = &cur[..i];
                                        Some(cur)
                                    })
                                    .skip(skip)
                                    .find_map(|base| {
                                        let want = format!("{}{}", base, want_tail);
                                        fp.defined.iter().find(|q| q.0 == want).cloned()
                                    })
                                }
                            };
                        }
                        let full = format!("::{}::{}", normalized, raw.bare_name);
                        // Bind only a candidate whose lexical scope encloses
                        // the *caller* (innermost wins) — that is what the
                        // path means in the source. The filter applies to a
                        // single match too: one `Foo::method` declared in a
                        // sibling module is a decoy, not the target, and
                        // must defer to pass B/C rather than take same-file
                        // Exact just for being alone. With several matches
                        // (`mod a`/`mod b` each with `Foo::method`) the same
                        // rule keeps the edge honestly Ambiguous instead of
                        // taking whichever match came first.
                        let caller = raw.source.0.as_str();
                        fp.defined
                            .iter()
                            .filter(|q| q.0.ends_with(&full))
                            .filter(|q| {
                                let scope = &q.0[..q.0.len() - full.len()];
                                caller == scope
                                    || caller.starts_with(&format!("{}::", scope))
                            })
                            .max_by_key(|q| q.0.len())
                            .cloned()
                    })
                };
                if let Some(qn) = local_target {
                    let edge = raw_to_edge(
                        raw.clone(),
                        CallTarget::Resolved(qn),
                        Confidence::Exact,
                        rel_path(root, &fp.file),
                        Vec::new(),
                    );
                    forward.entry(edge.source.clone()).or_default().push(edge);
                    continue;
                }
                // Explicit receiver with no locally-scoped match: fall
                // through to pass B/C rather than mis-bind.
            }

            // -------- Pass A (cont): import resolution -------- //
            // Same gate: `obj.parse()` must not bind to an imported free
            // function `parse`.
            if self_like {
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
            }

            // -------- Pass B: global symbol table -------- //
            //
            // Receiver-bearing calls (`obj.method()`) only get pass B
            // promotion when the dep graph confirms a relationship — without
            // a resolved type for `obj`, single-name matches are too noisy
            // (e.g. `builder.hidden()` would resolve to any project method
            // happening to be called `hidden`). Defer them to pass C.
            let has_receiver = !self_like;
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
    let mut closures = ClosureCache::default();
    for (raw, src_file, cands) in ambiguous_buffer {
        let (target, confidence, candidates) =
            disambiguate(deps, root, &src_file, &raw.bare_name, &cands, &mut closures);
        let edge = raw_to_edge(raw, target, confidence, rel_path(root, &src_file), candidates);
        forward.entry(edge.source.clone()).or_default().push(edge);
    }

    Resolved {
        forward,
        symbol_table,
    }
}

/// Is this repo-relative file a Rust crate root — the module `crate::`
/// names? Only there does the caller's file segment coincide with the crate
/// root, which is what the `crate::` anchor needs (`src/bin/*.rs` binaries
/// are roots of their own crates too). The `bin` test is anchored to Cargo's
/// layout: `tools/bin/helper.rs` and `vendor/bin/x.rs` are ordinary files,
/// not crate roots, and anchoring `crate::` at them mis-binds every call.
fn is_crate_root(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    matches!(name, "lib.rs" | "main.rs")
        || file.contains("/src/bin/")
        || file.starts_with("src/bin/")
}

/// A receiver that still points at the enclosing scope: absent (`foo()`),
/// or one of the language self/scope keywords. Anything else — a variable,
/// field, or type name — means the call targets another object, so
/// same-file and single-global-match promotion must not claim it.
/// (`self`/`Self`/`crate`/`super` — Rust; `this` — Java/TS/C#/C++/Kotlin/
/// Scala; `$this` — PHP; `cls` — Python, the conventional first parameter of
/// a `@classmethod`, where `cls.helper()` means the enclosing class exactly
/// as `self.helper()` does. PHP's `self::`/`static::`/`parent::` scoped calls
/// never reach here — the adapter normalizes those receivers to `None` —
/// so listing the bare words would only ever match user variables named
/// `static`/`parent`, which are common and NOT self-like.)
pub(crate) fn receiver_is_self_like(recv: Option<&str>) -> bool {
    matches!(
        recv,
        None | Some("self")
            | Some("Self")
            | Some("cls")
            | Some("crate")
            | Some("super")
            | Some("this")
            | Some("$this")
    )
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

/// Per-file forward-dep closures, memoized. A file's closure is the same for
/// every ambiguous edge inside it, and the incremental updater re-asks for it
/// across a whole sweep.
#[derive(Default)]
pub(crate) struct ClosureCache(HashMap<PathBuf, HashSet<PathBuf>>);

impl ClosureCache {
    fn get(&mut self, deps: &DepGraph, from: &Path) -> &HashSet<PathBuf> {
        self.0
            .entry(from.to_path_buf())
            .or_insert_with(|| forward_closure_files(deps, from))
    }
}

/// Pass C's decision for one ambiguous bare edge: filter the candidates to
/// those the caller's file can actually reach through the dep graph, then
/// resolve (single survivor) or stay honestly ambiguous.
///
/// Shared with the incremental updater in `graph_cache::delta` so a partial
/// update and a cold build of the same content produce the same edge. That
/// mattered concretely: the updater used to assign the *unfiltered* global
/// symbol-table entry as an edge's candidate set, so one unrelated file edit
/// gave every `Vec::new()` in the project a candidate the dep filter had
/// ruled out — inflating `callers CliError.new`'s unresolved-site count from
/// 1023 to 1073 with no source change behind it.
pub(crate) fn disambiguate(
    deps: &DepGraph,
    root: &Path,
    src_file: &Path,
    bare_name: &str,
    cands: &[Qn],
    closures: &mut ClosureCache,
) -> (CallTarget, Confidence, Vec<Qn>) {
    let closure = closures.get(deps, src_file);
    let filtered: Vec<Qn> = cands
        .iter()
        .filter(|qn| {
            let cand_file = root.join(qn.file().replace('/', std::path::MAIN_SEPARATOR_STR));
            closure.contains(&cand_file)
        })
        .cloned()
        .collect();

    if filtered.len() == 1 {
        (
            CallTarget::Resolved(filtered[0].clone()),
            Confidence::Inferred,
            Vec::new(),
        )
    } else if filtered.is_empty() {
        // No dep-relationship — fall back to ambiguous over the full set.
        (
            CallTarget::Bare(bare_name.to_string()),
            Confidence::Ambiguous,
            cands.to_vec(),
        )
    } else {
        // Multiple survivors — keep as ambiguous with the surviving set.
        (
            CallTarget::Bare(bare_name.to_string()),
            Confidence::Ambiguous,
            filtered,
        )
    }
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

