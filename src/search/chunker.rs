//! AST-aware code chunking for indexing.
//!
//! Two structural-aware strategies, picked by file extension:
//!
//! - **Anything ast-grep can parse** (bash, cpp, css, c#, dart, elixir, go,
//!   haskell, hcl, html, java, json, kotlin, lua, nix, php, python, ruby, rust,
//!   scala, solidity, swift, ts/tsx/js, yaml) — split at top-level declaration
//!   boundaries via ast-grep. The chunker doesn't need a per-language outline
//!   adapter; it only needs the AST root + iteration of named children.
//! - **Markdown** (`.md`/`.markdown`/`.mdx`/`.mdown`) — split at `section`
//!   boundaries via raw `tree_sitter_md`.
//!
//! Both paths feed the same greedy packer that targets `MAX_CHARS = 1500` and
//! never splits mid-declaration. Files outside both sets are not chunked —
//! `is_indexable` is the single source of truth for what gets indexed.

use ast_grep_core::{AstGrep, Language};
use ast_grep_language::{LanguageExt, SupportLang};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Target chunk size in characters. Chunks may exceed this when a single
/// top-level declaration is larger; they are not split mid-declaration.
pub const MAX_CHARS: usize = 1500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    /// Repo-relative POSIX path (caller's responsibility — the chunker only
    /// passes through whatever `file_path` it receives).
    pub file_path: String,
    /// 1-indexed inclusive line range.
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    /// Always set — language is known per `is_indexable`.
    pub language: String,
}

/// Strategy used to chunk a given file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkerKind {
    AstGrep(SupportLang),
    Markdown,
}

impl ChunkerKind {
    /// Lowercase canonical name. Uses the `SupportLang` Debug variant for
    /// ast-grep languages so we automatically pick up new languages without
    /// having to maintain a per-variant match.
    fn language_name(self) -> String {
        match self {
            ChunkerKind::AstGrep(lang) => format!("{lang:?}").to_ascii_lowercase(),
            ChunkerKind::Markdown => "markdown".to_string(),
        }
    }
}

/// Decide whether `path` is indexable, and how.
///
/// Returns `Some(Markdown)` for `.md`/`.markdown`/`.mdx`/`.mdown` (handled via
/// `tree_sitter_md`), `Some(AstGrep(lang))` for any extension ast-grep claims
/// it can parse, and `None` otherwise.
pub fn is_indexable(path: &Path) -> Option<ChunkerKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(ext.as_deref(), Some("md" | "markdown" | "mdx" | "mdown")) {
        return Some(ChunkerKind::Markdown);
    }
    SupportLang::from_path(path).map(ChunkerKind::AstGrep)
}

/// Read `path` from disk and chunk it. Returns an empty vec on read errors or
/// for unsupported file types (callers should normally pre-filter via
/// `is_indexable`).
pub fn chunk_file(path: &Path, file_path: &str) -> Vec<Chunk> {
    let Some(kind) = is_indexable(path) else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    chunk_source(&source, file_path, kind)
}

/// Chunk pre-read source text using the given `kind`.
pub fn chunk_source(source: &str, file_path: &str, kind: ChunkerKind) -> Vec<Chunk> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let split_points = match kind {
        ChunkerKind::AstGrep(lang) => ast_grep_split_points(source, lang),
        ChunkerKind::Markdown => markdown_split_points(source),
    };
    let lang_name = kind.language_name();
    pack(source, file_path, &lang_name, &split_points)
}

/// Top-level named-child end-bytes for an ast-grep parse.
fn ast_grep_split_points(source: &str, lang: SupportLang) -> Vec<usize> {
    let ast: AstGrep<_> = lang.ast_grep(source);
    let root = ast.root();

    let mut points: Vec<usize> = vec![0];
    for child in root.children() {
        if !child.is_named() {
            continue;
        }
        let end = child.range().end;
        if end > *points.last().unwrap() && end <= source.len() {
            points.push(end);
        }
    }
    if *points.last().unwrap() < source.len() {
        points.push(source.len());
    }
    points
}

