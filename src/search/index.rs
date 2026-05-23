//! Per-repo persistent search index.
//!
//! `Index::open(path_arg, cwd)` either loads the cached index from
//! `.ast-bro/index/` (and refreshes it if files have changed) or builds
//! one from scratch on first use. The home directory is resolved by walking
//! up from `path_arg` looking for an existing `.ast-bro/index/`, capped
//! at `cwd` so we never escape the project the user is working in. If no
//! existing index is found, the index is built at `cwd` (when `path_arg`
//! is under `cwd`) or at `path_arg` itself otherwise.
//!
//! ```text
//! search:        tokenize → BM25 + dense top-k → RRF → ranking → top-k
//!                (post-filtered by query_scope when set)
//! find-related:  resolve chunk → semantic top-k (lang-filtered) → exclude self → top-k
//! ```
//!
//! Schema v2 adds `indexed_corpus` to `meta.json` so search/find_related can
//! filter results by query scope without conflating it with index location.
//! v1 metas are read transparently as having `indexed_corpus = ""` (whole
//! home).
//!
//! Phase-7 simplification: any non-empty delta (added / modified / removed)
//! triggers a full rebuild. The on-disk format reserves the fields needed
//! for a v2 partial-rebuild path (per-file `chunk_range` + a tombstones
//! vector in `meta.json`) so swapping in incremental updates later doesn't
//! invalidate caches.

use crate::file_filter::{add_filters, should_skip_path};
use crate::project_root::{relative_posix, resolve_home, Marker};
use crate::search::bm25::Bm25Index;
use crate::search::cache::{compute_delta, hash_file, FileRecord};
use crate::search::chunker::{chunk_file, is_indexable, Chunk};
use crate::search::download::{ensure_model, ModelInfo};
use crate::search::embed::{cosine_topk, Embedder, DIM};
use crate::search::fusion::{combine, resolve_alpha, rrf_scores};
use crate::search::ranking::{apply_query_boost, boost_multi_chunk_files, rerank_topk};
use crate::search::tokens::tokenize;

use fs2::FileExt;
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

/// Current schema version written by all new builds.
const SCHEMA: &str = "ast-bro.search-index.v1";
/// Legacy v1 schema from pre-rename installs — still readable.
const SCHEMA_V1_LEGACY: &str = "ast-outline.search-index.v1";
/// Legacy v2 schema from pre-rename installs — still readable.
const SCHEMA_V2_LEGACY: &str = "ast-outline.search-index.v2";

/// On-disk paths under a repo's `.ast-bro/index/` directory.
#[derive(Debug, Clone)]
pub struct IndexPaths {
    pub root: PathBuf,
    pub index_dir: PathBuf,
    pub meta_json: PathBuf,
    pub chunks_bin: PathBuf,
    pub embeddings_f32: PathBuf,
    pub bm25_bin: PathBuf,
    pub files_bin: PathBuf,
    pub lock: PathBuf,
    pub gitignore: PathBuf,
}

impl IndexPaths {
    pub fn from_repo(repo_root: &Path) -> Self {
        let new_dir = repo_root.join(".ast-bro");
        let old_dir = repo_root.join(".ast-outline");

        // Process-wide guard via OnceLock<Mutex<HashSet>> so concurrent threads
        // (e.g. parallel MCP tool calls) don't race on std::fs::rename within
        // the same process, and multiple repos are each migrated at most once.
        // Inter-process races are not covered — fs::rename is atomic on most
        // platforms so the loser simply gets an error, but a filesystem-level
        // lock would be needed for full cross-process safety.
        static MIGRATED: OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
        let set = MIGRATED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
        let mut guard = set.lock().unwrap();
        if guard.insert(repo_root.to_path_buf()) && old_dir.exists() && !new_dir.exists() {
            if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
                eprintln!("warning: could not rename .ast-outline -> .ast-bro: {e}");
            } else {
                eprintln!("info: auto-renamed .ast-outline -> .ast-bro");
            }
        }

        let index_dir = new_dir.join("index");
        Self {
            root: repo_root.to_path_buf(),
            meta_json: index_dir.join("meta.json"),
            chunks_bin: index_dir.join("chunks.bin"),
            embeddings_f32: index_dir.join("embeddings.f32"),
            bm25_bin: index_dir.join("bm25.bin"),
            files_bin: index_dir.join("files.bin"),
            lock: index_dir.join("lock"),
            gitignore: new_dir.join(".gitignore"),
            index_dir,
        }
    }
}

/// Top-level metadata persisted as JSON for human readability + version checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub schema: String,
    #[serde(alias = "ast_outline_version")]
    pub ast_bro_version: String,
    pub model: ModelMeta,
    pub created_unix: u64,
    pub chunk_count: u32,
    /// Always `"f32_le"` for v1/v2. Reserved so a future schema can switch
    /// to f16/quantized.
    pub embedding_dtype: String,
    /// Reserved for incremental updates — empty in v1/v2.
    #[serde(default)]
    pub tombstones: Vec<u32>,
    /// Subdirectory of `paths.root` that this index covers, as a POSIX path
    /// (forward slashes, no leading `./`). `""` means the whole home.
    /// Added in schema v2; defaults to `""` when reading a v1 meta.
    #[serde(default)]
    pub indexed_corpus: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    pub id: String,
    pub dim: u32,
}

/// One search hit — a chunk with its final score.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub chunk: Chunk,
    pub score: f32,
}

/// Options for `search`. `find-related` doesn't need any (just `top_k`).
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub top_k: usize,
    /// Override the auto-resolved alpha. `None` = auto-detect from query type.
    pub alpha: Option<f32>,
    /// If set, restrict to chunks whose `language` field is in this set.
    pub languages: Option<Vec<String>>,
    /// If set, restrict to chunks whose `file_path` starts with this POSIX
    /// prefix (relative to home). `""` or `None` = no filter.
    pub query_scope: Option<String>,
}

