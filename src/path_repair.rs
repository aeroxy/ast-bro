//! Repair hints for a path that isn't where the caller said it was.
//!
//! A rejected path already gets a clear diagnosis ("`map` resolved 0 of 1
//! path(s); nothing was inspected"), which tells the caller *that* they were
//! wrong but not what to type instead — so the next move is a blind retry or
//! a detour through `ls` / `find`. These hints close that gap for the two
//! mistakes that are actually verifiable from disk:
//!
//! 1. **An unquoted path with spaces**, split into several arguments by the
//!    shell (`map The Sorting Bureau/spaced.py`). Rejoining a leading run of
//!    the arguments reproduces a real path, so the repair is certain.
//! 2. **A file that exists somewhere else** — right name, wrong directory.
//!    Found by a bounded walk over the same filter pipeline the commands
//!    use, so it never suggests something `map` would refuse to read.
//!
//! Only verifiable repairs are offered. A wrong suggestion costs more than
//! no suggestion — it sends the caller down a second dead end and reads as
//! authoritative — so when nothing on disk corroborates a guess, the hint is
//! omitted and the plain diagnosis stands.

use std::path::{Path, PathBuf};

/// Cap on suggested locations for one basename. Past a handful the list
/// stops being a repair and starts being a search result.
const MAX_CANDIDATES: usize = 5;

/// Cap on directory entries visited while looking for a basename. A repair
/// hint is a courtesy on an already-failed call; it must not turn a typo
/// into a full-repository walk.
const MAX_ENTRIES: usize = 20_000;

/// Cap on how many of the missing arguments get a location lookup. Each is one
/// bounded walk, and past a few the hints crowd out the diagnosis itself.
const MAX_REPAIRED_ARGS: usize = 3;

