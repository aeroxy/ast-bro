//! CLI dispatch for `ast-bro run`.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use ast_grep_language::SupportLang;

use super::{detect_lang, rewrite, search, search_with_pattern};

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
    let fixed_lang = lang_override.and_then(|l| match parse_lang(l) {
        Some(l) => Some(l),
        None => {
            eprintln!("# note: unknown language '{}'", l);
            None
        }
    });
    let compiled_pattern: Option<ast_grep_core::Pattern> =
        if let Some(lang) = fixed_lang {
            match ast_grep_core::Pattern::try_new(pattern, lang) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("# note: invalid pattern: {}", e);
                    None
                }
            }
        } else {
            None
        };

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => {
                error_count += 1;
                continue;
            },
        };
        let lang = if let Some(l) = fixed_lang {
            l
        } else {
            match detect_lang(path) {
                Some(l) => l,
                None => continue,
            }
        };

        if let Some(replacement) = rewrite_template {
            match rewrite(&source, lang, pattern, replacement) {
                Ok(Some(new_source)) => {
                    let file_str = path.display().to_string();
                    if write_changes {
                        match std::fs::write(path, &new_source) {
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
                search(&source, lang, pattern)
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
                mode: &'static str,
                dry_run: bool,
                rewrite_count: usize,
                error_count: usize,
                files: &'a [RewriteRecord],
            }
            let doc = RewriteDoc {
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
        } else if pretty {
            serde_json::to_string_pretty(&json_matches)
        } else {
            serde_json::to_string(&json_matches)
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
    // 2 = all files errored (and at least one file was attempted)
    if !files.is_empty() && error_count == files.len() {
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
            out.push_str(&format!(
                "{}:{}: {}{}",
                path.display(),
                display_idx,
                sign,
                change
            ));
        }
    }
    out
}

/// Print diff to stdout (CLI dry-run mode).
fn show_diff(path: &Path, old: &str, new: &str) {
    print!("{}", line_change_report(path, old, new));
}