/// Top-level `section` and `fenced_code_block` end-bytes for a markdown parse.
fn markdown_split_points(source: &str) -> Vec<usize> {
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .is_err()
    {
        // tree-sitter-md is bundled and version-pinned, so this shouldn't
        // happen — but if it does, emit one chunk covering the whole file
        // rather than dropping it.
        return vec![0, source.len()];
    }
    let Some(tree) = parser.parse(source, None) else {
        return vec![0, source.len()];
    };
    let root = tree.root_node();

    let mut points: Vec<usize> = vec![0];
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if matches!(child.kind(), "section" | "fenced_code_block") {
            let end = child.end_byte();
            if end > *points.last().unwrap() && end <= source.len() {
                points.push(end);
            }
        }
    }
    if *points.last().unwrap() < source.len() {
        points.push(source.len());
    }
    points
}

/// Greedy-pack regions defined by `split_points` into chunks ≤ `MAX_CHARS`.
///
/// All source bytes are covered exactly once: gaps between split points attach
/// to the *following* chunk so nothing is lost from the index. Single regions
/// bigger than the target are emitted on their own (no mid-decl splits).
fn pack(source: &str, file_path: &str, lang_name: &str, split_points: &[usize]) -> Vec<Chunk> {
    if split_points.len() < 2 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut cur_start = split_points[0];
    let mut cur_end = cur_start;

    for w in split_points.windows(2) {
        let region_size = w[1] - w[0];
        let cur_size = cur_end - cur_start;
        if cur_size == 0 {
            cur_start = w[0];
            cur_end = w[1];
        } else if cur_size + region_size <= MAX_CHARS {
            cur_end = w[1];
        } else {
            push_chunk(source, file_path, lang_name, cur_start, cur_end, &mut chunks);
            cur_start = w[0];
            cur_end = w[1];
        }
    }
    if cur_end > cur_start {
        push_chunk(source, file_path, lang_name, cur_start, cur_end, &mut chunks);
    }

    chunks
}

