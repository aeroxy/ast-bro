//! Shared `show` engine: resolve targets → parse → match symbols → render.
//!
//! `show` was single-file only for most of its life, and that made an
//! unquoted glob dangerous rather than merely unsupported. The shell expands
//! `show src/*.java greet` before the process starts, so the extra files
//! arrived in the *symbol* slot: the symbol resolved against the first file
//! alone, its definitions in every other file were dropped, a bogus
//! `symbol not found: src/Second.java` note was printed, and the exit code
//! stayed 0 — a silent false negative, which is the one failure mode an
//! agent cannot detect. Meanwhile the correct spelling didn't work either:
//! a quoted glob and a directory were both rejected as `path not found`.
//!
//! A `show` target is therefore a *list* now — explicit files, a directory,
//! or a quoted glob — and [`partition`] is what recovers the expanded-glob
//! spelling by splitting the positionals into leading targets and trailing
//! symbols. Multi-target answers carry their own coverage line (how many
//! files were searched, how many matched, whether the display was capped),
//! because a caller who didn't name the file can't otherwise tell a
//! genuinely absent symbol from one hidden behind a cap.

use std::path::{Path, PathBuf};

use crate::core::{ParseResult, SymbolMatch};

/// Default cap on rendered bodies in multi-target mode. A bare common name
/// (`new`, `build`) can match a hundred declarations across a repository,
/// each with a full body; the cap keeps that from becoming the whole
/// answer, and the header always reports the true total (issue #32).
pub const DEFAULT_LIMIT: usize = 20;

/// One searched file and the matches found in it.
pub struct FileMatches {
    pub result: ParseResult,
    pub matches: Vec<SymbolMatch>,
}

/// The result of a `show` query, rendered by [`render_text`] / [`render_json`].
pub struct Outcome {
    /// Files that produced at least one match, in path order. Trimmed by the
    /// display cap, so its length is *not* the coverage figure — use
    /// [`Outcome::files_matched`].
    pub files: Vec<FileMatches>,
    /// Parseable files actually searched — the coverage of a zero answer.
    pub files_scanned: usize,
    /// Files that matched, counted before the display cap. Reporting the
    /// post-cap length here would make a capped answer understate its own
    /// coverage ("1 of 3 files" when two matched), which is the exact
    /// miscount the header exists to prevent.
    files_matched: usize,
    /// Matches found before the display cap.
    pub total: usize,
    /// Matches actually rendered.
    pub shown: usize,
    /// Whether the cap trimmed the display.
    pub truncated: bool,
    /// Requested symbols that matched in no file at all.
    pub unmatched: Vec<String>,
    /// The requested symbols, as typed.
    pub symbols: Vec<String>,
    /// True when the caller named something other than one explicit file
    /// (several files, a directory, a glob). Single-file output stays
    /// byte-identical to what it has always been; only the multi-target
    /// form grows a coverage header.
    pub multi: bool,
}

impl Outcome {
    /// Files that matched, out of files searched — the true count, cap or no cap.
    pub fn files_matched(&self) -> usize {
        self.files_matched
    }
}

/// True if `arg` carries shell-glob metacharacters. Used to tell a quoted
/// glob (expand it) from a plain path that simply isn't there (reject it),
/// so `show 'src/*.rs' Sym` and `show src/typo.rs Sym` get different
/// diagnoses.
pub fn looks_like_glob(p: &Path) -> bool {
    p.to_string_lossy().contains(['*', '?', '['])
}

/// True if `arg` reads as a path the caller meant to name, whether or not it
/// exists: it carries a directory separator, or its extension is one an
/// adapter handles.
///
/// Needed because a non-existent path and a symbol are both "not a target",
/// and collapsing them means `show a.rs missing.rs greet` searches for a
/// *symbol* called `missing.rs` and reports it as absent — diagnosing the
/// wrong thing about the wrong argument. A symbol never contains a separator,
/// and `Type.method` (the suffix form) has no source extension, so the two
/// cases separate cleanly.
pub fn looks_like_path(arg: &Path) -> bool {
    let s = arg.to_string_lossy();
    if s.contains('/') || s.contains('\\') {
        return true;
    }
    // `can_parse_for_hook` on a non-existent path answers the extension
    // question without touching the filesystem for anything but a shebang,
    // which needs the file to exist anyway.
    arg.extension().is_some() && crate::main_helpers::can_parse_for_hook(arg)
}

