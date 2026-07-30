//! Unified on-disk + in-memory graph cache shared by `src/deps/` (file-level
//! import graph) and `src/calls/` (symbol-level call graph).
//!
//! One cache file at `.ast-bro/deps/graph.bin`, one fingerprint table,
//! one advisory lock. The deps half is built eagerly on first access; the
//! calls half lives behind `Option<CallGraph>` and only materialises when a
//! `callers`/`callees` query asks for it. Within a process — most importantly
//! `ast-bro mcp` — every consumer reads through `shared::get_or_init`,
//! which holds an `Arc<RwLock<Arc<UnifiedGraph>>>` so multiple tool calls
//! share one in-memory graph and pay the parse cost exactly once per session.
//!
//! Why keep the `deps/` directory name even though it now holds the call
//! graph too: renaming would force every existing user to rebuild from a new
//! path. The schema bump (`deps-index.v1` → `graph-index.v1`) already forces
//! a rebuild; further moving the file is unnecessary churn.

pub mod cache;
pub mod delta;
pub mod shared;

pub use shared::{get_or_init, promote_calls};

use crate::calls::graph::CallGraph;
use crate::deps::graph::DepGraph;
use serde::{Deserialize, Serialize};

/// In-memory state held inside the shared `Arc`. Cheap to clone (the `Arc`
/// indirection means consumers share storage); a fresh `Arc` is swapped in
/// when the call graph is promoted from `None` to `Some`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedGraph {
    /// File-level dep graph. Always populated.
    pub deps: DepGraph,
    /// Symbol-level call graph. `None` until a `callers`/`callees` query
    /// triggers `promote_calls`. The on-disk cache may persist this when
    /// present so subsequent sessions don't pay the build cost again.
    #[serde(default)]
    pub calls: Option<CallGraph>,
}

impl UnifiedGraph {
    pub fn from_deps(deps: DepGraph) -> Self {
        Self { deps, calls: None }
    }
}

/// Fetch the unified graph with its call-graph half guaranteed present,
/// promoting (building) the call graph on first access if it hasn't
/// materialised yet. Shared by `impact` and `context`, which both need the
/// call graph and would otherwise each carry an identical copy of this logic.
pub fn ensure_with_calls(
    root: &std::path::Path,
    force_rebuild: bool,
) -> std::io::Result<std::sync::Arc<UnifiedGraph>> {
    let unified = if force_rebuild {
        shared::rebuild(root)?
    } else {
        get_or_init(root)?
    };
    if unified.calls.is_some() {
        return Ok(unified);
    }
    promote_calls(root, |g| crate::calls::build::build_call_graph(root, &g.deps))
}

/// A unified graph whose calls half is known to be present. Only
/// `load_for_symbol_query` builds one, and it rejects a missing call graph
/// before returning, so the "calls are present" contract lives here next to
/// the check that establishes it instead of as an `expect` repeated at every
/// symbol-query call site.
pub struct SymbolGraph(std::sync::Arc<UnifiedGraph>);

impl SymbolGraph {
    /// The call graph. Infallible by construction — see the type's docs.
    pub fn calls(&self) -> &CallGraph {
        match &self.0.calls {
            Some(c) => c,
            None => unreachable!("SymbolGraph constructed without its calls half"),
        }
    }

    /// The file-level dep graph, always populated.
    pub fn deps(&self) -> &DepGraph {
        &self.0.deps
    }
}

/// Shared prologue for the symbol-query commands (`callers`, `callees`,
/// `trace`, `impact`, `context`): find the project root and load the
/// unified graph with the calls half present. Failures follow the error
/// contract (#36) — unresolvable root → exit 2, cache/build failure →
/// exit 1, both on stderr with a JSON envelope under `--json`. Returns
/// `Err(exit_code)` for the caller to propagate.
pub fn load_for_symbol_query(
    command: &str,
    path: &std::path::Path,
    rebuild: bool,
    json: bool,
) -> Result<(std::path::PathBuf, SymbolGraph), i32> {
    use crate::cli_error::{CliError, ErrorKind};
    let root = match crate::project_root::find_root_for(path) {
        Ok(r) => r,
        Err(e) => return Err(CliError::new(command, ErrorKind::PathNotFound, e).emit(json)),
    };
    let graph = match ensure_with_calls(&root, rebuild) {
        Ok(g) => g,
        Err(e) => {
            return Err(
                CliError::new(command, ErrorKind::IndexError, e.to_string()).emit(json),
            )
        }
    };
    if graph.calls.is_none() {
        return Err(
            CliError::new(command, ErrorKind::IndexError, "call graph is empty").emit(json),
        );
    }
    Ok((root, SymbolGraph(graph)))
}
