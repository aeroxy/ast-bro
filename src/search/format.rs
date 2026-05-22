//! Text + JSON renderers for the search subcommands.
//!
//! Mirrors the convention in `src/core.rs:render_*` — every subcommand has a
//! human-friendly text form (the default) and a JSON form gated by `--json`.
//! The CLI and MCP tool handlers both call into here so output is consistent
//! between the two surfaces.

use crate::search::index::{Meta, SearchHit};
use colored::Colorize;
use serde_json::{json, Value};
use std::path::Path;

pub const JSON_SCHEMA_SEARCH: &str = "ast-bro.search.v1";
pub const JSON_SCHEMA_RELATED: &str = "ast-bro.related.v1";
pub const JSON_SCHEMA_INDEX: &str = "ast-bro.index-stats.v1";

/// Text rendering of search hits. One block per hit, separated by blank lines.
pub fn render_search_text(query: &str, hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return format!("No results for '{}'.\n", query.yellow());
    }
    let mut out = String::new();
    out.push_str(&format!(
        "# {} result(s) for '{}'\n",
        hits.len().to_string().bold(),
        query.yellow(),
    ));
    for hit in hits {
        out.push('\n');
        out.push_str(&render_hit_header(hit));
        out.push_str(&render_hit_body(hit));
    }
    out
}

pub fn render_search_json(query: &str, alpha: f32, hits: &[SearchHit], pretty: bool) -> String {
    let v = json!({
        "schema": JSON_SCHEMA_SEARCH,
        "query": query,
        "alpha": alpha,
        "results": hits.iter().map(hit_to_json).collect::<Vec<_>>(),
    });
    to_json(&v, pretty)
}

pub fn render_related_text(file_path: &str, line: u32, hits: &[SearchHit]) -> String {
    let source_label = format!("{}:{}", file_path, line);
    if hits.is_empty() {
        return format!("No related chunks for {}.\n", source_label.cyan().bold());
    }
    let mut out = format!(
        "# {} related chunk(s) for {}\n",
        hits.len().to_string().bold(),
        source_label.cyan().bold(),
    );
    for hit in hits {
        out.push('\n');
        out.push_str(&render_hit_header(hit));
        out.push_str(&render_hit_body(hit));
    }
    out
}

/// Shared header for one hit: `<cyan-bold path>:<lines>  [score X.XXX]` (score dimmed).
fn render_hit_header(hit: &SearchHit) -> String {
    let location = format!(
        "{}:{}-{}",
        hit.chunk.file_path, hit.chunk.start_line, hit.chunk.end_line
    );
    let score = format!("[score {:.3}]", hit.score);
    format!("{}  {}\n", location.cyan().bold(), score.dimmed())
}

