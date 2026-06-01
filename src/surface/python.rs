//! Python public-surface resolver.
//!
//! Walk packages from `__init__.py`. Honour `__all__` when present;
//! otherwise filter by leading-underscore convention.
//!
//! Re-export resolution: `from .submod import Name [as Alias]` lines in
//! the package init are followed into `submod`. `from .sub import *`
//! recurses into the sub-package's own surface.

use crate::core::Declaration;
use crate::parse_file;
use crate::surface::entry::{ReExportHop, SurfaceEntry};
use crate::surface::entry_point::EntryPoint;
use crate::surface::imports::{self, FromImport};
use crate::surface::options::{SurfaceError, SurfaceOptions};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn resolve(
    entry: &EntryPoint,
    opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    let (init, pkg_name) = match entry {
        EntryPoint::PythonPackage { init, pkg_name } => (init.clone(), pkg_name.clone()),
        _ => {
            return Err(SurfaceError::NoEntryPoint {
                path: PathBuf::from("."),
                hint: "python::resolve called with non-Python entry point".into(),
            });
        }
    };

    let mut walker = Walker {
        max_depth: opts.max_depth,
        visited_packages: HashSet::new(),
        seen_qualified: HashSet::new(),
        entries: Vec::new(),
    };
    walker.walk_package(&init, &[pkg_name], 0);
    Ok(walker.entries)
}

struct Walker {
    max_depth: usize,
    visited_packages: HashSet<PathBuf>,
    seen_qualified: HashSet<String>,
    entries: Vec<SurfaceEntry>,
}