/// True if `arg` can be a `show` target: a directory, an existing file that
/// resolves to an adapter, or a glob that matches at least one path.
///
/// The "resolves to an adapter" half is what keeps a symbol name from being
/// mistaken for a file. `show README.md install` in a directory that also
/// holds an `install` shell script must still search for the *symbol*
/// `install`; requiring the candidate to be parseable makes that the
/// default rather than a coin flip.
pub fn is_target(arg: &Path) -> bool {
    if arg.is_dir() {
        return true;
    }
    if arg.exists() {
        return crate::main_helpers::can_parse_for_hook(arg);
    }
    looks_like_glob(arg) && !crate::path_glob::expand_pattern(arg).is_empty()
}

/// Split `show`'s positionals into leading targets and trailing symbols.
///
/// The grammar is `show <target>... <symbol>...`: consume targets while the
/// argument looks like one, and everything after the first non-target is a
/// symbol. That single rule covers all four spellings — one file, several
/// files (an expanded glob), a directory, a quoted glob — without the caller
/// having to say which they meant.
///
/// With one guard. The grammar needs at least one symbol, so the *final*
/// argument is target-eligible only when every argument is an existing file —
/// the signature of a shell-expanded glob whose symbol got swallowed
/// (`show src/*.cs`), which must be reported rather than guessed at. Any
/// other final argument stays a symbol, which is what keeps a name that
/// collides with a directory readable: `tests`, `core`, `utils` and `api` are
/// as common as symbol names as they are as directory names, and without the
/// guard `show src/core.rs tests` consumed its own symbol and failed with no
/// way to spell the query differently.
pub fn partition(args: &[String]) -> (Vec<PathBuf>, Vec<String>) {
    let expanded_glob = !args.is_empty()
        && args.iter().all(|a| {
            let p = Path::new(a);
            p.is_file() && is_target(p)
        });
    let ceiling = if expanded_glob {
        args.len()
    } else {
        args.len().saturating_sub(1)
    };

    let mut targets: Vec<PathBuf> = Vec::new();
    for arg in &args[..ceiling] {
        let p = PathBuf::from(arg);
        if !is_target(&p) {
            break;
        }
        targets.push(p);
    }
    let symbols = args[targets.len()..].to_vec();
    (targets, symbols)
}

/// The first "symbol" that is really a path the caller got wrong, if any.
///
/// Partition classifies anything that isn't a usable target as a symbol, which
/// silently rewrites a mistyped path into an absent symbol: `show a.rs
/// missing.rs greet` would answer "no symbol matching 'missing.rs'". Naming
/// the argument as a path restores the right diagnosis — exit 2, path not
/// found, with a repair hint — instead of a confident claim about a symbol
/// nobody asked for.
pub fn misread_path(symbols: &[String]) -> Option<&str> {
    symbols
        .iter()
        .find(|s| {
            let p = Path::new(s.as_str());
            !p.exists() && looks_like_path(p)
        })
        .map(|s| s.as_str())
}

/// Parse every target into a `ParseResult`, deduped and in path order.
///
/// Explicitly named files are parsed directly rather than walked: a file the
/// caller typed out is a file they want, even when the ignore rules would
/// have skipped it (`show target/generated.rs Sym`). Directories and globs
/// go through the shared walker, so they honour `.gitignore` and the rest of
/// the filter pipeline exactly like `map` does.
///
/// When both routes reach the same file the explicit parse is the one kept —
/// it is inserted first and the dedup keeps the first spelling of each file —
/// so naming a file alongside a directory that contains it cannot make the
/// ignore rules hide it.
pub fn collect(targets: &[PathBuf]) -> Vec<ParseResult> {
    let mut explicit: Vec<&PathBuf> = Vec::new();
    let mut walked: Vec<PathBuf> = Vec::new();
    for t in targets {
        if t.is_dir() || !t.exists() {
            walked.push(t.clone());
        } else {
            explicit.push(t);
        }
    }

    // `walk_and_parse` dedups within its own walk; this set is what spans the
    // two routes, which no single walk can see. Same identity key either way:
    // a path as spelled is not a path as it exists on disk, so
    // `show src/a.rs ./src Sym` reaches one file twice and used to answer as
    // if it were two. See [`crate::file_filter::file_identity`].
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut results: Vec<ParseResult> = Vec::new();
    for f in explicit {
        if let Some(r) = crate::parse_file(f) {
            if seen.insert(crate::file_filter::file_identity(&r.path)) {
                results.push(r);
            }
        }
    }
    if !walked.is_empty() {
        for r in crate::walk_and_parse(&walked, None) {
            if seen.insert(crate::file_filter::file_identity(&r.path)) {
                results.push(r);
            }
        }
    }

    results.sort_by(|a, b| a.path.cmp(&b.path));
    results
}

