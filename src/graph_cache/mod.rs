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
