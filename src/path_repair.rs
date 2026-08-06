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

    // A leading run that rejoins into a real path is *one* path the shell
    // split, not several missing arguments, so its fragments are excluded from
    // the lookups below. Left in, `Bureau/spaced.py` gets a basename walk of
    // its own and reports the very file the rejoin line just named — two
    // competing diagnoses of one mistake, the second built from a string the
    // caller never typed as an argument.
    let consumed = match rejoined(args) {
        Some((joined, end)) => {
            lines.push(format!(
                "that path exists when the arguments are rejoined — quote it: \"{}\"",
                joined
            ));
            end
        }
        None => 0,
    };

    // Look up each missing argument, capped: three repairs still read as
    // repairs, thirty read as a wall of text hiding the diagnosis. Each costs
    // one bounded walk, which is why the list is capped rather than unbounded.
    //
    // The filter precedes the cap because the cap counts *lookups*. Callers
    // hand over the whole argument list — `show` passes every positional,
    // existing files and the symbol included — so capping first lets args
    // that need no repair spend the budget and starve the one that does.
    for arg in args[consumed..]
        .iter()
        .filter(|a| !Path::new(a.as_str()).exists())
        .take(MAX_REPAIRED_ARGS)
    {
        let p = Path::new(arg);
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

/// If a leading run of `args` rejoins into an existing path, return it and how
/// many arguments it consumed — the count is what lets the caller stop treating
/// those fragments as arguments of their own.
///
/// Longest run first, so `show "The Sorting Bureau/x.py" Symbol` reports the
/// three-argument path rather than stopping at a two-argument prefix that
/// happens to exist as a directory.
fn rejoined(args: &[String]) -> Option<(String, usize)> {
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
            return Some((candidate, end));
        }
    }
    None
}

/// Search the project for files sharing `missing`'s basename, returning
/// posix-spelled paths relative to the search root. Bounded by
/// [`MAX_ENTRIES`] and [`MAX_CANDIDATES`].
fn elsewhere(missing: &Path) -> Option<Vec<String>> {
    let name = missing.file_name()?;
    // A glob that matched nothing is not a misplaced file: this walk compares
    // basenames literally, so `*.rs` can only ever match a file *named* `*.rs`.
    // Scanning up to MAX_ENTRIES for that is a certain miss, and the caller's
    // real fix is a different pattern or a different directory, neither of
    // which a basename lookup can suggest. `rejoined` still handles the split
    // glob (`map The Sorting Bureau/*.py`), where expansion proves the repair.
    if crate::show::looks_like_glob(Path::new(name)) {
        return None;
    }
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
    // A relative argument is spelled from the cwd, so the cwd is the caller's
    // frame of reference and the search must never be narrower than it.
    // Anchoring such an argument on its nearest existing ancestor was: in a
    // directory with no project marker, `show src/c.rs` searched only `src/`,
    // so a `lib/c.rs` one directory over — plainly inside the tree the caller
    // was working in — produced no hint at all. Silence there reads as "that
    // file is nowhere", not "I looked in one subdirectory", which is the exact
    // misreading the hint exists to prevent. `find_root_for` still widens to
    // the enclosing project when a marker or `.git` exists; with none it
    // returns the directory it was handed, which is what kept the narrow
    // anchor invisible.
    //
    // Absolute arguments keep the ancestor anchor: `/elsewhere/x.rs` says
    // nothing about where the shell sits, and searching the cwd for it would
    // offer a file from an unrelated project.
    if missing.is_relative() {
        if let Ok(cwd) = std::env::current_dir() {
            return crate::project_root::find_root_for(&cwd).unwrap_or(cwd);
        }
    }
    // Climb to the nearest ancestor that does exist, then let the usual root
    // detection take over from there. Anchoring at the argument's own parent
    // would find nothing precisely when the parent is the part that's wrong
    // (`map /abs/src/typo/file.rs`).
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
        // One mistake, one diagnosis: the tail fragment must not also be
        // reported as a misplaced file naming the path just named above.
        assert_eq!(
            hint.lines().count(),
            1,
            "the rejoined fragments must not get lookups of their own: {}",
            hint
        );
    }

    #[test]
    fn an_argument_past_the_rejoined_run_still_gets_its_lookup() {
        // Only the run the rejoin consumed is exempt. A genuinely separate
        // missing argument after it is a second mistake and keeps its repair.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("The Sorting Bureau");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("spaced.py"), "def f(): pass\n").unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("lib/widget.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();

        let split = args(&[
            dir.path().join("The").to_str().unwrap(),
            "Sorting",
            "Bureau/spaced.py",
            dir.path().join("src/widget.rs").to_str().unwrap(),
        ]);
        let hint = hints(&split).expect("two repairs");
        assert!(hint.contains("rejoined"), "hint was: {}", hint);
        assert!(hint.contains("lib/widget.rs"), "hint was: {}", hint);
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
    fn existing_args_do_not_spend_the_repair_budget() {
        // `show Cargo.toml a.md b.md src/typo.rs Sym` reaches `hints` with the
        // whole positional list. The three existing files must not consume the
        // cap and leave the one repairable argument unlooked-up.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("a.md"), "# a\n").unwrap();
        std::fs::write(dir.path().join("b.md"), "# b\n").unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();
        std::fs::write(dir.path().join("lib/widget.rs"), "fn a() {}\n").unwrap();

        let present = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
        let list = vec![
            present("Cargo.toml"),
            present("a.md"),
            present("b.md"),
            dir.path().join("src/widget.rs").to_str().unwrap().to_string(),
            "Sym".to_string(),
        ];
        let hint = hints(&list).expect("the fourth argument is still repairable");
        assert!(hint.contains("widget.rs"), "hint was: {}", hint);
        assert!(hint.contains("lib/"), "hint was: {}", hint);
    }

    #[test]
    fn does_not_scan_for_a_glob_basename() {
        // `show 'src/*.nope' Sym` reaches here whenever the pattern matched
        // nothing. A basename walk can only find a file literally named
        // `*.nope`, so it must not run at all.
        assert!(elsewhere(Path::new("src/*.nope")).is_none());
        assert!(elsewhere(Path::new("*.rs")).is_none());
        assert!(elsewhere(Path::new("src/widget[1].rs")).is_none());
        assert!(hints(&args(&["src/*.nope"])).is_none());
    }

    #[test]
    fn a_split_glob_still_gets_its_rejoin() {
        // The guard above must not silence the one glob repair that *is*
        // verifiable: an unquoted pattern the shell split on spaces.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("The Sorting Bureau");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("spaced.py"), "def f(): pass\n").unwrap();

        let split = args(&[
            dir.path().join("The").to_str().unwrap(),
            "Sorting",
            "Bureau/*.py",
        ]);
        let hint = hints(&split).expect("an expandable rejoined glob is a repair");
        assert!(hint.contains("rejoined"), "hint was: {}", hint);
        assert!(hint.contains("Bureau/*.py"), "hint was: {}", hint);
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
