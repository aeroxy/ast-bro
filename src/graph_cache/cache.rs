//! Disk persistence for the unified `UnifiedGraph` at
//! `.ast-bro/deps/graph.bin`. Replaces the previous `src/deps/cache.rs`
//! which serialized only `DepGraph`. Schema bump from `deps-index.v1` to
//! `graph-index.v1` forces a one-time rebuild for upgrading users.
//!
//! Mirrors the search-index pattern in `src/search/cache.rs`: mtime/size/
//! xxhash3 delta detection, advisory `fs2` lock, atomic `.tmp` + rename.

use crate::core::JSON_SCHEMA_GRAPH_INDEX;
use crate::deps::build_graph;
use crate::deps::manifest::ROOT_MANIFESTS;
use crate::deps::resolver::build::{deps_walk, is_deps_input};
use crate::file_filter::should_skip_path;
use crate::graph_cache::delta::{apply_delta_to_calls, apply_delta_to_deps, refresh_records};
use crate::graph_cache::UnifiedGraph;
use crate::search::cache::{delta_from_files, hash_file, Delta, FileRecord};
use bincode::serde::{decode_from_slice, encode_to_vec};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const CACHE_SCHEMA: &str = JSON_SCHEMA_GRAPH_INDEX;

/// On-disk wrapper combining the unified graph + the file fingerprints used
/// for freshness detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheFile {
  pub schema: String,
  /// The version of the binary that extracted `graph`. Another version's
  /// adapters may extract differently, so its cache is rebuilt even when
  /// `schema` matches (#81). `schema` changes only with the layout.
  pub version: String,
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
  /// Cache exists, schema matches, no files changed. Carries the persisted
  /// fingerprints so callers can keep them in memory for cheap re-validation.
  Fresh {
    graph: UnifiedGraph,
    records: Vec<FileRecord>,
  },
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
  if cf.schema != CACHE_SCHEMA || cf.version != env!("CARGO_PKG_VERSION") {
    return LoadOutcome::Missing;
  }
  let delta = compute_graph_delta(root, &cf.files);
  if !delta.requires_rebuild() {
    return LoadOutcome::Fresh {
      graph: cf.graph,
      records: cf.files,
    };
  }
  LoadOutcome::Stale {
    graph: cf.graph,
    delta,
    prev_records: cf.files,
  }
}

/// Build a fresh `UnifiedGraph` (deps half only — call graph is materialised
/// later via `promote_calls`), persist it, and return it with the file
/// fingerprints captured during the build.
fn build_with_records(root: &Path) -> std::io::Result<(UnifiedGraph, Vec<FileRecord>)> {
  build_with_records_by(root, build_graph)
}

/// `build_with_records` with the build step passed in, so a test can edit a
/// file while the build reads it.
///
/// The fingerprints are taken before the build reads anything. A file an
/// editor saves while the build runs then keeps its earlier fingerprint,
/// and the next check sees the edit. Taken after the build, they would
/// record the edit against a graph that read the earlier contents, and
/// that graph would load as fresh until another change.
fn build_with_records_by(
  root: &Path,
  build: impl FnOnce(&Path) -> Result<crate::deps::DepGraph, crate::deps::DepError>,
) -> std::io::Result<(UnifiedGraph, Vec<FileRecord>)> {
  let records = collect_file_records(root)?;
  let deps = build(root).map_err(std::io::Error::other)?;
  let graph = UnifiedGraph::from_deps(deps);
  let _ = save(root, &graph, &records);
  Ok((graph, records))
}

/// Patch `graph` for `delta` and return the records it reflects; `None`
/// when the deps half fails, and the caller rebuilds. The calls half is
/// patched only when the graph has it; a `None` calls half is promoted
/// lazily by the next `callers` / `callees` query.
pub(crate) fn patch(
  graph: &mut UnifiedGraph,
  prev: Vec<FileRecord>,
  root: &Path,
  delta: &Delta,
) -> Option<Vec<FileRecord>> {
  patch_by(graph, prev, root, delta, apply_delta)
}

fn apply_delta(graph: &mut UnifiedGraph, root: &Path, delta: &Delta) -> bool {
  if apply_delta_to_deps(&mut graph.deps, root, delta).is_err() {
    return false;
  }
  if let Some(calls) = graph.calls.as_mut() {
    apply_delta_to_calls(calls, &graph.deps, root, delta);
  }
  true
}

