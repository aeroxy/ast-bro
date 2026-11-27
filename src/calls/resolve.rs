//! Three-pass resolver: bare call names → qualified targets.
//!
//! Pass A — same-file: a call site `foo()` resolves if `foo` is defined in
//! the same file (a `Qn` we already collected) or if it appears in the
//! file's `ImportBinding`s, the import's module spec resolves to a project
//! file via the existing `src/deps/resolver` suffix index, and that file
//! declares the imported name. A name the file does not declare (a
//! re-export) goes on to pass B/C under its declared name.
//!
//! A call on a scope receiver (`CallSite::receiver_scope`) skips the
//! name-based passes: only a candidate inside that scope can be its target,
//! and a scope that the caller's language does not declare makes it
//! `External` (see `resolve_scoped`).
//!
//! Pass B — global symbol-table: any remaining bare name with exactly one
//! global qn match promotes to `Resolved(qn)`. Multiple matches defer. Only
//! a qn in a language the caller's file can call counts as a match:
//! TypeScript, TSX and JavaScript are one family, Java, Kotlin and Scala
//! another, and every other language is on its own (see `can_reach`).
//!
//! Pass C — dep-graph disambiguation: ambiguous names filter to candidates
//! whose file appears in the source file's transitive forward-dep closure.
//! Single survivor → `Inferred`; otherwise → `Ambiguous` with all
//! candidates kept under `CallEdge::candidates`.

use crate::calls::graph::{CallEdge, CallTarget, CallableMeta, Confidence, Qn};
use crate::calls::pass::{file_rel, raw_to_edge, FilePass, RawEdge};
use crate::core::{ImportBinding, ImportOnly, ReceiverScope};
use crate::deps::manifest::detect_aliases;
use crate::deps::resolver::{
  build_suffix_index, resolve as resolve_spec, resolve_from_import, Lang, ResolveCtx,
};
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

pub fn run(
  root: &Path,
  deps: &DepGraph,
  passes: Vec<FilePass>,
  type_by_name: &HashMap<String, Vec<Qn>>,
  callable_meta: &HashMap<Qn, CallableMeta>,
) -> Resolved {
  let symbol_table = build_symbol_table(&passes);
  let scopes = scope_paths(
    passes.iter().flat_map(|fp| &fp.defined),
    passes
      .iter()
      .flat_map(|fp| fp.types.iter().map(|(qn, _)| qn).chain(&fp.namespaces)),
  );
  run_with_table(
    root,
    deps,
    passes,
    symbol_table,
    &scopes,
    type_by_name,
    callable_meta,
  )
}

