//! AST-aware code chunking for indexing.
//!
//! Three strategies, picked by file extension:
//!
//! - **Anything ast-grep can parse** (bash, cpp, css, c#, dart, elixir, go,
//!   haskell, hcl, html, java, json, kotlin, lua, nix, php, python, ruby, rust,
//!   scala, solidity, swift, ts/tsx/js, yaml) — split at top-level declaration
//!   boundaries via ast-grep. The chunker doesn't need a per-language outline
//!   adapter; it only needs the AST root + iteration of named children.
//! - **Markdown** (`.md`/`.markdown`/`.mdx`/`.mdown`) — split at `section`
//!   boundaries via raw `tree_sitter_md`.
//! - **Plain text** (`.toml`, PowerShell `.ps1`/`.psm1`/`.psd1`) — formats
//!   with no tree-sitter grammar in `SupportLang`; split at blank-line
//!   boundaries (LF or CRLF). Files without blank lines become one oversized
//!   chunk, which the packer already tolerates. Note: sources must be UTF-8 —
//!   UTF-16-encoded `.ps1` files fail `read_to_string` and are skipped, like
//!   everywhere else in the codebase.
//!
//! All paths feed the same greedy packer that targets `MAX_CHARS = 1500` and
//! never splits mid-declaration. Files outside these sets are not chunked —
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
    /// Blank-line-delimited plain text; carries the language label to report
    /// (e.g. `"toml"`, `"powershell"`).
    Plain(&'static str),
}

impl ChunkerKind {
    /// Lowercase canonical name. Uses the `SupportLang` Debug variant for
    /// ast-grep languages so we automatically pick up new languages without
    /// having to maintain a per-variant match.
    fn language_name(self) -> String {
        match self {
            ChunkerKind::AstGrep(lang) => format!("{lang:?}").to_ascii_lowercase(),
            ChunkerKind::Markdown => "markdown".to_string(),
            ChunkerKind::Plain(name) => name.to_string(),
        }
    }
}

