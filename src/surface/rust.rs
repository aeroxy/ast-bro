//! Rust public-surface resolver.
//!
//! Walk the module graph from the crate root, collecting (a) every
//! `pub` declaration reachable through a chain of `pub mod`s, and
//! (b) everything re-exported via `pub use ...`. Globs (`pub use foo::*`)
//! enumerate every `pub` item visible at `foo`'s end of the chain.
//! Cycles are broken by a per-walk visited set keyed by
//! `(module_path, item_name)`.

use crate::core::{Declaration, DeclarationKind, ParseResult};
use crate::parse_file;
use crate::surface::entry::{ReExportHop, SurfaceEntry};
use crate::surface::entry_point::EntryPoint;
use crate::surface::imports::{self, RustImports, UseItem, UseSegmentKind};
use crate::surface::module_graph::resolve_mod_file;
use crate::surface::options::{SurfaceError, SurfaceOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn resolve(
    entry: &EntryPoint,
    opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    let (root_file, crate_name) = match entry {
        EntryPoint::RustCrate {
            root_file,
            crate_name,
            ..
        } => (root_file.clone(), crate_name.clone()),
        _ => {
            return Err(SurfaceError::NoEntryPoint {
                path: PathBuf::from("."),
                hint: "rust::resolve called with non-Rust entry point".into(),
            });
        }
    };

    let mut graph = ModuleGraph::new();
    graph.load_file(&root_file, vec![crate_name.clone()])?;

    let mut walker = SurfaceWalker::new(crate_name.clone(), graph, opts.max_depth);
    walker.walk();
    Ok(walker.entries)
}

// ---------------------------------------------------------------------------
// Module loading

struct ModuleData {
    file: PathBuf,
    parse: ParseResult,
    imports: RustImports,
}

struct ModuleGraph {
    /// `module_path` → data. `module_path` is segments like
    /// `["mycrate", "net", "client"]`. Lookup uses joined "::" form.
    modules: HashMap<String, ModuleData>,
    /// Maps a module path to its parent for `super::` resolution.
    parents: HashMap<String, String>,
}

impl ModuleGraph {
    fn new() -> Self {
        Self {
            modules: HashMap::new(),
            parents: HashMap::new(),
        }
    }

    fn key(segments: &[String]) -> String {
        segments.join("::")
    }

    /// Load a file as a module under the given path, recursively
    /// loading any external `mod foo;` references.
    fn load_file(&mut self, file: &Path, segments: Vec<String>) -> Result<(), SurfaceError> {
        let key = Self::key(&segments);
        if self.modules.contains_key(&key) {
            return Ok(());
        }
        let parse = parse_file(file).ok_or_else(|| SurfaceError::Parse {
            path: file.to_path_buf(),
            message: "could not parse file".into(),
        })?;
        let src = std::str::from_utf8(&parse.source).unwrap_or("").to_string();
        let imports = imports::extract_rust_imports(&src);

        // Record parent for `super::`.
        if segments.len() > 1 {
            let parent = segments[..segments.len() - 1].join("::");
            self.parents.insert(key.clone(), parent);
        }

        // Recurse into external `mod foo;` references *before* inserting,
        // so the borrow of `self` doesn't get tangled.
        let mod_refs: Vec<_> = imports
            .mods
            .iter()
            .filter(|m| m.is_external_file)
            .cloned()
            .collect();

        self.modules.insert(
            key,
            ModuleData {
                file: file.to_path_buf(),
                parse,
                imports,
            },
        );

        for m in mod_refs {
            if let Some(child_file) = resolve_mod_file(file, &m) {
                let mut child_segments = segments.clone();
                child_segments.push(m.name.clone());
                let _ = self.load_file(&child_file, child_segments);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Walker

struct SurfaceWalker {
    crate_name: String,
    graph: ModuleGraph,
    max_depth: usize,
    /// Per-walk visited set keyed by `(module_path, item_name)` — breaks
    /// cycles in `pub use` chains.
    visited: HashSet<(String, String)>,
    /// Final emitted entries, dedup'd by qualified_path.
    entries: Vec<SurfaceEntry>,
    seen_qualified: HashSet<String>,
}

impl SurfaceWalker {
    fn new(crate_name: String, graph: ModuleGraph, max_depth: usize) -> Self {
        Self {
            crate_name,
            graph,
            max_depth,
            visited: HashSet::new(),
            entries: Vec::new(),
            seen_qualified: HashSet::new(),
        }
    }

    fn walk(&mut self) {
        // BFS over modules reachable through `pub mod` chains.
        let mut frontier: Vec<Vec<String>> = vec![vec![self.crate_name.clone()]];
        let mut seen_modules: HashSet<String> = HashSet::new();

        while let Some(segments) = frontier.pop() {
            let key = ModuleGraph::key(&segments);
            if !seen_modules.insert(key.clone()) {
                continue;
            }
            let module_owned = match self.graph.modules.get(&key) {
                Some(m) => (
                    m.file.clone(),
                    m.parse.declarations.clone(),
                    m.imports.uses.clone(),
                    m.imports.mods.clone(),
                ),
                None => continue,
            };
            let (mod_file, decls, uses, mods) = module_owned;

            // (a) Emit every `pub` declaration in this module.
            for d in &decls {
                if matches!(d.kind, DeclarationKind::Namespace) {
                    continue;
                }
                // Lift `pub fn` methods out of `impl` blocks so the surface
                // shows `mycrate::Client::connect` even though the impl block
                // itself isn't `pub`.
                //
                // Two cases land here:
                //   (a) `impl Foo` for a foreign type — Rust adapter still
                //       emits these as `kind=Class, name="impl_Foo"`.
                //   (b) Local `impl` blocks — the adapter regroups them into
                //       the target type's `children` (see _walk_mod in
                //       src/adapters/rust.rs). So a Struct/Enum/Trait now
                //       carries its impl methods as children, and we lift
                //       them onto `Type::method` from there.
                if let Some(impl_target) = _impl_target(d) {
                    let mut nested = segments.clone();
                    nested.push(impl_target.to_string());
                    for child in &d.children {
                        if _is_public(child) {
                            self._emit_decl(&nested, child, &mod_file, vec![], false);
                        }
                    }
                    continue;
                }
                if _is_public(d) {
                    self._emit_decl(&segments, d, &mod_file, vec![], false);
                    if _is_type_with_methods(d) {
                        let mut nested = segments.clone();
                        nested.push(d.name.clone());
                        for child in &d.children {
                            if _is_public(child) && _is_method_like(child) {
                                self._emit_decl(&nested, child, &mod_file, vec![], false);
                            }
                        }
                    }
                }
            }

            // (b) Schedule child `pub mod` walks.
            for m in &mods {
                if !_vis_is_public(&m.visibility) {
                    continue;
                }
                let mut child = segments.clone();
                child.push(m.name.clone());
                frontier.push(child);
            }

            // (c) Process `pub use` re-exports.
            for u in &uses {
                if !_vis_is_public(&u.visibility) {
                    continue;
                }
                let hop = ReExportHop {
                    file: mod_file.clone(),
                    line: u.line,
                    module_path: key.clone(),
                    statement: u.statement.clone(),
                };
                self._resolve_use(&segments, u, vec![hop], 0);
            }
        }
    }

    fn _emit_decl(
        &mut self,
        module_segments: &[String],
        decl: &Declaration,
        source_file: &Path,
        chain: Vec<ReExportHop>,
        via_glob: bool,
    ) {
        if decl.name.is_empty() {
            return;
        }
        let qpath = format!("{}::{}", module_segments.join("::"), decl.name);
        if !self.seen_qualified.insert(qpath.clone()) {
            return;
        }
        self.entries.push(SurfaceEntry {
            qualified_path: qpath,
            kind: decl.kind,
            signature: decl.signature.clone(),
            source_path: source_file.to_path_buf(),
            source_line: decl.start_line,
            source_name: decl.name.clone(),
            re_export_chain: chain,
            via_glob,
            docs: decl.docs.clone(),
        });
    }

    fn _emit_renamed(
        &mut self,
        target_module: &[String],
        rename_to: &str,
        source_decl: &Declaration,
        source_file: &Path,
        chain: Vec<ReExportHop>,
        via_glob: bool,
    ) {
        let qpath = format!("{}::{}", target_module.join("::"), rename_to);
        if !self.seen_qualified.insert(qpath.clone()) {
            return;
        }
        self.entries.push(SurfaceEntry {
            qualified_path: qpath,
            kind: source_decl.kind,
            signature: source_decl.signature.clone(),
            source_path: source_file.to_path_buf(),
            source_line: source_decl.start_line,
            source_name: source_decl.name.clone(),
            re_export_chain: chain,
            via_glob,
            docs: source_decl.docs.clone(),
        });
    }

    fn _resolve_use(
        &mut self,
        from_segments: &[String],
        u: &UseItem,
        chain: Vec<ReExportHop>,
        depth: usize,
    ) {
        if depth > self.max_depth {
            return;
        }
        let path_segments: Vec<String> = u.path.split("::").map(|s| s.to_string()).collect();
        let resolved = match _resolve_path(&self.crate_name, from_segments, &path_segments) {
            Some(r) => r,
            None => return, // External crate or unresolvable.
        };

        match u.kind {
            UseSegmentKind::Item => {
                // Last segment is the item name; rest is the target module.
                if resolved.is_empty() {
                    return;
                }
                let item = resolved.last().unwrap().clone();
                let target_module = resolved[..resolved.len() - 1].to_vec();
                let cycle_key = (target_module.join("::"), item.clone());
                if !self.visited.insert(cycle_key) {
                    return;
                }
                let alias = u.alias.clone().unwrap_or_else(|| item.clone());
                self._republish(from_segments, &target_module, &item, &alias, chain, false, depth);
            }
            UseSegmentKind::Glob => {
                // `resolved` IS the target module here (no item segment).
                let target_module = resolved.clone();
                let key = ModuleGraph::key(&target_module);
                let target_data = match self.graph.modules.get(&key) {
                    Some(m) => (m.file.clone(), m.parse.declarations.clone(), m.imports.uses.clone()),
                    None => return,
                };
                let (target_file, target_decls, target_uses) = target_data;

                for d in &target_decls {
                    if !_is_public(d) || matches!(d.kind, DeclarationKind::Namespace) {
                        continue;
                    }
                    let cycle_key = (key.clone(), d.name.clone());
                    if !self.visited.insert(cycle_key) {
                        continue;
                    }
                    self._emit_renamed(
                        from_segments,
                        &d.name,
                        d,
                        &target_file,
                        chain.clone(),
                        true,
                    );
                }
                // Transitive globs: target's own pub uses become reachable too.
                for tu in target_uses {
                    if !_vis_is_public(&tu.visibility) {
                        continue;
                    }
                    let mut next_chain = chain.clone();
                    next_chain.push(ReExportHop {
                        file: target_file.clone(),
                        line: tu.line,
                        module_path: key.clone(),
                        statement: tu.statement.clone(),
                    });
                    self._republish_via_use(from_segments, &target_module, &tu, next_chain, depth + 1);
                }
            }
        }
    }

    /// Republish an item from `target_module::item` under
    /// `from_segments::alias`. If the item is itself a `pub use`, follow it
    /// transitively. If it's a real declaration, emit one entry.
    #[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
    fn _republish(
        &mut self,
        from_segments: &[String],
        target_module: &[String],
        item: &str,
        alias: &str,
        chain: Vec<ReExportHop>,
        via_glob: bool,
        depth: usize,
    ) {
        let key = ModuleGraph::key(target_module);
        let target_data = match self.graph.modules.get(&key) {
            Some(m) => (m.file.clone(), m.parse.declarations.clone(), m.imports.uses.clone()),
            None => return,
        };
        let (target_file, target_decls, target_uses) = target_data;

        // Is `item` an actual declaration in `target_module`?
        for d in &target_decls {
            if d.name == item && _is_public(d) {
                self._emit_renamed(from_segments, alias, d, &target_file, chain, via_glob);
                return;
            }
        }
        // Is `item` re-exported from inside `target_module`?
        for tu in target_uses {
            if !_vis_is_public(&tu.visibility) {
                continue;
            }
            let local_name = tu
                .alias
                .clone()
                .unwrap_or_else(|| _last_segment(&tu.path).to_string());
            let matches_item = match tu.kind {
                UseSegmentKind::Item => local_name == item,
                UseSegmentKind::Glob => true,
            };
            if !matches_item {
                continue;
            }
            let mut next_chain = chain.clone();
            next_chain.push(ReExportHop {
                file: target_file.clone(),
                line: tu.line,
                module_path: key.clone(),
                statement: tu.statement.clone(),
            });
            // For a transitive item match, walk into the cited module.
            let nested_segments: Vec<String> = tu.path.split("::").map(|s| s.to_string()).collect();
            let nested_resolved =
                match _resolve_path(&self.crate_name, target_module, &nested_segments) {
                    Some(r) => r,
                    None => continue,
                };
            match tu.kind {
                UseSegmentKind::Item => {
                    if nested_resolved.is_empty() {
                        continue;
                    }
                    let nested_item = nested_resolved.last().unwrap().clone();
                    let nested_module = nested_resolved[..nested_resolved.len() - 1].to_vec();
                    self._republish(
                        from_segments,
                        &nested_module,
                        &nested_item,
                        alias,
                        next_chain,
                        via_glob,
                        depth + 1,
                    );
                }
                UseSegmentKind::Glob => {
                    self._republish(
                        from_segments,
                        &nested_resolved,
                        item,
                        alias,
                        next_chain,
                        true,
                        depth + 1,
                    );
                }
            }
        }
    }

    fn _republish_via_use(
        &mut self,
        from_segments: &[String],
        upstream_module: &[String],
        u: &UseItem,
        chain: Vec<ReExportHop>,
        depth: usize,
    ) {
        if depth > self.max_depth {
            return;
        }
        let path_segments: Vec<String> = u.path.split("::").map(|s| s.to_string()).collect();
        let resolved =
            match _resolve_path(&self.crate_name, upstream_module, &path_segments) {
                Some(r) => r,
                None => return,
            };
        match u.kind {
            UseSegmentKind::Item => {
                if resolved.is_empty() {
                    return;
                }
                let item = resolved.last().unwrap().clone();
                let target_module = resolved[..resolved.len() - 1].to_vec();
                let alias = u.alias.clone().unwrap_or_else(|| item.clone());
                self._republish(
                    from_segments,
                    &target_module,
                    &item,
                    &alias,
                    chain,
                    true,
                    depth,
                );
            }
            UseSegmentKind::Glob => {
                let target_module = resolved.clone();
                let key = ModuleGraph::key(&target_module);
                let snap = match self.graph.modules.get(&key) {
                    Some(m) => (m.file.clone(), m.parse.declarations.clone()),
                    None => return,
                };
                for d in snap.1 {
                    if _is_public(&d) && !matches!(d.kind, DeclarationKind::Namespace) {
                        let cycle_key = (key.clone(), d.name.clone());
                        if !self.visited.insert(cycle_key) {
                            continue;
                        }
                        self._emit_renamed(from_segments, &d.name, &d, &snap.0, chain.clone(), true);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Path resolution

/// Resolve a `use`-path written in `from_segments` against the module
/// graph. Returns the absolute module path (segments) the user is
/// referring to, or `None` if it leaves the crate.
fn _resolve_path(
    crate_name: &str,
    from: &[String],
    path: &[String],
) -> Option<Vec<String>> {
    if path.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    let mut iter = path.iter().peekable();
    let first = iter.peek().map(|s| s.as_str()).unwrap_or("");

    match first {
        "crate" => {
            iter.next();
            out.push(crate_name.to_string());
        }
        "self" => {
            iter.next();
            out.extend(from.iter().cloned());
        }
        "super" => {
            // Each leading "super" pops a level.
            out.extend(from.iter().cloned());
            while iter.peek().map(|s| s.as_str()) == Some("super") {
                iter.next();
                out.pop()?;
            }
        }
        other => {
            // Bare identifier: could be (a) a child module, (b) a sibling
            // resolved through `from`'s parent, or (c) an external crate.
            // Try child first.
            let mut child = from.to_vec();
            child.push(other.to_string());
            if child[0] == crate_name {
                out.extend(child);
                iter.next();
            } else {
                out.push(crate_name.to_string());
                out.extend(from.iter().skip(1).cloned());
                out.push(other.to_string());
                iter.next();
            }
        }
    }
    for seg in iter {
        out.push(seg.clone());
    }
    Some(out)
}

fn _is_public(d: &Declaration) -> bool {
    _vis_is_public(&d.visibility)
}

/// If `d` is an `impl` block, return the type name it impls. The Rust
/// adapter stores impls as `kind=Class, name="impl_<Type>"`. For trait
/// impls (`impl Trait for Type`) we only surface inherent methods —
/// trait methods aren't published independently of the trait.
fn _impl_target(d: &Declaration) -> Option<&str> {
    if !matches!(d.kind, DeclarationKind::Class) {
        return None;
    }
    let name = d.name.strip_prefix("impl_")?;
    if !d.bases.is_empty() {
        // Trait impl — methods are exposed via the trait, not the type.
        return None;
    }
    Some(name)
}

fn _is_type_with_methods(d: &Declaration) -> bool {
    matches!(
        d.kind,
        DeclarationKind::Struct
            | DeclarationKind::Enum
            | DeclarationKind::Interface
            | DeclarationKind::Class
            | DeclarationKind::Record
    )
}

fn _is_method_like(d: &Declaration) -> bool {
    matches!(
        d.kind,
        DeclarationKind::Method
            | DeclarationKind::Function
            | DeclarationKind::Constructor
    )
}

fn _vis_is_public(v: &str) -> bool {
    let v = v.trim();
    v == "pub"
        || v.starts_with("pub ")
        || v.starts_with("pub(")
}

fn _last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}
