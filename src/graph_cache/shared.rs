//! Process-wide in-memory share of the unified graph.
//!
//! The whole point: in `ast-bro mcp` (and any other long-lived
//! invocation), every tool dispatch should reuse one parsed graph instead
//! of re-loading from disk. We key per repo root and hold the parsed
//! `Arc<UnifiedGraph>` alongside the `FileRecord` fingerprints it was built
//! from. Within a single one-shot CLI run this collapses to "load once,
//! exit"; across repeated MCP `tools/call`s, a second hit is essentially free.
//!
//! Freshness: unlike a load-once cache, `get_or_init` re-validates the cached
//! graph against the working tree on EVERY call — a stat-only `compute_delta`
//! against the in-memory fingerprints in the steady state (no `graph.bin`
//! re-read). When files changed it patches the in-memory graph via the same
//! `apply_delta_*` machinery the cold load uses (no full re-parse of the
//! unchanged corpus) and swaps the `Arc`. This closes the long-lived-session
//! staleness hole where `callers`/`callees`/`deps` would otherwise serve a
//! frozen graph until an explicit `rebuild`. Mirrors the search index's
//! `shared::open_shared` / `Index::is_fresh` design.
//!
//! Promotion (`promote_calls`) swaps the entire `Arc<UnifiedGraph>` for a
//! new one carrying `calls: Some(...)`. Existing `Arc` clones held by
//! readers stay valid with their pre-promotion view; future `get_or_init`
//! callers see the promoted version.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::graph_cache::cache;
use crate::graph_cache::delta::{apply_delta_to_calls, apply_delta_to_deps, refresh_records};
use crate::graph_cache::UnifiedGraph;
use crate::search::cache::{compute_delta, Delta, FileRecord};

/// One registry slot: the parsed graph plus the fingerprints it reflects.
/// Both behind `Arc` so cloning the entry out of the lock (to avoid holding
/// the read guard during the filesystem walk) is just two refcount bumps.
#[derive(Clone)]
struct Entry {
    graph: Arc<UnifiedGraph>,
    records: Arc<Vec<FileRecord>>,
}

/// Per-repo memoised graphs. Keyed by the canonical root path so multiple
/// repos worked on in one MCP session each get their own slot.
type Registry = RwLock<HashMap<PathBuf, Entry>>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

fn store(key: PathBuf, graph: Arc<UnifiedGraph>, records: Arc<Vec<FileRecord>>) {
    registry().write().unwrap().insert(key, Entry { graph, records });
}

/// Get the unified graph for `root`, building (and persisting) it if no
/// fresh cache exists. The deps half is always populated; the call half
/// stays `None` until `promote_calls` is invoked.
///
/// Re-validates a cached entry against the working tree on every call: an
/// unchanged tree returns the memoised `Arc` after a cheap stat-walk; a
/// changed tree is patched in place (no `graph.bin` re-read) and the entry
/// swapped, so a long-lived session never serves a stale graph.
pub fn get_or_init(root: &Path) -> std::io::Result<Arc<UnifiedGraph>> {
    let key = root_for(root);

    // Fast path: cached + still fresh. Bind the clone to a `let` so the read
    // guard is released at the `;` — never held across the `compute_delta`
    // walk, and (critically) never held into a later `store` write lock.
    let cached = registry().read().unwrap().get(&key).cloned();
    if let Some(entry) = cached {
        let delta = compute_delta(root, root, entry.records.as_slice());
        if !delta.requires_rebuild() {
            return Ok(entry.graph);
        }
    }

    // Slow path: serialize the stale-patch / rebuild so two threads that both
    // saw the entry stale don't each load + race on the on-disk write. The MCP
    // server is single-threaded today, but the registry is process-wide and
    // concurrency-designed, so close the TOCTOU properly. Mirrors
    // `search::shared::open_shared`.
    static LOAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOAD_LOCK.lock().unwrap();

    // Re-read + re-check under the lock — another thread may have refreshed it.
    // Bind to a `let` so the read guard drops before `store`'s write lock
    // (holding a read guard into a write() on the same thread self-deadlocks).
    let cached = registry().read().unwrap().get(&key).cloned();
    if let Some(entry) = cached {
        let delta = compute_delta(root, root, entry.records.as_slice());
        if !delta.requires_rebuild() {
            return Ok(entry.graph);
        }
        // Stale: patch the in-memory graph and swap. On patch failure, fall
        // through to a disk reload / full rebuild.
        if let Some((graph, records)) =
            patch_in_memory(&entry.graph, entry.records.as_slice(), root, &delta)
        {
            let arc = Arc::new(graph);
            store(key, arc.clone(), records);
            return Ok(arc);
        }
    }

    let (graph, records) = cache::load_or_build_with_records(&key, false)?;
    let arc = Arc::new(graph);
    store(key, arc.clone(), Arc::new(records));
    Ok(arc)
}