/// Decide whether `path` is indexable, and how.
///
/// Returns `Some(Markdown)` for `.md`/`.markdown`/`.mdx`/`.mdown` (handled via
/// `tree_sitter_md`), `Some(Plain(_))` for `.toml` and PowerShell
/// `.ps1`/`.psm1`/`.psd1` (blank-line-delimited plain text),
/// `Some(AstGrep(lang))` for any extension ast-grep claims it can parse, and
/// `None` otherwise.
pub fn is_indexable(path: &Path) -> Option<ChunkerKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(ext.as_deref(), Some("md" | "markdown" | "mdx" | "mdown")) {
        return Some(ChunkerKind::Markdown);
    }
    if ext.as_deref() == Some("toml") {
        return Some(ChunkerKind::Plain("toml"));
    }
    if matches!(ext.as_deref(), Some("ps1" | "psm1" | "psd1")) {
        return Some(ChunkerKind::Plain("powershell"));
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
        ChunkerKind::Plain(_) => paragraph_split_points(source),
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

/// Blank-line end-bytes for plain-text formats (TOML, PowerShell) that have
/// no tree-sitter grammar. A "blank line" is a run of two or more line
/// terminators, where a terminator is `\n` or `\r\n` — PowerShell files
/// are routinely CRLF, so matching bare `\n\n` only would never split them.
/// A line holding only spaces/tabs is still blank: horizontal whitespace
/// between terminators is consumed with the run (`"\n \t\n"` separates
/// paragraphs just like `"\n\n"`).
///
/// Every emitted offset sits immediately after an ASCII `\n` byte, which is
/// always a valid UTF-8 boundary (`b'\n'` never appears inside a multi-byte
/// sequence), so `pack` can slice at these points safely.
fn paragraph_split_points(source: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut points: Vec<usize> = vec![0];
    let mut i = 0;
    while i < len {
        // A line terminator at `i` is either "\n" or "\r\n".
        let first = match bytes[i] {
            b'\n' => 1,
            b'\r' if i + 1 < len && bytes[i + 1] == b'\n' => 2,
            _ => {
                i += 1;
                continue;
            }
        };
        // Consume the whole run of terminators, skipping spaces/tabs
        // between them. The skip only commits when another terminator
        // follows, so indentation at the start of a non-blank line stays
        // with that line.
        let mut terminators = 1;
        i += first;
        loop {
            let mut j = i;
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < len && bytes[j] == b'\n' {
                i = j + 1;
            } else if j + 1 < len && bytes[j] == b'\r' && bytes[j + 1] == b'\n' {
                i = j + 2;
            } else {
                break;
            }
            terminators += 1;
        }
        // ≥2 terminators means at least one blank line — split after the run.
        if terminators >= 2 && i < len && i > *points.last().unwrap() {
            points.push(i);
        }
    }
    if *points.last().unwrap() < len {
        points.push(len);
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

    // ── Plain text (TOML / PowerShell) ────────────────────────────────────

    #[test]
    fn is_indexable_accepts_plain_text_formats() {
        assert_eq!(
            is_indexable(&PathBuf::from("Cargo.toml")),
            Some(ChunkerKind::Plain("toml"))
        );
        for ext in ["ps1", "psm1", "psd1"] {
            assert_eq!(
                is_indexable(&PathBuf::from(format!("script.{ext}"))),
                Some(ChunkerKind::Plain("powershell")),
                "expected .{ext} to map to the plain powershell chunker"
            );
        }
    }

    #[test]
    fn paragraph_split_detects_lf_blank_lines() {
        let src = "function A {}\n\nfunction B {}\n\nfunction C {}\n";
        let points = paragraph_split_points(src);
        // 0, after each blank-line run, and EOF → 4 points / 3 regions.
        assert_eq!(points.len(), 4, "points: {points:?}");
        assert_eq!(points[0], 0);
        assert_eq!(*points.last().unwrap(), src.len());
    }

    #[test]
    fn paragraph_split_detects_crlf_blank_lines() {
        // Regression for upstream PR #29: its splitter only matched bare
        // "\n\n", so CRLF files — the norm for PowerShell — never split.
        let src = "function A {}\r\n\r\nfunction B {}\r\n\r\nfunction C {}\r\n";
        let points = paragraph_split_points(src);
        assert_eq!(points.len(), 4, "points: {points:?}");
        assert_eq!(points[0], 0);
        assert_eq!(*points.last().unwrap(), src.len());
    }

    #[test]
    fn paragraph_split_treats_whitespace_only_lines_as_blank() {
        // A "blank" line holding spaces or tabs still separates paragraphs,
        // for both LF and CRLF files — and coverage stays byte-exact.
        for src in [
            "function A {}\n \nfunction B {}\n\t\nfunction C {}\n",
            "function A {}\r\n \t \r\nfunction B {}\r\n\t\r\nfunction C {}\r\n",
        ] {
            let points = paragraph_split_points(src);
            assert_eq!(points.len(), 4, "src: {src:?}, points: {points:?}");
            assert_eq!(points[0], 0);
            assert_eq!(*points.last().unwrap(), src.len());
            let chunks = chunk_source(src, "f", ChunkerKind::Plain("toml"));
            let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
            assert_eq!(joined, src, "chunks must cover the source exactly");
        }
        // Indentation on a *non-blank* line stays with that line: no split.
        let src = "a = 1\n    indented = 2\nb = 3\n";
        assert_eq!(paragraph_split_points(src).len(), 2, "no blank line, no split");
    }

    #[test]
    fn plain_splits_into_multiple_chunks_when_over_max() {
        let para = format!("[section]\nkey = \"{}\"\n", "x".repeat(1200));
        let src = format!("{para}\n{para}\n{para}");
        let chunks = chunk_source(&src, "big.toml", ChunkerKind::Plain("toml"));
        assert!(chunks.len() >= 2, "expected splitting, got {}", chunks.len());
        assert_eq!(chunks[0].language, "toml");
    }

    #[test]
    fn plain_chunks_cover_source() {
        for src in [
            "[package]\nname = \"demo\"\n\n[dependencies]\nserde = \"1\"\n",
            "function A {}\r\n\r\nfunction B {}\r\n\r\nfunction C {}\r\n",
        ] {
            let chunks = chunk_source(src, "f", ChunkerKind::Plain("toml"));
            let joined: String = chunks.iter().map(|c| c.content.as_str()).collect();
            assert_eq!(joined, src);
        }
    }

    #[test]
    fn plain_no_blank_lines_is_one_chunk() {
        let src = "a = 1\nb = 2\nc = 3\n";
        let chunks = chunk_source(src, "f.toml", ChunkerKind::Plain("toml"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 3);
    }
}