impl Walker {
    fn walk_package(&mut self, init_file: &Path, segments: &[String], depth: usize) {
        if depth > self.max_depth {
            return;
        }
        if !self.visited_packages.insert(init_file.to_path_buf()) {
            return;
        }
        let parse = match parse_file(init_file) {
            Some(p) => p,
            None => return,
        };
        let src = std::str::from_utf8(&parse.source).unwrap_or("").to_string();
        let imports = imports::extract_python_imports(&src);

        let pkg_dir = init_file
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        // Determine the public set.
        let public: PublicSet = match &imports.dunder_all {
            Some(names) => PublicSet::Explicit(names.iter().cloned().collect()),
            None => PublicSet::Implicit,
        };

        // 1. Definitions in __init__.py itself.
        for d in &parse.declarations {
            if !public.includes(&d.name) {
                continue;
            }
            self._emit(segments, &d.name, d, init_file, vec![]);
        }

        // 2. Names re-exported via `from .x import Y`.
        for fi in &imports.from_imports {
            if fi.relative_dots == 0 {
                continue;
            }
            let target_dir = _ascend(&pkg_dir, fi.relative_dots.saturating_sub(1));
            let target_dir = match target_dir {
                Some(d) => d,
                None => continue,
            };
            let target_dir = if fi.module.is_empty() {
                target_dir
            } else {
                target_dir.join(fi.module.replace('.', "/"))
            };

            let hop = ReExportHop {
                file: init_file.to_path_buf(),
                line: fi.line,
                module_path: segments.join("."),
                statement: fi.statement.clone(),
            };

            if fi.is_glob {
                // `from .sub import *` — recurse and import everything that
                // sub-package considers public.
                let sub_init = target_dir.join("__init__.py");
                if sub_init.is_file() {
                    let mut sub_segments = segments.to_vec();
                    if !fi.module.is_empty() {
                        sub_segments.push(fi.module.clone());
                    }
                    let mut sub_walker = Walker {
                        max_depth: self.max_depth.saturating_sub(1),
                        visited_packages: self.visited_packages.clone(),
                        seen_qualified: HashSet::new(),
                        entries: Vec::new(),
                    };
                    sub_walker.walk_package(&sub_init, &sub_segments, depth + 1);
                    // Re-publish into our segments with a glob hop.
                    for e in sub_walker.entries {
                        let last = e.qualified_path.rsplit('.').next().unwrap_or("").to_string();
                        let mut chain = e.re_export_chain.clone();
                        chain.insert(0, hop.clone());
                        self._publish_external(segments, &last, &e, chain);
                    }
                }
                continue;
            }

            for name in &fi.names {
                let exposed = name.alias.clone().unwrap_or_else(|| name.name.clone());
                if !public.includes(&exposed) {
                    continue;
                }
                self._follow_from_import(
                    segments,
                    &exposed,
                    &name.name,
                    &target_dir,
                    fi,
                    hop.clone(),
                    depth + 1,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn _follow_from_import(
        &mut self,
        from_segments: &[String],
        exposed_name: &str,
        source_name: &str,
        target_dir: &Path,
        fi: &FromImport,
        hop: ReExportHop,
        depth: usize,
    ) {
        // Two cases:
        //   (a) `from .submod import Name` → look in <target_dir>/submod.py
        //       (or .pyi) for a top-level `Name`, OR in
        //       <target_dir>/submod/__init__.py.
        //   (b) `from . import submod` → expose the sub-package itself.
        let candidates = [
            target_dir.with_extension("py"),
            target_dir.join(format!("{}.py", source_name)),
            target_dir.join(format!("{}.pyi", source_name)),
            target_dir.join(source_name).join("__init__.py"),
            target_dir.join("__init__.py"),
        ];
        for cand in candidates {
            if !cand.is_file() {
                continue;
            }
            let parse = match parse_file(&cand) {
                Some(p) => p,
                None => continue,
            };
            // If the import statement was `from . import submod`, we don't
            // search by name — the imported name IS a sub-module reference.
            let want_module_itself =
                fi.module.is_empty() && cand.file_name().and_then(|s| s.to_str()) == Some("__init__.py");
            if want_module_itself {
                // Re-export every public symbol from that module.
                let src = std::str::from_utf8(&parse.source).unwrap_or("").to_string();
                let sub_imports = imports::extract_python_imports(&src);
                let public = match &sub_imports.dunder_all {
                    Some(names) => PublicSet::Explicit(names.iter().cloned().collect()),
                    None => PublicSet::Implicit,
                };
                for d in &parse.declarations {
                    if public.includes(&d.name) {
                        let mut nested = from_segments.to_vec();
                        nested.push(source_name.to_string());
                        self._emit(&nested, &d.name, d, &cand, vec![hop.clone()]);
                    }
                }
                let _ = depth;
                return;
            }

            // Look for the target name in this file.
            for d in &parse.declarations {
                if d.name == source_name {
                    self._emit(from_segments, exposed_name, d, &cand, vec![hop]);
                    return;
                }
            }
            // Not directly defined — maybe further re-exported. Search the
            // file's own `from .x import Y` lines.
            let src = std::str::from_utf8(&parse.source).unwrap_or("").to_string();
            let sub_imports = imports::extract_python_imports(&src);
            for sub_fi in &sub_imports.from_imports {
                if sub_fi.relative_dots == 0 {
                    continue;
                }
                for sub_name in &sub_fi.names {
                    let local = sub_name.alias.clone().unwrap_or_else(|| sub_name.name.clone());
                    if local != source_name {
                        continue;
                    }
                    let sub_dir = _ascend(
                        cand.parent().unwrap_or(Path::new(".")),
                        sub_fi.relative_dots.saturating_sub(1),
                    );
                    let sub_dir = match sub_dir {
                        Some(d) => d,
                        None => continue,
                    };
                    let sub_dir = if sub_fi.module.is_empty() {
                        sub_dir
                    } else {
                        sub_dir.join(sub_fi.module.replace('.', "/"))
                    };
                    let mut chain = vec![hop.clone()];
                    chain.push(ReExportHop {
                        file: cand.clone(),
                        line: sub_fi.line,
                        module_path: from_segments.join("."),
                        statement: sub_fi.statement.clone(),
                    });
                    self._follow_from_import(
                        from_segments,
                        exposed_name,
                        &sub_name.name,
                        &sub_dir,
                        sub_fi,
                        chain[0].clone(),
                        depth + 1,
                    );
                }
            }
            return;
        }
    }

    fn _emit(
        &mut self,
        segments: &[String],
        exposed_name: &str,
        decl: &Declaration,
        source: &Path,
        chain: Vec<ReExportHop>,
    ) {
        let qpath = format!("{}.{}", segments.join("."), exposed_name);
        if !self.seen_qualified.insert(qpath.clone()) {
            return;
        }
        self.entries.push(SurfaceEntry {
            qualified_path: qpath,
            kind: decl.kind,
            signature: decl.signature.clone(),
            source_path: source.to_path_buf(),
            source_line: decl.start_line,
            source_name: decl.name.clone(),
            re_export_chain: chain,
            via_glob: false,
            docs: decl.docs.clone(),
        });
    }

    fn _publish_external(
        &mut self,
        segments: &[String],
        exposed_name: &str,
        existing: &SurfaceEntry,
        chain: Vec<ReExportHop>,
    ) {
        let qpath = format!("{}.{}", segments.join("."), exposed_name);
        if !self.seen_qualified.insert(qpath.clone()) {
            return;
        }
        self.entries.push(SurfaceEntry {
            qualified_path: qpath,
            kind: existing.kind,
            signature: existing.signature.clone(),
            source_path: existing.source_path.clone(),
            source_line: existing.source_line,
            source_name: existing.source_name.clone(),
            re_export_chain: chain,
            via_glob: true,
            docs: existing.docs.clone(),
        });
    }
}

enum PublicSet {
    Explicit(HashSet<String>),
    Implicit,
}

impl PublicSet {
    fn includes(&self, name: &str) -> bool {
        match self {
            Self::Explicit(set) => set.contains(name),
            Self::Implicit => !name.starts_with('_'),
        }
    }
}

fn _ascend(start: &Path, levels: usize) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    for _ in 0..levels {
        cur = cur.parent()?.to_path_buf();
    }
    Some(cur)
}