/// `patch` with the patch step passed in, so a test can edit a file while
/// the patch reads it. The fingerprints are taken first, for the reason
/// `build_with_records_by` gives.
fn patch_by(
  graph: &mut UnifiedGraph,
  prev: Vec<FileRecord>,
  root: &Path,
  delta: &Delta,
  apply: impl FnOnce(&mut UnifiedGraph, &Path, &Delta) -> bool,
) -> Option<Vec<FileRecord>> {
  let records = refresh_records(prev, root, delta);
  apply(graph, root, delta).then_some(records)
}

/// Build a fresh `UnifiedGraph` + fingerprints and persist them, returning
/// the graph with the file records it reflects. The shared registry holds
/// the records in memory so it can re-validate against the working tree
/// later without re-reading `graph.bin`.
pub fn load_or_build_with_records(
  root: &Path,
  force_rebuild: bool,
) -> std::io::Result<(UnifiedGraph, Vec<FileRecord>)> {
  if force_rebuild {
    return build_with_records(root);
  }
  match load_with_delta(root) {
    LoadOutcome::Fresh { graph, records } => Ok((graph, records)),
    LoadOutcome::Stale {
      mut graph,
      delta,
      prev_records,
    } => {
      // A failed patch (e.g. an extractor erroring) falls back to a full
      // rebuild, so the user's query still succeeds.
      let Some(new_records) = patch(&mut graph, prev_records, root, &delta) else {
        return build_with_records(root);
      };
      // Best-effort: a write failure leaves the in-memory state correct
      // for this session; the next launch re-detects the delta and patches.
      let _ = save(root, &graph, &new_records);
      Ok((graph, new_records))
    }
    LoadOutcome::Missing => build_with_records(root),
  }
}

/// Persist a graph + file fingerprints atomically. Writes via `.tmp` +
/// rename, holds an advisory exclusive lock during the write.
pub fn save(root: &Path, graph: &UnifiedGraph, files: &[FileRecord]) -> std::io::Result<()> {
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
    version: env!("CARGO_PKG_VERSION").to_string(),
    graph: graph.clone(),
    files: files.to_vec(),
  };
  let bytes = encode_to_vec(&cf, bincode::config::standard()).map_err(std::io::Error::other)?;

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

fn write_gitignore(dir: &Path) -> std::io::Result<()> {
  let p = dir.parent().map(|d| d.join(".gitignore"));
  if let Some(p) = p {
    if !p.exists() {
      fs::write(&p, "*\n")?;
    }
  }
  Ok(())
}

/// The files the graph depends on, each with its metadata when the graph
/// reads its contents, or with `None` when only its existence counts.
///
/// - Contents: every deps input (`deps_walk` + `is_deps_input`, the walk
///   and rule `build_suffix_index` uses) and the root manifests
///   `detect_aliases` reads by name, whatever the ignore files say.
/// - Existence: every other walked file. A relative import may name any
///   file, and the resolver checks that it exists (`./data.json`,
///   `./logo.svg`), so adding or deleting one can move an edge, while
///   editing one cannot.
///
/// The search corpus is a different set — it skips hidden files and files
/// over its size cap — so tracking it left changes the graph depends on
/// unseen (#123).
fn graph_inputs(root: &Path) -> Vec<(PathBuf, Option<fs::Metadata>)> {
  let mut out = Vec::new();
  let mut manifests_walked = HashSet::new();
  for entry in deps_walk(root).flatten() {
    if !entry.file_type().is_some_and(|t| t.is_file()) || should_skip_path(entry.path(), root) {
      continue;
    }
    if !tracks_contents(entry.path(), root) {
      out.push((entry.into_path(), None));
      continue;
    }
    let Ok(meta) = entry.metadata() else {
      continue;
    };
    if is_root_manifest(entry.path(), root) {
      manifests_walked.insert(entry.path().to_path_buf());
    }
    out.push((entry.into_path(), Some(meta)));
  }
  for name in ROOT_MANIFESTS {
    let path = root.join(name);
    if manifests_walked.contains(&path) {
      continue;
    }
    if let Some(meta) = fs::metadata(&path).ok().filter(|m| m.is_file()) {
      out.push((path, Some(meta)));
    }
  }
  out
}

/// Whether the graph reads the contents of `path`, not only whether it
/// exists: a deps input or a root manifest.
pub(crate) fn tracks_contents(path: &Path, root: &Path) -> bool {
  is_deps_input(path, root) || is_root_manifest(path, root)
}

fn is_root_manifest(path: &Path, root: &Path) -> bool {
  path.parent() == Some(root)
    && path
      .file_name()
      .and_then(|n| n.to_str())
      .is_some_and(|n| ROOT_MANIFESTS.contains(&n))
}