impl SearchOptions {
    #[allow(dead_code)] // used by network-gated tests; CLI/MCP build via struct literal
    pub fn with_top_k(top_k: usize) -> Self {
        Self {
            top_k,
            ..Default::default()
        }
    }
}

pub struct Index {
    pub paths: IndexPaths,
    pub meta: Meta,
    chunks: Vec<Chunk>,
    /// `chunk_count × DIM` row-major. Held in memory for v1; mmap is a v2 swap.
    embeddings: Vec<f32>,
    bm25: Bm25Index,
    files: Vec<FileRecord>,
    embedder: Arc<Embedder>,
    /// `live[i] == false` iff chunk id `i` is in `meta.tombstones`.
    /// `None` (the fast path) when no tombstones exist — search/find_related
    /// skip the live filter entirely.
    live_mask: Option<Vec<bool>>,
    /// Memoised dep graph for `find-related` boost. `None` until the
    /// first call; then either Some(graph) when `.ast-bro/deps/`
    /// has a fresh cache, or stays None to mean "no boost available".
    /// Mutated via `RwLock` so the borrow remains shared.
    dep_graph: std::sync::RwLock<Option<Option<crate::deps::DepGraph>>>,
}

/// Compaction kicks in when tombstones occupy more than this fraction of
/// total chunk slots — a full rebuild reclaims the space and resets BM25
/// IDF skew. Override at build time with `AST_BRO_COMPACTION_RATIO` (or
/// legacy `AST_OUTLINE_COMPACTION_RATIO`).
const DEFAULT_COMPACTION_RATIO: f32 = 0.30;

fn compaction_ratio() -> f32 {
    std::env::var("AST_BRO_COMPACTION_RATIO")
        .or_else(|_| std::env::var("AST_OUTLINE_COMPACTION_RATIO"))
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(DEFAULT_COMPACTION_RATIO)
}

