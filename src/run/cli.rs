//! CLI dispatch for `ast-bro run`.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use ast_grep_language::SupportLang;

use super::{detect_lang, search_with_pattern};

pub fn run(
    pattern: &str,
    rewrite_template: Option<&str>,
    lang_override: Option<&str>,
    paths: &[PathBuf],
    glob: Option<&str>,
    write_changes: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    let search_paths = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };

    let files = crate::walk_paths(&search_paths, glob);
    let mut match_count: usize = 0;
    let mut rewrite_count: usize = 0;
    let mut error_count: usize = 0;
    let mut attempted_files: usize = 0;
    // Collect search matches when emitting JSON so the whole run produces
    // one valid JSON array (consistent with `map` / `show` / etc.) rather
    // than newline-delimited objects.
    let mut json_matches: Vec<super::RunMatch> = Vec::new();

    #[derive(serde::Serialize)]
    struct RewriteRecord {
        file: String,
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }
    let mut json_rewrites: Vec<RewriteRecord> = Vec::new();

    // Pre-resolve language and compile pattern once when --lang is specified,
    // so we avoid redundant parsing on every file in the loop.
    let (fixed_lang, compiled_pattern) = if let Some(l) = lang_override {
        let lang = match parse_lang(l) {
            Some(l) => l,
            None => {
                eprintln!("error: unsupported language '{}'", l);
                return 2;
            }
        };
        let pat = match ast_grep_core::Pattern::try_new(pattern, lang) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: invalid pattern: {}", e);
                return 2;
            }
        };
        (Some(lang), Some(pat))
    } else {
        (None, None)
    };
    // Cache compiled patterns per language when lang is auto-detected.
    let mut pattern_cache: std::collections::HashMap<ast_grep_language::SupportLang, Result<ast_grep_core::Pattern, String>> = std::collections::HashMap::new();

    for path in &files {
        // Detect language first to avoid reading non-source files.
        let lang = if let Some(l) = fixed_lang {
            l
        } else {
            match detect_lang(path) {
                Some(l) => l,
                None => continue,
            }
        };
        attempted_files += 1;
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{}: read failed: {}", path.display(), e);
                error_count += 1;
                continue;
            },
        };

        if let Some(replacement) = rewrite_template {
            let result = if let Some(ref compiled) = compiled_pattern {
                super::rewrite_with_pattern(&source, lang, compiled, replacement)
            } else {
                let compiled = pattern_cache.entry(lang).or_insert_with(|| {
                    ast_grep_core::Pattern::try_new(pattern, lang)
                        .map_err(|e| format!("invalid pattern for {}: {}", lang, e))
                });
                match compiled {
                    Ok(p) => super::rewrite_with_pattern(&source, lang, p, replacement),
                    Err(e) => {
                        eprintln!("{}: {}", path.display(), e);
                        error_count += 1;
                        continue;
                    }
                }
            };
            match result {
                Ok(Some(new_source)) => {
                    let file_str = path.display().to_string();
                    if write_changes {
                        match super::atomic_write(path, new_source.as_bytes()) {
                            Err(e) => {
                                if json {
                                    json_rewrites.push(RewriteRecord {
                                        file: file_str,
                                        status: "write_failed",
                                        diff: None,
                                        error: Some(e.to_string()),
                                    });
                                } else {
                                    eprintln!("{}: write failed: {}", path.display(), e);
                                }
                                error_count += 1;
                            }
                            Ok(()) => {
                                if json {
                                    json_rewrites.push(RewriteRecord {
                                        file: file_str,
                                        status: "rewritten",
                                        diff: None,
                                        error: None,
                                    });
                                } else {
                                    println!("{}: rewritten", path.display());
                                }
                                rewrite_count += 1;
                            }
                        }
                    } else if json {
                        let diff = line_change_report(path, &source, &new_source);
                        json_rewrites.push(RewriteRecord {
                            file: file_str,
                            status: "diff",
                            diff: Some(diff),
                            error: None,
                        });
                        rewrite_count += 1;
                    } else {
                        show_diff(path, &source, &new_source);
                        rewrite_count += 1;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    if json {
                        json_rewrites.push(RewriteRecord {
                            file: path.display().to_string(),
                            status: "rewrite_error",
                            diff: None,
                            error: Some(e.clone()),
                        });
                    } else {
                        eprintln!("{}: {}", path.display(), e);
                    }
                    error_count += 1;
                },
            }
        } else {
            let result = if let Some(ref compiled) = compiled_pattern {
                search_with_pattern(&source, lang, compiled)
            } else {
                let compiled = pattern_cache.entry(lang).or_insert_with(|| {
                    ast_grep_core::Pattern::try_new(pattern, lang)
                        .map_err(|e| format!("invalid pattern for {}: {}", lang, e))
                });
                match compiled {
                    Ok(p) => search_with_pattern(&source, lang, p),
                    Err(e) => {
                        eprintln!("{}: {}", path.display(), e);
                        error_count += 1;
                        continue;
                    }
                }
            };
            match result {
                Ok(matches) => {
                    match_count += matches.len();
                    for mut m in matches {
                        m.file = path.display().to_string();
                        if json {
                            json_matches.push(m);
                        } else {
                            let first_line = m
                                .matched_text
                                .lines()
                                .next()
                                .unwrap_or("");
                            println!(
                                "{}:{}:{}-{}:{}: {}",
                                m.file, m.start_line, m.start_col, m.end_line, m.end_col, first_line
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", path.display(), e);
                    error_count += 1;
                },
            }
        }
    }

    // Flush collected results as a single JSON document so machine
    // consumers can parse one valid object per invocation.
    if json {
        let serialized = if rewrite_template.is_some() {
            #[derive(serde::Serialize)]
            struct RewriteDoc<'a> {
                schema: &'static str,
                mode: &'static str,
                dry_run: bool,
                rewrite_count: usize,
                error_count: usize,
                files: &'a [RewriteRecord],
            }
            let doc = RewriteDoc {
                schema: crate::core::JSON_SCHEMA_RUN,
                mode: "rewrite",
                dry_run: !write_changes,
                rewrite_count,
                error_count,
                files: &json_rewrites,
            };
            if pretty {
                serde_json::to_string_pretty(&doc)
            } else {
                serde_json::to_string(&doc)
            }
        } else {
            #[derive(serde::Serialize)]
            struct SearchDoc<'a> {
                schema: &'static str,
                matches: &'a [super::RunMatch],
                error_count: usize,
            }
            let doc = SearchDoc {
                schema: crate::core::JSON_SCHEMA_RUN,
                matches: &json_matches,
                error_count,
            };
            if pretty {
                serde_json::to_string_pretty(&doc)
            } else {
                serde_json::to_string(&doc)
            }
        };
        match serialized {
            Ok(s) => println!("{}", s),
            Err(e) => {
                eprintln!("error: failed to serialize JSON output: {}", e);
                return 2;
            }
        }
    }

    // Exit code semantics:
    // 0 = success (matches found, or rewrites applied)
    // 1 = no matches found (search mode) or no rewrites possible (rewrite mode)
    // 2 = all attempted files errored (and at least one file was attempted)
    if attempted_files > 0 && error_count == attempted_files {
        2
    } else if rewrite_template.is_some() && rewrite_count == 0 {
        1
    } else if rewrite_template.is_none() && match_count == 0 {
        1
    } else {
        0
    }
}

