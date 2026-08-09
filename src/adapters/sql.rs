//! Regex-based SQL adapter. Tree-sitter-sql doesn't ship in the ast-grep
//! language set, so we surface the most common DDL forms (TABLE / VIEW /
//! FUNCTION / PROCEDURE / INDEX / SEQUENCE) by scanning the source for
//! `CREATE …` headers. This intentionally only sees the *header* line of
//! each statement — column lists and bodies are skipped, which is fine
//! for outline/digest purposes but means SQL declarations don't carry
//! full multi-line ranges.

use crate::core::{Declaration, DeclarationKind, ParseResult};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Compile each pattern once at first use. `unwrap()` is intentional: a bad
/// pattern is a programmer error and should panic loudly the first time the
/// adapter runs, not silently produce empty matches forever.
///
/// The name capture `(?:\w+\.)?\w+` accepts an optional `schema.` prefix —
/// e.g. `CREATE TABLE app.users (...)` captures `app.users`, while plain
/// `CREATE TABLE users` still captures `users`.
static RE_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(?:TEMP(?:ORARY)?\s+)?TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?["']?((?:\w+\.)?\w+)["']?"#).unwrap()
});
static RE_VIEW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(?:TEMP(?:ORARY)?\s+)?(?:MATERIALIZED\s+)?VIEW\s+(?:IF\s+NOT\s+EXISTS\s+)?["']?((?:\w+\.)?\w+)["']?"#).unwrap()
});
static RE_FUNC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(FUNCTION|PROCEDURE)\s+["']?((?:\w+\.)?\w+)["']?"#).unwrap()
});
static RE_INDEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)^\s*CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:IF\s+NOT\s+EXISTS\s+)?["']?((?:\w+\.)?\w+)["']?\s+ON"#).unwrap()
});
static RE_SEQ: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)^\s*CREATE\s+SEQUENCE\s+(?:IF\s+NOT\s+EXISTS\s+)?["']?((?:\w+\.)?\w+)["']?"#).unwrap()
});

/// Parse a SQL source. Takes `&str` because UTF-8 validation has already
/// happened upstream (`fs::read_to_string`) — re-validating here was
/// dead-code defensive.
pub fn parse_sql(path: &Path, source: &str) -> ParseResult {
    let text = source;
    let line_starts = compute_line_starts(text);
    let mut decls = Vec::new();

    for caps in RE_TABLE.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        decls.push(make_decl(
            DeclarationKind::Class,
            "table",
            &name,
            &format!("table {}", name),
            m.start(),
            text,
            &line_starts,
        ));
    }
    for caps in RE_VIEW.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        decls.push(make_decl(
            DeclarationKind::Class,
            "view",
            &name,
            &format!("view {}", name),
            m.start(),
            text,
            &line_starts,
        ));
    }
    for caps in RE_FUNC.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let kind_str = caps.get(1).unwrap().as_str().to_lowercase();
        let name = caps.get(2).unwrap().as_str().to_string();
        decls.push(make_decl(
            DeclarationKind::Function,
            // Native kind preserves the source-true keyword
            // (`function` vs `procedure`).
            if kind_str == "procedure" { "procedure" } else { "function" },
            &name,
            &format!("{} {}", kind_str, name),
            m.start(),
            text,
            &line_starts,
        ));
    }
    for caps in RE_INDEX.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        decls.push(make_decl(
            DeclarationKind::Field,
            "index",
            &name,
            &format!("index {}", name),
            m.start(),
            text,
            &line_starts,
        ));
    }
    for caps in RE_SEQ.captures_iter(text) {
        let m = caps.get(0).unwrap();
        let name = caps.get(1).unwrap().as_str().to_string();
        decls.push(make_decl(
            DeclarationKind::Field,
            "sequence",
            &name,
            &format!("sequence {}", name),
            m.start(),
            text,
            &line_starts,
        ));
    }

    // Caller iterates in source order (digest/outline render top-to-bottom).
    decls.sort_by_key(|d| d.start_byte);

    ParseResult {
        path: path.to_path_buf(),
        language: "sql",
        source: source.as_bytes().to_vec(),
        line_count: line_starts.len(),
        declarations: decls,
        error_count: 0,
        imports: Vec::new(),
    }
}

/// 1-based line number for a byte offset, via binary search of `line_starts`.
fn line_of(byte: usize, line_starts: &[usize]) -> usize {
    match line_starts.binary_search(&byte) {
        Ok(i) => i + 1,
        Err(i) => i, // i is the next line's index; we want the previous (1-based)
    }
}