/// Build a hint for a failed path list, or `None` when nothing on disk
/// corroborates a repair. The returned string may span several lines — the
/// error renderer indents each one.
pub fn hints(args: &[String]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();

    if let Some(joined) = rejoined(args) {
        lines.push(format!(
            "that path exists when the arguments are rejoined — quote it: \"{}\"",
            joined
        ));
    }

    // Look up each missing argument, capped: three repairs still read as
    // repairs, thirty read as a wall of text hiding the diagnosis. Each costs
    // one bounded walk, which is why the list is capped rather than unbounded.
    for arg in args.iter().take(MAX_REPAIRED_ARGS) {
        let p = Path::new(arg);
        if p.exists() {
            continue;
        }
        if let Some(found) = elsewhere(p) {
            let prefix = if args.len() > 1 {
                format!("{}: ", arg)
            } else {
                String::new()
            };
            if found.len() == 1 {
                lines.push(format!("{}found the same file name at: {}", prefix, found[0]));
            } else {
                lines.push(format!(
                    "{}that file name exists at: {} (pick one)",
                    prefix,
                    found.join(", ")
                ));
            }
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// The hint for a single missing path, used for a *partial* miss where the
/// command still has other paths to work with. Skips the rejoin check: with
/// something else resolving, an unquoted-spaces split is not the story.
pub fn hint_for_one(missing: &str) -> Option<String> {
    let found = elsewhere(Path::new(missing))?;
    Some(if found.len() == 1 {
        format!("found the same file name at: {}", found[0])
    } else {
        format!("that file name exists at: {} (pick one)", found.join(", "))
    })
}

/// If a leading run of `args` rejoins into an existing path, return it.
/// Longest run first, so `show "The Sorting Bureau/x.py" Symbol` reports the
/// three-argument path rather than stopping at a two-argument prefix that
/// happens to exist as a directory.
fn rejoined(args: &[String]) -> Option<String> {
    if args.len() < 2 {
        return None;
    }
    for end in (2..=args.len()).rev() {
        let candidate = args[..end].join(" ");
        let p = Path::new(&candidate);
        // A rejoined *glob* counts too: `map The Sorting Bureau/*.py` unquoted
        // arrives split on spaces with the pattern intact but unmatched, so
        // the literal path never exists and only expansion proves the repair.
        if p.exists() || !crate::path_glob::expand_pattern(p).is_empty() {
            return Some(candidate);
        }
    }
    None
}

/// Search the project for files sharing `missing`'s basename, returning
/// posix-spelled paths relative to the search root. Bounded by
/// [`MAX_ENTRIES`] and [`MAX_CANDIDATES`].
fn elsewhere(missing: &Path) -> Option<Vec<String>> {
    let name = missing.file_name()?;
    // A bare *word* with no extension is far more likely a mistyped symbol or
    // subcommand than a misplaced file, and a coincidental match there points
    // the caller further from the real fix. An extensionless name that carries
    // a directory separator is unambiguously a path, though, so
    // `build/Dockerfile` and `docs/LICENSE` still get their repair.
    let spelled_as_path = missing
        .parent()
        .is_some_and(|p| !p.as_os_str().is_empty());
    if missing.extension().is_none() && !spelled_as_path {
        return None;
    }

    let root = search_root(missing);
    let mut builder = ignore::WalkBuilder::new(&root);
    builder.hidden(false);
    crate::file_filter::add_filters(&mut builder, &root);

    let mut found: Vec<String> = Vec::new();
    let mut visited = 0usize;
    for entry in builder.build() {
        visited += 1;
        if visited > MAX_ENTRIES {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.file_name() != Some(name) {
            continue;
        }
        if crate::file_filter::should_skip_path(path, &root) {
            continue;
        }
        found.push(display_path(path, &root));
        if found.len() >= MAX_CANDIDATES {
            break;
        }
    }

    if found.is_empty() {
        None
    } else {
        Some(found)
    }
}

/// Spell a found path so the caller can paste it straight back.
///
/// Relative to the current directory first — a hint is a command fragment, and
/// a path relative to the *project root* is not one when the shell sits in a
/// subdirectory. Project-relative is the fallback (shorter and still
/// meaningful when the root is elsewhere), absolute the last resort.
fn display_path(path: &Path, root: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::project_root::relative_posix(path, &cwd))
        .or_else(|| crate::project_root::relative_posix(path, root))
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"))
}

/// Where to look for a misplaced file: the project root when the argument
/// sits inside a project, else the current directory. Anchoring at the root
/// is what lets `map src/typo/file.rs` find `lib/file.rs` — a search from
/// the argument's own (non-existent) parent would find nothing.
fn search_root(missing: &Path) -> PathBuf {
    // Climb to the nearest ancestor that does exist, then let the usual root
    // detection take over from there. Anchoring at the argument's own parent
    // would find nothing precisely when the parent is the part that's wrong
    // (`map src/typo/file.rs`), and anchoring at the cwd would search an
    // unrelated project when the argument points outside it.
    let mut cur = missing.parent();
    while let Some(p) = cur {
        if !p.as_os_str().is_empty() && p.exists() {
            return crate::project_root::find_root_for(p).unwrap_or_else(|_| p.to_path_buf());
        }
        cur = p.parent();
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rejoins_an_unquoted_path_with_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("The Sorting Bureau");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("spaced.py"), "def f(): pass\n").unwrap();

        // The shell hands us three arguments for one path.
        let split = args(&[
            dir.path().join("The").to_str().unwrap(),
            "Sorting",
            "Bureau/spaced.py",
        ]);
        let hint = hints(&split).expect("a rejoinable path must be reported");
        assert!(hint.contains("rejoined"), "hint was: {}", hint);
        assert!(hint.contains("spaced.py"), "hint was: {}", hint);
    }

    #[test]
    fn finds_the_same_file_name_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("lib/widget.rs"), "fn a() {}\n").unwrap();
        // Mark the tempdir as a project root so the search anchors there.
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();

        let wrong = dir.path().join("src/widget.rs");
        let hint = hints(&args(&[wrong.to_str().unwrap()])).expect("a relocated file is a repair");
        assert!(hint.contains("widget.rs"), "hint was: {}", hint);
        assert!(hint.contains("lib/"), "hint was: {}", hint);
    }

    #[test]
    fn stays_quiet_when_nothing_corroborates_a_repair() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let nowhere = dir.path().join("no_such_file_anywhere.rs");
        assert!(hints(&args(&[nowhere.to_str().unwrap()])).is_none());
    }

    #[test]
    fn does_not_guess_for_an_extensionless_bare_word() {
        // `map outlien` is a mistyped subcommand, not a misplaced file; a
        // coincidental hit on some extensionless `outlien` would mislead.
        assert!(elsewhere(Path::new("outlien")).is_none());
    }

    #[test]
    fn hint_for_one_skips_the_rejoin_branch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("lib/thing.rs"), "fn a() {}\n").unwrap();
        let missing = dir.path().join("src/thing.rs");
        let hint = hint_for_one(missing.to_str().unwrap()).expect("relocated file");
        assert!(hint.contains("thing.rs"));
    }
}