/// Patch a cloned copy of `graph` for `delta` using the same per-file
/// patchers the cold load uses, persist best-effort, and return the new
/// graph + refreshed fingerprints. `None` signals the deps patch failed and
/// the caller should fall back to a full rebuild.
fn patch_in_memory(
    graph: &UnifiedGraph,
    prev_records: &[FileRecord],
    root: &Path,
    delta: &Delta,
) -> Option<(UnifiedGraph, Arc<Vec<FileRecord>>)> {
    let mut g = graph.clone();
    if apply_delta_to_deps(&mut g.deps, root, delta).is_err() {
        return None;
    }
    if g.calls.is_some() {
        let deps_snapshot = g.deps.clone();
        if let Some(calls) = g.calls.as_mut() {
            apply_delta_to_calls(calls, &deps_snapshot, root, delta);
        }
    }
    let new_records = refresh_records(prev_records.to_vec(), root, delta);
    let _ = cache::save(root, &g, &new_records);
    Some((g, Arc::new(new_records)))
}

/// Force a rebuild from disk (drops the in-memory entry, ignores the cache).
pub fn rebuild(root: &Path) -> std::io::Result<Arc<UnifiedGraph>> {
    let key = root_for(root);
    let (graph, records) = cache::load_or_build_with_records(&key, true)?;
    let arc = Arc::new(graph);
    store(key, arc.clone(), Arc::new(records));
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
    // Reuse the fingerprints the just-run `get_or_init` stored for this key so
    // the promoted entry stays re-validatable without re-walking.
    let records = registry()
        .read()
        .unwrap()
        .get(&key)
        .map(|e| e.records.clone())
        .unwrap_or_else(|| Arc::new(cache::collect_file_records(&key).unwrap_or_default()));
    store(key.clone(), promoted.clone(), records.clone());
    // Best-effort persist; failure is non-fatal — the in-memory state is
    // still correct for the rest of the session.
    let _ = cache::save(&key, &promoted, records.as_slice());
    Ok(promoted)
}

/// Drop the cached entry for `root`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// The graph build is pure parsing (no model), so this runs in CI.
    /// An unchanged tree reuses the cached `Arc`; an edit reloads a fresh one
    /// that reflects the change — proving `get_or_init` re-validates per call
    /// instead of freezing after the first query.
    #[test]
    fn get_or_init_revalidates_on_change() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            &root.join("Cargo.toml"),
            "[package]\nname = \"reval\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write(&root.join("src/a.rs"), "pub fn a() {}\n");

        forget(root);
        let first = get_or_init(root).expect("cold build");
        let second = get_or_init(root).expect("warm reuse");
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged tree must reuse the cached Arc"
        );
        let files_before = first.deps.forward.len();

        // Add a second file → the graph must reload into a fresh Arc that
        // knows about the new file.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(&root.join("src/b.rs"), "pub fn b() {}\n");
        let third = get_or_init(root).expect("revalidate after add");
        assert!(
            !Arc::ptr_eq(&first, &third),
            "added file must trigger a reload into a fresh Arc"
        );
        assert!(
            third.deps.forward.len() > files_before,
            "reloaded graph must include the added file"
        );

        forget(root);
    }
}
