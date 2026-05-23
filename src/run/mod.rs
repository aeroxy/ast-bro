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
#[allow(dead_code)] // public API; prefer search_with_pattern in loops
pub fn search(
    source: &str,
    lang: SupportLang,
    pattern: &str,
) -> Result<Vec<RunMatch>, String> {
    use ast_grep_core::Pattern;
    let compiled = Pattern::try_new(pattern, lang.clone())
        .map_err(|e| format!("invalid pattern: {}", e))?;
    search_with_pattern(source, lang, &compiled)
}

/// Search for pattern matches using a pre-compiled pattern.
///
/// Use this variant in loops where the same pattern is applied to many files
/// with the same language — compile once, clone per file.
pub fn search_with_pattern(
    source: &str,
    lang: SupportLang,
    pattern: &ast_grep_core::Pattern,
) -> Result<Vec<RunMatch>, String> {
    let ast = lang.ast_grep(source.to_string());
    let matches: Vec<RunMatch> = ast
        .root()
        .find_all(pattern.clone())
        .map(|m| {
            let start = m.start_pos();
            let end = m.end_pos();
            RunMatch {
                file: String::new(),
                start_line: start.line() + 1,
                end_line: end.line() + 1,
                start_col: start.column(&m) + 1,
                end_col: end.column(&m) + 1,
                matched_text: m.text().to_string(),
            }
        })
        .collect();
    Ok(matches)
}

/// Rewrite matches in source using a pre-compiled pattern.
///
/// Use this variant in loops where the same pattern is applied to many files
/// with the same language — compile once, clone per file.
pub fn rewrite_with_pattern(
    source: &str,
    lang: SupportLang,
    pattern: &ast_grep_core::Pattern,
    replacement: &str,
) -> Result<Option<String>, String> {
    let mut ast = lang.ast_grep(source.to_string());
    let replaced = ast.replace(pattern.clone(), replacement)?;
    if replaced {
        Ok(Some(ast.generate()))
    } else {
        Ok(None)
    }
}

/// Crash-safe in-place file replacement: writes to a sibling temp file,
/// fsyncs it, then renames over the target. On POSIX the rename is atomic;
/// on Windows std::fs::rename uses `MOVEFILE_REPLACE_EXISTING`. Either way,
/// an interrupted write can no longer truncate or corrupt the original.
///
/// Permissions are best-effort copied from the original before the rename,
/// since the rename swaps the inode.
pub fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "atomic_write: path has no parent directory",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "atomic_write: path has no file name",
        )
    })?;

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.ast-bro-tmp-{}-{}",
        file_name.to_string_lossy(),
        std::process::id(),
        n
    );
    let tmp_path = dir.join(tmp_name);

    let orig_perms = std::fs::metadata(path).map(|m| m.permissions()).ok();

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(contents)?;
        f.sync_all()
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Some(perms) = orig_perms {
        let _ = std::fs::set_permissions(&tmp_path, perms);
    }

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Rewrite matches in source. Returns the new source if any replacements were made.
#[allow(dead_code)] // public API; prefer rewrite_with_pattern in loops
pub fn rewrite(
    source: &str,
    lang: SupportLang,
    pattern: &str,
    replacement: &str,
) -> Result<Option<String>, String> {
    use ast_grep_core::Pattern;
    let compiled = Pattern::try_new(pattern, lang.clone())
        .map_err(|e| format!("invalid pattern: {}", e))?;
    rewrite_with_pattern(source, lang, &compiled, replacement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn atomic_write_replaces_contents_and_leaves_no_tempfiles() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("src.rs");
        std::fs::write(&p, "old\n").unwrap();
        atomic_write(&p, b"new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("ast-bro-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {:?}", leftovers);
    }

    #[test]
    fn atomic_write_creates_new_file_when_target_missing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("fresh.rs");
        atomic_write(&p, b"hello\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\n");
    }
}