/// Resolve `passes`'s raw edges against a *prebuilt* symbol_table (which
/// must include every qn the resolver should be allowed to see). The
/// incremental path passes the full project's symbol_table here while
/// only handing in raw edges from changed files — so new edges in changed
/// files still resolve to qns defined elsewhere in the project.
/// `type_by_name` and `callable_meta` are the project's, which an Elixir
/// struct literal and an Elixir call's arity are resolved against.
pub fn run_with_table(
  root: &Path,
  deps: &DepGraph,
  passes: Vec<FilePass>,
  symbol_table: HashMap<String, Vec<Qn>>,
  scopes: &Scopes,
  type_by_name: &HashMap<String, Vec<Qn>>,
  callable_meta: &HashMap<Qn, CallableMeta>,
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

    // Build a quick `local_name -> binding` lookup for pass A.
    let import_lookup: HashMap<String, ImportBinding> = fp
      .imports
      .iter()
      .map(|b| (b.local.clone(), b.clone()))
      .collect();

    let lang = Lang::from_path(&fp.file);

    for mut raw in fp.raw_edges {
      // -------- Scope receiver -------- //
      // The receiver says where the callee lives: the scope it names,
      // in this file or another, decides the target. The name-based
      // passes below would bind `File.join()` to a project `join`.
      if raw.receiver_scope != ReceiverScope::None {
        let receiver = raw.receiver.as_deref().unwrap_or_default();
        match resolve_scoped(
          &raw.source,
          receiver,
          raw.receiver_scope,
          &raw.bare_name,
          raw.arity,
          &raw.nesting,
          &symbol_table,
          callable_meta,
          scopes,
        ) {
          Scoped::Done(target, confidence) => {
            let edge = raw_to_edge(
              raw,
              target,
              confidence,
              rel_path(root, &fp.file),
              Vec::new(),
            );
            forward.entry(edge.source.clone()).or_default().push(edge);
          }
          Scoped::Ambiguous(cands) => {
            ambiguous_buffer.push((raw, fp.file.clone(), cands));
          }
        }
        continue;
      }

      // Elixir names its call targets statically; see `calls::elixir`.
      if lang == Some(Lang::Elixir) {
        let tables = (&symbol_table, type_by_name, callable_meta);
        let (target, confidence, candidates) = crate::calls::elixir::resolve(&raw, tables);
        let edge = raw_to_edge(
          raw,
          target,
          confidence,
          rel_path(root, &fp.file),
          candidates,
        );
        forward.entry(edge.source.clone()).or_default().push(edge);
        continue;
      }

      // An explicit receiver disqualifies same-file binding: a local
      // homonym must not shadow `connection.getCtx()` — the receiver
      // says the target lives on another object (issue #31). Self-like
      // receivers (`self`, `this`, …) and receiver-less calls still
      // bind locally, and a type-qualified call (`Foo::bar()`,
      // `Foo.bar()`) binds only to a local qn actually scoped under
      // that type.
      let self_like = receiver_is_self_like(raw.receiver.as_deref());
      // `crate`/`super` name a *different* scope than the caller's
      // own: `crate::helper()` means the crate root and
      // `super::helper()` the parent module, never a sibling. Route
      // them through the anchored SelfRel walk below instead of
      // sibling preference, which would hand `crate::helper()` to the
      // caller-scope homonym tagged Exact. (In OO languages a bare
      // `super.m()` receiver names the parent type — also never the
      // caller's own scope.)
      let scope_shifting = receiver_is_scope_shifting(raw.receiver.as_deref());

      // -------- Pass A: same-file -------- //
      if file_qns.contains(&raw.bare_name) {
        let local_target = if self_like && !scope_shifting {
          // Prefer the sibling under the caller's own scope:
          // `self.shared()` inside `Greeter::caller` is
          // `Greeter::shared`, not a same-file homonym from
          // another class.
          raw
            .source
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
              } else if rest == "super" {
                // A chain ending in the bare keyword has no
                // trailing `::` for the arm above to strip:
                // `super::super::helper()` arrives with
                // receiver "super::super", and a plain
                // `super::helper()` as just "super". Consume
                // it like its prefixed form, leaving an
                // empty rest.
                rest = "";
                rel = Some(match rel {
                  Some(SelfRel::Super(n)) => SelfRel::Super(n + 1),
                  _ => SelfRel::Super(1),
                });
              } else if rest == "crate" {
                // Same bare-keyword form for the crate
                // anchor: `crate::helper()` arrives with
                // receiver "crate".
                rest = "";
                rel = Some(SelfRel::Crate);
              } else {
                break;
              }
            }
            if let Some(rel) = rel {
              // rest is empty when the receiver was nothing
              // but prefix keywords ("super::super") — the
              // target then sits directly in the anchored
              // scope, with no path segments in between.
              let want_tail = if rest.is_empty() {
                format!("::{}", raw.bare_name)
              } else {
                format!("::{}::{}", rest, raw.bare_name)
              };
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
                  let file = caller.split_once("::").map_or(caller, |(f, _)| f);
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
                caller == scope || caller.starts_with(&format!("{}::", scope))
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
      // function `parse`. Scope-shifting receivers bypass imports too:
      // `crate::helper()` explicitly names a path, not the imported
      // `helper` binding.
      if self_like && !scope_shifting {
        if let Some(binding) = import_lookup.get(&raw.bare_name) {
          match resolve_via_imports(
            binding,
            &fp.file,
            lang,
            &aliases,
            &suffix_idx,
            root,
            &symbol_table,
            type_by_name,
            callable_meta,
          ) {
            Imported::Declared(target) => {
              let mut raw = raw.clone();
              raw.import = Some(binding.clone());
              let edge = raw_to_edge(
                raw,
                CallTarget::Resolved(target),
                Confidence::Exact,
                rel_path(root, &fp.file),
                Vec::new(),
              );
              forward.entry(edge.source.clone()).or_default().push(edge);
              continue;
            }
            // The module is a project file that does not declare the
            // name: it re-exports it (`pub use`, a barrel), or the import
            // is stale. The passes below look the name up by what it is
            // called where it is declared, which is also what the
            // incremental update knows it by once the edge is resolved.
            Imported::Undeclared(name) => {
              raw.bare_name = name;
              raw.import = Some(binding.clone());
            }
            Imported::Outside => {}
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
      // Scope-shifting receivers defer too: `crate::helper()` whose
      // one global `helper` sits in a sibling module names a symbol
      // that does not exist — promoting the single match binds Exact
      // to a scope the keyword explicitly rules out. Pass B cannot
      // check the anchor, so the edge goes to pass C's dep filter.
      let has_receiver = !self_like || scope_shifting;
      // Only a function this file's language can call is a candidate:
      // a Python call never reaches a Ruby method, however unique its
      // name (issue #96).
      let cands = reachable_from(
        lang,
        symbol_table
          .get(&raw.bare_name)
          .map_or(&[][..], Vec::as_slice),
      );
      if cands.len() == 1 && !has_receiver {
        let edge = raw_to_edge(
          raw.clone(),
          CallTarget::Resolved(cands[0].clone()),
          Confidence::Exact,
          rel_path(root, &fp.file),
          Vec::new(),
        );
        forward.entry(edge.source.clone()).or_default().push(edge);
      } else if !cands.is_empty() {
        // Defer to pass C — either ambiguous (>1) or
        // receiver-bearing single match.
        ambiguous_buffer.push((raw.clone(), fp.file.clone(), cands));
      } else {
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
    let edge = raw_to_edge(
      raw,
      target,
      confidence,
      rel_path(root, &src_file),
      candidates,
    );
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
  matches!(name, "lib.rs" | "main.rs") || file.contains("/src/bin/") || file.starts_with("src/bin/")
}

/// A receiver that is a scope keyword rather than an object: absent
/// (`foo()`), or one of the language self/scope keywords. Anything else —
/// a variable, field, or type name — means the call targets another object,
/// so same-file and single-global-match promotion must not claim it.
/// (`self`/`Self`/`crate`/`super` — Rust; `this` — Java/TS/C#/C++/Kotlin/
/// Scala; `$this` — PHP; `cls` — Python, the conventional first parameter of
/// a `@classmethod`, where `cls.helper()` means the enclosing class exactly
/// as `self.helper()` does. PHP's `self::`/`static::`/`parent::` scoped calls
/// never reach here — the adapter normalizes those receivers to `None` —
/// so listing the bare words would only ever match user variables named
/// `static`/`parent`, which are common and NOT self-like.)
///
/// Nuance: `crate` and `super` are keywords but name a scope *other than*
/// the caller's own, so both pass A and pass B refine this gate with
/// [`receiver_is_scope_shifting`]: same-file binding routes them through
/// the anchored SelfRel walk instead of sibling preference, and pass B
/// withholds single-global-match promotion — the one match may sit in a
/// scope the keyword explicitly rules out, and pass B cannot check the
/// anchor. `delta.rs` mirrors pass B through both functions.
pub(crate) fn receiver_is_self_like(recv: Option<&str>) -> bool {
  matches!(
    recv,
    None
      | Some("self")
      | Some("Self")
      | Some("cls")
      | Some("crate")
      | Some("super")
      | Some("this")
      | Some("$this")
  )
}

/// A scope-keyword receiver that names a scope *other than* the caller's
/// own — the refinement pass A and pass B apply on top of
/// [`receiver_is_self_like`] (see its doc). Keyword *chains*
/// ("super::super") never appear here: they are not self-like in the first
/// place, so both passes already treat them as receiver-bearing.
pub(crate) fn receiver_is_scope_shifting(recv: Option<&str>) -> bool {
  matches!(recv, Some("crate") | Some("super"))
}

/// The scopes a scope receiver can name, keyed by the declaring language.
#[derive(Default)]
pub struct Scopes {
  paths: HashSet<(Option<Lang>, String)>,
  /// Each path without one or more leading segments (`Utils` for
  /// `Jekyll::Utils`): what a receiver may name through ancestors.
  tails: HashSet<(Option<Lang>, String)>,
}

/// Every scope path the project declares, with the language that declares
/// it: each declaration enclosing a callable or a scope, and each scope
/// itself, a type or a namespace (`module Shop; class Cart` declares `Shop`
/// and `Shop::Cart`). One dotted declaration is one scope: a declaration
/// named `Shop.Cart` does not declare `Shop`.
pub(crate) fn scope_paths<'a>(
  callables: impl Iterator<Item = &'a Qn>,
  scopes: impl Iterator<Item = &'a Qn>,
) -> Scopes {
  let mut out = Scopes::default();
  let mut add = |qn: &Qn, drop: usize| {
    let lang = qn_lang(qn);
    let parts: Vec<&str> = qn.0.split("::").skip(1).collect();
    for end in 1..=parts.len().saturating_sub(drop) {
      let path = dotted_path(&parts[..end]);
      for start in 1..path.len() {
        out.tails.insert((lang, path[start..].join("::")));
      }
      out.paths.insert((lang, path.join("::")));
    }
  };
  for qn in callables {
    add(qn, 1);
  }
  for qn in scopes {
    add(qn, 0);
  }
  out
}

/// Qn parts as one path, with a dotted part split as pass A splits a
/// receiver (`Shop.Cart` reads as `Shop::Cart`).
fn dotted_path<'a>(parts: &[&'a str]) -> Vec<&'a str> {
  parts
    .iter()
    .flat_map(|p| p.split('.'))
    .filter(|p| !p.is_empty())
    .collect()
}

/// The language of the file a qn is declared in.
pub(crate) fn qn_lang(qn: &Qn) -> Option<Lang> {
  Lang::from_path(Path::new(qn.file()))
}

/// The languages whose bare calls reach one another's functions: TypeScript,
/// TSX and JavaScript are one family, Java, Kotlin and Scala another, and
/// every other language is on its own.
fn lang_family(lang: Lang) -> Lang {
  match lang {
    Lang::Tsx | Lang::JavaScript => Lang::TypeScript,
    Lang::Kotlin | Lang::Scala => Lang::Java,
    other => other,
  }
}

/// Whether a bare call written in `lang` can reach `qn`: the two are in one
/// language family. A language not known on either side rules nothing out.
pub(crate) fn can_reach(lang: Option<Lang>, qn: &Qn) -> bool {
  match (lang, qn_lang(qn)) {
    (Some(a), Some(b)) => lang_family(a) == lang_family(b),
    _ => true,
  }
}

/// The candidates a bare call written in `lang` can reach (see `can_reach`).
pub(crate) fn reachable_from(lang: Option<Lang>, cands: &[Qn]) -> Vec<Qn> {
  cands
    .iter()
    .filter(|qn| can_reach(lang, qn))
    .cloned()
    .collect()
}

/// The scope a callable sits in: its qn parts after the file, without its
/// own name.
fn enclosing_parts(qn: &Qn) -> Vec<&str> {
  let parts: Vec<&str> = qn.0.split("::").skip(1).collect();
  parts[..parts.len().saturating_sub(1)].to_vec()
}

/// The outcome of a call on a scope receiver.
pub(crate) enum Scoped {
  Done(CallTarget, Confidence),
  /// Several callables in that scope share the name; pass C narrows them.
  Ambiguous(Vec<Qn>),
}

/// Resolve `name` called by `caller` on a scope `receiver`, among the scopes
/// and callables of the caller's language. An absolute receiver names that
/// path from the top level. A lexical one names the innermost scope of that
/// path the project defines, searched from the caller's own scope outward,
/// one `::` segment at a time, as Ruby and C++ look a name up. A callable
/// of that name in it is the target. A scope that does not define the name
/// (an inherited or generated method) leaves the call bare; no such scope at
/// all makes it external, unless a lexical receiver may name a scope found
/// through ancestors. Where the language tells functions apart by `arity`
/// (Elixir), a callable must take it and be public.
#[allow(clippy::too_many_arguments)] // resolution context is genuinely wide
pub(crate) fn resolve_scoped(
  caller: &Qn,
  receiver: &str,
  lookup: ReceiverScope,
  name: &str,
  arity: Option<usize>,
  nesting: &[String],
  symbol_table: &HashMap<String, Vec<Qn>>,
  callable_meta: &HashMap<Qn, CallableMeta>,
  scopes: &Scopes,
) -> Scoped {
  let lang = qn_lang(caller);
  let is_scope = |path: &[&str]| scopes.paths.contains(&(lang, path.join("::")));
  let normalized = receiver.replace(['\\', '.'], "::");
  let path: Vec<&str> = normalized.split("::").filter(|p| !p.is_empty()).collect();
  // The scopes the path is looked up in, innermost first, ending at the
  // top level: the caller's enclosing scopes, or the ones written around
  // it where a scope is named apart (`RawEdge::nesting`).
  let bases: Vec<Vec<&str>> = match lookup {
    ReceiverScope::Lexical if !nesting.is_empty() => nesting
      .iter()
      .map(|s| dotted_path(&s.split("::").collect::<Vec<_>>()))
      .chain(std::iter::once(Vec::new()))
      .collect(),
    ReceiverScope::Lexical => {
      let enclosing = enclosing_parts(caller);
      (0..=enclosing.len())
        .rev()
        .map(|depth| dotted_path(&enclosing[..depth]))
        .collect()
    }
    ReceiverScope::None | ReceiverScope::Absolute => vec![Vec::new()],
  };
  let found = bases.into_iter().find_map(|mut scope| {
    scope.extend(&path);
    is_scope(&scope).then_some(scope)
  });
  let Some(scope) = found else {
    // Ruby also finds a constant through ancestors (`include Jekyll`
    // makes `Utils` mean `Jekyll::Utils`), which this lookup does not
    // walk. A scope of that name in the caller's language is then a likely
    // target, so the call stays unresolved.
    let a_scope_of_that_name =
      lookup == ReceiverScope::Lexical && scopes.tails.contains(&(lang, path.join("::")));
    if a_scope_of_that_name {
      return Scoped::Done(CallTarget::Bare(name.to_string()), Confidence::Ambiguous);
    }
    return Scoped::Done(
      CallTarget::External(format!("{}.{}", receiver, name)),
      Confidence::Exact,
    );
  };
  let mut cands: Vec<Qn> = symbol_table
    .get(name)
    .into_iter()
    .flatten()
    .filter(|qn| qn_lang(qn) == lang && dotted_path(&enclosing_parts(qn)) == scope)
    .filter(|qn| {
      callable_meta
        .get(*qn)
        .is_none_or(|m| m.accepts(arity, Some(ImportOnly::Any)))
    })
    .cloned()
    .collect();
  match cands.len() {
    0 => Scoped::Done(CallTarget::Bare(name.to_string()), Confidence::Ambiguous),
    1 => Scoped::Done(CallTarget::Resolved(cands.remove(0)), Confidence::Exact),
    _ => Scoped::Ambiguous(cands),
  }
}

/// What an import binding says about a call to its local name.
pub(crate) enum Imported {
  /// The module is a project file that declares the imported name.
  Declared(Qn),
  /// The module is a project file that does not declare the name: the
  /// name to look the call up by elsewhere.
  Undeclared(String),
  /// The module is not a project file, or the language has no resolver.
  Outside,
}

/// Resolve `from <module> import <name>` (or equivalent) by mapping `module`
/// to a project file via the suffix index, then looking `name` up among the
/// declarations of that file.
#[allow(clippy::too_many_arguments)] // resolution context is genuinely wide
pub(crate) fn resolve_via_imports(
  binding: &ImportBinding,
  from_file: &Path,
  lang: Option<Lang>,
  aliases: &crate::deps::manifest::ProjectAliases,
  idx: &crate::deps::resolver::SuffixIndex,
  root: &Path,
  symbol_table: &HashMap<String, Vec<Qn>>,
  type_by_name: &HashMap<String, Vec<Qn>>,
  callable_meta: &HashMap<Qn, CallableMeta>,
) -> Imported {
  let Some(lang) = lang else {
    return Imported::Outside;
  };
  let ctx = ResolveCtx {
    from_file,
    lang,
    path_aliases: &aliases.ts_path_aliases,
    base_url: aliases.ts_base_url.as_deref(),
    php_psr4: &aliases.php_psr4,
  };
  // A binding of one declaration (`from a import real`) may name it in
  // the module's spec; see `resolve_from_import`.
  let target_file = if binding.imported.is_some() {
    resolve_from_import(&binding.module, &ctx, idx)
  } else {
    resolve_spec(&binding.module, &ctx, idx)
  };
  let Some(target_file) = target_file else {
    return Imported::Outside;
  };
  let rel = file_rel(root, &target_file);
  // The name the module declares: `real` in `real as alias`. A binding of
  // a whole module or a default export has none, and goes by its local name.
  let name = binding.imported.as_deref().unwrap_or(&binding.local);

  // A function in the resolved file, or a type there: a construction
  // targets its type's qn. A method is not something a module exports, so
  // `A::real` is not the `real` it exports.
  let declared = symbol_table
    .get(name)
    .into_iter()
    .flatten()
    .filter(|qn| callable_meta.get(*qn).is_some_and(|m| m.kind == "function"))
    .chain(
      type_by_name
        .get(&crate::calls::graph::type_key(
          Lang::from_path(&target_file),
          name,
        ))
        .into_iter()
        .flatten()
        .filter(|qn| qn.name() == name),
    )
    .filter(|cand| cand.file() == rel)
    .filter_map(|cand| Some((import_scope(cand, &rel, name, binding, lang)?, cand)))
    // The deepest match, and the first of equals: `max_by_key` would take
    // the last.
    .fold(
      None,
      |best: Option<(usize, &Qn)>, (depth, cand)| match best {
        Some((d, _)) if d >= depth => best,
        _ => Some((depth, cand)),
      },
    )
    .map(|(_, cand)| cand);
  match declared {
    Some(qn) => Imported::Declared(qn.clone()),
    None => Imported::Undeclared(name.to_string()),
  }
}

/// Whether an import binding names `cand`, a declaration of `name` in the
/// file the binding resolved to (`rel`), and if so how deep a match it is:
/// the deepest wins, and the first of equals.
///
/// An import that names one declaration (`imported` is set) names one at
/// module level: `mod inner { fn real() }` is not the `real` a
/// `use crate::a::real` names, which may be a re-export (`pub use`) the
/// passes below follow. Rust is the exception, since its path goes on into
/// the file: `use crate::a::inner::real` names `inner::real`, so the scope
/// must be the tail of the path before the name, and the longest such tail
/// wins. A binding of a whole module or a default export goes by its local
/// name and keeps the first match, as all matches rank 0.
fn import_scope(
  cand: &Qn,
  rel: &str,
  name: &str,
  binding: &ImportBinding,
  lang: Lang,
) -> Option<usize> {
  if binding.imported.is_none() {
    return Some(0);
  }
  let scope = cand
    .as_str()
    .strip_prefix(rel)
    .and_then(|s| s.strip_prefix("::"))
    .and_then(|s| s.strip_suffix(name))
    .map(|s| s.trim_end_matches("::"))
    .unwrap_or("");
  let depth = if scope.is_empty() {
    0
  } else {
    scope.split("::").count()
  };
  if depth == 0 {
    return Some(0);
  }
  let path_ends_in_scope = lang == Lang::Rust
    && binding
      .module
      .strip_suffix(name)
      .and_then(|m| m.strip_suffix("::"))
      .is_some_and(|m| m == scope || m.ends_with(&format!("::{scope}")));
  path_ends_in_scope.then_some(depth)
}

/// Per-file forward-dep closures, memoized. A file's closure is the same for
/// every ambiguous edge inside it, and the incremental updater re-asks for it
/// across a whole sweep.
#[derive(Default)]
pub(crate) struct ClosureCache(HashMap<PathBuf, HashSet<PathBuf>>);

impl ClosureCache {
  fn get(&mut self, deps: &DepGraph, from: &Path) -> &HashSet<PathBuf> {
    self
      .0
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
  file
    .strip_prefix(root)
    .map(|p| p.to_path_buf())
    .unwrap_or_else(|_| file.to_path_buf())
}