/// The delta of the files the graph depends on, against `cached`.
pub fn compute_graph_delta(root: &Path, cached: &[FileRecord]) -> Delta {
  delta_from_files(graph_inputs(root), root, cached)
}

/// The fingerprints of the files the graph depends on, now. A file whose
/// existence alone counts gets a placeholder record: its contents are never
/// compared, so it is not hashed.
pub fn collect_file_records(root: &Path) -> std::io::Result<Vec<FileRecord>> {
  let mut out = Vec::new();
  for (path, meta) in graph_inputs(root) {
    let rel = rel_posix(root, &path);
    let Some(meta) = meta else {
      out.push(existence_record(rel));
      continue;
    };
    out.push(FileRecord {
      path: rel,
      mtime_ns: mtime_nanos(&meta),
      size: meta.len(),
      content_hash: hash_file(&path).unwrap_or(0),
      chunk_start: 0,
      chunk_end: 0,
    });
  }
  Ok(out)
}

/// The record of a file whose existence alone counts.
pub(crate) fn existence_record(path: String) -> FileRecord {
  FileRecord {
    path,
    mtime_ns: 0,
    size: 0,
    content_hash: 0,
    chunk_start: 0,
    chunk_end: 0,
  }
}

/// `path` relative to `root`, with `/` separators, as records store it.
pub(crate) fn rel_posix(root: &Path, path: &Path) -> String {
  path
    .strip_prefix(root)
    .map(|r| {
      r.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
    })
    .unwrap_or_else(|_| path.display().to_string())
}