fn compute_line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn make_decl(
    kind: DeclarationKind,
    native: &str,
    name: &str,
    sig: &str,
    start_byte: usize,
    text: &str,
    line_starts: &[usize],
) -> Declaration {
    let end_byte = statement_end(text, start_byte);
    let start_line = line_of(start_byte, line_starts);
    let end_line = line_of(end_byte.saturating_sub(1).max(start_byte), line_starts);
    Declaration {
        kind,
        name: name.to_string(),
        signature: sig.to_string(),
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: String::new(),
        start_line,
        end_line,
        start_byte,
        end_byte,
        doc_start_byte: start_byte,
        native_kind: Some(native.to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    }
}

/// Walk forward from `start` to the byte after the next statement-terminating
/// `;`, skipping over contexts where a `;` doesn't terminate a statement:
/// `'…'` and `"…"` quoted literals, `--` line comments, `/* … */` block
/// comments, and `$tag$ … $tag$` dollar-quoted strings (PostgreSQL).
///
/// Necessary for PL/pgSQL — without dollar-quote handling, a CREATE FUNCTION
/// body collapses to its first internal `;` and `show` returns half a body.
/// If no terminator is found, return the end of the text.
fn statement_end(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        // Line comment: `-- … \n`
        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment: `/* … */`. PostgreSQL allows these to nest, so
        // track depth — `/* outer /* inner */ still in outer */` must not
        // terminate at the first `*/`. Other dialects treat the first `*/`
        // as the end; our deeper handling is a strict superset (a non-PG
        // file with un-nested comments has depth that never exceeds 1).
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            let mut depth: usize = 1;
            while i + 1 < bytes.len() && depth > 0 {
                if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            i = i.min(bytes.len());
            continue;
        }
        // Single-quoted string: `'…'` with `''` escape.
        if b == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Double-quoted identifier: `"…"` with `""` escape.
        if b == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Dollar-quoted string: `$tag$ … $tag$`. The tag is `[A-Za-z_]\w*` or empty.
        if b == b'$' {
            if let Some(tag_end) = find_dollar_tag_end(bytes, i) {
                let tag = &bytes[i..=tag_end];
                let after = tag_end + 1;
                if let Some(close) = find_subslice(bytes, after, tag) {
                    i = close + tag.len();
                    continue;
                }
                // No closing tag found — bail out at end of text.
                return bytes.len();
            }
            // Lone `$`, not a dollar-quote start.
            i += 1;
            continue;
        }
        if b == b';' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// If `bytes[i..]` starts with a dollar-quote tag (`$$`, `$body$`, `$_x$`),
/// return the byte index of the closing `$`. Otherwise None.
fn find_dollar_tag_end(bytes: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(bytes[i], b'$');
    let mut j = i + 1;
    while j < bytes.len() {
        let c = bytes[j];
        if c == b'$' {
            return Some(j);
        }
        if !(c.is_ascii_alphanumeric() || c == b'_') {
            return None;
        }
        j += 1;
    }
    None
}

/// Find the next occurrence of `needle` in `bytes` starting at `from`.
/// Returns the start index of the match, or None.
fn find_subslice(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_table_view_index_function_sequence() {
        let src = "
CREATE TABLE users (id SERIAL);
CREATE VIEW active AS SELECT * FROM users;
CREATE INDEX idx_users_id ON users(id);
CREATE FUNCTION foo() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;
CREATE SEQUENCE order_seq;
";
        let r = parse_sql(Path::new("x.sql"), src);
        let names: Vec<_> = r.declarations.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["users", "active", "idx_users_id", "foo", "order_seq"]
        );
    }

    #[test]
    fn handles_multiline_create_table_header() {
        let src = "CREATE TABLE\n  IF NOT EXISTS\n  customers (\n    id BIGINT PRIMARY KEY,\n    name TEXT\n  );\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 1);
        assert_eq!(r.declarations[0].name, "customers");
    }

    #[test]
    fn captures_schema_qualified_names() {
        let src = "CREATE TABLE app.users (id INT);\nCREATE VIEW reporting.daily_sales AS SELECT 1;\nCREATE INDEX app.idx_x ON app.users(id);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        let names: Vec<_> = r.declarations.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["app.users", "reporting.daily_sales", "app.idx_x"]);
    }

    #[test]
    fn computes_byte_offsets_and_line_numbers() {
        let src = "-- header\nCREATE TABLE foo (id INT);\nCREATE INDEX idx ON foo(id);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 2);
        assert_eq!(r.declarations[0].name, "foo");
        assert_eq!(r.declarations[0].start_line, 2);
        assert!(r.declarations[0].start_byte > 0);
        assert!(r.declarations[0].end_byte > r.declarations[0].start_byte);
        assert_eq!(r.declarations[1].name, "idx");
        assert_eq!(r.declarations[1].start_line, 3);
    }

    #[test]
    fn end_line_spans_multi_line_create_statement() {
        let src = "CREATE TABLE customers (\n  id BIGINT,\n  name TEXT\n);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 1);
        let d = &r.declarations[0];
        assert_eq!(d.start_line, 1);
        assert_eq!(d.end_line, 4, "end_line should reach the closing `;`");
    }

    #[test]
    fn statement_end_skips_dollar_quoted_function_body() {
        // Without dollar-quote awareness the `;` inside the body would end
        // the statement prematurely, truncating CREATE FUNCTION at line 2.
        let src = "CREATE FUNCTION f() RETURNS int AS $body$\n  SELECT 1;\n  SELECT 2;\n$body$ LANGUAGE sql;\nCREATE TABLE t (id INT);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 2);
        assert_eq!(r.declarations[0].name, "f");
        assert_eq!(r.declarations[0].start_line, 1);
        assert_eq!(
            r.declarations[0].end_line, 4,
            "function end should be the `$body$ LANGUAGE sql;` line, not the first inner `;`"
        );
        assert_eq!(r.declarations[1].name, "t");
        assert_eq!(r.declarations[1].start_line, 5);
    }

    #[test]
    fn statement_end_skips_string_with_semicolon() {
        let src = "CREATE TABLE t (s TEXT DEFAULT 'a;b');\nCREATE TABLE u (id INT);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 2);
        // First statement's end_byte should pass the embedded `;` and reach
        // the real terminator.
        let first_text = &src[r.declarations[0].start_byte..r.declarations[0].end_byte];
        assert!(first_text.ends_with(");"), "got: {:?}", first_text);
    }

    #[test]
    fn statement_end_skips_block_and_line_comments() {
        let src = "CREATE TABLE t /* inner ; comment */ (\n  id INT -- trailing ;\n);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 1);
        assert_eq!(r.declarations[0].end_line, 3);
    }

    #[test]
    fn statement_end_handles_nested_block_comments() {
        // PostgreSQL allows nested `/* ... /* ... */ ... */`. The outer `*/`
        // is the real comment end; the inner `*/` must not terminate it,
        // and the embedded `;` inside the comment must not end the
        // statement. Without depth tracking, the first `*/` would close
        // the comment, leaving `still in outer */ (id INT);` as parser
        // input and producing a wrong end_line.
        let src = "CREATE TABLE t /* outer /* inner ; */ still in outer */ (\n  id INT\n);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations.len(), 1);
        assert_eq!(r.declarations[0].name, "t");
        assert_eq!(r.declarations[0].end_line, 3);
    }

    #[test]
    fn distinguishes_function_from_procedure_native_kind() {
        let src = "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$;\nCREATE PROCEDURE p() LANGUAGE sql AS $$ SELECT 1 $$;\n";
        let r = parse_sql(Path::new("x.sql"), src);
        assert_eq!(r.declarations[0].native_kind.as_deref(), Some("function"));
        assert_eq!(r.declarations[1].native_kind.as_deref(), Some("procedure"));
    }

    #[test]
    fn matches_create_or_replace_and_if_not_exists() {
        let src = "CREATE OR REPLACE VIEW v AS SELECT 1;\nCREATE TABLE IF NOT EXISTS t (id INT);\n";
        let r = parse_sql(Path::new("x.sql"), src);
        let names: Vec<_> = r.declarations.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["v", "t"]);
    }

    #[test]
    fn populate_markers_preserves_sql_native_kind() {
        // Regression guard: the central populate_markers pass must not
        // clobber the procedure/function/table/etc. native_kind that the
        // SQL adapter sets manually.
        use crate::core::populate_markers;
        let src = "CREATE PROCEDURE p() LANGUAGE sql AS $$ SELECT 1 $$;\n";
        let mut r = parse_sql(Path::new("x.sql"), src);
        populate_markers(&mut r.declarations, r.language);
        assert_eq!(r.declarations[0].native_kind.as_deref(), Some("procedure"));
    }
}
