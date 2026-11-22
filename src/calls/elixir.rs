//! Elixir call resolution for calls without a module receiver.
//!
//! A module receiver is a scope receiver, which `resolve.rs` handles for
//! every language. The other Elixir calls skip the name-based passes too:
//!
//! - a bare call is the caller's own module's function, then a function of
//!   a module imported where the call is (`CallSite::imports`), or else
//!   external (`Kernel`, a dependency);
//! - a variable receiver (`mod.fun()`) is dispatched at run time and stays
//!   bare;
//! - a struct literal names its module in full, the key `type_by_name`
//!   holds an Elixir module by.
//!
//! `f/1` and `f/2` share a qn; a bare call takes only a function with a
//! definition that accepts its argument count.

use crate::calls::graph::{
  type_key_of, Arity, CallEdge, CallKindCompat, CallTarget, CallableMeta, Confidence, Qn,
};
use crate::calls::pass::RawEdge;
use crate::calls::resolve::qn_lang;
use crate::core::{Declaration, ImportOnly, Imported};
use crate::deps::resolver::Lang;
use std::collections::HashMap;
use std::path::Path;

/// The project's symbol table, type index and callable metadata.
pub(crate) type Tables<'a> = (
  &'a HashMap<String, Vec<Qn>>,
  &'a HashMap<String, Vec<Qn>>,
  &'a HashMap<Qn, CallableMeta>,
);

/// The argument counts an Elixir definition accepts, read from its
/// signature. Empty for another language's file.
pub(crate) fn arities(rel_file: &str, d: &Declaration) -> Vec<Arity> {
  if Lang::from_path(Path::new(rel_file)) != Some(Lang::Elixir) {
    return Vec::new();
  }
  crate::adapters::elixir::arity_range(&d.signature)
    .into_iter()
    .map(|(min, max)| Arity {
      min,
      max,
      is_private: d.visibility == "private",
      is_macro: d.modifiers.iter().any(|m| m == "macro" || m == "guard"),
      is_callback: d.modifiers.iter().any(|m| m == "callback"),
    })
    .collect()
}

/// Resolve a raw edge of an Elixir file.
pub(crate) fn resolve(raw: &RawEdge, tables: Tables) -> (CallTarget, Confidence, Vec<Qn>) {
  let call = Call {
    source: &raw.source,
    name: &raw.bare_name,
    kind: raw.kind,
    dynamic: raw.receiver.is_some(),
    imports: &raw.imports,
    arity: raw.arity,
  };
  call.target(tables)
}

/// Re-resolve a cached edge after an incremental update, as the cold build
/// does: its function, or another definition of it, may have been added or
/// deleted since. Returns whether the edge was an Elixir edge.
pub(crate) fn reresolve(e: &mut CallEdge, tables: Tables) -> bool {
  if Lang::from_path(&e.file) != Some(Lang::Elixir) {
    return false;
  }
  // A struct literal's module is its target's whole qn (`Shop::Pricing`).
  let name = match &e.target {
    CallTarget::Resolved(qn) if matches!(e.kind, CallKindCompat::Construct) => {
      type_key_of(qn, "module")
    }
    target => target.name_or_raw(),
  };
  let call = Call {
    source: &e.source,
    name: &name,
    kind: e.kind,
    dynamic: e.receiver.is_some(),
    imports: &e.imports,
    arity: e.arity,
  };
  (e.target, e.confidence, e.candidates) = call.target(tables);
  true
}

struct Call<'a> {
  source: &'a Qn,
  name: &'a str,
  kind: CallKindCompat,
  dynamic: bool,
  imports: &'a [Imported],
  arity: Option<usize>,
}

impl Call<'_> {
  fn target(&self, tables: Tables) -> (CallTarget, Confidence, Vec<Qn>) {
    let (symbol_table, type_by_name, callable_meta) = tables;
    let name = self.name;
    if matches!(self.kind, CallKindCompat::Construct) {
      let modules = elixir_only(type_by_name.get(name)).collect();
      return pick(name, modules).unwrap_or_else(|| external(name));
    }
    if self.dynamic {
      return (
        CallTarget::Bare(name.to_string()),
        Confidence::Ambiguous,
        Vec::new(),
      );
    }
    // `None`: a call in the module itself, which may name a private one.
    let accepts = |q: &Qn, imported: Option<ImportOnly>| {
      callable_meta
        .get(q)
        .is_none_or(|m| m.accepts(self.arity, imported))
    };
    // A module defined in two files calls its own file's function.
    let own = module_of(self.source);
    let mut local: Vec<Qn> = elixir_only(symbol_table.get(name))
      .filter(|q| module_of(q) == own && accepts(q, None))
      .collect();
    if local.iter().any(|q| q.file() == self.source.file()) {
      local.retain(|q| q.file() == self.source.file());
    }
    if let Some(hit) = pick(name, local) {
      return hit;
    }
    let imported = elixir_only(symbol_table.get(name))
      .filter(|q| {
        let module = module_of(q);
        self
          .imports
          .iter()
          .any(|i| i.module == module && accepts(q, Some(i.only)))
      })
      .collect();
    pick(name, imported).unwrap_or_else(|| external(name))
  }
}

/// The Elixir qns among `qns`.
fn elixir_only(qns: Option<&Vec<Qn>>) -> impl Iterator<Item = Qn> + '_ {
  qns
    .into_iter()
    .flatten()
    .filter(|q| qn_lang(q) == Some(Lang::Elixir))
    .cloned()
}

/// One candidate is the target; several leave the call ambiguous.
fn pick(name: &str, cands: Vec<Qn>) -> Option<(CallTarget, Confidence, Vec<Qn>)> {
  match cands.len() {
    0 => None,
    1 => Some((
      CallTarget::Resolved(cands[0].clone()),
      Confidence::Exact,
      Vec::new(),
    )),
    _ => Some((
      CallTarget::Bare(name.to_string()),
      Confidence::Ambiguous,
      cands,
    )),
  }
}

fn external(name: &str) -> (CallTarget, Confidence, Vec<Qn>) {
  (
    CallTarget::External(name.to_string()),
    Confidence::Inferred,
    Vec::new(),
  )
}

/// The module a qn sits in: its segments between the file and the name,
/// without an ExUnit `describe …` level. A script's top-level function is in
/// no module, `""`.
pub(crate) fn module_of(qn: &Qn) -> String {
  let segments: Vec<&str> = qn.0.split("::").collect();
  if segments.len() < 3 {
    return String::new();
  }
  segments[1..segments.len() - 1]
    .iter()
    .filter(|s| !s.starts_with("describe "))
    .copied()
    .collect::<Vec<_>>()
    .join(".")
}