impl Index {
    /// Open the index for `path_arg`. Walks up from `path_arg` to `cwd`
    /// looking for an existing `.ast-bro/index/`; if found, refreshes
    /// it on detected file changes, otherwise builds at the resolved home.
    pub fn open(path_arg: &Path, cwd: &Path) -> io::Result<Self> {
        let (home, _found) = resolve_home(path_arg, cwd, Marker::SearchIndex);
        let paths = IndexPaths::from_repo(&home);

        // Try to load. If anything fails (missing files, schema mismatch,
        // corruption) fall back to a fresh build.
        if paths.meta_json.exists() {
            match Self::load_unlocked(&paths) {
                Ok(mut loaded) => {
                    // Compaction trigger first — fires even on empty-delta
                    // opens so a stale-but-quiet repo gets cleaned up too.
                    let total_chunks = loaded.meta.chunk_count as usize;
                    let dead = loaded.meta.tombstones.len();
                    if total_chunks > 0
                        && (dead as f32) / (total_chunks as f32) > compaction_ratio()
                    {
                        eprintln!(
                            "ast-bro: tombstones {}/{} exceed {:.0}% — compacting (full rebuild)",
                            dead,
                            total_chunks,
                            compaction_ratio() * 100.0,
                        );
                        return Self::build_with_corpus(
                            path_arg,
                            cwd,
                            &loaded.meta.indexed_corpus,
                        );
                    }

                    let corpus_dir = corpus_walk_dir(&paths.root, &loaded.meta.indexed_corpus);
                    let delta = compute_delta(&corpus_dir, &paths.root, &loaded.files);
                    if !delta.requires_rebuild() && delta.mtime_only.is_empty() {
                        return Ok(loaded);
                    }

                    if delta.requires_rebuild() {
                        eprintln!(
                            "ast-bro: index stale ({} added, {} modified, {} removed) — applying delta",
                            delta.added.len(),
                            delta.modified.len(),
                            delta.removed.len(),
                        );
                    }
                    match loaded.apply_delta(&delta) {
                        Ok(()) => return Ok(loaded),
                        Err(e) => {
                            eprintln!(
                                "ast-bro: delta apply failed ({e}); falling back to full rebuild"
                            );
                            return Self::build_with_corpus(
                                path_arg,
                                cwd,
                                &loaded.meta.indexed_corpus,
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("ast-bro: index unreadable ({e}); rebuilding");
                }
            }
        }

        Self::build(path_arg, cwd)
    }

    /// Force a full rebuild from scratch. Corpus = `path_arg` relative to
    /// the resolved home (or `""` when `path_arg == home`).
    pub fn build(path_arg: &Path, cwd: &Path) -> io::Result<Self> {
        let (home, _) = resolve_home(path_arg, cwd, Marker::SearchIndex);
        let corpus = relative_posix(path_arg, &home).unwrap_or_default();
        Self::build_with_corpus(path_arg, cwd, &corpus)
    }

    /// Force a full rebuild with an explicit corpus. Used by the corpus
    /// reconciliation logic in `run_index` and by `Index::open` when
    /// rebuilding a stale index (preserves the recorded corpus).
    pub fn build_with_corpus(
        path_arg: &Path,
        cwd: &Path,
        corpus: &str,
    ) -> io::Result<Self> {
        let (home, _) = resolve_home(path_arg, cwd, Marker::SearchIndex);
        let paths = IndexPaths::from_repo(&home);
        fs::create_dir_all(&paths.index_dir)?;
        // Always ensure the .gitignore is present so users don't accidentally
        // commit the cache.
        ensure_gitignore(&paths)?;

        let lock_file = acquire_lock(&paths)?;

        let started = std::time::Instant::now();
        let walk_dir = corpus_walk_dir(&paths.root, corpus);
        if corpus.is_empty() {
            eprintln!("ast-bro: building index for {}", paths.root.display());
        } else {
            eprintln!(
                "ast-bro: building index for {} (corpus: {})",
                paths.root.display(),
                corpus
            );
        }

        // 1. Walk + chunk every indexable file under `walk_dir`. Chunk
        //    file_paths are stored relative to `home` (paths.root) so search
        //    can post-filter by query_scope without remapping.
        let (file_paths, chunks_per_file): (Vec<PathBuf>, Vec<Vec<Chunk>>) =
            walk_and_chunk(&walk_dir, &paths.root);

        // 2. Build flat chunks vec + per-file chunk_range.
        let mut chunks = Vec::new();
        let mut files: Vec<FileRecord> = Vec::with_capacity(file_paths.len());
        for (path, file_chunks) in file_paths.iter().zip(chunks_per_file.into_iter()) {
            let rel = match path.strip_prefix(&paths.root) {
                Ok(r) => normalise_path(r),
                Err(_) => continue,
            };
            let meta_io = match fs::metadata(path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime_ns = mtime_nanos(&meta_io);
            let size = meta_io.len();
            let content_hash = hash_file(path).unwrap_or(0);
            let chunk_start = chunks.len() as u32;
            chunks.extend(file_chunks);
            let chunk_end = chunks.len() as u32;
            files.push(FileRecord {
                path: rel,
                mtime_ns,
                size,
                content_hash,
                chunk_start,
                chunk_end,
            });
        }
        let chunk_count = chunks.len() as u32;
        eprintln!(
            "ast-bro: chunked {} files → {} chunks in {:.1}s",
            file_paths.len(),
            chunk_count,
            started.elapsed().as_secs_f64()
        );

        // 3. Load model + embed all chunks (parallel via rayon).
        let model_dir = ensure_model(&ModelInfo::potion_code_16m())?;
        let embedder = Arc::new(Embedder::open(&model_dir)?);
        let started_embed = std::time::Instant::now();
        let embeddings: Vec<f32> = chunks
            .par_iter()
            .flat_map(|c| {
                let v = embedder.encode_one(&c.content);
                v.to_vec()
            })
            .collect();
        eprintln!(
            "ast-bro: embedded in {:.1}s",
            started_embed.elapsed().as_secs_f64()
        );

        // 4. Build BM25.
        let started_bm25 = std::time::Instant::now();
        let bm25_docs: Vec<Vec<String>> = chunks
            .par_iter()
            .map(|c| tokenize(&enrich_for_bm25(c)))
            .collect();
        let bm25 = Bm25Index::build(bm25_docs);
        eprintln!(
            "ast-bro: bm25 built in {:.1}s",
            started_bm25.elapsed().as_secs_f64()
        );

        // 5. Persist everything atomically — write to temp paths then rename.
        let meta = Meta {
            schema: SCHEMA.to_string(),
            ast_bro_version: env!("CARGO_PKG_VERSION").to_string(),
            model: ModelMeta {
                id: ModelInfo::potion_code_16m().id,
                dim: DIM as u32,
            },
            created_unix: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            chunk_count,
            embedding_dtype: "f32_le".to_string(),
            tombstones: Vec::new(),
            indexed_corpus: corpus.to_string(),
        };
        write_meta(&paths.meta_json, &meta)?;
        write_bincode(&paths.chunks_bin, &chunks)?;
        write_bincode(&paths.files_bin, &files)?;
        write_bincode(&paths.bm25_bin, &bm25)?;
        write_embeddings(&paths.embeddings_f32, &embeddings)?;

        eprintln!(
            "ast-bro: index built in {:.1}s total",
            started.elapsed().as_secs_f64()
        );

        // Lock auto-released on drop.
        drop(lock_file);

        Ok(Self {
            paths,
            meta,
            chunks,
            embeddings,
            bm25,
            files,
            embedder,
            live_mask: None, // fresh build → no tombstones
            dep_graph: std::sync::RwLock::new(None),
        })
    }

    /// Apply a delta in place: tombstone removed/modified files' old
    /// chunks, re-chunk + re-embed modified/added files, append new chunks
    /// + embedding rows, rebuild BM25 over the live set, refresh mtime-only
    /// records, and persist the four binaries + meta.
    ///
    /// On any I/O failure during persistence, the in-memory state may be
    /// partially updated; the caller (Index::open) treats this as a hard
    /// failure and falls back to a full rebuild.
    fn apply_delta(&mut self, delta: &crate::search::cache::Delta) -> io::Result<()> {
        use std::collections::HashSet;

        let started = std::time::Instant::now();

        // --- 1. Identify which existing FileRecords get tombstoned ---
        // Keys: home-relative POSIX paths (FileRecord.path).
        let removed_keys: HashSet<&str> =
            delta.removed.iter().map(|s| s.as_str()).collect();
        let modified_keys: HashSet<String> = delta
            .modified
            .iter()
            .map(|p| {
                p.strip_prefix(&self.paths.root)
                    .map(normalise_path)
                    .unwrap_or_else(|_| p.display().to_string())
            })
            .collect();

        let mut new_tombstones: Vec<u32> = Vec::new();
        let mut new_files: Vec<FileRecord> = Vec::with_capacity(self.files.len());
        let mut mtime_refresh: std::collections::HashMap<String, (i128, u64)> =
            std::collections::HashMap::new();
        for p in &delta.mtime_only {
            let rel = p
                .strip_prefix(&self.paths.root)
                .map(normalise_path)
                .unwrap_or_else(|_| p.display().to_string());
            if let Ok(m) = fs::metadata(p) {
                mtime_refresh.insert(rel, (mtime_nanos(&m), m.len()));
            }
        }

        for f in self.files.drain(..) {
            let key = f.path.as_str();
            let is_removed = removed_keys.contains(key);
            let is_modified = modified_keys.contains(&f.path);
            if is_removed || is_modified {
                for id in f.chunk_start..f.chunk_end {
                    new_tombstones.push(id);
                }
                continue; // dropped (modified files re-added below)
            }
            if let Some((m, sz)) = mtime_refresh.remove(&f.path) {
                let mut updated = f;
                updated.mtime_ns = m;
                updated.size = sz;
                new_files.push(updated);
            } else {
                new_files.push(f);
            }
        }
        self.files = new_files;

        // --- 2. Re-chunk modified + added in parallel; embed sequentially
        //         (embedder is cheap per call but its internal state isn't
        //          shared across threads in our wrapper). Append in stable
        //          input order so chunk_range is contiguous per file. ---
        let mut to_index: Vec<PathBuf> = Vec::with_capacity(
            delta.modified.len() + delta.added.len(),
        );
        to_index.extend(delta.modified.iter().cloned());
        to_index.extend(delta.added.iter().cloned());
        // Deterministic ordering for reproducible chunk ids.
        to_index.sort();

        let chunked: Vec<(PathBuf, Vec<Chunk>)> = to_index
            .par_iter()
            .map(|p| {
                let rel = p
                    .strip_prefix(&self.paths.root)
                    .map(normalise_path)
                    .unwrap_or_else(|_| p.display().to_string());
                (p.clone(), chunk_file(p, &rel))
            })
            .collect();

        let mut added_chunks: u32 = 0;
        let tombstoned_chunks: u32 = new_tombstones.len() as u32;
        for (path, file_chunks) in chunked {
            let chunk_start = self.chunks.len() as u32;
            for c in file_chunks {
                let v = self.embedder.encode_one(&c.content);
                self.embeddings.extend_from_slice(&v);
                self.chunks.push(c);
            }
            let chunk_end = self.chunks.len() as u32;
            added_chunks += chunk_end - chunk_start;

            let meta_io = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime_ns = mtime_nanos(&meta_io);
            let size = meta_io.len();
            let content_hash = hash_file(&path).unwrap_or(0);
            let rel = path
                .strip_prefix(&self.paths.root)
                .map(normalise_path)
                .unwrap_or_else(|_| path.display().to_string());
            self.files.push(FileRecord {
                path: rel,
                mtime_ns,
                size,
                content_hash,
                chunk_start,
                chunk_end,
            });
        }
        // Stable sort so persistence is deterministic.
        self.files.sort_by(|a, b| a.path.cmp(&b.path));

        // --- 3. Update tombstones + meta counters ---
        if !new_tombstones.is_empty() {
            self.meta.tombstones.extend(new_tombstones);
            self.meta.tombstones.sort_unstable();
            self.meta.tombstones.dedup();
        }
        self.meta.chunk_count = self.chunks.len() as u32;
        self.live_mask = build_live_mask(self.chunks.len(), &self.meta.tombstones);

        // --- 4. Rebuild BM25 from live chunks. Tombstoned slots produce
        //         empty doc-tokens so they don't contribute terms; the
        //         empty docs still occupy doc-ids 1:1 with `chunks`,
        //         keeping `get_scores` slot-aligned. ---
        let live_mask_view = self.live_mask.as_deref();
        let bm25_docs: Vec<Vec<String>> = self
            .chunks
            .par_iter()
            .enumerate()
            .map(|(i, c)| {
                if live_mask_view.is_some_and(|m| !m[i]) {
                    Vec::new()
                } else {
                    tokenize(&enrich_for_bm25(c))
                }
            })
            .collect();
        self.bm25 = Bm25Index::build(bm25_docs);

        // --- 5. Persist atomically (best-effort: each file via write_atomic;
        //         meta.json is renamed last so partial-write recovery on
        //         next open will see either the old or new consistent state
        //         once we add directory-rename atomicity). ---
        ensure_gitignore(&self.paths)?;
        let _lock = acquire_lock(&self.paths)?;
        write_bincode(&self.paths.chunks_bin, &self.chunks)?;
        write_bincode(&self.paths.files_bin, &self.files)?;
        write_bincode(&self.paths.bm25_bin, &self.bm25)?;
        write_embeddings(&self.paths.embeddings_f32, &self.embeddings)?;
        write_meta(&self.paths.meta_json, &self.meta)?;

        eprintln!(
            "ast-bro: delta applied (+{added_chunks} chunks, +{tombstoned_chunks} tombstones) in {:.2}s",
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// Load from disk without delta-checking. Used by `open` and tests.
    fn load_unlocked(paths: &IndexPaths) -> io::Result<Self> {
        let meta: Meta = read_meta(&paths.meta_json)?;
        // Accept current schema and the older legacy schemas.
        if meta.schema != SCHEMA && meta.schema != SCHEMA_V1_LEGACY && meta.schema != SCHEMA_V2_LEGACY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("schema {} not in [{SCHEMA}, {SCHEMA_V1_LEGACY}, {SCHEMA_V2_LEGACY}]", meta.schema),
            ));
        }
        if meta.model.dim as usize != DIM {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("model dim {} != {DIM}", meta.model.dim),
            ));
        }

        let chunks: Vec<Chunk> = read_bincode(&paths.chunks_bin)?;
        let files: Vec<FileRecord> = read_bincode(&paths.files_bin)?;
        let bm25: Bm25Index = read_bincode(&paths.bm25_bin)?;
        let embeddings = read_embeddings(&paths.embeddings_f32)?;

        if embeddings.len() != chunks.len() * DIM {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "embeddings.f32 length {} != chunks ({}) × DIM ({DIM})",
                    embeddings.len(),
                    chunks.len()
                ),
            ));
        }

        let model_dir = ensure_model(&ModelInfo::potion_code_16m())?;
        let embedder = Arc::new(Embedder::open(&model_dir)?);
        let live_mask = build_live_mask(chunks.len(), &meta.tombstones);

        Ok(Self {
            paths: paths.clone(),
            meta,
            chunks,
            embeddings,
            bm25,
            files,
            embedder,
            live_mask,
            dep_graph: std::sync::RwLock::new(None),
        })
    }

    /// Total chunk slots, including tombstones.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Live chunks (non-tombstoned) — what search actually retrieves.
    #[allow(dead_code)] // public for embedders + future --stats wiring
    pub fn live_chunk_count(&self) -> usize {
        self.chunks.len() - self.meta.tombstones.len()
    }

    /// Number of tombstoned chunk slots.
    #[allow(dead_code)] // public for embedders + future --stats wiring
    pub fn tombstone_count(&self) -> usize {
        self.meta.tombstones.len()
    }

    /// Hybrid BM25 + dense search with full ranking pipeline.
    pub fn search(&self, query: &str, opts: &SearchOptions) -> Vec<SearchHit> {
        if self.chunks.is_empty() || opts.top_k == 0 {
            return Vec::new();
        }
        let alpha = resolve_alpha(query, opts.alpha);
        let candidate_count = opts.top_k * 5;

        // Build combined mask: language ∧ query_scope ∧ live. Any may be None.
        let mask = build_combined_mask(
            &self.chunks,
            opts.languages.as_deref(),
            opts.query_scope.as_deref(),
            self.live_mask.as_deref(),
        );

        // Coverage-gap warning: if query_scope is set and points outside the
        // indexed corpus, results will be empty silently otherwise.
        if let Some(scope) = opts.query_scope.as_deref() {
            let corpus = self.meta.indexed_corpus.as_str();
            if !scope.is_empty()
                && !corpus.is_empty()
                && !path_starts_with(scope, corpus)
                && !path_starts_with(corpus, scope)
            {
                eprintln!(
                    "ast-bro: query scope '{scope}' is outside the indexed corpus '{corpus}' \
                     — results will be empty. Re-run `ast-bro index .` to widen."
                );
            }
        }

        // Semantic top-N.
        let q_embed = self.embedder.encode_one(query);
        let semantic_scored = cosine_topk(
            &q_embed,
            &self.embeddings,
            mask.as_deref(),
            candidate_count,
        );

        // BM25 top-N.
        let query_tokens = tokenize(query);
        let bm25_scores = if query_tokens.is_empty() {
            Vec::new()
        } else {
            let raw = self.bm25.get_scores(&query_tokens, mask.as_deref());
            top_k_indices(&raw, candidate_count)
        };

        // RRF + alpha combine.
        let sem_rrf = rrf_scores(&semantic_scored);
        let bm25_rrf = rrf_scores(&bm25_scores);
        let combined = combine(&sem_rrf, &bm25_rrf, alpha);

        // File coherence + query-aware boosts.
        let mut scored = combined;
        boost_multi_chunk_files(&mut scored, &self.chunks);
        let scored = apply_query_boost(scored, query, &self.chunks);

        // Final top-k with path penalties + saturation decay.
        let ranked = rerank_topk(&scored, &self.chunks, opts.top_k, /* penalise_paths */ true);
        ranked
            .into_iter()
            .map(|(id, score)| SearchHit {
                chunk: self.chunks[id as usize].clone(),
                score,
            })
            .collect()
    }

    /// Lazily load the dep graph cache (if any). Returns None when no
    /// fresh cache exists — `find_related` then skips the boost.
    fn dep_graph_cached(&self) -> Option<crate::deps::DepGraph> {
        {
            let guard = self.dep_graph.read().ok()?;
            if let Some(slot) = guard.as_ref() {
                return slot.clone();
            }
        }
        let loaded = crate::graph_cache::shared::get_or_init(&self.paths.root).ok().map(|u| u.deps.clone());
        if let Ok(mut w) = self.dep_graph.write() {
            *w = Some(loaded.clone());
        }
        loaded
    }

    /// Semantic-only similarity from a chunk identified by its file + line.
    /// Filters to chunks of the same language and excludes the source itself.
    /// When a fresh dep-graph cache exists, also applies a multiplicative
    /// boost to chunks in the importer/importee neighbourhood.
    pub fn find_related(
        &self,
        file_path: &str,
        line: u32,
        top_k: usize,
    ) -> Option<Vec<SearchHit>> {
        self.find_related_opts(
            file_path, line, top_k, /* dep_boost */ true, /* dep_depth */ 2, None,
        )
    }

    pub fn find_related_opts(
        &self,
        file_path: &str,
        line: u32,
        top_k: usize,
        dep_boost: bool,
        dep_depth: usize,
        query_scope: Option<&str>,
    ) -> Option<Vec<SearchHit>> {
        let source_id = resolve_chunk(&self.chunks, file_path, line)?;
        let source = &self.chunks[source_id as usize];

        // Build language-restricted + self-excluding (+ scope-filtered + live) mask.
        let live = self.live_mask.as_deref();
        let mut mask = vec![false; self.chunks.len()];
        for (i, c) in self.chunks.iter().enumerate() {
            mask[i] = i as u32 != source_id
                && c.language == source.language
                && scope_matches(query_scope, &c.file_path)
                && live.is_none_or(|m| m[i]);
        }

        // Pull a wider candidate window when boosting so the boost can
        // promote items that wouldn't be in the top-k by raw similarity.
        let candidate_k = if dep_boost { top_k * 5 } else { top_k };
        let q_embed = self.embedder.encode_one(&source.content);
        let mut scored = cosine_topk(&q_embed, &self.embeddings, Some(&mask), candidate_k);

        if dep_boost {
            if let Some(graph) = self.dep_graph_cached() {
                let abs_source = self.paths.root.join(&source.file_path);
                let abs_source = abs_source.canonicalize().unwrap_or(abs_source);
                let depths = crate::deps::traverse::neighbourhood_depths(&graph, &abs_source, dep_depth);
                if !depths.is_empty() {
                    for (id, score) in scored.iter_mut() {
                        let chunk = &self.chunks[*id as usize];
                        let abs = self.paths.root.join(&chunk.file_path);
                        let abs = abs.canonicalize().unwrap_or(abs);
                        if let Some(d) = depths.get(&abs) {
                            *score *= match *d {
                                0 => 1.0, // self — masked already
                                1 => 1.40,
                                2 => 1.20,
                                _ => 1.0,
                            };
                        }
                    }
                    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    scored.truncate(top_k);
                }
            }
        }

        // Truncate (no-op if dep_boost was off).
        scored.truncate(top_k);

        Some(
            scored
                .into_iter()
                .map(|(id, score)| SearchHit {
                    chunk: self.chunks[id as usize].clone(),
                    score,
                })
                .collect(),
        )
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

/// Combine language + query_scope + tombstone (`live`) masks. Returns
/// `None` when no filter is active (semantically: all chunks pass).
fn build_combined_mask(
    chunks: &[Chunk],
    languages: Option<&[String]>,
    query_scope: Option<&str>,
    live_mask: Option<&[bool]>,
) -> Option<Vec<bool>> {
    let lang_active = languages.is_some_and(|l| !l.is_empty());
    let scope_active = query_scope.is_some_and(|s| !s.is_empty());
    let live_active = live_mask.is_some();
    if !lang_active && !scope_active && !live_active {
        return None;
    }
    Some(
        chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let lang_ok = match languages {
                    Some(langs) if !langs.is_empty() => langs.iter().any(|l| l == &c.language),
                    _ => true,
                };
                let scope_ok = scope_matches(query_scope, &c.file_path);
                let live_ok = live_mask.is_none_or(|m| m[i]);
                lang_ok && scope_ok && live_ok
            })
            .collect(),
    )
}

