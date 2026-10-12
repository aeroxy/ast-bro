//! `DepGraph` and edge types — the canonical in-memory representation
//! of the file-level import graph for a project.
//!
//! The graph stores only forward edges; the reverse map is computed
//! on-demand by `reverse_adjacency()` — avoids the invariant-maintenance
//! burden and is cheap enough at our scale (single-pass invert, ~5ms
//! even on huge repos).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::core::JSON_SCHEMA_DEPS_INDEX;

/// What kind of import statement produced an edge. Used for filtering and
/// human-readable output. Keep this enum stable across schema versions —
/// new variants append at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    /// Rust `use foo::bar::Baz;`
    Use,
    /// Rust `mod foo;` referencing an external file.
    Mod,
    /// Python `from a.b import c` (or `import a.b`).
    From,
    /// TS/JS `import x from 'y'`, Java `import com.foo.Bar`, Kotlin `import com.foo.Bar`,
    /// Scala `import a.b.c`. The "default" import kind for most languages.
    Bare,
    /// `import { Foo, Bar } from './x'` — at least one named binding.
    NamedFrom,
    /// `export * from './x'` / Python `from x import *` / Kotlin `import com.foo.*`.
    StarFrom,
    /// `using static X.Y.z;` (C#) / `import static X.Y.z;` (Java).
    Static,
    /// `using A = X.Y;` (C#), `import com.foo.Bar as Quux` (Kotlin), `as` rename in TS.
    Alias,
    /// Globbed wildcard import where we don't expand members.
    Glob,
    /// Elixir `require Foo` — the module must be compiled first so its
    /// macros can be used.
    Require,
    /// Elixir module used without a directive: `Foo.Bar.baz(…)`, `%Foo{}`.
    Reference,
}

impl ImportKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Use => "use",
            Self::Mod => "mod",
            Self::From => "from",
            Self::Bare => "import",
            Self::NamedFrom => "named",
            Self::StarFrom => "star",
            Self::Static => "static",
            Self::Alias => "alias",
            Self::Glob => "glob",
            Self::Require => "require",
            Self::Reference => "ref",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepEdge {
    pub target: PathBuf,
    pub kind: ImportKind,
    pub line: u32,
    /// Local binding the importer sees (`as Quux`, `using A = X.Y`).
    /// `None` when the import preserves the original name.
    ///
    /// No `skip_serializing_if` here on purpose: `DepEdge` is serialized
    /// into the bincode cache, which uses positional encoding. A skipped
    /// field breaks decoding — the next byte is interpreted as this
    /// field's discriminant, throwing the entire downstream read off by
    /// one. (We learned this the hard way: the cache silently failed to
    /// round-trip and `load_if_fresh` quietly fell through to "rebuild
    /// from scratch every time", which is exactly the per-file
    /// invalidation regression we shipped this change to fix.)
    #[serde(default)]
    pub local_name: Option<String>,
    /// Dotted/source path before resolution. Useful for inner-class
    /// display ("com.foo.Bar.Inner") and to keep raw context for debugging.
    /// See `local_name` above for why no `skip_serializing_if`.
    #[serde(default)]
    pub raw_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStats {
    pub file_count: usize,
    pub edge_count: usize,
    pub external_count: usize,
    /// Build duration in milliseconds — populated by `build`.
    pub build_ms: u64,
}

/// On-disk + in-memory representation of the project's file-level
/// import graph. `forward` is the source of truth; everything else
/// is derivable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepGraph {
    /// Schema version of the *cache file* on disk. Bumped when the
    /// shape of `DepGraph`/`DepEdge` changes in a non-backwards-
    /// compatible way.
    pub schema: String,
    /// All known files in the project. Always non-empty after a
    /// successful build (a project with zero source files is rejected
    /// upstream).
    pub forward: HashMap<PathBuf, Vec<DepEdge>>,
    /// Imports that couldn't be resolved to a file in the project.
    /// Useful for `--external` reporting and debugging.
    pub external: HashMap<PathBuf, Vec<String>>,
    pub root: PathBuf,
    pub built_at: SystemTime,
    pub stats: GraphStats,
}

