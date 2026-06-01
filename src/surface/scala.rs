//! Scala 3 public-surface resolver.
//!
//! Scala doesn't have a single "package entry point" the way Rust has
//! `lib.rs`. Instead, every `.scala` file under `root` contributes
//! top-level declarations to its declared package. The resolver:
//!
//! 1. Walks all `.scala` files under `root`, parsing each.
//! 2. Records every public top-level decl, qualified by package.
//! 3. Honours Scala 3 `export` clauses (`export foo.Bar`,
//!    `export foo.{A, B as C}`, `export foo.*`) — these re-publish
//!    decls from one package/object under another name.
//!
//! `private`/`private[...]` decls are filtered. Methods inside public
//! `object`/`class`/`trait` bodies are lifted as `pkg.Owner.method`.

use crate::core::{Declaration, DeclarationKind};
use crate::surface::entry::{ReExportHop, SurfaceEntry};
use crate::surface::entry_point::EntryPoint;
use crate::surface::imports::{self, ScalaExportItem, ScalaExportKind};
use crate::surface::options::{SurfaceError, SurfaceOptions};
use crate::walk_and_parse;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn resolve(
    entry: &EntryPoint,
    _opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    let root = match entry {
        EntryPoint::ScalaPackage { root, .. } => root.clone(),
        _ => {
            return Err(SurfaceError::NoEntryPoint {
                path: PathBuf::from("."),
                hint: "scala::resolve called with non-Scala entry point".into(),
            });
        }
    };

    // Walk every Scala file under root using the existing infrastructure.
    let results = walk_and_parse(std::slice::from_ref(&root), None);

    // Index decls by qualified path (package.Decl) for `export` resolution.
    let mut by_qpath: HashMap<String, (Declaration, PathBuf)> = HashMap::new();
    let mut exports: Vec<(PathBuf, String, Vec<ScalaExportItem>)> = Vec::new();

    for r in &results {
        if r.language != "scala" {
            continue;
        }
        let src = std::str::from_utf8(&r.source).unwrap_or("").to_string();
        let parsed_exports = imports::extract_scala_exports(&src);
        let pkg_from_clause = parsed_exports.package.clone().unwrap_or_default();
        // Walk top-level decls. The Scala adapter wraps everything under
        // a Namespace decl named for the package (`mypkg.internal`),
        // so use that name as our prefix instead of stacking it onto
        // the package clause.
        for d in &r.declarations {
            _absorb_top_level(d, &pkg_from_clause, &r.path, &mut by_qpath);
        }
        if !parsed_exports.items.is_empty() {
            exports.push((r.path.clone(), pkg_from_clause, parsed_exports.items));
        }
    }

    let mut out: Vec<SurfaceEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Emit every indexed public decl directly.
    let mut keys: Vec<&String> = by_qpath.keys().collect();
    keys.sort();
    for k in keys {
        let (d, file) = &by_qpath[k];
        if !seen.insert(k.clone()) {
            continue;
        }
        out.push(SurfaceEntry {
            qualified_path: k.clone(),
            kind: d.kind,
            signature: d.signature.clone(),
            source_path: file.clone(),
            source_line: d.start_line,
            source_name: d.name.clone(),
            re_export_chain: Vec::new(),
            via_glob: false,
            docs: d.docs.clone(),
        });
    }

    // 2. Process `export` clauses — re-publish under the local package.
    //    Scala identifiers in export paths can be relative (resolved
    //    against the enclosing package) or absolute. We try both.
    for (file, pkg, items) in &exports {
        for item in items {
            let hop = ReExportHop {
                file: file.clone(),
                line: item.line,
                module_path: pkg.clone(),
                statement: item.statement.clone(),
            };
            match item.kind {
                ScalaExportKind::Selectors => {
                    let bindings: Vec<NamedBindingLite> = if item.bindings.is_empty() {
                        // `export foo.Bar` — last segment of `from` is the binding.
                        let mut parts: Vec<&str> = item.from.split('.').collect();
                        let last = parts.pop().unwrap_or("").to_string();
                        let new_from = parts.join(".");
                        vec![NamedBindingLite {
                            from: new_from,
                            name: last.clone(),
                            alias: last,
                        }]
                    } else {
                        item.bindings
                            .iter()
                            .map(|b| NamedBindingLite {
                                from: item.from.clone(),
                                name: b.name.clone(),
                                alias: b.alias.clone().unwrap_or_else(|| b.name.clone()),
                            })
                            .collect()
                    };
                    for b in bindings {
                        let candidates = _resolve_candidates(&b.from, &b.name, pkg);
                        for source_q in &candidates {
                            if let Some((d, src_file)) = by_qpath.get(source_q) {
                                let local_q = if pkg.is_empty() {
                                    b.alias.clone()
                                } else {
                                    format!("{}.{}", pkg, b.alias)
                                };
                                if !seen.insert(local_q.clone()) {
                                    break;
                                }
                                out.push(SurfaceEntry {
                                    qualified_path: local_q,
                                    kind: d.kind,
                                    signature: d.signature.clone(),
                                    source_path: src_file.clone(),
                                    source_line: d.start_line,
                                    source_name: d.name.clone(),
                                    re_export_chain: vec![hop.clone()],
                                    via_glob: false,
                                    docs: d.docs.clone(),
                                });
                                break;
                            }
                        }
                    }
                }
                ScalaExportKind::Glob => {
                    let candidates = _resolve_glob_prefixes(&item.from, pkg);
                    for prefix in &candidates {
                        let dotted = format!("{}.", prefix);
                        let mut hit = false;
                        for (q, (d, src_file)) in &by_qpath {
                            if !q.starts_with(&dotted) {
                                continue;
                            }
                            let leaf = &q[dotted.len()..];
                            if leaf.contains('.') {
                                continue;
                            }
                            hit = true;
                            let local_q = if pkg.is_empty() {
                                leaf.to_string()
                            } else {
                                format!("{}.{}", pkg, leaf)
                            };
                            if seen.insert(local_q.clone()) {
                                out.push(SurfaceEntry {
                                    qualified_path: local_q,
                                    kind: d.kind,
                                    signature: d.signature.clone(),
                                    source_path: src_file.clone(),
                                    source_line: d.start_line,
                                    source_name: d.name.clone(),
                                    re_export_chain: vec![hop.clone()],
                                    via_glob: true,
                                    docs: d.docs.clone(),
                                });
                            }
                        }
                        if hit {
                            break;
                        }
                    }
                }
            }
        }
    }

    out.sort_by(|a, b| a.qualified_path.cmp(&b.qualified_path));
    Ok(out)
}

