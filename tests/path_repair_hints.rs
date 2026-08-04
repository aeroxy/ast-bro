//! A path that isn't where the caller said it was now tries to repair
//! itself — but only when the repair is verifiable on disk. A wrong
//! suggestion costs more than none: it reads as authoritative and sends the
//! caller down a second dead end.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("utf8"),
        String::from_utf8(out.stderr).expect("utf8"),
    )
}

/// A project root (so the search anchors inside the fixture) holding
/// `lib/widget.rs` and `The Sorting Bureau/spaced.py`.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"fx\"\n").unwrap();
    std::fs::create_dir(dir.path().join("lib")).unwrap();
    std::fs::write(dir.path().join("lib/widget.rs"), "pub fn a() {}\n").unwrap();
    std::fs::create_dir(dir.path().join("The Sorting Bureau")).unwrap();
    std::fs::write(
        dir.path().join("The Sorting Bureau/spaced.py"),
        "def f():\n    pass\n",
    )
    .unwrap();
    dir
}

#[test]
fn an_unquoted_path_with_spaces_is_offered_back_quoted() {
    let dir = fixture();
    // The shell split one path into three arguments.
    let first = dir.path().join("The");
    let (code, stdout, stderr) = run(&[
        "map",
        first.to_str().unwrap(),
        "Sorting",
        "Bureau/spaced.py",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("rejoined") && stderr.contains("The Sorting Bureau/spaced.py"),
        "expected a pasteable rejoined path:\n{stderr}"
    );
}

#[test]
fn a_relocated_file_is_pointed_at() {
    let dir = fixture();
    let wrong = dir.path().join("src/widget.rs");
    let (code, _, stderr) = run(&["map", wrong.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("lib/widget.rs"),
        "expected the real location:\n{stderr}"
    );
}

#[test]
fn nothing_corroborating_means_no_hint() {
    let dir = fixture();
    let nowhere = dir.path().join("src/absent_everywhere.rs");
    let (code, _, stderr) = run(&["map", nowhere.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("found the same file name"),
        "a guess with no evidence must not be offered:\n{stderr}"
    );
}

#[test]
fn an_extensionless_bare_word_gets_no_file_guess() {
    // `map mapp` is a mistyped subcommand or symbol, not a misplaced file.
    let (code, _, stderr) = run(&["map", "zznotasubcommand"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("found the same file name"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn a_rejoined_glob_counts_as_a_repair() {
    // Unquoted `map The Sorting Bureau/*.py` arrives split on spaces with the
    // pattern intact but unmatched, so only expansion proves the repair.
    let dir = fixture();
    let first = dir.path().join("The");
    let (code, _, stderr) = run(&[
        "map",
        first.to_str().unwrap(),
        "Sorting",
        "Bureau/*.py",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("rejoined") && stderr.contains("Bureau/*.py"),
        "expected the rejoined glob:\n{stderr}"
    );
}

#[test]
fn an_extensionless_path_with_a_separator_still_gets_a_repair() {
    // `Dockerfile` / `Makefile` / `LICENSE` have no extension, but spelled with
    // a directory they are unambiguously paths, not mistyped bare words.
    let dir = fixture();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/Dockerfile"), "FROM scratch\n").unwrap();
    let wrong = dir.path().join("deploy/Dockerfile");
    let (code, _, stderr) = run(&["map", wrong.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("docs/Dockerfile"),
        "expected the real location:\n{stderr}"
    );
}

#[test]
fn every_missing_path_gets_its_own_repair() {
    let dir = fixture();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/notes.rs"), "fn n() {}\n").unwrap();
    let a = dir.path().join("src/widget.rs");
    let b = dir.path().join("src/notes.rs");
    let (code, _, stderr) = run(&["map", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("lib/widget.rs") && stderr.contains("docs/notes.rs"),
        "both misses deserve their repair:\n{stderr}"
    );
}

#[test]
fn a_repair_is_not_offered_for_an_ignored_location() {
    // `build/` is on the hardcoded ignore list, so a file found only there is
    // one the tool would refuse to read — suggesting it would be a dead end.
    let dir = fixture();
    std::fs::create_dir(dir.path().join("build")).unwrap();
    std::fs::write(dir.path().join("build/generated.rs"), "fn g() {}\n").unwrap();
    let wrong = dir.path().join("src/generated.rs");
    let (code, _, stderr) = run(&["map", wrong.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("build/generated.rs"),
        "an unwalkable location is not a repair:\n{stderr}"
    );
}

#[test]
fn squeeze_also_repairs_its_path() {
    let dir = fixture();
    let wrong = dir.path().join("src/widget.rs");
    let (code, _, stderr) = run(&["squeeze", wrong.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("lib/widget.rs"),
        "squeeze names one explicit file; repair it too:\n{stderr}"
    );
}

#[test]
fn a_partial_miss_keeps_the_result_on_stdout_and_the_hint_on_stderr() {
    let dir = fixture();
    let good = dir.path().join("lib/widget.rs");
    let wrong = dir.path().join("src/widget.rs");
    let (code, stdout, stderr) = run(&["map", good.to_str().unwrap(), wrong.to_str().unwrap()]);
    assert_eq!(code, Some(0), "a partial miss is still an answer:\n{stderr}");
    assert!(stdout.contains("widget.rs"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("path not found") && stderr.contains("# hint:"),
        "the miss and its repair belong on stderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("# hint:"),
        "hints must never enter the result stream:\n{stdout}"
    );
}
