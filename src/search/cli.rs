//! Clap-side handlers for `search`, `find-related`, `index`. Called from
//! `main.rs` after the `Commands` enum has been parsed.
//!
//! Keeps `main.rs` thin: each subcommand is a one-line dispatch into here.

use crate::project_root::{compare_corpus, relative_posix, resolve_home, CorpusRel, Marker};
use crate::search::format::{
    render_index_stats_json, render_index_stats_text, render_related_json, render_related_text,
    render_search_json, render_search_text,
};
use crate::search::fusion::resolve_alpha;
use crate::search::index::{Index, SearchOptions};
use std::path::{Path, PathBuf};

/// Run `ast-bro search <QUERY> [PATH]`. Returns process exit code.
pub fn run_search(
    query: &str,
    path: &PathBuf,
    top_k: usize,
    alpha: Option<f32>,
    languages: Vec<String>,
    json: bool,
    pretty: bool,
) -> i32 {
    let cwd = current_dir_or_dot();
    let index = match Index::open(path, &cwd) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("ast-bro: failed to open index: {e}");
            return 1;
        }
    };
    let scope = derive_query_scope(path, &index.paths.root);
    let opts = SearchOptions {
        top_k,
        alpha,
        languages: if languages.is_empty() { None } else { Some(languages) },
        query_scope: scope,
    };
    let hits = index.search(query, &opts);
    if json {
        let alpha_used = resolve_alpha(query, alpha);
        println!("{}", render_search_json(query, alpha_used, &hits, pretty));
    } else {
        print!("{}", render_search_text(query, &hits));
    }
    0
}

/// Run `ast-bro find-related <FILE>:<LINE> [PATH]`.
pub fn run_find_related(
    file_path: &str,
    line: u32,
    path: &PathBuf,
    top_k: usize,
    json: bool,
    pretty: bool,
) -> i32 {
    let cwd = current_dir_or_dot();
    let index = match Index::open(path, &cwd) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("ast-bro: failed to open index: {e}");
            return 1;
        }
    };
    // Normalize the file path against the index home so it matches stored
    // chunk paths (which are home-relative).
    let key = normalize_chunk_key(file_path, &index.paths.root, &cwd);
    let hits = match index.find_related(&key, line, top_k) {
        Some(h) => h,
        None => {
            eprintln!(
                "ast-bro: no chunk at {file_path}:{line} (was the file indexed?)"
            );
            return 2;
        }
    };
    if json {
        println!("{}", render_related_json(file_path, line, &hits, pretty));
    } else {
        print!("{}", render_related_text(file_path, line, &hits));
    }
    0
}

/// Run `ast-bro index [PATH]`. With `--rebuild`, drops any existing
/// cache and rebuilds from scratch. With `--stats`, just prints stats and
/// exits 0 if an index exists, 2 if not.
pub fn run_index(path: &PathBuf, rebuild: bool, stats: bool, json: bool, pretty: bool) -> i32 {
    let cwd = current_dir_or_dot();

    if stats {
        let index = match Index::open(path, &cwd) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("ast-bro: no readable index at {}: {e}", path.display());
                return 2;
            }
        };
        let file_count = std::fs::read(&index.paths.files_bin)
            .ok()
            .and_then(|b| {
                let cfg = bincode::config::standard();
                bincode::serde::decode_from_slice::<Vec<crate::search::cache::FileRecord>, _>(
                    &b, cfg,
                )
                .ok()
                .map(|(v, _)| v.len())
            })
            .unwrap_or(0);
        if json {
            println!(
                "{}",
                render_index_stats_json(&index.meta, file_count, &index.paths.root, pretty)
            );
        } else {
            print!(
                "{}",
                render_index_stats_text(&index.meta, file_count, &index.paths.root)
            );
        }
        return 0;
    }

    // Resolve home + requested corpus before deciding what to do.
    let (home, _) = resolve_home(path, &cwd, Marker::SearchIndex);
    let requested_corpus = relative_posix(path, &home).unwrap_or_default();

    if rebuild {
        // Explicit user override: build at the requested corpus, no
        // reconciliation. Print the action so users know what happened.
        eprintln!(
            "ast-bro: forced rebuild — corpus = {}",
            display_corpus(&requested_corpus)
        );
        return match Index::build_with_corpus(path, &cwd, &requested_corpus) {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("ast-bro: index build failed: {e}");
                1
            }
        };
    }

    // Look at any recorded corpus and reconcile against the request.
    let recorded = peek_recorded_corpus(&home);
    let result = match recorded {
        Some(recorded_corpus) => {
            match compare_corpus(&recorded_corpus, &requested_corpus) {
                CorpusRel::Subset => {
                    // Already covered — refresh stale chunks against the
                    // recorded (wider-or-equal) corpus.
                    eprintln!(
                        "ast-bro: corpus already covers {} (recorded: {}); refreshing",
                        display_corpus(&requested_corpus),
                        display_corpus(&recorded_corpus)
                    );
                    Index::open(path, &cwd)
                }
                CorpusRel::Superset => {
                    eprintln!(
                        "ast-bro: widening corpus from {} to {}",
                        display_corpus(&recorded_corpus),
                        display_corpus(&requested_corpus)
                    );
                    Index::build_with_corpus(path, &cwd, &requested_corpus)
                }
                CorpusRel::Sibling { common } => {
                    eprintln!(
                        "ast-bro: widening corpus from {} to {} (common ancestor of {} and {})",
                        display_corpus(&recorded_corpus),
                        display_corpus(&common),
                        display_corpus(&recorded_corpus),
                        display_corpus(&requested_corpus)
                    );
                    Index::build_with_corpus(path, &cwd, &common)
                }
            }
        }
        None => {
            // Fresh index at this home.
            Index::build_with_corpus(path, &cwd, &requested_corpus)
        }
    };

    match result {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("ast-bro: index build failed: {e}");
            1
        }
    }
}