/// Match `symbols` against every parsed file and assemble the [`Outcome`].
///
/// `limit` caps rendered bodies in multi-target mode only; `total` still
/// counts every match, so a capped answer reports what it left out.
pub fn resolve(
    results: Vec<ParseResult>,
    symbols: &[String],
    limit: usize,
    multi: bool,
) -> Outcome {
    let files_scanned = results.len();
    let mut matched_symbols: Vec<bool> = vec![false; symbols.len()];
    let mut files: Vec<FileMatches> = Vec::new();
    let mut total = 0usize;

    for res in results {
        // Dedupe per file: overlapping suffixes (`greet` and `A.greet`) can
        // name the same declaration twice.
        let mut seen = std::collections::HashSet::new();
        let mut matches: Vec<SymbolMatch> = Vec::new();
        for (i, sym) in symbols.iter().enumerate() {
            let found = crate::core::find_symbols(&res, sym);
            if !found.is_empty() {
                matched_symbols[i] = true;
            }
            for m in found {
                let key = (m.start_line, m.end_line, m.qualified_name.clone());
                if seen.insert(key) {
                    matches.push(m);
                }
            }
        }
        if !matches.is_empty() {
            total += matches.len();
            files.push(FileMatches {
                result: res,
                matches,
            });
        }
    }

    let unmatched: Vec<String> = symbols
        .iter()
        .zip(matched_symbols.iter())
        .filter(|(_, ok)| !**ok)
        .map(|(s, _)| s.clone())
        .collect();

    // The cap governs display, not the search: every file was searched, so
    // `total` and `files_matched` are exact even when the render stops early.
    let files_matched = files.len();
    let cap = if multi { limit } else { usize::MAX };
    let mut budget = cap;
    let mut shown = 0usize;
    for f in &mut files {
        if f.matches.len() > budget {
            f.matches.truncate(budget);
        }
        shown += f.matches.len();
        budget -= f.matches.len();
    }
    files.retain(|f| !f.matches.is_empty());

    Outcome {
        files,
        files_scanned,
        files_matched,
        total,
        shown,
        truncated: shown < total,
        unmatched,
        symbols: symbols.to_vec(),
        multi,
    }
}

/// The coverage header for a multi-target answer: how many matches, across
/// how much ground, and whether the display was capped.
pub fn coverage_header(o: &Outcome) -> String {
    let mut s = format!(
        "# {} match(es) for '{}' in {} of {} file(s) searched",
        o.total,
        o.symbols.join(", "),
        o.files_matched(),
        o.files_scanned,
    );
    if o.truncated {
        s.push_str(&format!(
            " (showing {}; raise --limit to see the rest)",
            o.shown
        ));
    }
    s
}

/// Text rendering. Every body carries its own `path:start-end` header, so a
/// multi-file answer is self-describing line by line.
pub fn render_text(o: &Outcome) -> String {
    let mut out = String::new();
    if o.multi {
        out.push_str(&coverage_header(o));
        out.push('\n');
    }
    for f in &o.files {
        for m in &f.matches {
            out.push_str(&format!(
                "# {}:{}-{} {} ({})\n",
                f.result.path.display(),
                m.start_line,
                m.end_line,
                m.qualified_name,
                m.kind
            ));
            if !m.ancestor_signatures.is_empty() {
                out.push_str(&format!("# in: {}\n", m.ancestor_signatures.join(" → ")));
            }
            out.push_str(&m.source);
            out.push('\n');
        }
    }
    out
}

