//! Shared helpers used by both `cli.rs` (one-shot CLI) and `mcp.rs`
//! (long-lived MCP server). Pulled out so the two callers can stay thin
//! while the symbol-resolution logic has one home.

use crate::calls::graph::{CallGraph, Qn};
use crate::calls::resolve::qn_lang;
use crate::deps::resolver::Lang;
use std::collections::BTreeSet;

/// What a resolved target qn refers to. Lets the caller dispatch to either
/// the call-edge path (`Callable`) or the type-aware path (`Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// qn is a function/method/constructor — present in `forward`/`reverse`.
    Callable,
    /// qn is a class/struct/trait/interface/enum/record — present in
    /// `types` / `type_by_name` but not in the call edges.
    Type,
}

#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub qn: Qn,
    pub kind: SymbolKind,
}

/// Suffix-match `target` against every qn in the graph, mirroring the
/// `show`/`implements` mental model. Accepts:
/// - bare name: `TakeDamage` → matches `*::TakeDamage`.
/// - dotted path: `Player.TakeDamage` → matches `*::Player::TakeDamage`.
/// - explicit qn: `src/Player.cs::Player::TakeDamage` → exact.
/// - file-scoped: `src/Player.cs:TakeDamage` (single `:`) → restricts the
///   match to qns whose file equals or ends with that path. Useful when the
///   same name lives in multiple files. The symbol part still supports the
///   dotted form (`src/Player.cs:Player.TakeDamage`).
///
/// Returns *only* callable qns. For the kind-aware variant that also
/// surfaces types, use [`resolve_target_full`].
pub fn resolve_target_qns(calls: &CallGraph, target: &str) -> Vec<Qn> {
    resolve_target_full(calls, target)
        .into_iter()
        .filter(|r| r.kind == SymbolKind::Callable)
        .map(|r| r.qn)
        .collect()
}

/// Kind-aware resolution: returns every matching qn paired with whether
/// it's a callable or a type. Both halves are searched and deduped together.
pub fn resolve_target_full(calls: &CallGraph, target: &str) -> Vec<ResolvedTarget> {
    let (file_filter, symbol) = split_file_filter(target);
    let parts: Vec<&str> = symbol.split('.').collect();

    // Each match with whether it starts at a declaration's name rather than
    // inside a dotted one: `Shop.Money` names `Shop.Money` in full, and only
    // ends `Inspect.Shop.Money`.
    let mut matches: Vec<(ResolvedTarget, bool)> = Vec::new();
    // An Elixir struct literal's edge targets its module, which is a type.
    // Elsewhere a function and a type may share a qn (PHP `function Foo`
    // beside `class Foo`), and both are matched.
    for qn in collect_callable_qns(calls)
        .into_iter()
        .filter(|q| !(calls.types.contains_key(q) && qn_lang(q) == Some(Lang::Elixir)))
    {
        if let Some(filter) = file_filter {
            if !file_matches(&qn, filter) {
                continue;
            }
        }
        if let Some(whole) = qn_matches(&qn, symbol, &parts) {
            let target = ResolvedTarget {
                qn,
                kind: SymbolKind::Callable,
            };
            matches.push((target, whole));
        }
    }
    for qn in calls.types.keys() {
        if let Some(filter) = file_filter {
            if !file_matches(qn, filter) {
                continue;
            }
        }
        if let Some(whole) = qn_matches(qn, symbol, &parts) {
            let target = ResolvedTarget {
                qn: qn.clone(),
                kind: SymbolKind::Type,
            };
            matches.push((target, whole));
        }
    }
    let any_whole = matches.iter().any(|(_, whole)| *whole);
    let mut out: Vec<ResolvedTarget> = matches
        .into_iter()
        .filter(|(_, whole)| *whole || !any_whole)
        .map(|(target, _)| target)
        .collect();
    out.sort_by(|a, b| a.qn.0.cmp(&b.qn.0));
    out.dedup_by(|a, b| a.qn == b.qn && a.kind == b.kind);
    out
}

/// `src/foo.rs:bar` → `(Some("src/foo.rs"), "bar")`. Plain symbol → `(None, target)`.
/// `::`-only inputs (already-qualified qns) stay intact.
fn split_file_filter(target: &str) -> (Option<&str>, &str) {
    let mut i = 0;
    let bytes = target.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b':' {
            // Skip over `::` (already-qualified-name segment separator).
            if bytes.get(i + 1) == Some(&b':') {
                i += 2;
                continue;
            }
            let (file, rest) = (&target[..i], &target[i + 1..]);
            if file.is_empty() || rest.is_empty() {
                return (None, target);
            }
            return (Some(file), rest);
        }
        i += 1;
    }
    (None, target)
}

fn file_matches(qn: &Qn, filter: &str) -> bool {
    let f = qn.file();
    if f == filter {
        return true;
    }
    // Trailing-path match — `Player.cs` matches `src/game/Player.cs`.
    f.ends_with(filter) && f.as_bytes().get(f.len().saturating_sub(filter.len()).saturating_sub(1)) == Some(&b'/')
}

fn collect_callable_qns(calls: &CallGraph) -> Vec<Qn> {
    let mut s: BTreeSet<Qn> = BTreeSet::new();
    for k in calls.forward.keys() {
        s.insert(k.clone());
    }
    for k in calls.reverse.keys() {
        s.insert(k.clone());
    }
    s.into_iter().collect()
}

/// `None` when `qn` does not end with the query, else whether the query
/// starts at the start of a qn segment.
fn qn_matches(qn: &Qn, raw: &str, parts: &[&str]) -> Option<bool> {
    if qn.0 == raw {
        return Some(true);
    }
    // A dotted name (`namespace Shop.Billing`) is one qn segment but one
    // query part per dot, as `show` reads it. The file stays whole, and so
    // do a name that is nothing but dots and a free-text label with a space
    // (ExUnit's `test reads map.field`).
    let mut split = qn.0.split("::");
    let file = split.next();
    let segments: Vec<(&str, bool)> = file
        .into_iter()
        .map(|f| (f, true))
        .chain(split.flat_map(|s| {
            let pieces: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
            if pieces.is_empty() || s.contains(' ') {
                vec![(s, true)]
            } else {
                pieces
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| (p, i == 0))
                    .collect()
            }
        }))
        .collect();
    if parts.len() > segments.len() {
        return None;
    }
    let start = segments.len() - parts.len();
    parts
        .iter()
        .enumerate()
        .all(|(i, p)| segments[start + i].0 == *p)
        .then_some(segments[start].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_file_filter_plain_symbol() {
        assert_eq!(split_file_filter("greet"), (None, "greet"));
    }

    #[test]
    fn split_file_filter_with_file() {
        assert_eq!(
            split_file_filter("src/main.rs:parse_file"),
            (Some("src/main.rs"), "parse_file")
        );
    }

    #[test]
    fn split_file_filter_double_colon_stays_intact() {
        assert_eq!(
            split_file_filter("src/main.rs::parse_file"),
            (None, "src/main.rs::parse_file")
        );
    }

    #[test]
    fn split_file_filter_with_dotted_symbol() {
        assert_eq!(
            split_file_filter("src/foo.rs:Foo.bar"),
            (Some("src/foo.rs"), "Foo.bar")
        );
    }
}
