//! Process-wide in-memory share of the unified graph.
//!
//! The whole point: in `ast-bro mcp` (and any other long-lived
//! invocation), every tool dispatch should reuse one parsed graph instead
//! of re-loading from disk. `OnceLock<RwLock<HashMap<root, Arc<UnifiedGraph>>>>`
//! gives us per-repo memoization without Tokio or extra deps. Within a
//! single one-shot CLI run, this collapses to "load once, exit"; across
//! repeated MCP `tools/call`s, the second hit is essentially free.
//!
//! Promotion (`promote_calls`) swaps the entire `Arc<UnifiedGraph>` for a
//! new one carrying `calls: Some(...)`. Existing `Arc` clones held by
//! readers stay valid with their pre-promotion view; future `get_or_init`
//! callers see the promoted version.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::graph_cache::cache;
use crate::graph_cache::UnifiedGraph;

/// Per-repo memoised graphs. Keyed by the canonical root path so multiple
/// repos worked on in one MCP session each get their own slot.
type Registry = RwLock<HashMap<PathBuf, Arc<UnifiedGraph>>>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Get the unified graph for `root`, building (and persisting) it if no
/// fresh cache exists. The deps half is always populated; the call half
/// stays `None` until `promote_calls` is invoked.
pub fn get_or_init(root: &Path) -> std::io::Result<Arc<UnifiedGraph>> {
    let key = root_for(root);
    {
        let r = registry().read().unwrap();
        if let Some(g) = r.get(&key) {
            return Ok(g.clone());
        }
    }
    let graph = cache::load_or_build(&key, false)?;
    let arc = Arc::new(graph);
    registry().write().unwrap().insert(key, arc.clone());
    Ok(arc)
}

/// Force a rebuild from disk (drops the in-memory entry, ignores the cache).
pub fn rebuild(root: &Path) -> std::io::Result<Arc<UnifiedGraph>> {
    let key = root_for(root);
    let graph = cache::load_or_build(&key, true)?;
    let arc = Arc::new(graph);
    registry().write().unwrap().insert(key, arc.clone());
    Ok(arc)
}

/// Ensure the call graph half is materialised. If `current.calls` is
/// already `Some`, returns the same `Arc` unchanged. Otherwise builds the
/// call graph (using the current deps half), persists, and swaps the
/// per-root entry to the promoted version. Returns the promoted `Arc`.
pub fn promote_calls<F>(root: &Path, build: F) -> std::io::Result<Arc<UnifiedGraph>>
where
    F: FnOnce(&UnifiedGraph) -> crate::calls::graph::CallGraph,
{
    let current = get_or_init(root)?;
    if current.calls.is_some() {
        return Ok(current);
    }
    let calls = build(&current);
    let promoted = Arc::new(UnifiedGraph {
        deps: current.deps.clone(),
        calls: Some(calls),
    });
    let key = root_for(root);
    registry().write().unwrap().insert(key.clone(), promoted.clone());
    // Best-effort persist; failure is non-fatal — the in-memory state is
    // still correct for the rest of the session.
    if let Ok(records) = cache::collect_file_records(&key) {
        let _ = cache::save(&key, &promoted, &records);
    }
    Ok(promoted)
}

/// Drop the cached `Arc` for `root` (used by tests; not wired to user-facing
/// commands today).
#[allow(dead_code)]
pub fn forget(root: &Path) {
    let key = root_for(root);
    registry().write().unwrap().remove(&key);
}

/// Canonicalise the repo root for use as a registry key. Falls back to the
/// raw path when canonicalisation fails (e.g. test fixtures).
pub fn root_for(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Convenience: run a closure with the unified graph for `root`. Mainly
/// useful in tests; production code holds the `Arc` for as long as needed.
#[allow(dead_code)]
pub fn with_root<R, F: FnOnce(&UnifiedGraph) -> R>(root: &Path, f: F) -> std::io::Result<R> {
    let g = get_or_init(root)?;
    Ok(f(&g))
}
