//! TypeScript / JavaScript public-surface resolver.
//!
//! Algorithm:
//! 1. Resolve the entry file via `package.json` (`exports` w/ conditional
//!    resolution, then `module`/`main`/`types`) or `index.{ts,tsx,...}`.
//! 2. For each loaded file, run [`extract_ts_exports`] to enumerate
//!    every export form (the existing TS adapter only catches the
//!    inline `export class/fn/const/...` forms; barrels and rename
//!    re-exports come from here).
//! 3. BFS through `export ... from './x'` chains, expanding namespace
//!    re-exports and `export * from` globs.
//!
//! Module resolution is the small subset of Node we actually need:
//! relative paths only, with extension probing (`.ts` → `.tsx` → `.js`
//! → ... → `.d.ts`) and directory `index.*` fallback. Bare specifiers
//! (`react`, `lodash`, etc.) are recorded as external hops but not
//! followed.

use crate::core::{Declaration, DeclarationKind};
use crate::parse_file;
use crate::surface::entry::{ReExportHop, SurfaceEntry};
use crate::surface::entry_point::EntryPoint;
use crate::surface::imports::{self, NamedBinding, TsExportItem, TsKind};
use crate::surface::options::{SurfaceError, SurfaceOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn resolve(
    entry: &EntryPoint,
    opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    let (root_file, pkg_name) = match entry {
        EntryPoint::TsPackage { root_file, pkg_name } => (root_file.clone(), pkg_name.clone()),
        _ => {
            return Err(SurfaceError::NoEntryPoint {
                path: PathBuf::from("."),
                hint: "typescript::resolve called with non-TS entry point".into(),
            });
        }
    };

    let mut walker = Walker {
        max_depth: opts.max_depth,
        loaded: HashMap::new(),
        entries: Vec::new(),
        seen_qualified: HashSet::new(),
    };
    walker.walk_file(&root_file, &[pkg_name], 0, vec![]);
    Ok(walker.entries)
}

struct Walker {
    max_depth: usize,
    loaded: HashMap<PathBuf, FileSnapshot>,
    entries: Vec<SurfaceEntry>,
    seen_qualified: HashSet<String>,
}

struct FileSnapshot {
    decls: Vec<Declaration>,
    exports: Vec<TsExportItem>,
}

impl Walker {
    fn walk_file(
        &mut self,
        file: &Path,
        prefix: &[String],
        depth: usize,
        chain: Vec<ReExportHop>,
    ) {
        if depth > self.max_depth {
            return;
        }
        let snap = match self._load(file) {
            Some(s) => s,
            None => return,
        };
        let decls = snap.decls.clone();
        let exports = snap.exports.clone();

        // 1. Pick up every inline-exported declaration. The TS adapter
        //    already filters non-`export` decls, so anything in `decls`
        //    is part of the module's namespace.
        //    We restrict to ones the imports.rs scan also flagged, so
        //    bare locals aren't surfaced.
        let inline_names: HashSet<String> = exports
            .iter()
            .filter_map(|e| match e {
                TsExportItem::Local { name, .. } => Some(name.clone()),
                TsExportItem::Default { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();

        for d in &decls {
            if inline_names.contains(&d.name) {
                self._emit(prefix, &d.name, d, file, chain.clone(), false);
            }
        }

        // 2. Process named re-exports of locals (`export { local }`).
        for ex in &exports {
            if let TsExportItem::Named { bindings, .. } = ex {
                for b in bindings {
                    if let Some(d) = decls.iter().find(|d| d.name == b.name) {
                        let exposed = b.alias.clone().unwrap_or_else(|| b.name.clone());
                        self._emit(prefix, &exposed, d, file, chain.clone(), false);
                    }
                }
            }
        }

        // 3. Follow re-exports from other files.
        for ex in &exports {
            match ex {
                TsExportItem::NamedFrom {
                    from,
                    bindings,
                    line,
                    statement,
                } => {
                    if let Some(target) = _resolve_module(file, from) {
                        let hop = ReExportHop {
                            file: file.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        self._follow_named(
                            &target,
                            prefix,
                            bindings,
                            depth + 1,
                            _push(chain.clone(), hop),
                        );
                    }
                }
                TsExportItem::StarFrom {
                    from,
                    line,
                    statement,
                } => {
                    if let Some(target) = _resolve_module(file, from) {
                        let hop = ReExportHop {
                            file: file.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        self._follow_star(
                            &target,
                            prefix,
                            depth + 1,
                            _push(chain.clone(), hop),
                            true,
                        );
                    }
                }
                TsExportItem::NamespaceFrom {
                    ns,
                    from,
                    line,
                    statement,
                } => {
                    if let Some(target) = _resolve_module(file, from) {
                        let hop = ReExportHop {
                            file: file.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        let mut ns_prefix = prefix.to_vec();
                        ns_prefix.push(ns.clone());
                        self._follow_star(
                            &target,
                            &ns_prefix,
                            depth + 1,
                            _push(chain.clone(), hop),
                            false,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn _follow_named(
        &mut self,
        target: &Path,
        prefix: &[String],
        bindings: &[NamedBinding],
        depth: usize,
        chain: Vec<ReExportHop>,
    ) {
        if depth > self.max_depth {
            return;
        }
        let snap = match self._load(target) {
            Some(s) => s,
            None => return,
        };
        let decls = snap.decls.clone();
        let exports = snap.exports.clone();
        for b in bindings {
            let exposed = b.alias.clone().unwrap_or_else(|| b.name.clone());
            // Defined in target?
            if let Some(d) = decls.iter().find(|d| d.name == b.name) {
                self._emit(prefix, &exposed, d, target, chain.clone(), false);
                continue;
            }
            // Re-exported by target?
            self._chase_indirect(target, prefix, &b.name, &exposed, &exports, depth, chain.clone());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn _chase_indirect(
        &mut self,
        from_file: &Path,
        prefix: &[String],
        source_name: &str,
        exposed: &str,
        exports: &[TsExportItem],
        depth: usize,
        chain: Vec<ReExportHop>,
    ) {
        for ex in exports {
            match ex {
                TsExportItem::NamedFrom {
                    from,
                    bindings,
                    line,
                    statement,
                } => {
                    let hit = bindings.iter().find(|b| {
                        let local = b.alias.as_deref().unwrap_or(&b.name);
                        local == source_name
                    });
                    if let Some(b) = hit {
                        if let Some(target) = _resolve_module(from_file, from) {
                            let hop = ReExportHop {
                                file: from_file.to_path_buf(),
                                line: *line,
                                module_path: prefix.join("."),
                                statement: statement.clone(),
                            };
                            self._follow_named(
                                &target,
                                prefix,
                                &[NamedBinding {
                                    name: b.name.clone(),
                                    alias: Some(exposed.to_string()),
                                }],
                                depth + 1,
                                _push(chain.clone(), hop),
                            );
                            return;
                        }
                    }
                }
                TsExportItem::StarFrom { from, line, statement } => {
                    if let Some(target) = _resolve_module(from_file, from) {
                        let hop = ReExportHop {
                            file: from_file.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        // Star may transit any name; recurse looking for it.
                        let snap = self._load(&target);
                        if let Some(s) = snap {
                            if let Some(d) = s.decls.iter().find(|d| d.name == source_name).cloned()
                            {
                                self._emit(prefix, exposed, &d, &target, _push(chain.clone(), hop), true);
                                return;
                            }
                            let exports2 = s.exports.clone();
                            self._chase_indirect(
                                &target,
                                prefix,
                                source_name,
                                exposed,
                                &exports2,
                                depth + 1,
                                _push(chain.clone(), hop),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn _follow_star(
        &mut self,
        target: &Path,
        prefix: &[String],
        depth: usize,
        chain: Vec<ReExportHop>,
        skip_default: bool,
    ) {
        if depth > self.max_depth {
            return;
        }
        let snap = match self._load(target) {
            Some(s) => s,
            None => return,
        };
        let decls = snap.decls.clone();
        let exports = snap.exports.clone();

        // Names locally defined and exported.
        for ex in &exports {
            match ex {
                TsExportItem::Local { name, .. } => {
                    if let Some(d) = decls.iter().find(|d| &d.name == name) {
                        self._emit(prefix, name, d, target, chain.clone(), true);
                    }
                }
                TsExportItem::Default { name, .. } => {
                    if skip_default {
                        // Per Node.js semantics, `export *` skips default.
                        continue;
                    }
                    if let Some(d) = decls.iter().find(|d| &d.name == name) {
                        self._emit(prefix, "default", d, target, chain.clone(), true);
                    }
                }
                TsExportItem::Named { bindings, .. } => {
                    for b in bindings {
                        let exposed = b.alias.clone().unwrap_or_else(|| b.name.clone());
                        if let Some(d) = decls.iter().find(|d| d.name == b.name) {
                            self._emit(prefix, &exposed, d, target, chain.clone(), true);
                        }
                    }
                }
                _ => {}
            }
        }
        // Recurse through `export *` and `export { ... } from`.
        for ex in &exports {
            match ex {
                TsExportItem::StarFrom {
                    from,
                    line,
                    statement,
                } => {
                    if let Some(t2) = _resolve_module(target, from) {
                        let hop = ReExportHop {
                            file: target.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        self._follow_star(&t2, prefix, depth + 1, _push(chain.clone(), hop), true);
                    }
                }
                TsExportItem::NamedFrom {
                    from,
                    bindings,
                    line,
                    statement,
                } => {
                    if let Some(t2) = _resolve_module(target, from) {
                        let hop = ReExportHop {
                            file: target.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        self._follow_named(
                            &t2,
                            prefix,
                            bindings,
                            depth + 1,
                            _push(chain.clone(), hop),
                        );
                    }
                }
                TsExportItem::NamespaceFrom {
                    ns,
                    from,
                    line,
                    statement,
                } => {
                    if let Some(t2) = _resolve_module(target, from) {
                        let hop = ReExportHop {
                            file: target.to_path_buf(),
                            line: *line,
                            module_path: prefix.join("."),
                            statement: statement.clone(),
                        };
                        let mut ns_prefix = prefix.to_vec();
                        ns_prefix.push(ns.clone());
                        self._follow_star(
                            &t2,
                            &ns_prefix,
                            depth + 1,
                            _push(chain.clone(), hop),
                            false,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn _load(&mut self, file: &Path) -> Option<&FileSnapshot> {
        if !self.loaded.contains_key(file) {
            let parse = parse_file(file)?;
            let src = std::str::from_utf8(&parse.source).ok()?.to_string();
            let kind = TsKind::from_path(file).unwrap_or(TsKind::TypeScript);
            let exports = imports::extract_ts_exports(&src, kind).items;
            self.loaded.insert(
                file.to_path_buf(),
                FileSnapshot {
                    decls: parse.declarations,
                    exports,
                },
            );
        }
        self.loaded.get(file)
    }

    fn _emit(
        &mut self,
        prefix: &[String],
        exposed: &str,
        decl: &Declaration,
        source: &Path,
        chain: Vec<ReExportHop>,
        via_glob: bool,
    ) {
        if exposed.is_empty() {
            return;
        }
        let qpath = format!("{}.{}", prefix.join("."), exposed);
        if !self.seen_qualified.insert(qpath.clone()) {
            return;
        }
        // Lift class methods so `pkg.Foo.bar` shows up too. Skip private.
        let kind_lifts = matches!(
            decl.kind,
            DeclarationKind::Class | DeclarationKind::Interface | DeclarationKind::Enum
        );
        self.entries.push(SurfaceEntry {
            qualified_path: qpath.clone(),
            kind: decl.kind,
            signature: decl.signature.clone(),
            source_path: source.to_path_buf(),
            source_line: decl.start_line,
            source_name: decl.name.clone(),
            re_export_chain: chain.clone(),
            via_glob,
            docs: decl.docs.clone(),
        });
        if kind_lifts {
            for child in &decl.children {
                if child.visibility == "private" || child.visibility == "protected" {
                    continue;
                }
                if child.name.is_empty() {
                    continue;
                }
                let child_q = format!("{}.{}", qpath, child.name);
                if !self.seen_qualified.insert(child_q.clone()) {
                    continue;
                }
                self.entries.push(SurfaceEntry {
                    qualified_path: child_q,
                    kind: child.kind,
                    signature: child.signature.clone(),
                    source_path: source.to_path_buf(),
                    source_line: child.start_line,
                    source_name: child.name.clone(),
                    re_export_chain: chain.clone(),
                    via_glob,
                    docs: child.docs.clone(),
                });
            }
        }
    }
}

/// Resolve a relative module specifier (`./foo`, `../bar/baz`) to a file
/// on disk. Bare specifiers (`react`, `@scope/pkg`) are intentionally
/// returned as `None` — we can't follow them without traversing
/// `node_modules`, and that's out of scope.
fn _resolve_module(from_file: &Path, spec: &str) -> Option<PathBuf> {
    if !spec.starts_with('.') {
        return None;
    }
    let parent = from_file.parent()?;
    let base = parent.join(spec);

    // Strip an explicit `.js`/`.mjs`/`.cjs` extension and try `.ts` first
    // (TS source for compiled JS imports — common pattern).
    if let Some(stem_path) = _strip_js_extension(&base) {
        if let Some(p) = _probe_extensions(&stem_path) {
            return Some(p);
        }
    }

    // Direct file with one of the source extensions.
    if let Some(p) = _probe_extensions(&base) {
        return Some(p);
    }

    // Directory with index.*
    if base.is_dir() {
        if let Some(p) = _probe_extensions(&base.join("index")) {
            return Some(p);
        }
    }

    None
}

fn _strip_js_extension(p: &Path) -> Option<PathBuf> {
    let ext = p.extension().and_then(|s| s.to_str())?;
    if matches!(ext, "js" | "jsx" | "mjs" | "cjs") {
        let stem = p.file_stem()?.to_str()?;
        return Some(p.with_file_name(stem));
    }
    None
}

fn _probe_extensions(stem: &Path) -> Option<PathBuf> {
    // If the path already exists as a file, take it.
    if stem.is_file() {
        return Some(stem.to_path_buf());
    }
    for ext in ["ts", "tsx", "mts", "cts", "d.ts", "js", "jsx", "mjs", "cjs"] {
        let cand = stem.with_extension(ext);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn _push(mut v: Vec<ReExportHop>, h: ReExportHop) -> Vec<ReExportHop> {
    v.push(h);
    v
}
