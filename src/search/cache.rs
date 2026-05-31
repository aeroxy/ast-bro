//! On-disk record of indexed files + cheap mtime/hash delta detection.
//!
//! Per-file record stored in `.ast-bro/index/files.bin` (bincode). On
//! `Index::open`, we walk the working tree once and compare each file's
//! `(mtime_ns, size)` against the recorded value — only mismatches advance to
//! an xxhash3 of the file bytes. This keeps the steady-state "no changes"
//! cost dominated by stat syscalls (~30 ms on a 10k-file repo).
//!
//! A non-empty delta is applied incrementally by `Index::apply_delta` (the
//! per-file `chunk_start..chunk_end` range plus the `meta.json` tombstones
//! vector let it patch only the changed files); a full rebuild is the
//! fallback path when that errors or when compaction kicks in.

use crate::file_filter::{add_filters, should_skip_path};
use crate::search::chunker::is_indexable;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Files larger than this are skipped during indexing. Above a couple of MB a
/// file is almost always generated/minified/vendored output: it bloats both
/// the chunk store and the embedding pass while adding little searchable
/// signal, and a single such file read whole into memory on every
/// `Index::open` is a needless cost. The same guard is applied at both walk
/// sites — `compute_delta` here and `walk_and_chunk` in `index.rs` — so the
/// two never disagree on the file set (a mismatch would make `compute_delta`
/// report a perpetual delta against files the build never chunked).
pub const MAX_INDEX_FILE_BYTES: u64 = 2_000_000;

/// Cached metadata for one indexed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Repo-relative POSIX path.
    pub path: String,
    /// Modification time in nanos since UNIX epoch (i128 for negative pre-1970 timestamps).
    pub mtime_ns: i128,
    /// File size in bytes.
    pub size: u64,
    /// xxhash3-64 of the file bytes. Only used when `(mtime, size)` differs
    /// from the cache — lets us distinguish "touched but unchanged" from a real edit.
    pub content_hash: u64,
    /// `[start, end)` indices into `chunks.bin` for this file's chunks.
    pub chunk_start: u32,
    pub chunk_end: u32,
}

/// Result of comparing the cache against the working tree.
#[derive(Debug, Default, Clone)]
pub struct Delta {
    /// Files present on disk but not in the cache.
    pub added: Vec<PathBuf>,
    /// Files present in both, but with a different content hash.
    pub modified: Vec<PathBuf>,
    /// Files in the cache but no longer on disk.
    pub removed: Vec<String>,
    /// Files where mtime/size changed but content hash matched. Records can
    /// have their mtime refreshed without re-embedding.
    pub mtime_only: Vec<PathBuf>,
    /// Total number of indexable files seen on disk.
    pub seen_count: usize,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    pub fn requires_rebuild(&self) -> bool {
        !self.is_empty()
    }
}

/// Walk `walk_root` and compute the delta against `cached_files`.
///
/// FileRecord paths are stored relative to the index home, which may be a
/// parent of `walk_root` when an index covers a subdirectory corpus. Pass
/// `strip_root = home` so that walked paths get stripped to home-relative
/// form before comparison. When the corpus equals home, `walk_root ==
/// strip_root` and behaviour matches the original single-root form.
///
/// Honours `.gitignore` etc. via `ignore::WalkBuilder` and only considers
/// files that pass `is_indexable`.
pub fn compute_delta(
    walk_root: &Path,
    strip_root: &Path,
    cached_files: &[FileRecord],
) -> Delta {
    let cached: HashMap<&str, &FileRecord> =
        cached_files.iter().map(|r| (r.path.as_str(), r)).collect();

    let mut delta = Delta::default();
    let mut seen: HashSet<String> = HashSet::with_capacity(cached.len());

    let mut builder = WalkBuilder::new(walk_root);
    add_filters(&mut builder, walk_root);
    let walker = builder.build();

    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        // Belt-and-suspenders: skip the hardcoded denylist even when
        // .gitignore / .ast-bro-ignore don't list it.
        if should_skip_path(path, walk_root) {
            continue;
        }
        if is_indexable(path).is_none() {
            continue;
        }
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = meta.len();
        // Oversized files are not indexed — see MAX_INDEX_FILE_BYTES. Skipping
        // before `seen` means a file that grows past the cap is treated as
        // removed on the next open (and dropped from the index), matching the
        // build-side walk which won't chunk it either.
        if size > MAX_INDEX_FILE_BYTES {
            continue;
        }
        delta.seen_count += 1;

        let rel = match path.strip_prefix(strip_root) {
            Ok(r) => normalise_path(r),
            Err(_) => continue,
        };
        seen.insert(rel.clone());

        let mtime_ns = mtime_nanos(&meta);

        match cached.get(rel.as_str()) {
            None => delta.added.push(path.to_path_buf()),
            Some(record) => {
                if record.mtime_ns == mtime_ns && record.size == size {
                    // Cheap path: unchanged, skip the hash.
                    continue;
                }
                // Expensive path: hash the file to disambiguate.
                match hash_file(path) {
                    Ok(hash) if hash == record.content_hash => {
                        delta.mtime_only.push(path.to_path_buf());
                    }
                    Ok(_) => delta.modified.push(path.to_path_buf()),
                    Err(_) => delta.modified.push(path.to_path_buf()),
                }
            }
        }
    }

    for record in cached_files {
        if !seen.contains(record.path.as_str()) {
            delta.removed.push(record.path.clone());
        }
    }

    delta
}

