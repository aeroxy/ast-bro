//! Disk persistence for the unified `UnifiedGraph` at
//! `.ast-bro/deps/graph.bin`. Replaces the previous `src/deps/cache.rs`
//! which serialized only `DepGraph`. Schema bump from `deps-index.v1` to
//! `graph-index.v1` forces a one-time rebuild for upgrading users.
//!
//! Mirrors the search-index pattern in `src/search/cache.rs`: mtime/size/
//! xxhash3 delta detection, advisory `fs2` lock, atomic `.tmp` + rename.

use crate::core::JSON_SCHEMA_GRAPH_INDEX;
use crate::deps::build_graph;
use crate::graph_cache::delta::{apply_delta_to_calls, apply_delta_to_deps, refresh_records};
use crate::graph_cache::UnifiedGraph;
use crate::search::cache::{compute_delta, hash_file, Delta, FileRecord};
use bincode::serde::{decode_from_slice, encode_to_vec};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::path::{Path, PathBuf};

pub const CACHE_SCHEMA: &str = JSON_SCHEMA_GRAPH_INDEX;
/// Legacy schema from pre-rename installs — still readable.
pub const CACHE_SCHEMA_LEGACY: &str = "ast-outline.graph-index.v2";

/// On-disk wrapper combining the unified graph + the file fingerprints used
/// for freshness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheFile {
    pub schema: String,
    pub graph: UnifiedGraph,
    /// Mtime/size/hash records for delta detection. Reuses the search
    /// index's `FileRecord` to avoid a parallel implementation.
    pub files: Vec<FileRecord>,
}

pub fn cache_dir(root: &Path) -> PathBuf {
    let new_root = root.join(".ast-bro");
    migrate_legacy_cache_root(root, &new_root);
    new_root.join("deps")
}

/// Per-process guard so the `.ast-outline` -> `.ast-bro` rename is attempted
/// at most once per repo root, even when called concurrently from parallel
/// walkers or from multiple MCP `tools/call` handlers in the same process.
fn migrate_legacy_cache_root(root: &Path, new_root: &Path) {
    static ATTEMPTED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let set = ATTEMPTED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = set.lock().unwrap();
    if !guard.insert(root.to_path_buf()) {
        return;
    }
    let old_root = root.join(".ast-outline");
    if old_root.exists() && !new_root.exists() {
        if let Err(e) = fs::rename(&old_root, new_root) {
            eprintln!("warning: could not rename .ast-outline -> .ast-bro: {e}");
        } else {
            eprintln!("info: auto-renamed .ast-outline -> .ast-bro");
        }
    }
}

pub fn cache_path(root: &Path) -> PathBuf {
    cache_dir(root).join("graph.bin")
}

pub fn lock_path(root: &Path) -> PathBuf {
    cache_dir(root).join("lock")
}

/// Outcome of trying to load the on-disk cache. The `Stale` variant carries
/// enough state for an incremental update — caller can choose to apply the
/// delta in place rather than rebuilding from scratch.
pub enum LoadOutcome {
    /// Cache exists, schema matches, no files changed.
    Fresh(UnifiedGraph),
    /// Cache exists and schema matches, but files changed on disk. The
    /// `delta` distinguishes added / modified / removed / mtime_only so a
    /// per-file patcher can apply only the diff.
    Stale {
        graph: UnifiedGraph,
        delta: Delta,
        prev_records: Vec<FileRecord>,
    },
    /// No cache, schema mismatch, or unreadable file — caller must rebuild.
    Missing,
}

/// Read the cache and compute its delta against the working tree. Splits
/// the freshness decision (this fn) from the rebuild-vs-patch decision
/// (the caller, in `load_or_build`).
pub fn load_with_delta(root: &Path) -> LoadOutcome {
    let path = cache_path(root);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(_) => return LoadOutcome::Missing,
    };
    let cf: CacheFile = match decode_from_slice(&bytes, bincode::config::standard()) {
        Ok((cf, _)) => cf,
        Err(_) => return LoadOutcome::Missing,
    };
    if cf.schema != CACHE_SCHEMA && cf.schema != CACHE_SCHEMA_LEGACY {
        return LoadOutcome::Missing;
    }
    let delta = compute_delta(root, root, &cf.files);
    if !delta.requires_rebuild() {
        return LoadOutcome::Fresh(cf.graph);
    }
    LoadOutcome::Stale {
        graph: cf.graph,
        delta,
        prev_records: cf.files,
    }
}

/// Build a fresh `UnifiedGraph` (deps half only — call graph is materialised
/// later via `promote_calls`) and persist it.
pub fn build_and_save(root: &Path) -> std::io::Result<UnifiedGraph> {
    let deps = build_graph(root).map_err(std::io::Error::other)?;
    let graph = UnifiedGraph::from_deps(deps);
    let records = collect_file_records(root)?;
    let _ = save(root, &graph, &records);
    Ok(graph)
}