fn push_chunk(
    source: &str,
    file_path: &str,
    lang_name: &str,
    start: usize,
    end: usize,
    out: &mut Vec<Chunk>,
) {
    let content = &source[start..end];
    if content.trim().is_empty() {
        return;
    }
    // Count newlines in the byte-range directly — slicing `&str[..n]` would
    // panic when `n` falls inside a multi-byte UTF-8 char. Counting on the
    // raw byte slice is safe and equivalent (`b'\n'` is never part of any
    // multi-byte sequence in valid UTF-8).
    let bytes = source.as_bytes();
    let start_line = bytes[..start].iter().filter(|&&b| b == b'\n').count() as u32 + 1;
    let last_byte = end.saturating_sub(1).max(start);
    let end_line = bytes[..last_byte].iter().filter(|&&b| b == b'\n').count() as u32 + 1;

    out.push(Chunk {
        content: content.to_string(),
        file_path: file_path.to_string(),
        start_line,
        end_line,
        start_byte: start as u32,
        end_byte: end as u32,
        language: lang_name.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rust(src: &str) -> Vec<Chunk> {
        chunk_source(src, "f.rs", ChunkerKind::AstGrep(SupportLang::Rust))
    }

    #[test]
    fn empty_source_yields_no_chunks() {
        assert!(rust("").is_empty());
        assert!(rust("   \n\t\n").is_empty());
    }

    #[test]
    fn is_indexable_rejects_truly_unknown() {
        assert!(is_indexable(&PathBuf::from("foo.txt")).is_none());
        assert!(is_indexable(&PathBuf::from("foo.bin")).is_none());
        // No extension → none.
        assert!(is_indexable(&PathBuf::from("Makefile")).is_none());
    }

    #[test]
    fn is_indexable_accepts_adapter_languages() {
        assert_eq!(
            is_indexable(&PathBuf::from("a.rs")),
            Some(ChunkerKind::AstGrep(SupportLang::Rust))
        );
        assert_eq!(
            is_indexable(&PathBuf::from("a.py")),
            Some(ChunkerKind::AstGrep(SupportLang::Python))
        );
        assert_eq!(
            is_indexable(&PathBuf::from("a.kt")),
            Some(ChunkerKind::AstGrep(SupportLang::Kotlin))
        );
        assert_eq!(
            is_indexable(&PathBuf::from("README.md")),
            Some(ChunkerKind::Markdown)
        );
        assert_eq!(
            is_indexable(&PathBuf::from("doc.MDX")), // case-insensitive
            Some(ChunkerKind::Markdown)
        );
    }

    #[test]
    fn is_indexable_accepts_extra_ast_grep_languages() {
        // We rely on whatever ast-grep claims it can parse — these are languages
        // ast-bro has no outline adapter for, but the chunker still works.
        for ext in ["lua", "yaml", "yml", "json", "html", "css", "rb", "php"] {
            let p = PathBuf::from(format!("a.{ext}"));
            assert!(
                is_indexable(&p).is_some(),
                "expected ast-grep to claim .{ext}"
            );
        }
    }

    #[test]
    fn rust_one_function_one_chunk() {
        let chunks = rust("fn hello() {\n    println!(\"hi\");\n}\n");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].language, "rust");
        assert!(chunks[0].content.contains("fn hello"));
    }

    #[test]
    fn rust_groups_small_decls_under_max_chars() {
        let chunks = rust("fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }\n");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("fn a"));
        assert!(chunks[0].content.contains("fn c"));
    }

    #[test]
    fn rust_splits_when_total_exceeds_max() {
        let big_body: String = "    let x = 0;\n".repeat(80); // ~1200 chars per body
        let src = format!(
            "fn one() {{\n{big_body}}}\nfn two() {{\n{big_body}}}\nfn three() {{\n{big_body}}}\n"
        );
        let chunks = rust(&src);
        assert!(chunks.len() >= 2, "expected packer to split, got {}", chunks.len());
        for c in &chunks {
            assert!(!c.content.trim().is_empty());
        }
    }

    #[test]
    fn ast_chunks_are_contiguous_and_cover_source() {
        let src = "// header comment\nfn a() {}\n\nfn b() {}\n// trailer\n";
        let chunks = rust(src);
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(joined, src);
    }

    #[test]
    fn handles_multibyte_chars_in_source() {
        // Box-drawing char `─` is 3 bytes in UTF-8. A previous bug had us
        // slicing `&str[..end-1]` for line counting, which panicked when
        // the char before `end` was multi-byte.
        let src = "// ── header ──\nfn a() {}\n";
        let chunks = rust(src);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn line_numbers_are_one_indexed_and_correct() {
        let src = "fn a() {}\nfn b() {}\nfn c() {}\n";
        let chunks = rust(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
    }

    // ── Markdown ──────────────────────────────────────────────────────────

    fn md(src: &str) -> Vec<Chunk> {
        chunk_source(src, "README.md", ChunkerKind::Markdown)
    }

    #[test]
    fn markdown_single_section_one_chunk() {
        let src = "# Title\n\nSome paragraph.\n";
        let chunks = md(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].language, "markdown");
        assert!(chunks[0].content.contains("# Title"));
    }

    #[test]
    fn markdown_groups_small_sections() {
        let src = "\
# Section A
content a

# Section B
content b

# Section C
content c
";
        let chunks = md(src);
        // All three small sections should pack into one chunk.
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("Section A"));
        assert!(chunks[0].content.contains("Section C"));
    }

    #[test]
    fn markdown_splits_when_section_exceeds_max() {
        let big: String = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(40);
        let src = format!("# A\n{big}\n\n# B\n{big}\n\n# C\n{big}\n");
        let chunks = md(&src);
        assert!(chunks.len() >= 2, "expected splitting, got {}", chunks.len());
    }

    #[test]
    fn markdown_chunks_cover_source() {
        let src = "# Hello\n\nworld\n\n# Goodbye\n\ncruel world\n";
        let chunks = md(src);
        let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(joined, src);
    }
}