/// Read just the `indexed_corpus` field from an existing meta.json at
/// `home/.ast-bro/index/meta.json`. Returns `None` if no readable meta
/// exists yet.
fn peek_recorded_corpus(home: &Path) -> Option<String> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct PeekMeta {
        #[serde(default)]
        indexed_corpus: String,
    }
    let meta_path = home
        .join(".ast-bro")
        .join("index")
        .join("meta.json");
    let bytes = std::fs::read(&meta_path).ok()?;
    let m: PeekMeta = serde_json::from_slice(&bytes).ok()?;
    Some(m.indexed_corpus)
}

/// Compute the `query_scope` for a CLI invocation: the path arg expressed
/// relative to the resolved index home, normalised to POSIX. Returns
/// `Some("")` (no filter) when path == home, `Some("packages/x")` when path
/// is a subdir, and `None` when path doesn't fall under home.
fn derive_query_scope(path_arg: &Path, home: &Path) -> Option<String> {
    relative_posix(path_arg, home).map(|s| s)
}

/// Best-effort canonicalize for find-related's chunk lookup.
/// User input may be relative-to-cwd or absolute; we want it relative to home.
fn normalize_chunk_key(input: &str, home: &Path, cwd: &Path) -> String {
    let p = Path::new(input);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    if let Some(rel) = relative_posix(&abs, home) {
        return rel;
    }
    input.to_string()
}

fn current_dir_or_dot() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn display_corpus(corpus: &str) -> String {
    if corpus.is_empty() {
        "(whole repo)".to_string()
    } else {
        corpus.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn peek_recorded_corpus_reads_current_meta() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let idx = home.join(".ast-bro").join("index");
        fs::create_dir_all(&idx).unwrap();
        fs::write(
            idx.join("meta.json"),
            r#"{ "schema": "ast-bro.search-index.v1",
                 "ast_outline_version": "0.0.0",
                 "model": { "id": "m", "dim": 256 },
                 "created_unix": 0,
                 "chunk_count": 0,
                 "embedding_dtype": "f32_le",
                 "tombstones": [],
                 "indexed_corpus": "packages" }"#,
        )
        .unwrap();
        assert_eq!(
            peek_recorded_corpus(home).as_deref(),
            Some("packages")
        );
    }

    #[test]
    fn peek_recorded_corpus_treats_legacy_v1_as_empty() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let idx = home.join(".ast-bro").join("index");
        fs::create_dir_all(&idx).unwrap();
        fs::write(
            idx.join("meta.json"),
            r#"{ "schema": "ast-outline.search-index.v1",
                 "ast_outline_version": "0.0.0",
                 "model": { "id": "m", "dim": 256 },
                 "created_unix": 0,
                 "chunk_count": 0,
                 "embedding_dtype": "f32_le",
                 "tombstones": [] }"#,
        )
        .unwrap();
        // Legacy v1 lacks the field → serde(default) → empty string.
        assert_eq!(peek_recorded_corpus(home).as_deref(), Some(""));
    }

    #[test]
    fn peek_recorded_corpus_returns_none_when_missing() {
        let dir = tempdir().unwrap();
        assert!(peek_recorded_corpus(dir.path()).is_none());
    }

    #[test]
    fn normalize_chunk_key_relative_to_home() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("src")).unwrap();
        let cwd = home;
        let key = normalize_chunk_key("src/foo.rs", home, cwd);
        assert_eq!(key, "src/foo.rs");
    }

    #[test]
    fn normalize_chunk_key_absolute() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("src")).unwrap();
        let abs = home.join("src").join("foo.rs");
        let key = normalize_chunk_key(abs.to_str().unwrap(), home, home);
        assert_eq!(key, "src/foo.rs");
    }
}
