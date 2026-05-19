//! AST-aware search and rewrite via ast-grep-core pattern matching.
//!
//! `ast-bro run -p 'pattern'` finds AST nodes matching the pattern.
//! `ast-bro run -p 'pattern' -r 'replacement'` rewrites matched nodes.
//!
//! Uses ast-grep-core's `Root::find_all()` for search and `Root::replace()`
//! + `Root::generate()` for rewrite. Meta-variables ($A, $$$ARGS, $_) work
//! exactly like ast-grep.

pub mod cli;

use ast_grep_core::Language;
use ast_grep_language::{LanguageExt, SupportLang};
use std::path::Path;

/// A single match result.
#[derive(serde::Serialize)]
pub struct RunMatch {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub matched_text: String,
}

/// Detect language from file extension.
pub fn detect_lang(path: &Path) -> Option<SupportLang> {
    SupportLang::from_path(path)
}

/// Search for pattern matches in source.
pub fn search(
    source: &str,
    lang: SupportLang,
    pattern: &str,
) -> Result<Vec<RunMatch>, String> {
    let ast = lang.ast_grep(source.to_string());
    let matches: Vec<RunMatch> = ast
        .root()
        .find_all(pattern)
        .map(|m| {
            let start = m.start_pos();
            let end = m.end_pos();
            let (_, start_col) = start.byte_point();
            let (_, end_col) = end.byte_point();
            RunMatch {
                file: String::new(),
                start_line: start.line() + 1,
                end_line: end.line() + 1,
                start_col: start_col + 1,
                end_col: end_col + 1,
                matched_text: m.text().to_string(),
            }
        })
        .collect();
    Ok(matches)
}

/// Rewrite matches in source. Returns the new source if any replacements were made.
pub fn rewrite(
    source: &str,
    lang: SupportLang,
    pattern: &str,
    replacement: &str,
) -> Result<Option<String>, String> {
    let mut ast = lang.ast_grep(source.to_string());
    let replaced = ast.replace(pattern, replacement)?;
    if replaced {
        Ok(Some(ast.generate()))
    } else {
        Ok(None)
    }
}