/// Compute xxhash3-64 of file contents.
pub fn hash_file(path: &Path) -> std::io::Result<u64> {
    use std::io::Read;
    use xxhash_rust::xxh3::Xxh3;

    let mut hasher = Xxh3::new();
    let mut file = fs::File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest())
}

fn mtime_nanos(meta: &fs::Metadata) -> i128 {
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    match mtime.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

fn normalise_path(p: &Path) -> String {
    // POSIX separators for stable cross-platform records.
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

    fn touch(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn delta_all_added_on_empty_cache() {
        let dir = tmp_repo();
        touch(dir.path(), "src/a.rs", "fn a(){}");
        touch(dir.path(), "src/b.py", "def b(): pass");
        touch(dir.path(), "skip.txt", "ignored extension");

        let delta = compute_delta(dir.path(), dir.path(), &[]);
        assert_eq!(delta.added.len(), 2, "expected only the 2 indexable files");
        assert_eq!(delta.seen_count, 2);
        assert!(delta.removed.is_empty());
        assert!(delta.modified.is_empty());
    }

    #[test]
    fn delta_unchanged_when_nothing_changed() {
        let dir = tmp_repo();
        let path = touch(dir.path(), "f.rs", "fn x() {}");
        let meta = fs::metadata(&path).unwrap();
        let hash = hash_file(&path).unwrap();
        let record = FileRecord {
            path: "f.rs".to_string(),
            mtime_ns: mtime_nanos(&meta),
            size: meta.len(),
            content_hash: hash,
            chunk_start: 0,
            chunk_end: 1,
        };

        let delta = compute_delta(dir.path(), dir.path(), &[record]);
        assert!(delta.is_empty());
        assert_eq!(delta.seen_count, 1);
    }

    #[test]
    fn delta_detects_modified_via_hash() {
        let dir = tmp_repo();
        let path = touch(dir.path(), "f.rs", "fn x() {}");
        let record = FileRecord {
            path: "f.rs".to_string(),
            mtime_ns: 0, // forces hash check
            size: 999,   // mismatch on size too
            content_hash: 42, // wrong hash
            chunk_start: 0,
            chunk_end: 1,
        };
        // Briefly bump the mtime to ensure the cheap check fails.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&path, "fn x() {} // changed").unwrap();

        let delta = compute_delta(dir.path(), dir.path(), &[record]);
        assert_eq!(delta.modified.len(), 1);
    }

    #[test]
    fn delta_mtime_only_when_hash_unchanged() {
        let dir = tmp_repo();
        let path = touch(dir.path(), "f.rs", "fn x() {}");
        let hash = hash_file(&path).unwrap();
        let size = fs::metadata(&path).unwrap().len();

        // Build a record with the right hash + size but a deliberately wrong mtime.
        let record = FileRecord {
            path: "f.rs".to_string(),
            mtime_ns: 0, // forces hash check
            size,
            content_hash: hash,
            chunk_start: 0,
            chunk_end: 1,
        };
        let delta = compute_delta(dir.path(), dir.path(), &[record]);
        assert_eq!(delta.mtime_only.len(), 1);
        assert!(delta.modified.is_empty());
        assert!(delta.added.is_empty());
    }

    #[test]
    fn delta_detects_removed() {
        let dir = tmp_repo();
        // No file on disk; cache claims one was here.
        let record = FileRecord {
            path: "ghost.rs".to_string(),
            mtime_ns: 0,
            size: 0,
            content_hash: 0,
            chunk_start: 0,
            chunk_end: 0,
        };
        let delta = compute_delta(dir.path(), dir.path(), &[record]);
        assert_eq!(delta.removed.len(), 1);
        assert_eq!(delta.removed[0], "ghost.rs");
    }

    #[test]
    fn delta_skips_oversized_file() {
        let dir = tmp_repo();
        touch(dir.path(), "small.rs", "fn small() {}");
        // One indexable file just over the cap.
        let big = "x".repeat((MAX_INDEX_FILE_BYTES as usize) + 1);
        touch(dir.path(), "big.rs", &big);

        let delta = compute_delta(dir.path(), dir.path(), &[]);
        assert_eq!(delta.added.len(), 1, "oversized file must be skipped");
        assert_eq!(delta.seen_count, 1);
        assert!(delta.added[0].ends_with("small.rs"));
    }

    #[test]
    fn skip_dir_blocks_node_modules() {
        let dir = tmp_repo();
        touch(dir.path(), "node_modules/lib/index.js", "export {}");
        touch(dir.path(), "src/main.rs", "fn main(){}");

        let delta = compute_delta(dir.path(), dir.path(), &[]);
        assert_eq!(delta.added.len(), 1);
        assert!(delta.added[0].ends_with("main.rs"));
    }
}