/// Shared body: chunk content indented by 4 spaces. No syntax colouring —
/// agents and humans both want the bytes verbatim.
fn render_hit_body(hit: &SearchHit) -> String {
    let mut out = String::with_capacity(hit.chunk.content.len() + 64);
    for line in hit.chunk.content.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub fn render_related_json(
    file_path: &str,
    line: u32,
    hits: &[SearchHit],
    pretty: bool,
) -> String {
    let v = json!({
        "schema": JSON_SCHEMA_RELATED,
        "source": { "path": file_path, "line": line },
        "results": hits.iter().map(hit_to_json).collect::<Vec<_>>(),
    });
    to_json(&v, pretty)
}

pub fn render_index_stats_text(meta: &Meta, file_count: usize, home: &Path) -> String {
    let label = |s: &str| format!("{:<8}", s).bold().to_string();
    let dim = |s: String| s.dimmed().to_string();
    let corpus_display = if meta.indexed_corpus.is_empty() {
        "(whole repo)".to_string()
    } else {
        meta.indexed_corpus.clone()
    };
    let total = meta.chunk_count as usize;
    let tombs = meta.tombstones.len();
    let live = total.saturating_sub(tombs);
    let chunks_line = if tombs == 0 {
        meta.chunk_count.to_string().green().bold().to_string()
    } else {
        format!(
            "{} {}",
            live.to_string().green().bold(),
            dim(format!("({} live · {} tombstoned)", live, tombs)),
        )
    };
    format!(
        "# Index stats\n\
         {schema_l} {schema}\n\
         {home_l} {home}\n\
         {corpus_l} {corpus}\n\
         {model_l} {model_id} {dim_suffix}\n\
         {chunks_l} {chunks}\n\
         {files_l} {files}\n\
         {built_l} {created} {unix_tag}\n",
        schema_l = label("Schema:"),
        home_l = label("Home:"),
        corpus_l = label("Corpus:"),
        model_l = label("Model:"),
        chunks_l = label("Chunks:"),
        files_l = label("Files:"),
        built_l = label("Built:"),
        schema = meta.schema,
        home = home.display().to_string().cyan(),
        corpus = corpus_display.cyan(),
        model_id = meta.model.id.cyan(),
        dim_suffix = dim(format!("(dim {})", meta.model.dim)),
        chunks = chunks_line,
        files = file_count.to_string().green().bold(),
        created = meta.created_unix,
        unix_tag = dim("(UNIX)".to_string()),
    )
}

pub fn render_index_stats_json(
    meta: &Meta,
    file_count: usize,
    home: &Path,
    pretty: bool,
) -> String {
    let total = meta.chunk_count as usize;
    let tombs = meta.tombstones.len();
    let live = total.saturating_sub(tombs);
    let v = json!({
        "schema": JSON_SCHEMA_INDEX,
        "ast_bro_version": meta.ast_bro_version,
        "home": home.display().to_string(),
        "indexed_corpus": meta.indexed_corpus,
        "model_id": meta.model.id,
        "dim": meta.model.dim,
        "chunk_count": meta.chunk_count,
        "live_chunk_count": live,
        "tombstone_count": tombs,
        "file_count": file_count,
        "created_unix": meta.created_unix,
    });
    to_json(&v, pretty)
}

fn hit_to_json(hit: &SearchHit) -> Value {
    json!({
        "path": hit.chunk.file_path,
        "start_line": hit.chunk.start_line,
        "end_line": hit.chunk.end_line,
        "language": hit.chunk.language,
        "score": hit.score,
        "content": hit.chunk.content,
    })
}

fn to_json(v: &Value, pretty: bool) -> String {
    if pretty {
        serde_json::to_string_pretty(v).unwrap_or_else(|e| format!("<json error: {e}>"))
    } else {
        serde_json::to_string(v).unwrap_or_else(|e| format!("<json error: {e}>"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::chunker::Chunk;
    use crate::search::index::ModelMeta;

    fn hit(path: &str, score: f32, content: &str) -> SearchHit {
        SearchHit {
            chunk: Chunk {
                content: content.to_string(),
                file_path: path.to_string(),
                start_line: 1,
                end_line: 5,
                start_byte: 0,
                end_byte: content.len() as u32,
                language: "rust".to_string(),
            },
            score,
        }
    }

    #[test]
    fn search_text_empty() {
        let out = render_search_text("foo", &[]);
        assert!(out.contains("No results for"));
        assert!(out.contains("foo"));
    }

    #[test]
    fn search_text_includes_path_score_and_content() {
        let h = hit("src/foo.rs", 0.842, "fn x() {}\nfn y() {}");
        let out = render_search_text("foo", &[h]);
        assert!(out.contains("src/foo.rs:1-5"));
        assert!(out.contains("[score 0.842]"));
        assert!(out.contains("fn x()"));
        // Content lines are indented.
        assert!(out.contains("    fn x() {}"));
    }

    #[test]
    fn search_json_has_schema_and_results() {
        let h = hit("a.rs", 0.5, "x");
        let out = render_search_json("q", 0.3, &[h], true);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["schema"], JSON_SCHEMA_SEARCH);
        assert_eq!(v["query"], "q");
        assert!((v["alpha"].as_f64().unwrap() - 0.3).abs() < 1e-6);
        assert_eq!(v["results"].as_array().unwrap().len(), 1);
        assert_eq!(v["results"][0]["path"], "a.rs");
    }

    #[test]
    fn related_text_and_json() {
        let h = hit("b.rs", 0.7, "y");
        let txt = render_related_text("a.rs", 12, &[h.clone()]);
        assert!(txt.contains("a.rs:12"));
        let v: Value = serde_json::from_str(&render_related_json("a.rs", 12, &[h], true)).unwrap();
        assert_eq!(v["schema"], JSON_SCHEMA_RELATED);
        assert_eq!(v["source"]["path"], "a.rs");
        assert_eq!(v["source"]["line"], 12);
    }

    #[test]
    fn index_stats_json() {
        let meta = Meta {
            schema: "ast-bro.search-index.v1".to_string(),
            ast_bro_version: "0.0.0".to_string(),
            model: ModelMeta { id: "m".into(), dim: 256 },
            created_unix: 42,
            chunk_count: 7,
            embedding_dtype: "f32_le".to_string(),
            tombstones: vec![],
            indexed_corpus: "packages".to_string(),
        };
        let home = Path::new("/r");
        let v: Value =
            serde_json::from_str(&render_index_stats_json(&meta, 3, home, true)).unwrap();
        assert_eq!(v["schema"], JSON_SCHEMA_INDEX);
        assert_eq!(v["chunk_count"], 7);
        assert_eq!(v["file_count"], 3);
        assert_eq!(v["created_unix"], 42);
        assert_eq!(v["home"], "/r");
        assert_eq!(v["indexed_corpus"], "packages");
    }

    #[test]
    fn index_stats_text_includes_home_and_corpus() {
        let meta = Meta {
            schema: "ast-bro.search-index.v1".to_string(),
            ast_bro_version: "0.0.0".to_string(),
            model: ModelMeta { id: "m".into(), dim: 256 },
            created_unix: 0,
            chunk_count: 0,
            embedding_dtype: "f32_le".to_string(),
            tombstones: vec![],
            indexed_corpus: String::new(),
        };
        let out = render_index_stats_text(&meta, 0, Path::new("/r"));
        assert!(out.contains("Home:"));
        assert!(out.contains("/r"));
        assert!(out.contains("Corpus:"));
        assert!(out.contains("(whole repo)"));
    }
}