impl DepGraph {
    pub fn empty(root: PathBuf) -> Self {
        Self {
            schema: JSON_SCHEMA_DEPS_INDEX.to_string(),
            forward: HashMap::new(),
            external: HashMap::new(),
            root,
            built_at: SystemTime::now(),
            stats: GraphStats::default(),
        }
    }

    /// Single-pass invert of `forward` into a reverse adjacency list.
    /// Cheap (~5ms even for large repos) — built fresh per call.
    pub fn reverse_adjacency(&self) -> HashMap<PathBuf, Vec<PathBuf>> {
        let mut rev: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for (src, edges) in &self.forward {
            for edge in edges {
                rev.entry(edge.target.clone())
                    .or_default()
                    .push(src.clone());
            }
        }
        // Deterministic order so snapshot tests are stable.
        for v in rev.values_mut() {
            v.sort();
            v.dedup();
        }
        rev
    }

    /// Edges sorted by (source, target) for stable diffs.
    pub fn sorted_edges(&self) -> Vec<(PathBuf, PathBuf, ImportKind)> {
        let mut all = Vec::new();
        for (src, edges) in &self.forward {
            for e in edges {
                all.push((src.clone(), e.target.clone(), e.kind));
            }
        }
        all.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        all
    }

    /// Same as `sorted_edges` but bucketed by source file. Mainly used
    /// by the tree renderer.
    #[allow(dead_code)]
    pub fn grouped(&self) -> BTreeMap<PathBuf, Vec<DepEdge>> {
        let mut out = BTreeMap::new();
        for (k, v) in &self.forward {
            let mut sorted = v.clone();
            sorted.sort_by(|a, b| a.target.cmp(&b.target));
            out.insert(k.clone(), sorted);
        }
        out
    }

    /// Repo-relative path display helper (POSIX separators), falling back
    /// to the absolute path when stripping fails.
    pub fn rel(&self, p: &Path) -> String {
        match p.strip_prefix(&self.root) {
            Ok(r) => r
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
            Err(_) => p.display().to_string(),
        }
    }

    /// Whether `p` is under the POSIX `scope` (relative to `self.root`).
    /// Empty / matching scope passes everything.
    pub fn in_scope(&self, p: &Path, scope: &str) -> bool {
        if scope.is_empty() {
            return true;
        }
        let rel = self.rel(p);
        if rel == scope {
            return true;
        }
        if rel.len() <= scope.len() {
            return false;
        }
        rel.starts_with(scope) && rel.as_bytes()[scope.len()] == b'/'
    }

    /// Return a clone restricted to nodes (and edges between nodes) under
    /// `scope`. Empty scope returns a clone of the original.
    pub fn subgraph_for_scope(&self, scope: &str) -> DepGraph {
        if scope.is_empty() {
            return self.clone();
        }
        let mut forward: HashMap<PathBuf, Vec<DepEdge>> = HashMap::new();
        for (src, edges) in &self.forward {
            if !self.in_scope(src, scope) {
                continue;
            }
            let kept: Vec<DepEdge> = edges
                .iter()
                .filter(|e| self.in_scope(&e.target, scope))
                .cloned()
                .collect();
            forward.insert(src.clone(), kept);
        }
        let external: HashMap<PathBuf, Vec<String>> = self
            .external
            .iter()
            .filter(|(p, _)| self.in_scope(p, scope))
            .map(|(p, v)| (p.clone(), v.clone()))
            .collect();
        let edge_count: usize = forward.values().map(|v| v.len()).sum();
        let external_count: usize = external.values().map(|v| v.len()).sum();
        DepGraph {
            schema: self.schema.clone(),
            forward,
            external,
            root: self.root.clone(),
            built_at: self.built_at,
            stats: GraphStats {
                file_count: self.stats.file_count,
                edge_count,
                external_count,
                build_ms: self.stats.build_ms,
            },
        }
    }
}

/// Drop duplicate `(source, target)` edges, keeping the first occurrence.
/// Run once at the end of `build_graph` to collapse repeated imports
/// (e.g. one file importing several names from the same module).
pub fn dedup_edges(graph: &mut DepGraph) {
    for edges in graph.forward.values_mut() {
        let mut seen = std::collections::HashSet::new();
        edges.retain(|e| seen.insert(e.target.clone()));
    }
}
