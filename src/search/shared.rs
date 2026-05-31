//! Process-wide in-memory share of parsed search indexes.
//!
//! In `ast-bro mcp` (a long-lived process), every `search` / `find_related`
//! tool call would otherwise re-read the whole on-disk index — chunks, BM25,
//! file records, and the multi-megabyte `embeddings.f32` — and re-open the
//! embedding model. This registry keeps one parsed `Arc<Index>` per repo root
//! so repeated calls within a session reuse it.
//!
//! Unlike [`crate::graph_cache::shared`], a cached entry is **not** trusted
//! blindly. An agent edits files mid-session, so [`open_shared`] re-validates
//! the cached index against the working tree on every call — a stat-only
//! `compute_delta` (via [`Index::is_fresh`]) in the common no-change case — and
//! falls back to a full [`Index::open`], which applies the delta and
//! re-persists, whenever anything changed. The cache buys back the disk-read +
//! deserialize + model-open cost on the unchanged path; it never short-circuits
//! correctness.
//!
//! The registry is unbounded, matching `graph_cache::shared`: the number of
//! distinct repo roots touched in one session is small. Each entry does pin an
//! index's embeddings in memory for the session's lifetime, so a per-entry cap
//! is a future option if multi-repo MCP sessions ever grow large.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::project_root::{resolve_home, Marker};
use crate::search::index::Index;

type Registry = RwLock<HashMap<PathBuf, Arc<Index>>>;

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Canonicalise a repo root for use as a registry key, matching the home that
/// `Index::open` resolves to. Falls back to the raw path when canonicalisation
/// fails (e.g. a path that doesn't exist yet).
fn key_for(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Open the index for `path_arg`, reusing a cached in-memory `Index` when the
/// working tree is unchanged since it was loaded. On a cache miss — or when
/// [`Index::is_fresh`] reports the tree changed — falls back to
/// [`Index::open`] (disk load + delta apply + persist) and caches the result.
pub fn open_shared(path_arg: &Path, cwd: &Path) -> std::io::Result<Arc<Index>> {
    let (home, _) = resolve_home(path_arg, cwd, Marker::SearchIndex);
    let key = key_for(&home);

    // Drop the read guard before the (FS-walking) freshness check so a
    // concurrent `store` never blocks behind it.
    let cached = registry().read().unwrap().get(&key).cloned();
    if let Some(cached) = cached {
        if cached.is_fresh() {
            return Ok(cached);
        }
    }

    let fresh = Arc::new(Index::open(path_arg, cwd)?);
    store(fresh.clone());
    Ok(fresh)
}

/// Insert (or replace) the cached entry for an index that was just built or
/// opened, keying off its resolved home. Keeps the registry coherent after an
/// explicit `index` / `--rebuild` so a later `search` doesn't reuse a stale
/// `Arc`.
pub fn store(index: Arc<Index>) {
    let key = key_for(&index.paths.root);
    registry().write().unwrap().insert(key, index);
}

/// Drop the cached entry for `root` (tests only).
#[cfg(test)]
pub fn forget(root: &Path) {
    let key = key_for(root);
    registry().write().unwrap().remove(&key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    /// Reuse the cached `Arc` while the tree is unchanged; reload (new `Arc`)
    /// after an edit. Network-gated — builds a real index (downloads the
    /// model), so it's `#[ignore]`d in CI but documents the contract.
    #[test]
    #[ignore]
    fn open_shared_reuses_until_files_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        let write = |rel: &str, body: &str| {
            let p = root.join(rel);
            let mut f = fs::File::create(&p).unwrap();
            f.write_all(body.as_bytes()).unwrap();
        };
        write("src/a.rs", "pub fn a() {}");

        forget(root);
        let first = open_shared(root, root).expect("build");
        let second = open_shared(root, root).expect("reopen");
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged tree must reuse the cached Arc"
        );

        // Add a file → cache must reload (new Arc) and reflect the change.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write("src/b.rs", "pub fn b() {}");
        let third = open_shared(root, root).expect("reopen after edit");
        assert!(
            !Arc::ptr_eq(&first, &third),
            "edited tree must reload into a fresh Arc"
        );
        assert!(third.chunk_count() >= second.chunk_count());

        forget(root);
    }
}
