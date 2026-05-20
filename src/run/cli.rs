//! CLI dispatch for `ast-bro run`.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use ast_grep_language::SupportLang;

use super::{detect_lang, rewrite, search};

pub fn run(
    pattern: &str,
    rewrite_template: Option<&str>,
    lang_override: Option<&str>,
    paths: &[PathBuf],
    write_changes: bool,
    json: bool,
    pretty: bool,
) -> i32 {
    let search_paths = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.to_vec()
    };

    let files = crate::walk_and_parse(&search_paths, None);
    let mut match_count: usize = 0;
    let mut rewrite_count: usize = 0;
    let mut error_count: usize = 0;

    for result in &files {
        let source = match std::fs::read_to_string(&result.path) {
            Ok(s) => s,
            Err(_) => {
                error_count += 1;
                continue;
            },
        };
        let lang = if let Some(l) = lang_override {
            match parse_lang(l) {
                Some(l) => l,
                None => {
                    eprintln!("# note: unknown language '{}'", l);
                    error_count += 1;
                    continue;
                }
            }
        } else {
            match detect_lang(&result.path) {
                Some(l) => l,
                None => continue,
            }
        };

        if let Some(replacement) = rewrite_template {
            match rewrite(&source, lang, pattern, replacement) {
                Ok(Some(new_source)) => {
                    if write_changes {
                        if let Err(e) = std::fs::write(&result.path, &new_source) {
                            eprintln!("{}: write failed: {}", result.path.display(), e);
                            error_count += 1;
                        } else {
                            println!("{}: rewritten", result.path.display());
                            rewrite_count += 1;
                        }
                    } else {
                        show_diff(&result.path, &source, &new_source);
                        rewrite_count += 1;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("{}: {}", result.path.display(), e);
                    error_count += 1;
                },
            }
        } else {
            match search(&source, lang, pattern) {
                Ok(matches) => {
                    match_count += matches.len();
                    for mut m in matches {
                        m.file = result.path.display().to_string();
                        if json {
                            let s = if pretty {
                                serde_json::to_string_pretty(&m)
                            } else {
                                serde_json::to_string(&m)
                            };
                            if let Ok(s) = s {
                                println!("{}", s);
                            }
                        } else {
                            let first_line = m
                                .matched_text
                                .lines()
                                .next()
                                .unwrap_or("");
                            println!(
                                "{}:{}:{}: {}",
                                m.file, m.start_line, m.start_col, first_line
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{}: {}", result.path.display(), e);
                    error_count += 1;
                },
            }
        }
    }

    // Exit code semantics:
    // 0 = success (matches found, or rewrites applied)
    // 1 = no matches found (search mode) or no rewrites possible (rewrite mode)
    // 2 = all files errored
    if error_count > 0 && match_count == 0 && rewrite_count == 0 {
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

/// Produce a unified diff string between `old` and `new`.
/// Used by both CLI (dry-run rewrite) and MCP `run` tool.
pub fn unified_diff(path: &Path, old: &str, new: &str) -> String {
    use similar::TextDiff;
    let mut out = String::new();
    let diff = TextDiff::from_lines(old, new);
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            let sign = match change.tag() {
                similar::ChangeTag::Delete => "-",
                similar::ChangeTag::Insert => "+",
                similar::ChangeTag::Equal => " ",
            };
            out.push_str(&format!(
                "{}:{}: {}{}",
                path.display(),
                change.old_index().unwrap_or(0),
                sign,
                change
            ));
        }
    }
    out
}

/// Print diff to stderr (CLI dry-run mode).
fn show_diff(path: &Path, old: &str, new: &str) {
    eprint!("{}", unified_diff(path, old, new));
}