pub fn parse_lang(s: &str) -> Option<SupportLang> {
    match s.to_lowercase().as_str() {
        "rs" | "rust" => Some(SupportLang::Rust),
        "py" | "python" => Some(SupportLang::Python),
        "ts" | "typescript" => Some(SupportLang::TypeScript),
        "tsx" => Some(SupportLang::Tsx),
        "js" | "javascript" => Some(SupportLang::JavaScript),
        "cs" | "csharp" => Some(SupportLang::CSharp),
        "go" => Some(SupportLang::Go),
        "java" => Some(SupportLang::Java),
        "kt" | "kotlin" => Some(SupportLang::Kotlin),
        "scala" => Some(SupportLang::Scala),
        "cpp" | "c++" => Some(SupportLang::Cpp),
        "rb" | "ruby" => Some(SupportLang::Ruby),
        "php" => Some(SupportLang::Php),
        other => SupportLang::from_str(other).ok(),
    }
}

/// Produce a path-prefixed, line-by-line change report between `old` and
/// `new`. Each changed line is emitted as `path:line: -old` / `path:line: +new`
/// (a custom format — *not* a standard `--- / +++ / @@` unified diff). Used
/// by both CLI (dry-run rewrite) and MCP `run` tool.
pub fn line_change_report(path: &Path, old: &str, new: &str) -> String {
    use similar::TextDiff;
    let mut out = String::new();
    let diff = TextDiff::from_lines(old, new);
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            if change.tag() == similar::ChangeTag::Equal {
                continue;
            }
            let sign = match change.tag() {
                similar::ChangeTag::Delete => "-",
                similar::ChangeTag::Insert => "+",
                similar::ChangeTag::Equal => unreachable!(),
            };
            let display_idx = change
                .old_index()
                .or_else(|| change.new_index())
                .unwrap_or(0)
                + 1;
            let line_content = change.to_string();
            out.push_str(&format!(
                "{}:{}: {}{}",
                path.display(),
                display_idx,
                sign,
                line_content
            ));
            if !line_content.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// Print diff to stdout (CLI dry-run mode).
fn show_diff(path: &Path, old: &str, new: &str) {
    print!("{}", line_change_report(path, old, new));
}