/// Build a `live[i] == !is_tombstoned(i)` mask, or `None` when there are no
/// tombstones (callers can short-circuit the AND in the hot loop).
fn build_live_mask(chunk_count: usize, tombstones: &[u32]) -> Option<Vec<bool>> {
    if tombstones.is_empty() {
        return None;
    }
    let mut m = vec![true; chunk_count];
    for &id in tombstones {
        if let Some(slot) = m.get_mut(id as usize) {
            *slot = false;
        }
    }
    Some(m)
}

/// True when `file_path` is under (or equal to) the `query_scope` prefix.
/// Empty / `None` scope passes everything.
fn scope_matches(query_scope: Option<&str>, file_path: &str) -> bool {
    match query_scope {
        None => true,
        Some(s) if s.is_empty() => true,
        Some(s) => path_starts_with(file_path, s),
    }
}

/// Component-wise prefix check. `path_starts_with("packages/a", "packages")`
/// is true; `path_starts_with("packagesfoo", "packages")` is false.
fn path_starts_with(child: &str, parent: &str) -> bool {
    if parent.is_empty() {
        return true;
    }
    if child == parent {
        return true;
    }
    if child.len() <= parent.len() {
        return false;
    }
    child.starts_with(parent) && child.as_bytes()[parent.len()] == b'/'
}

