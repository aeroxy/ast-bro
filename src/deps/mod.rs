//! `ast-bro deps|reverse-deps|cycles|graph` — file-level
//! dependency-graph analysis.
//!
//! Two-stage pipeline:
//! 1. `extract`: per-language tree-sitter pass produces normalised
//!    `RawImport` records.
//! 2. `resolver`: a single suffix-index resolver maps each `RawImport`
//!    to a concrete file path inside the project.
//!
//! The resulting `DepGraph` powers `deps`, `reverse-deps`, `cycles`,
//! `graph`, and the dep-aware ranking boost in `find-related`.

pub mod cli;
pub mod extract;
pub mod graph;
pub mod manifest;
pub mod options;
pub mod render;
pub mod resolver;
pub mod scc;
pub mod traverse;

pub use graph::{DepEdge, DepGraph};
pub use options::DepError;

use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::deps::extract::extract;
use crate::deps::manifest::detect_aliases;
use crate::deps::resolver::{build_suffix_index, resolve, ResolveCtx};

/// Build a `DepGraph` from scratch by walking `root`, parsing every
/// supported file, and resolving each import.
///
/// The build is parallel across files via rayon. Suffix-index build
/// happens first (single pass), then per-file extraction + resolution
/// runs in parallel.
pub fn build_graph(root: &Path) -> Result<DepGraph, DepError> {
    let start = Instant::now();
    let root_canon = root
        .canonicalize()
        .map_err(|e| DepError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;

    let aliases = detect_aliases(&root_canon);
    let idx = build_suffix_index(&root_canon);

    // Collect a list of files to process from the index (already filtered).
    let files: Vec<_> = idx.by_file.keys().cloned().collect();

    // Per-file extract + resolve in parallel.
    let resolved: Vec<(PathBuf, Vec<DepEdge>, Vec<String>)> = files
        .par_iter()
        .map(|file| {
            let info = match idx.by_file.get(file) {
                Some(i) => i,
                None => return (file.clone(), Vec::new(), Vec::new()),
            };
            let raw_imports = extract(file, info.language);

            let mut edges = Vec::new();
            let mut external = Vec::new();
            let ctx = ResolveCtx {
                from_file: file,
                lang: info.language,
                alias_prefix: aliases.go_module.as_deref(),
                path_aliases: &aliases.ts_path_aliases,
                php_psr4: &aliases.php_psr4,
            };
            for ri in raw_imports {
                match resolve(&ri.spec, &ctx, &idx) {
                    Some(target) => {
                        if target == *file {
                            // self-edge from path collisions — skip.
                            continue;
                        }
                        edges.push(DepEdge {
                            target,
                            kind: ri.kind,
                            line: ri.line,
                            local_name: ri.local_name,
                            raw_path: ri.raw_path,
                        });
                    }
                    None => external.push(ri.raw_path.unwrap_or(ri.spec)),
                }
            }
            (file.clone(), edges, external)
        })
        .collect();

    let mut g = DepGraph::empty(root_canon.clone());
    for (file, edges, external) in resolved {
        if !edges.is_empty() {
            g.forward.insert(file.clone(), edges);
        } else {
            // Insert empty list so the file is known to the graph.
            g.forward.insert(file.clone(), Vec::new());
        }
        if !external.is_empty() {
            g.external.insert(file, external);
        }
    }
    graph::dedup_edges(&mut g);

    let elapsed = start.elapsed();
    let edge_count: usize = g.forward.values().map(|v| v.len()).sum();
    let external_count: usize = g.external.values().map(|v| v.len()).sum();
    g.stats = graph::GraphStats {
        file_count: g.forward.len(),
        edge_count,
        external_count,
        build_ms: elapsed.as_millis() as u64,
    };
    Ok(g)
}

// `load_or_build` and `collect_file_records` moved to `crate::graph_cache::cache`
// when the unified deps+calls cache landed. All consumers now go through
// `graph_cache::shared::get_or_init` so the in-process `Arc<UnifiedGraph>` is
// reused across MCP `tools/call`s instead of re-deserialising per call.
