use std::path::Path;

use ast_grep_core::Language;
use ast_grep_language::{LanguageExt, SupportLang};

use crate::adapters::base::LanguageAdapter;
use crate::adapters::cpp::CppAdapter;
use crate::adapters::csharp::CSharpAdapter;
use crate::adapters::go::GoAdapter;
use crate::adapters::java::JavaAdapter;
use crate::adapters::kotlin::KotlinAdapter;
use crate::adapters::php::PhpAdapter;
use crate::adapters::python::PythonAdapter;
use crate::adapters::ruby::RubyAdapter;
use crate::adapters::rust::RustAdapter;
use crate::adapters::scala::ScalaAdapter;
use crate::adapters::typescript::TypeScriptAdapter;
use crate::core::ParseResult;

/// Cheap check: would `parse_file_for_hook` accept this path?
/// Lets the hook skip files we can't render without reading them.
/// For extensionless files, peeks at the shebang line before reading the
/// full content — so scripts (#!/usr/bin/env python3, etc.) are detected.
pub fn can_parse_for_hook(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|o| o.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        "sql" | "ddl" | "dml" | "md" | "markdown" | "mdx" | "mdown"
    ) {
        return true;
    }
    if SupportLang::from_path(path).is_some() {
        return true;
    }
    // Extensionless file — check shebang
    if path.extension().is_none() {
        return crate::file_filter::detect_language(path).is_some();
    }
    false
}

pub fn parse_file_for_hook(path: &Path) -> Option<ParseResult> {
    let source = std::fs::read_to_string(path).ok()?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Handle SQL files (regex-based parsing). `parse_sql` takes `&str`
    // directly — UTF-8 was already validated by `read_to_string` above.
    if matches!(ext, "sql" | "ddl" | "dml") {
        let mut r = crate::adapters::sql::parse_sql(path, &source);
        crate::core::populate_markers(&mut r.declarations, r.language);
        return Some(r);
    }

    if matches!(ext, "md" | "markdown" | "mdx" | "mdown") {
        let mut r = crate::adapters::markdown::parse_markdown(path, source.as_bytes());
        crate::core::populate_markers(&mut r.declarations, r.language);
        return Some(r);
    }

    // Try extension first; for extensionless files, fall back to shebang.
    let lang = SupportLang::from_path(path).or_else(|| {
        if path.extension().is_none() {
            crate::file_filter::detect_language(path)
        } else {
            None
        }
    })?;
    let mut result = match lang {
        SupportLang::Rust => RustAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Python => PythonAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            TypeScriptAdapter.parse(path, source.as_bytes(), lang.ast_grep(source.clone()).root())
        }
        SupportLang::CSharp => CSharpAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Go => GoAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Java => JavaAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Kotlin => KotlinAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Scala => ScalaAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Cpp => CppAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Ruby => RubyAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        SupportLang::Php => PhpAdapter.parse(
            path,
            source.as_bytes(),
            lang.ast_grep(source.clone()).root(),
        ),
        _ => return None,
    };
    // One central pass enriches every adapter's output with `native_kind`,
    // `modifiers`, and `deprecated` derived from the language's source
    // conventions. Adapters stay focused on tree traversal.
    crate::core::populate_markers(&mut result.declarations, result.language);
    Some(result)
}