/// Resolve the absolute directory we should walk for the given corpus.
/// Empty corpus = the whole home.
fn corpus_walk_dir(home: &Path, corpus: &str) -> PathBuf {
    if corpus.is_empty() {
        home.to_path_buf()
    } else {
        home.join(corpus)
    }
}

/// Convert a dense scores vector into the top-k `(id, score)` pairs (descending).
/// Used for BM25 (which returns one score per chunk).
fn top_k_indices(scores: &[f32], k: usize) -> Vec<(u32, f32)> {
    if scores.is_empty() || k == 0 {
        return Vec::new();
    }
    let take = k.min(scores.len());
    let mut idx: Vec<u32> = (0..scores.len() as u32).collect();
    idx.select_nth_unstable_by(take - 1, |&a, &b| {
        scores[b as usize]
            .partial_cmp(&scores[a as usize])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut top: Vec<u32> = idx.into_iter().take(take).collect();
    top.sort_by(|&a, &b| {
        scores[b as usize]
            .partial_cmp(&scores[a as usize])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Drop zero-score entries — BM25 zeros mean "no query token matched".
    top.into_iter()
        .map(|i| (i, scores[i as usize]))
        .filter(|(_, s)| *s > 0.0)
        .collect()
}

/// Find the chunk that best contains `file_path:line`.
fn resolve_chunk(chunks: &[Chunk], file_path: &str, line: u32) -> Option<u32> {
    let normalised = file_path.replace('\\', "/");
    let mut fallback: Option<u32> = None;
    for (i, c) in chunks.iter().enumerate() {
        if c.file_path != normalised {
            continue;
        }
        if c.start_line <= line && line < c.end_line {
            return Some(i as u32);
        }
        if line == c.end_line {
            fallback = Some(i as u32);
        }
    }
    fallback
}

/// Append file path components to chunk content to boost path-based queries.
fn enrich_for_bm25(chunk: &Chunk) -> String {
    let path = Path::new(&chunk.file_path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let dir_parts: Vec<&str> = path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| c.as_os_str().to_str())
                .filter(|s| *s != "." && *s != "/")
                .collect()
        })
        .unwrap_or_default();
    let dir_text: String = dir_parts
        .iter()
        .rev()
        .take(3)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} {} {} {}", chunk.content, stem, stem, dir_text)
}

/// Walk `walk_root` and return absolute file paths + their chunks.
/// Chunk `file_path` strings are stored relative to `strip_root` so that a
/// corpus-narrowed walk still produces stable paths relative to the index
/// home (used by `query_scope` filtering at search time).
fn walk_and_chunk(walk_root: &Path, strip_root: &Path) -> (Vec<PathBuf>, Vec<Vec<Chunk>>) {
    // Collect indexable paths first so chunking can run in parallel.
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut builder = WalkBuilder::new(walk_root);
    add_filters(&mut builder, walk_root);
    let walker = builder.build();
    for entry in walker.flatten() {
        let p = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if is_indexable(p).is_none() {
            continue;
        }
        if should_skip_path(p, walk_root) {
            continue;
        }
        paths.push(p.to_path_buf());
    }
    paths.sort(); // deterministic order

    let chunks_per_file: Vec<Vec<Chunk>> = paths
        .par_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(strip_root)
                .map(normalise_path)
                .unwrap_or_else(|_| p.display().to_string());
            chunk_file(p, &rel)
        })
        .collect();

    (paths, chunks_per_file)
}