/// Nanoseconds since the epoch, as records store a modification time.
pub(crate) fn mtime_nanos(meta: &fs::Metadata) -> i128 {
  let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
  match mtime.duration_since(std::time::SystemTime::UNIX_EPOCH) {
    Ok(d) => d.as_nanos() as i128,
    Err(e) => -(e.duration().as_nanos() as i128),
  }
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

  fn cached_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("src/lib.rs"), "pub fn f() {}\n");
    load_or_build_with_records(tmp.path(), false).expect("build");
    assert!(matches!(
      load_with_delta(tmp.path()),
      LoadOutcome::Fresh { .. }
    ));
    tmp
  }

  /// A cache written by another version was extracted by other adapters:
  /// files that did not change since would keep that answer (#81).
  #[test]
  fn a_cache_from_another_version_is_rebuilt() {
    let tmp = cached_repo();
    let bytes = fs::read(cache_path(tmp.path())).unwrap();
    let (mut cf, _): (CacheFile, _) =
      decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    cf.version = "0.0.0".to_string();
    let bytes = encode_to_vec(&cf, bincode::config::standard()).unwrap();
    fs::write(cache_path(tmp.path()), bytes).unwrap();
    assert!(matches!(load_with_delta(tmp.path()), LoadOutcome::Missing));
  }

  /// A cache from before the version was stored does not decode, and is
  /// rebuilt, so adding the field needs no schema bump.
  #[test]
  fn a_cache_with_no_version_is_rebuilt() {
    #[derive(Serialize)]
    struct Unversioned {
      schema: String,
      graph: UnifiedGraph,
      files: Vec<FileRecord>,
    }
    let tmp = cached_repo();
    let bytes = fs::read(cache_path(tmp.path())).unwrap();
    let (cf, _): (CacheFile, _) = decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    let old = Unversioned {
      schema: cf.schema,
      graph: cf.graph,
      files: cf.files,
    };
    let bytes = encode_to_vec(&old, bincode::config::standard()).unwrap();
    fs::write(cache_path(tmp.path()), bytes).unwrap();
    assert!(matches!(load_with_delta(tmp.path()), LoadOutcome::Missing));
  }

  /// A binary from before the version field reads `schema` first and then
  /// `graph` where `version` now is. That misread usually fails, but bincode
  /// does not reject trailing bytes, so only the schema makes the rejection
  /// certain after a downgrade (#81).
  #[test]
  fn an_older_binary_rejects_this_cache_by_its_schema() {
    #[derive(Deserialize)]
    struct SchemaOnly {
      schema: String,
    }
    let tmp = cached_repo();
    let bytes = fs::read(cache_path(tmp.path())).unwrap();
    let (head, _): (SchemaOnly, _) =
      decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    assert_ne!(head.schema, "ast-bro.graph-index.v8");
  }

  /// A call edge stores the imports in scope at its call site (Elixir).
  /// bincode reads fields by position, so a field left out when empty
  /// makes the whole cache undecodable, and every query rebuilds it.
  #[test]
  fn a_call_graph_with_imports_decodes_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join("mix.exs"), "defmodule App.MixProject do\nend\n");
    write(
      &root.join("lib/m.ex"),
      "defmodule M do\n  def f(x), do: x\nend\n",
    );
    write(
      &root.join("lib/u.ex"),
      "defmodule U do\n  import M\n  def go(x), do: f(x)\nend\n",
    );
    shared::forget(root);
    shared::get_or_init(root).expect("cold get_or_init");
    shared::promote_calls(root, |g| {
      crate::calls::build::build_call_graph(root, &g.deps)
    })
    .expect("promote_calls");
    shared::forget(root);

    let bytes = fs::read(cache_path(root)).expect("read promoted cache");
    let (cf, _): (CacheFile, _) =
      decode_from_slice(&bytes, bincode::config::standard()).expect("decode promoted");
    let calls = cf.graph.calls.expect("promoted cache carries calls");
    assert!(
      calls
        .forward
        .values()
        .flatten()
        .any(|e| !e.imports.is_empty()),
      "the edge from `U.go` carries the import of `M`"
    );
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
    assert!(
      cf.graph.calls.is_none(),
      "on-disk cold cache should be calls: None"
    );

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

  /// The incremental update resolves an unchanged file from the imports
  /// the cache holds, without parsing it again (#93). The cached imports of
  /// `a.py` are changed to name `c` while the file still says `b`: only an
  /// update that decoded them and kept them can give `a.py → c.py`. A
  /// failed decode rebuilds, and a re-parse reads the file; both give
  /// `a.py → b.py`.
  #[test]
  fn an_incremental_update_resolves_an_unchanged_file_from_its_cached_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write(&root.join("a.py"), "import b\n");
    write(&root.join("b.py"), "X = 1\n");
    write(&root.join("c.py"), "Y = 1\n");
    load_or_build_with_records(&root, false).expect("build");

    let a = root.join("a.py");
    let bytes = fs::read(cache_path(&root)).unwrap();
    let (mut cf, _): (CacheFile, _) =
      decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    let imports = cf.graph.deps.imports.get_mut(&a).expect("a.py cached");
    assert_eq!(imports.len(), 1);
    imports[0].spec = "c".to_string();
    let bytes = encode_to_vec(&cf, bincode::config::standard()).unwrap();
    fs::write(cache_path(&root), bytes).unwrap();

    // A change elsewhere, so the load takes the incremental path.
    write(&root.join("b.py"), "X = 22\n");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
    let (graph, _) = load_or_build_with_records(&root, false).expect("update");
    let targets: Vec<&PathBuf> = graph.deps.forward[&a].iter().map(|e| &e.target).collect();
    assert_eq!(targets, vec![&root.join("c.py")]);
  }

  /// A tree with every kind of file the graph and the search corpus disagree
  /// on: a hidden source file, one over the search size cap, a `go.mod`,
  /// the root manifests, and Markdown the graph never reads.
  fn mixed_inputs() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write(&root.join("a.py"), "import b\n");
    write(&root.join("b.py"), "X = 1\n");
    write(&root.join(".cfg/h.py"), "import b\n");
    write(
      &root.join("big.py"),
      &format!("import b\n{}\n", "#".repeat(2_100_000)),
    );
    write(&root.join("svc/go.mod"), "module example.com/svc\n");
    write(&root.join("svc/main.go"), "package main\n");
    write(&root.join("Cargo.toml"), "[package]\nname = \"x\"\n");
    write(&root.join("tsconfig.json"), "{}\n");
    write(&root.join("composer.json"), "{}\n");
    write(&root.join("README.md"), "# Title\n");
    (tmp, root)
  }

  /// The records cover what the graph depends on (#123): the contents of
  /// every file the index reads, each `go.mod`, and the root manifests; and
  /// the existence of every other file, which a relative import may name.
  /// Only the first set is fingerprinted.
  #[test]
  fn records_cover_the_files_the_deps_graph_reads() {
    let (_tmp, root) = mixed_inputs();
    let records = collect_file_records(&root).unwrap();
    let mut contents: Vec<&str> = Vec::new();
    let mut existence: Vec<&str> = Vec::new();
    for r in &records {
      if *r == existence_record(r.path.clone()) {
        existence.push(&r.path);
      } else {
        contents.push(&r.path);
      }
    }
    contents.sort();
    let idx = crate::deps::resolver::build_suffix_index(&root);
    let mut want: Vec<String> = idx
      .by_file
      .keys()
      .map(|p| {
        p.strip_prefix(&root)
          .unwrap()
          .to_string_lossy()
          .replace('\\', "/")
      })
      .chain(["svc/go.mod", "Cargo.toml", "tsconfig.json", "composer.json"].map(String::from))
      .collect();
    want.sort();
    assert_eq!(contents, want);
    assert_eq!(existence, vec!["README.md"]);
  }

  /// An edit to a file whose existence alone counts is no change; adding
  /// or deleting one is.
  #[test]
  fn only_the_existence_of_a_non_input_counts() {
    let (_tmp, root) = mixed_inputs();
    load_or_build_with_records(&root, false).expect("build");
    write(&root.join("README.md"), "# Another title, longer\n");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Fresh { .. }));
    fs::remove_file(root.join("README.md")).unwrap();
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
  }

  /// The walk that writes the records and the walk that checks them are
  /// the same one: a build, and then an update, both load as fresh. A
  /// mismatch would show every load as a delta.
  #[test]
  fn the_next_load_is_fresh_after_a_build_and_after_an_update() {
    let (_tmp, root) = mixed_inputs();
    load_or_build_with_records(&root, false).expect("build");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Fresh { .. }));
    write(&root.join(".cfg/h.py"), "import a\n");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
    load_or_build_with_records(&root, false).expect("update");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Fresh { .. }));
    // A file whose existence alone counts, added and then deleted: each is
    // one update, and the update's records hold it.
    write(&root.join("notes.txt"), "later\n");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
    load_or_build_with_records(&root, false).expect("update after an add");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Fresh { .. }));
    fs::remove_file(root.join("notes.txt")).unwrap();
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
    load_or_build_with_records(&root, false).expect("update after a delete");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Fresh { .. }));
  }

  /// A tree where `a.py` imports `b`, for the race tests.
  fn race_tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write(&root.join("a.py"), "import b\n");
    write(&root.join("b.py"), "X = 1\n");
    write(&root.join("c.py"), "Y = 1\n");
    (tmp, root)
  }

  /// An editor saves `a.py` after the build read it: the graph holds the
  /// old import, so the next load must not call it fresh. The fingerprints
  /// are taken before the build for this.
  #[test]
  fn an_edit_during_a_build_is_seen_by_the_next_load() {
    let (_tmp, root) = race_tree();
    build_with_records_by(&root, |r| {
      let deps = build_graph(r);
      write(&r.join("a.py"), "import c\nimport b\n");
      deps
    })
    .expect("build");
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
  }

  fn set_mtime(p: &Path, secs: u64) {
    let file = fs::File::options().write(true).open(p).unwrap();
    file
      .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
      .unwrap();
  }

  /// A file touched with no change is not parsed again. An editor saves it
  /// after the load hashed it and before the patch: its record must still
  /// describe what the load hashed, so the next load sees the edit.
  #[test]
  fn an_edit_to_a_touched_file_before_the_patch_is_seen_by_the_next_load() {
    let (_tmp, root) = race_tree();
    load_or_build_with_records(&root, false).expect("build");
    set_mtime(&root.join("a.py"), 1_000_000);
    write(&root.join("c.py"), "Y = 2\n");
    let LoadOutcome::Stale {
      mut graph,
      delta,
      prev_records,
    } = load_with_delta(&root)
    else {
      panic!("the edit to c.py is a delta");
    };
    assert_eq!(delta.mtime_only.len(), 1, "premise: a.py is touched only");
    write(&root.join("a.py"), "import c\n");
    set_mtime(&root.join("a.py"), 2_000_000);
    let records = patch(&mut graph, prev_records, &root, &delta).expect("patch");
    save(&root, &graph, &records).unwrap();
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
  }

  /// The same during a patch: the edit after the patch read the file must
  /// stay visible to the next load.
  #[test]
  fn an_edit_during_a_patch_is_seen_by_the_next_load() {
    let (_tmp, root) = race_tree();
    load_or_build_with_records(&root, false).expect("build");
    write(&root.join("a.py"), "import c\n");
    let LoadOutcome::Stale {
      mut graph,
      delta,
      prev_records,
    } = load_with_delta(&root)
    else {
      panic!("the edit is a delta");
    };
    let records = patch_by(&mut graph, prev_records, &root, &delta, |g, r, d| {
      let ok = apply_delta(g, r, d);
      write(&r.join("a.py"), "import b\nimport c\n");
      ok
    })
    .expect("patch");
    save(&root, &graph, &records).unwrap();
    assert!(matches!(load_with_delta(&root), LoadOutcome::Stale { .. }));
  }
}