/// JSON rendering (`ast-bro.show.v2`).
///
/// The shape is a `files` array even for a single file, matching
/// `ast-bro.map.v1` — one shape for one schema, rather than a payload whose
/// keys depend on how many files the caller happened to name. The version
/// bump from v1 (top-level `path` / `language` / `matches`) is what tells a
/// consumer which of the two it is looking at. The coverage counters that
/// the text header carries are fields here for the same reason.
pub fn render_json(o: &Outcome, pretty: bool) -> String {
    let files: Vec<serde_json::Value> = o
        .files
        .iter()
        .map(|f| {
            serde_json::json!({
                "path": f.result.path.to_string_lossy(),
                "language": f.result.language,
                "matches": &f.matches,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": crate::core::JSON_SCHEMA_SHOW,
        "symbols": o.symbols,
        "files_scanned": o.files_scanned,
        "files_matched": o.files_matched(),
        "total": o.total,
        "shown": o.shown,
        "truncated": o.truncated,
        "unmatched": o.unmatched,
        "files": files,
    });
    if pretty {
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string())
    } else {
        doc.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn partition_splits_one_file_and_one_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn greet() {}\n").unwrap();
        let (t, s) = partition(&args(&[f.to_str().unwrap(), "greet"]));
        assert_eq!(t.len(), 1);
        assert_eq!(s, vec!["greet".to_string()]);
    }

    #[test]
    fn partition_recovers_a_shell_expanded_glob() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "fn greet() {}\n").unwrap();
        std::fs::write(&b, "fn greet() {}\n").unwrap();
        let (t, s) = partition(&args(&[a.to_str().unwrap(), b.to_str().unwrap(), "greet"]));
        assert_eq!(t.len(), 2, "both files belong in the target slot");
        assert_eq!(s, vec!["greet".to_string()]);
    }

    #[test]
    fn partition_treats_a_directory_as_a_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn greet() {}\n").unwrap();
        let (t, s) = partition(&args(&[dir.path().to_str().unwrap(), "greet"]));
        assert_eq!(t.len(), 1);
        assert_eq!(s, vec!["greet".to_string()]);
    }

    #[test]
    fn partition_keeps_a_symbol_that_shares_a_directorys_name() {
        // `tests`, `core`, `utils`, `api` are as common as symbol names as
        // they are as directory names. The final argument must stay a symbol
        // unless *every* argument is a file (the expanded-glob signature).
        let dir = tempfile::tempdir().unwrap();
        let code = dir.path().join("a.rs");
        std::fs::write(&code, "mod tests {}\n").unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        let collide = dir.path().join("tests");
        let (t, s) = partition(&args(&[code.to_str().unwrap(), collide.to_str().unwrap()]));
        assert_eq!(t.len(), 1, "the directory must not eat the symbol slot");
        assert_eq!(s.len(), 1);
        assert!(s[0].ends_with("tests"));
    }

    #[test]
    fn partition_still_takes_a_leading_directory_when_a_symbol_follows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn greet() {}\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let (t, s) = partition(&args(&[
            dir.path().to_str().unwrap(),
            dir.path().join("sub").to_str().unwrap(),
            "greet",
        ]));
        assert_eq!(t.len(), 2, "two directories, then the symbol");
        assert_eq!(s, vec!["greet".to_string()]);
    }

    #[test]
    fn partition_keeps_an_unparseable_existing_file_out_of_the_target_slot() {
        // A symbol name that happens to collide with a file in the cwd must
        // stay a symbol: only parseable files are targets.
        let dir = tempfile::tempdir().unwrap();
        let code = dir.path().join("a.rs");
        std::fs::write(&code, "fn install() {}\n").unwrap();
        let collide = dir.path().join("install");
        std::fs::write(&collide, "not source, no shebang\n").unwrap();
        let (t, s) = partition(&args(&[code.to_str().unwrap(), collide.to_str().unwrap()]));
        assert_eq!(t.len(), 1);
        assert_eq!(s.len(), 1, "the colliding path stays a symbol");
    }

    #[test]
    fn partition_yields_no_symbol_when_the_expansion_ate_it() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        std::fs::write(&a, "fn greet() {}\n").unwrap();
        let (t, s) = partition(&args(&[a.to_str().unwrap()]));
        assert_eq!(t.len(), 1);
        assert!(s.is_empty(), "caller must reject this, not guess a symbol");
    }

    #[test]
    fn resolve_finds_the_symbol_in_every_file_not_just_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        let b = dir.path().join("b.rs");
        std::fs::write(&a, "fn greet() { }\n").unwrap();
        std::fs::write(&b, "fn greet() { }\n").unwrap();
        let results = collect(&[a, b]);
        let o = resolve(results, &["greet".to_string()], DEFAULT_LIMIT, true);
        assert_eq!(o.files_scanned, 2);
        assert_eq!(o.total, 2, "the second definition must not be dropped");
        assert_eq!(o.files_matched(), 2);
        assert!(o.unmatched.is_empty());
        assert!(!o.truncated);
    }

    #[test]
    fn resolve_caps_the_display_and_reports_the_true_total() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{}.rs", i)), "fn greet() { }\n").unwrap();
        }
        let results = collect(&[dir.path().to_path_buf()]);
        let o = resolve(results, &["greet".to_string()], 2, true);
        assert_eq!(o.total, 5, "total counts the full search, not the display");
        assert_eq!(o.shown, 2);
        assert!(o.truncated);
        assert_eq!(
            o.files_matched(),
            5,
            "coverage must not shrink to the capped display"
        );
        assert!(coverage_header(&o).contains("raise --limit"));
    }

    #[test]
    fn resolve_never_caps_single_file_mode() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        std::fs::write(&a, "fn one() {}\nfn two() {}\nfn three() {}\n").unwrap();
        let results = collect(&[a]);
        let o = resolve(
            results,
            &["one".to_string(), "two".to_string(), "three".to_string()],
            1,
            false,
        );
        assert_eq!(o.shown, 3);
        assert!(!o.truncated);
    }

    #[test]
    fn resolve_reports_a_symbol_that_matched_nowhere() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        std::fs::write(&a, "fn greet() {}\n").unwrap();
        let results = collect(&[a]);
        let o = resolve(
            results,
            &["greet".to_string(), "absent".to_string()],
            DEFAULT_LIMIT,
            false,
        );
        assert_eq!(o.unmatched, vec!["absent".to_string()]);
        assert_eq!(o.total, 1);
    }

    #[test]
    fn collect_expands_a_quoted_glob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn greet() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn greet() {}\n").unwrap();
        std::fs::write(dir.path().join("c.py"), "def greet(): pass\n").unwrap();
        let pattern = dir.path().join("*.rs");
        assert!(is_target(&pattern), "a matching glob is a valid target");
        let results = collect(&[pattern]);
        assert_eq!(results.len(), 2, "only the .rs files");
    }

    #[test]
    fn collect_dedups_the_same_file_spelled_two_ways() {
        // `<dir>/a.rs` and `<dir>/sub/../a.rs` are one file. `PathBuf` equality
        // compares components, so it folds away an *interior* `.` on its own
        // but keeps `..` (and a leading `./`) — those spellings compared
        // unequal, and the file was parsed, counted and rendered twice.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn greet() {}\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let bare = dir.path().join("a.rs");
        let traversed = dir.path().join("sub").join("..").join("a.rs");
        assert_ne!(bare, traversed, "the two spellings must compare unequal");

        let results = collect(&[bare, traversed]);
        assert_eq!(results.len(), 1, "one file, one parse");
        let o = resolve(results, &["greet".to_string()], DEFAULT_LIMIT, true);
        assert_eq!(o.files_scanned, 1);
        assert_eq!(o.total, 1);
        assert_eq!(o.files_matched(), 1);
        assert_eq!(o.shown, 1);
        assert_eq!(render_text(&o).matches("fn greet").count(), 1);
    }

    #[test]
    fn collect_dedups_an_explicit_file_against_a_differently_spelled_directory() {
        // The overlap that motivated the fix: an explicit file plus a
        // directory that contains it, reached by two spellings. Both the
        // duplicate body and the inflated `files_scanned` have to go.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn greet() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn other() {}\n").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let explicit = dir.path().join("a.rs");
        let walked = dir.path().join("sub").join("..");

        let results = collect(&[explicit.clone(), walked]);
        assert_eq!(results.len(), 2, "two files on disk, two results");
        assert_eq!(
            results.iter().filter(|r| r.path == explicit).count(),
            1,
            "the explicit spelling is the one kept"
        );
        let o = resolve(results, &["greet".to_string()], DEFAULT_LIMIT, true);
        assert_eq!(o.files_scanned, 2, "coverage must not count a.rs twice");
        assert_eq!(o.total, 1);
        assert!(!o.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn collect_dedups_the_same_file_reached_through_a_symlink() {
        // The case that decides the implementation: no amount of string
        // surgery on `link/a.rs` reveals that it is `real/a.rs`. Only asking
        // the filesystem does.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("a.rs"), "fn greet() {}\n").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let direct = real.join("a.rs");
        let results = collect(&[direct.clone(), link.join("a.rs")]);
        assert_eq!(results.len(), 1, "one file, two routes to it");
        assert_eq!(
            results[0].path, direct,
            "the first spelling given is the one displayed"
        );
    }

    #[test]
    fn json_carries_coverage_and_the_v2_schema() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.rs");
        std::fs::write(&a, "fn greet() {}\n").unwrap();
        let o = resolve(collect(&[a]), &["greet".to_string()], DEFAULT_LIMIT, true);
        let v: serde_json::Value = serde_json::from_str(&render_json(&o, false)).unwrap();
        assert_eq!(v["schema"], "ast-bro.show.v2");
        assert_eq!(v["files_scanned"], 1);
        assert_eq!(v["total"], 1);
        assert_eq!(v["files"][0]["language"], "rust");
    }
}