fn ensure_gitignore(paths: &IndexPaths) -> io::Result<()> {
    if paths.gitignore.exists() {
        return Ok(());
    }
    if let Some(parent) = paths.gitignore.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&paths.gitignore, "*\n")?;
    Ok(())
}

fn acquire_lock(paths: &IndexPaths) -> io::Result<fs::File> {
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&paths.lock)?;
    lock_file.lock_exclusive().map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("could not acquire index lock: {e}"),
        )
    })?;
    Ok(lock_file)
}

fn write_meta(path: &Path, meta: &Meta) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(meta)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    write_atomic(path, &json)
}

fn read_meta(path: &Path) -> io::Result<Meta> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_bincode<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    write_atomic(path, &bytes)
}

fn read_bincode<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    let (value, _): (T, _) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(value)
}

fn write_embeddings(path: &Path, values: &[f32]) -> io::Result<()> {
    // Header-less, contiguous little-endian f32. Length is known from
    // chunk_count × DIM in meta.json.
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for v in values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    write_atomic(path, &bytes)
}

fn read_embeddings(path: &Path) -> io::Result<Vec<f32>> {
    let bytes = fs::read(path)?;
    if bytes.len() % 4 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "embeddings.f32 length not a multiple of 4",
        ));
    }
    let n = bytes.len() / 4;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let arr: [u8; 4] = bytes[i * 4..i * 4 + 4].try_into().unwrap();
        out.push(f32::from_le_bytes(arr));
    }
    Ok(out)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    Ok(())
}