/// Load from cache when possible; on a stale cache try a per-file patch
/// (covering both halves), only falling back to a full rebuild when the
/// patch fails. `force_rebuild` skips the cache entirely.
pub fn load_or_build(root: &Path, force_rebuild: bool) -> std::io::Result<UnifiedGraph> {
    if force_rebuild {
        return build_and_save(root);
    }
    match load_with_delta(root) {
        LoadOutcome::Fresh(g) => Ok(g),
        LoadOutcome::Stale {
            mut graph,
            delta,
            prev_records,
        } => {
            // Deps half: per-file patch. Failure (e.g. an extractor erroring)
            // falls back to a full rebuild so the user's query still succeeds.
            if apply_delta_to_deps(&mut graph.deps, root, &delta).is_err() {
                return build_and_save(root);
            }
            // Calls half: only patch when the on-disk cache had it. A
            // None calls field stays None; the next callers/callees query
            // promotes it via the existing lazy path.
            if graph.calls.is_some() {
                let deps_snapshot = graph.deps.clone();
                if let Some(calls) = graph.calls.as_mut() {
                    apply_delta_to_calls(calls, &deps_snapshot, root, &delta);
                }
            }
            // Persist the patched graph + refreshed fingerprints. Best-effort:
            // a write failure leaves the in-memory state correct for this
            // session and the on-disk cache slightly stale (next launch will
            // re-detect the same delta and re-patch).
            let new_records = refresh_records(prev_records, root, &delta);
            let _ = save(root, &graph, &new_records);
            Ok(graph)
        }
        LoadOutcome::Missing => build_and_save(root),
    }
}

/// Persist a graph + file fingerprints atomically. Writes via `.tmp` +
/// rename, holds an advisory exclusive lock during the write.
pub fn save(
    root: &Path,
    graph: &UnifiedGraph,
    files: &[FileRecord],
) -> std::io::Result<()> {
    let dir = cache_dir(root);
    fs::create_dir_all(&dir)?;
    write_gitignore(&dir)?;

    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path(root))?;
    lock.lock_exclusive()?;

    let cf = CacheFile {
        schema: CACHE_SCHEMA.to_string(),
        graph: graph.clone(),
        files: files.to_vec(),
    };
    let bytes = encode_to_vec(&cf, bincode::config::standard())
        .map_err(std::io::Error::other)?;

    let final_path = cache_path(root);
    let tmp = final_path.with_extension("bin.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &final_path)?;

    fs2::FileExt::unlock(&lock).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_cache::shared;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// Cold build → on-disk cache has `calls: None`. After `promote_calls`,
    /// re-reading and decoding the file directly (NOT via the in-memory
    /// registry) must show `calls: Some(...)`. Then `forget` + `get_or_init`
    /// must return an Arc whose calls half is still present, proving the
    /// load path round-trips the option correctly.
    #[test]
    fn promote_calls_persists_calls_half_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        write(
            &root.join("Cargo.toml"),
            "[package]\nname = \"persist_smoke\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write(
            &root.join("src/lib.rs"),
            "pub fn callee() {}\npub fn caller() { callee(); }\n",
        );

        // 1. Cold build via the production path. Ensures the in-memory
        //    registry slot exists for the later promote_calls call.
        shared::forget(root);
        let cold = shared::get_or_init(root).expect("cold get_or_init");
        assert!(cold.calls.is_none(), "fresh build should not promote");

        let bytes = fs::read(cache_path(root)).expect("read cold cache");
        let (cf, _): (CacheFile, _) =
            decode_from_slice(&bytes, bincode::config::standard()).expect("decode cold");
        assert!(cf.graph.calls.is_none(), "on-disk cold cache should be calls: None");

        // 2. Promote and re-read the bytes from disk.
        shared::promote_calls(root, |g| {
            crate::calls::build::build_call_graph(root, &g.deps)
        })
        .expect("promote_calls");

        let bytes = fs::read(cache_path(root)).expect("read promoted cache");
        let (cf, _): (CacheFile, _) =
            decode_from_slice(&bytes, bincode::config::standard()).expect("decode promoted");
        let calls = cf
            .graph
            .calls
            .expect("promoted cache must persist calls: Some");
        assert!(
            calls.forward.values().any(|edges| !edges.is_empty()),
            "persisted call graph should carry edges"
        );

        // 3. Drop the in-memory entry and reload — the production load
        //    path must surface calls: Some after rehydration.
        shared::forget(root);
        let reloaded = shared::get_or_init(root).expect("reload after forget");
        assert!(
            reloaded.calls.is_some(),
            "reload from disk must see promoted calls"
        );
    }
}

fn write_gitignore(dir: &Path) -> std::io::Result<()> {
    let p = dir.parent().map(|d| d.join(".gitignore"));
    if let Some(p) = p {
        if !p.exists() {
            fs::write(&p, "*\n")?;
        }
    }
    Ok(())
}

/// Build a `Vec<FileRecord>` describing the current state of every
/// indexable file in `root`. Reuses search's `compute_delta` against an
/// empty cache to populate hashes.
pub fn collect_file_records(root: &Path) -> std::io::Result<Vec<FileRecord>> {
    let delta = compute_delta(root, root, &[]);
    let mut out = Vec::with_capacity(delta.added.len());
    for path in delta.added {
        let meta = std::fs::metadata(&path)?;
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let mtime_ns = match mtime.duration_since(std::time::SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        let rel = path
            .strip_prefix(root)
            .map(|r| {
                r.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_else(|_| path.display().to_string());
        let hash = hash_file(&path).unwrap_or(0);
        out.push(FileRecord {
            path: rel,
            mtime_ns,
            size: meta.len(),
            content_hash: hash,
            chunk_start: 0,
            chunk_end: 0,
        });
    }
    Ok(out)
}