/// Top-level entry: if the decl is a Namespace (the package wrapper
/// the Scala adapter inserts), descend into its children using the
/// namespace name as the qualified prefix. Otherwise apply the
/// `pkg_from_clause` prefix and emit the decl directly.
fn _absorb_top_level(
    d: &Declaration,
    pkg_from_clause: &str,
    file: &std::path::Path,
    out: &mut HashMap<String, (Declaration, PathBuf)>,
) {
    if matches!(d.kind, DeclarationKind::Namespace) {
        let pkg_name = if !d.name.is_empty() {
            d.name.clone()
        } else {
            pkg_from_clause.to_string()
        };
        for child in &d.children {
            _absorb_top_level(child, &pkg_name, file, out);
        }
        return;
    }
    if !_is_public(d) || d.name.is_empty() {
        return;
    }
    let q = if pkg_from_clause.is_empty() {
        d.name.clone()
    } else {
        format!("{}.{}", pkg_from_clause, d.name)
    };
    out.entry(q.clone())
        .or_insert_with(|| (d.clone(), file.to_path_buf()));
    _index_children(d, &q, file, out);
}

fn _index_children(
    d: &Declaration,
    parent_q: &str,
    file: &std::path::Path,
    out: &mut HashMap<String, (Declaration, PathBuf)>,
) {
    if !_is_object_like(d) {
        return;
    }
    for child in &d.children {
        if !_is_public(child) || child.name.is_empty() {
            continue;
        }
        let q = format!("{}.{}", parent_q, child.name);
        out.entry(q.clone())
            .or_insert_with(|| (child.clone(), file.to_path_buf()));
        _index_children(child, &q, file, out);
    }
}

fn _is_object_like(d: &Declaration) -> bool {
    use DeclarationKind::*;
    matches!(d.kind, Class | Interface | Struct | Record | Enum | Namespace)
}

fn _is_public(d: &Declaration) -> bool {
    let v = d.visibility.trim();
    v.is_empty() || v == "public"
}

struct NamedBindingLite {
    from: String,
    name: String,
    alias: String,
}

/// Try the bare `from.name` (already qualified) and then prefixed
/// candidates of decreasing specificity. Scala's `export internal.Foo`
/// inside `package mypkg` resolves to `mypkg.internal.Foo`; an
/// `export mypkg.internal.Foo` would resolve absolutely.
fn _resolve_candidates(from: &str, name: &str, enclosing_pkg: &str) -> Vec<String> {
    let leaf = if from.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", from, name)
    };
    let mut out = vec![leaf.clone()];
    if !enclosing_pkg.is_empty() {
        out.push(format!("{}.{}", enclosing_pkg, leaf));
        // Also try parent packages so `export utils.foo` from
        // `mypkg.api` finds `mypkg.utils.foo`.
        let mut parts: Vec<&str> = enclosing_pkg.split('.').collect();
        while parts.pop().is_some() && !parts.is_empty() {
            out.push(format!("{}.{}", parts.join("."), leaf));
        }
    }
    out
}

fn _resolve_glob_prefixes(from: &str, enclosing_pkg: &str) -> Vec<String> {
    let mut out = vec![from.to_string()];
    if !enclosing_pkg.is_empty() {
        out.push(format!("{}.{}", enclosing_pkg, from));
        let mut parts: Vec<&str> = enclosing_pkg.split('.').collect();
        while parts.pop().is_some() && !parts.is_empty() {
            out.push(format!("{}.{}", parts.join("."), from));
        }
    }
    out
}