fn mtime_nanos(meta: &fs::Metadata) -> i128 {
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    match mtime.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

fn normalise_path(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn tmp_repo() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_file(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn index_paths_layout() {
        let p = IndexPaths::from_repo(Path::new("/r"));
        assert!(p.index_dir.ends_with(".ast-bro/index"));
        assert!(p.gitignore.ends_with(".ast-bro/.gitignore"));
        assert!(p.meta_json.ends_with("meta.json"));
        assert!(p.embeddings_f32.ends_with("embeddings.f32"));
    }

    #[test]
    fn enrich_for_bm25_includes_stem_twice_and_dirs() {
        let chunk = Chunk {
            content: "fn x() {}".to_string(),
            file_path: "src/auth/login.rs".to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 9,
            language: "rust".to_string(),
        };
        let enriched = enrich_for_bm25(&chunk);
        // Stem appears twice; "src" and "auth" appear once each in dir text.
        let count = |s: &str, n: &str| s.matches(n).count();
        assert_eq!(count(&enriched, "login"), 2);
        assert!(enriched.contains("src"));
        assert!(enriched.contains("auth"));
    }

    #[test]
    fn resolve_chunk_finds_overlapping() {
        let mk = |sl, el| Chunk {
            content: String::new(),
            file_path: "f.rs".to_string(),
            start_line: sl,
            end_line: el,
            start_byte: 0,
            end_byte: 0,
            language: "rust".to_string(),
        };
        let chunks = vec![mk(1, 10), mk(20, 30), mk(40, 50)];
        assert_eq!(resolve_chunk(&chunks, "f.rs", 5), Some(0));
        assert_eq!(resolve_chunk(&chunks, "f.rs", 25), Some(1));
        assert_eq!(resolve_chunk(&chunks, "f.rs", 9), Some(0));
        // line == end_line: fallback path.
        assert_eq!(resolve_chunk(&chunks, "f.rs", 50), Some(2));
        // No matching file.
        assert_eq!(resolve_chunk(&chunks, "other.rs", 5), None);
        // Out-of-range line.
        assert_eq!(resolve_chunk(&chunks, "f.rs", 60), None);
    }

    #[test]
    fn top_k_indices_orders_and_drops_zeros() {
        let scores = vec![0.0, 0.5, 0.0, 0.9, 0.1];
        let top = top_k_indices(&scores, 5);
        assert_eq!(top.len(), 3); // zeros dropped
        assert_eq!(top[0].0, 3);
        assert_eq!(top[1].0, 1);
        assert_eq!(top[2].0, 4);
    }

    /// Smoke test of the persistence round-trip without touching the embedder.
    /// Builds tiny structures by hand, writes, reads back, asserts equality.
    #[test]
    fn persistence_roundtrip() {
        let dir = tmp_repo();
        let paths = IndexPaths::from_repo(dir.path());
        fs::create_dir_all(&paths.index_dir).unwrap();

        let chunks = vec![Chunk {
            content: "hello".to_string(),
            file_path: "a.rs".to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 5,
            language: "rust".to_string(),
        }];
        let files = vec![FileRecord {
            path: "a.rs".to_string(),
            mtime_ns: 0,
            size: 5,
            content_hash: 0,
            chunk_start: 0,
            chunk_end: 1,
        }];
        let bm25 = Bm25Index::build(vec![vec!["hello".to_string()]]);
        let embeddings = vec![0.0; DIM];
        let meta = Meta {
            schema: SCHEMA.to_string(),
            ast_bro_version: env!("CARGO_PKG_VERSION").to_string(),
            model: ModelMeta { id: "m".into(), dim: DIM as u32 },
            created_unix: 0,
            chunk_count: 1,
            embedding_dtype: "f32_le".to_string(),
            tombstones: Vec::new(),
            indexed_corpus: String::new(),
        };

        write_meta(&paths.meta_json, &meta).unwrap();
        write_bincode(&paths.chunks_bin, &chunks).unwrap();
        write_bincode(&paths.files_bin, &files).unwrap();
        write_bincode(&paths.bm25_bin, &bm25).unwrap();
        write_embeddings(&paths.embeddings_f32, &embeddings).unwrap();

        let meta2: Meta = read_meta(&paths.meta_json).unwrap();
        let chunks2: Vec<Chunk> = read_bincode(&paths.chunks_bin).unwrap();
        let files2: Vec<FileRecord> = read_bincode(&paths.files_bin).unwrap();
        let _bm25_2: Bm25Index = read_bincode(&paths.bm25_bin).unwrap();
        let emb2 = read_embeddings(&paths.embeddings_f32).unwrap();

        assert_eq!(meta2.chunk_count, 1);
        assert_eq!(chunks2, chunks);
        assert_eq!(files2, files);
        assert_eq!(emb2, embeddings);
    }

    /// Full end-to-end: build, search, find_related against a tiny tmp repo.
    /// Network-gated — requires the model be downloadable.
    #[test]
    #[ignore]
    fn network_end_to_end_build_and_search() {
        let dir = tmp_repo();
        // Plant some Rust files with semantically distinct content.
        write_file(
            dir.path(),
            "src/auth/login.rs",
            "pub fn login(username: &str, password: &str) -> bool { username == \"admin\" }",
        );
        write_file(
            dir.path(),
            "src/auth/logout.rs",
            "pub fn logout(session_id: &str) { drop_session(session_id) }",
        );
        write_file(
            dir.path(),
            "src/http/handler.rs",
            "pub struct HandlerStack { items: Vec<u32> }",
        );

        let index = Index::build(dir.path(), dir.path()).expect("build failed");
        assert!(index.chunk_count() >= 3);

        // Symbol query: should rank handler.rs first.
        let hits = index.search("HandlerStack", &SearchOptions::with_top_k(3));
        assert!(!hits.is_empty());
        assert!(hits[0].chunk.file_path.contains("handler.rs"));

        // NL query: should rank one of the auth files first.
        let hits = index.search("how does login work", &SearchOptions::with_top_k(3));
        assert!(!hits.is_empty());
        assert!(hits[0].chunk.file_path.contains("login.rs"));

        // find-related from login.rs:1 should pull logout.rs (same lang, related).
        let related = index
            .find_related("src/auth/login.rs", 1, 5)
            .expect("source chunk not found");
        assert!(!related.is_empty());
        // The source chunk itself must be excluded.
        assert!(related.iter().all(|h| !h.chunk.file_path.contains("login.rs")));

        // Re-open from cache: should detect no changes and skip rebuild.
        let reopened = Index::open(dir.path(), dir.path()).expect("re-open failed");
        assert_eq!(reopened.chunk_count(), index.chunk_count());
    }

    #[test]
    fn path_starts_with_component_boundary() {
        assert!(path_starts_with("packages/a", "packages"));
        assert!(path_starts_with("packages", "packages"));
        assert!(!path_starts_with("packagesfoo", "packages"));
        assert!(!path_starts_with("packages", "packages/a"));
        assert!(path_starts_with("anything", ""));
    }

    #[test]
    fn scope_matches_basic() {
        assert!(scope_matches(None, "src/foo.rs"));
        assert!(scope_matches(Some(""), "src/foo.rs"));
        assert!(scope_matches(Some("src"), "src/foo.rs"));
        assert!(!scope_matches(Some("packages"), "src/foo.rs"));
    }

    #[test]
    fn build_combined_mask_returns_none_when_no_filters() {
        let mk = || Chunk {
            content: String::new(),
            file_path: "src/a.rs".to_string(),
            start_line: 0,
            end_line: 0,
            start_byte: 0,
            end_byte: 0,
            language: "rust".to_string(),
        };
        let chunks = vec![mk()];
        assert!(build_combined_mask(&chunks, None, None, None).is_none());
        assert!(build_combined_mask(&chunks, None, Some(""), None).is_none());
    }

    #[test]
    fn build_combined_mask_combines_lang_scope_and_live() {
        let mk = |lang: &str, p: &str| Chunk {
            content: String::new(),
            file_path: p.to_string(),
            start_line: 0,
            end_line: 0,
            start_byte: 0,
            end_byte: 0,
            language: lang.to_string(),
        };
        let chunks = vec![
            mk("rust", "src/a.rs"),
            mk("rust", "packages/b.rs"),
            mk("python", "src/c.py"),
            mk("rust", "src/d.rs"),
        ];
        let mask = build_combined_mask(
            &chunks,
            Some(&["rust".to_string()]),
            Some("src"),
            None,
        )
        .expect("filters active → some mask");
        assert_eq!(mask, vec![true, false, false, true]);

        // Tombstone src/d.rs (id 3) — should drop out.
        let live = vec![true, true, true, false];
        let mask = build_combined_mask(
            &chunks,
            Some(&["rust".to_string()]),
            Some("src"),
            Some(&live),
        )
        .expect("filters active");
        assert_eq!(mask, vec![true, false, false, false]);
    }

    #[test]
    fn live_mask_returns_none_when_no_tombstones() {
        assert!(build_live_mask(10, &[]).is_none());
    }

    #[test]
    fn live_mask_marks_tombstoned_slots_dead() {
        let m = build_live_mask(5, &[1, 3]).expect("tombstones present");
        assert_eq!(m, vec![true, false, true, false, true]);
    }
}
