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
use crate::deps::manifest::{detect_aliases, ProjectAliases};
use crate::deps::resolver::{build_suffix_index, resolve, ResolveCtx, SuffixIndex};

/// Build a `DepGraph` from scratch by walking `root`, parsing every
/// supported file, and resolving each import.
///
/// The build is parallel across files via rayon. Suffix-index build
/// happens first (single pass), then per-file extraction + resolution
/// runs in parallel.
pub fn build_graph(root: &Path) -> Result<DepGraph, DepError> {
  let start = Instant::now();
  let root_canon = root.canonicalize().map_err(|e| DepError::Io {
    path: root.to_path_buf(),
    source: e,
  })?;

  let aliases = detect_aliases(&root_canon);
  let idx = build_suffix_index(&root_canon);

  // Per-file extract in parallel; the index has already filtered the walk.
  let mut g = DepGraph::empty(root_canon.clone());
  g.imports = idx
    .by_file
    .par_iter()
    .map(|(file, info)| (file.clone(), extract(file, info.language)))
    .collect();
  resolve_all(&mut g, &idx, &aliases);
  g.stats.build_ms = start.elapsed().as_millis() as u64;
  Ok(g)
}

/// Derive `forward` and `external` from `graph.imports` against `idx`, and
/// recount the stats. The cold build and the incremental update both end
/// here, so an unchanged file resolves the same way in both (#93). Leaves
/// `stats.build_ms` to the caller.
pub(crate) fn resolve_all(graph: &mut DepGraph, idx: &SuffixIndex, aliases: &ProjectAliases) {
  let resolved: Vec<(PathBuf, Vec<DepEdge>, Vec<String>)> = graph
    .imports
    .par_iter()
    .map(|(file, imports)| {
      let Some(info) = idx.by_file.get(file) else {
        return (file.clone(), Vec::new(), Vec::new());
      };
      let ctx = ResolveCtx {
        from_file: file,
        lang: info.language,
        path_aliases: &aliases.ts_path_aliases,
        base_url: aliases.ts_base_url.as_deref(),
        php_psr4: &aliases.php_psr4,
      };
      let mut edges = Vec::new();
      let mut external = Vec::new();
      for ri in imports {
        match resolve(&ri.spec, &ctx, idx) {
          // A self-edge from a path collision — skip.
          Some(target) if target == *file => {}
          Some(target) => edges.push(DepEdge {
            target,
            kind: ri.kind,
            line: ri.line,
            local_name: ri.local_name.clone(),
            raw_path: ri.raw_path.clone(),
          }),
          None => external.push(ri.raw_path.clone().unwrap_or_else(|| ri.spec.clone())),
        }
      }
      (file.clone(), edges, external)
    })
    .collect();

  graph.forward.clear();
  graph.external.clear();
  for (file, edges, external) in resolved {
    // Every file goes into `forward`, even with no edges, so the graph
    // knows it.
    if !external.is_empty() {
      graph.external.insert(file.clone(), external);
    }
    graph.forward.insert(file, edges);
  }
  graph::dedup_edges(graph);

  graph.stats.file_count = graph.forward.len();
  graph.stats.edge_count = graph.forward.values().map(|v| v.len()).sum();
  graph.stats.external_count = graph.external.values().map(|v| v.len()).sum();
}

// `load_or_build` and `collect_file_records` moved to `crate::graph_cache::cache`
// when the unified deps+calls cache landed. All consumers now go through
// `graph_cache::shared::get_or_init` so the in-process `Arc<UnifiedGraph>` is
// reused across MCP `tools/call`s instead of re-deserialising per call.
