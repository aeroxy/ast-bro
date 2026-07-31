//! MCP-side handlers for `search`, `find_related`, `index`. Called from
//! `src/mcp/tools.rs::call`. Mirrors `src/search/cli.rs` in behaviour but
//! takes `serde_json::Value` args and returns `CallResult` instead of an
//! exit code.
//!
//! When `json: true` is passed, the tool returns the same JSON document the
//! CLI emits — the schema constants in `format.rs` are shared between the
//! two surfaces. When `json: false` (the default), the human-readable text
//! form is returned, mirroring how the existing MCP tools (outline, digest,
//! show, implements) behave.
//!
//! ## CWD semantics
//!
//! MCP servers don't share a meaningful CWD with the caller — the daemon's
//! `current_dir()` reflects where the server was launched, not where the
//! agent is working. To keep the index-location resolver predictable, we
//! pass `cwd = path` (the caller's `path` argument) so the walk-up cap
//! equals the start point: no walk-up, the path is treated as
//! authoritative. Agents that want to share an index across calls must
//! pass an absolute project path consistently.

use crate::mcp::tools::CallResult;
use crate::project_root::relative_posix;
use crate::search::format::{
    render_index_stats_json, render_index_stats_text, render_related_json, render_related_text,
    render_search_json, render_search_text,
};
use crate::search::fusion::resolve_alpha;
use crate::search::index::{Index, SearchOptions};
use crate::search::shared;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Default)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_path")]
    path: PathBuf,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default)]
    alpha: Option<f32>,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    json: bool,
}

#[derive(Deserialize, Default)]
struct FindRelatedArgs {
    path: String,
    line: u32,
    #[serde(default = "default_path")]
    root: PathBuf,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default)]
    json: bool,
}

#[derive(Deserialize, Default)]
struct IndexArgs {
    #[serde(default = "default_path")]
    path: PathBuf,
    #[serde(default)]
    rebuild: bool,
    #[serde(default)]
    stats: bool,
    #[serde(default)]
    json: bool,
}

fn default_path() -> PathBuf {
    PathBuf::from(".")
}

fn default_top_k() -> usize {
    10
}

pub fn run_search(args: Value) -> CallResult {
    let args: SearchArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return CallResult::Error(format!("invalid args: {e}")),
    };
    if args.query.trim().is_empty() {
        return CallResult::Error("query is required".to_string());
    }
    // MCP: cwd = path (no walk-up, path is authoritative). Reuse a cached
    // in-memory index across calls when the tree is unchanged.
    let index = match shared::open_shared(&args.path, &args.path) {
        Ok(i) => i,
        Err(e) => return CallResult::Error(format!("failed to open index: {e}")),
    };
    let scope = relative_posix(&args.path, &index.paths.root);
    // Peel field-qualified filters (lang:/path:/name:) out of the query; lang:
    // unions with the `languages` arg.
    let parsed = crate::search::query::parse_query(&args.query);
    let mut langs = args.languages;
    langs.extend(parsed.languages);
    let opts = SearchOptions {
        top_k: args.top_k,
        alpha: args.alpha,
        languages: if langs.is_empty() { None } else { Some(langs) },
        query_scope: scope,
        path_contains: parsed.paths,
        name_contains: parsed.names,
    };
    let hits = index.search(&parsed.text, &opts);
    let out = if args.json {
        let alpha_used = resolve_alpha(&parsed.text, args.alpha);
        // MCP clients typically parse JSON, so default to compact form.
        render_search_json(&args.query, alpha_used, &hits, /* pretty */ false)
    } else {
        render_search_text(&args.query, &hits)
    };
    CallResult::Text(out)
}

pub fn run_find_related(args: Value) -> CallResult {
    let args: FindRelatedArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return CallResult::Error(format!("invalid args: {e}")),
    };
    if args.path.is_empty() || args.line == 0 {
        return CallResult::Error("path and line (1-indexed) are required".to_string());
    }
    // Check the source location exists on disk *before* opening the index,
    // mirroring the CLI: `open_shared` falls back to a full build when there
    // is no cache, which downloads the embedding model and chunks the whole
    // repository. A typo'd path shouldn't pay for that just to be told it
    // was a typo. Validate against the same home `Index::open` will resolve.
    let as_typed = Path::new(&args.path);
    let (home, _found) = crate::project_root::resolve_home(
        &args.root,
        &args.root,
        crate::project_root::Marker::SearchIndex,
    );
    if !as_typed.is_file() && !home.join(as_typed).is_file() {
        // A directory is as unindexable as a typo — reject both before the
        // index opens (which would otherwise build, model download and all).
        let what = if as_typed.exists() || home.join(as_typed).exists() {
            "not a file"
        } else {
            "no such file"
        };
        return CallResult::Error(format!(
            "{}: {} — `find_related` takes a file that exists; its chunk is then looked up in the index by path",
            what, args.path
        ));
    }
    let index = match shared::open_shared(&args.root, &args.root) {
        Ok(i) => i,
        Err(e) => return CallResult::Error(format!("failed to open index: {e}")),
    };
    let key = normalize_chunk_key(&args.path, &index.paths.root, &args.root);
    let hits = match index.find_related(&key, args.line, args.top_k) {
        Some(h) => h,
        None => {
            // The location could not be found, so the query did not run —
            // a `results: []` envelope would say "nothing is similar to
            // this chunk", a different and wrong claim (#33/#36). Same
            // rejection in both output modes, matching the CLI contract.
            return CallResult::Error(format!(
                "no indexed chunk at {}:{} (was the file indexed?)",
                args.path, args.line
            ));
        }
    };
    let out = if args.json {
        render_related_json(&args.path, args.line, &hits, false)
    } else {
        render_related_text(&args.path, args.line, &hits)
    };
    CallResult::Text(out)
}

pub fn run_index(args: Value) -> CallResult {
    let args: IndexArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return CallResult::Error(format!("invalid args: {e}")),
    };
    // Rebuild always hits disk; otherwise reuse the shared registry. Either
    // way we (re)store the result so a later `search` / `find_related` in this
    // session sees the fresh index instead of a stale cached `Arc`.
    let index = if args.rebuild {
        match Index::build(&args.path, &args.path) {
            Ok(i) => {
                let arc = std::sync::Arc::new(i);
                shared::store(arc.clone());
                arc
            }
            Err(e) => return CallResult::Error(format!("index build failed: {e}")),
        }
    } else {
        match shared::open_shared(&args.path, &args.path) {
            Ok(i) => i,
            Err(e) => return CallResult::Error(format!("index build failed: {e}")),
        }
    };
    if args.stats || args.json {
        let file_count = index.file_count();
        let out = if args.json {
            render_index_stats_json(&index.meta, file_count, &index.paths.root, false)
        } else {
            render_index_stats_text(&index.meta, file_count, &index.paths.root)
        };
        return CallResult::Text(out);
    }
    CallResult::Text(format!(
        "Indexed {} chunk(s) at {}\n",
        index.chunk_count(),
        index.paths.root.display()
    ))
}

fn normalize_chunk_key(input: &str, home: &Path, fallback: &Path) -> String {
    let p = Path::new(input);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        fallback.join(p)
    };
    if let Some(rel) = relative_posix(&abs, home) {
        return rel;
    }
    input.to_string()
}
