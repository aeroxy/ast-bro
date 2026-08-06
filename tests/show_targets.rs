//! `show` takes a target *list*: one file, several files, a directory, or a
//! quoted glob.
//!
//! The regression that motivated this: an unquoted `show src/*.java greet` is
//! expanded by the shell before the process starts, so the extra files landed
//! in the symbol slot. The symbol resolved against the first file only, its
//! definitions elsewhere were dropped, and the exit code stayed 0 — a silent
//! false negative. `first_file_only_is_not_an_answer` is that case.

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

/// Same, with `cwd` set. A relative target is the only way to spell a leading
/// `./`, and cwd is process-global — so the tests that need one drive it
/// through the subprocess rather than through `set_current_dir`, which would
/// race the rest of the suite.
fn run_in(cwd: &std::path::Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("utf8"),
        String::from_utf8(out.stderr).expect("utf8"),
    )
}

/// Two Java files that each declare `greet`, plus a third file that doesn't.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Greeter.java"),
        "public class Greeter {\n    public String greet() { return \"hi\"; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Second.java"),
        "public class Second {\n    public void greet() {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Unrelated.java"),
        "public class Unrelated {\n    public void other() {}\n}\n",
    )
    .unwrap();
    dir
}

#[test]
fn first_file_only_is_not_an_answer() {
    // The shell hands `show a.java b.java greet` to us; both definitions must
    // come back, and neither file may be reported as a missing symbol.
    let dir = fixture();
    let a = dir.path().join("Greeter.java");
    let b = dir.path().join("Second.java");
    let (code, stdout, stderr) = run(&["show", a.to_str().unwrap(), b.to_str().unwrap(), "greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("Greeter.greet") && stdout.contains("Second.greet"),
        "both definitions must be shown:\n{stdout}"
    );
    assert!(
        !stderr.contains("no symbol matching 'Second.java")
            && !stderr.contains("no symbol matching '/"),
        "a file argument must never be reported as a missing symbol:\n{stderr}"
    );
    assert!(
        stderr.contains("quote it"),
        "an expanded glob should be named as such:\n{stderr}"
    );
}

#[test]
fn quoted_glob_is_a_valid_target() {
    let dir = fixture();
    let pattern = dir.path().join("*.java");
    let (code, stdout, stderr) = run(&["show", pattern.to_str().unwrap(), "greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("Greeter.greet") && stdout.contains("Second.greet"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn directory_target_locates_the_symbol_and_reports_coverage() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&["show", dir.path().to_str().unwrap(), "greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("2 match(es) for 'greet' in 2 of 3 file(s) searched"),
        "coverage header missing or wrong:\n{stdout}"
    );
    assert!(
        stdout.contains("Greeter.greet") && stdout.contains("Second.greet"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn limit_caps_the_display_but_not_the_reported_total() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&[
        "show",
        dir.path().to_str().unwrap(),
        "greet",
        "--limit",
        "1",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    // The true total and the true matched-file count survive the cap; only
    // the rendered body count shrinks (issue #32).
    assert!(
        stdout.contains("2 match(es) for 'greet' in 2 of 3 file(s) searched"),
        "a capped answer must not understate its coverage:\n{stdout}"
    );
    assert!(
        stdout.contains("showing 1") && stdout.contains("raise --limit"),
        "truncation must be reported on stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("Second.greet"),
        "the cap should have trimmed the second body:\n{stdout}"
    );
}

#[test]
fn single_file_output_keeps_its_old_shape() {
    // No coverage header for a single explicit file: the caller named the
    // file, so there is no scope question to answer.
    let dir = fixture();
    let a = dir.path().join("Greeter.java");
    let (code, stdout, stderr) = run(&["show", a.to_str().unwrap(), "greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.starts_with("# "), "stdout:\n{stdout}");
    assert!(
        !stdout.contains("file(s) searched"),
        "single-file show must not grow a coverage header:\n{stdout}"
    );
}

#[test]
fn every_argument_a_file_is_rejected_not_guessed() {
    let dir = fixture();
    let a = dir.path().join("Greeter.java");
    let b = dir.path().join("Second.java");
    let (code, stdout, stderr) = run(&["show", a.to_str().unwrap(), b.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("no symbol"),
        "expected an explicit no-symbol rejection:\n{stderr}"
    );
}

#[test]
fn absent_symbol_in_a_directory_reports_the_scan_size() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&["show", dir.path().to_str().unwrap(), "ZzNoSuchZz"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("3 file(s) searched"),
        "a zero must say how much ground it covers:\n{stderr}"
    );
}

#[test]
fn absent_symbol_json_envelope_carries_files_scanned() {
    let dir = fixture();
    let (code, _, stderr) = run(&["show", dir.path().to_str().unwrap(), "ZzNoSuchZz", "--json"]);
    assert_eq!(code, Some(2));
    let line = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(line).expect("valid json");
    assert_eq!(doc["kind"], "symbol_not_found");
    assert_eq!(doc["files_scanned"], 3);
}

#[test]
fn json_is_show_v2_with_a_files_array() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&[
        "show",
        dir.path().to_str().unwrap(),
        "greet",
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["schema"], "ast-bro.show.v2");
    assert_eq!(doc["files_scanned"], 3);
    assert_eq!(doc["files_matched"], 2);
    assert_eq!(doc["total"], 2);
    assert_eq!(doc["truncated"], false);
    assert_eq!(doc["files"].as_array().expect("files array").len(), 2);
}

#[test]
fn a_symbol_that_collides_with_a_bare_file_stays_a_symbol() {
    // Run from inside the fixture, where a plain file named `greet` sits next
    // to the sources: `show Greeter.java greet` must still search for the
    // *symbol*. Only parseable files count as targets, which is what makes
    // that the default rather than a coin flip.
    let dir = fixture();
    std::fs::write(dir.path().join("greet"), "not source, no shebang\n").unwrap();
    let (code, stdout, stderr) = run_in(dir.path(), &["show", "Greeter.java", "greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("Greeter.greet"),
        "the colliding file name should have been read as the symbol:\n{stdout}"
    );
}

#[test]
fn a_symbol_colliding_with_a_parseable_file_fails_loudly_and_has_an_escape_hatch() {
    // The other side of `a_symbol_that_collides_with_a_bare_file_stays_a_symbol`:
    // when the colliding file *is* parseable (here by shebang), it is a target,
    // so every argument is a target and nothing is left to search for. That is
    // the documented tradeoff — guessing which argument was meant as the symbol
    // would be a coin flip — and it has to stay loud rather than silently
    // resolving against one file. `Type.method` is the way out, so the test
    // pins both halves.
    let dir = fixture();
    std::fs::write(
        dir.path().join("greet"),
        "#!/usr/bin/env python3\ndef greet(): pass\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_in(dir.path(), &["show", "Greeter.java", "greet"]);
    assert_eq!(code, Some(2), "stdout:\n{stdout}");
    assert!(stdout.is_empty(), "a rejection puts nothing on stdout:\n{stdout}");
    assert!(
        stderr.contains("file argument(s) and no symbol"),
        "the collision must be named, not guessed at:\n{stderr}"
    );
    assert!(
        stderr.contains("qualify it"),
        "the hint must name the escape hatch:\n{stderr}"
    );

    // And the escape hatch the hint names actually works.
    let (code, stdout, stderr) = run_in(dir.path(), &["show", "Greeter.java", "Greeter.greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("Greeter.greet"), "stdout:\n{stdout}");
}

#[test]
fn a_symbol_that_shares_a_directorys_name_is_still_a_symbol() {
    // `tests` / `core` / `utils` name directories as often as they name
    // symbols. Only an all-files argument list is read as an expanded glob,
    // so the trailing argument here stays the symbol.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn a() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("tests")).unwrap();
    let (code, stdout, stderr) = run_in(dir.path(), &["show", "lib.rs", "tests"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("mod tests"),
        "the directory must not consume the symbol slot:\n{stdout}"
    );
}

#[test]
fn a_missing_path_in_the_symbol_slot_is_a_path_error() {
    // `show a.java missing.java greet` must not report "no symbol matching
    // 'missing.java'" — that diagnoses the wrong thing about the wrong
    // argument and sends the caller looking for a symbol they never named.
    let dir = fixture();
    let a = dir.path().join("Greeter.java");
    let missing = dir.path().join("Absent.java");
    let (code, stdout, stderr) = run(&[
        "show",
        a.to_str().unwrap(),
        missing.to_str().unwrap(),
        "greet",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("path not found") && stderr.contains("Absent.java"),
        "expected a path diagnosis:\n{stderr}"
    );
    assert!(
        !stderr.contains("no symbol matching"),
        "a path must not be reported as an absent symbol:\n{stderr}"
    );
}

#[test]
fn a_dotted_symbol_is_not_mistaken_for_a_path() {
    // The suffix form `Type.method` has a dot but no source extension, so it
    // must survive the path check above.
    let dir = fixture();
    let a = dir.path().join("Greeter.java");
    let (code, stdout, stderr) = run(&["show", a.to_str().unwrap(), "Greeter.greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("Greeter.greet"), "stdout:\n{stdout}");
}

#[test]
fn limit_zero_renders_the_header_and_no_bodies() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&[
        "show",
        dir.path().to_str().unwrap(),
        "greet",
        "--limit",
        "0",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("2 match(es)") && stdout.contains("showing 0"),
        "an explicit zero cap still reports the true total:\n{stdout}"
    );
    assert!(!stdout.contains("public String greet"), "stdout:\n{stdout}");
}

#[test]
fn a_directory_of_only_unparseable_files_is_not_a_missing_symbol() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "nothing parseable here\n").unwrap();
    let (code, stdout, stderr) = run(&["show", dir.path().to_str().unwrap(), "greet"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout:\n{stdout}");
    assert!(
        stderr.contains("no parseable file"),
        "an unsearchable target is not an absent symbol:\n{stderr}"
    );
}

#[test]
fn a_glob_matching_nothing_is_a_path_error_not_a_symbol_error() {
    let dir = fixture();
    let pattern = dir.path().join("*.zzz");
    let (code, stdout, stderr) = run(&["show", pattern.to_str().unwrap(), "greet"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout:\n{stdout}");
    assert!(
        stderr.contains("path not found") && !stderr.contains("no symbol"),
        "an empty expansion is a wrong path, not an absent symbol:\n{stderr}"
    );
}

#[test]
fn overlapping_targets_do_not_search_the_same_file_twice() {
    // `Greeter.java` and `./Greeter.java` are one file, and a walk of `.`
    // reaches it by the second spelling while the explicit target names it by
    // the first. It must be rendered once, and the coverage header must count
    // the three files on disk, not four.
    let dir = fixture();
    let (code, stdout, stderr) = run_in(dir.path(), &["show", "Greeter.java", ".", "greet"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("2 match(es) for 'greet' in 2 of 3 file(s) searched"),
        "an overlapping target must not inflate coverage:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("Greeter.greet").count(),
        1,
        "the shared file must be rendered once:\n{stdout}"
    );
}

#[test]
fn overlapping_targets_keep_the_explicitly_named_spelling() {
    // A file the caller typed is parsed directly, so it survives even when the
    // walk would have hidden it. Its spelling is also the one reported back,
    // but that falls out of the shortest-spelling tie-break rather than being
    // enforced separately: `Greeter.java` beats the walk's `./Greeter.java`.
    let dir = fixture();
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &["show", "Greeter.java", ".", "greet", "--json", "--compact"],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(doc["files_scanned"], 3);
    assert_eq!(doc["files_matched"], 2);
    assert_eq!(doc["total"], 2);
    let paths: Vec<&str> = doc["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert!(
        paths.contains(&"Greeter.java"),
        "the explicit spelling is the one kept: {paths:?}"
    );
}

#[test]
fn two_spellings_of_one_file_are_one_target() {
    // The plainest form of the overlap: the same file named twice, once bare
    // and once `./`-prefixed. Comparing path spellings made that two files.
    let dir = fixture();
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &["show", "Greeter.java", "./Greeter.java", "greet"],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("1 match(es) for 'greet' in 1 of 1 file(s) searched"),
        "one file searched, not two:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("Greeter.greet").count(),
        1,
        "stdout:\n{stdout}"
    );
}

#[test]
fn unparseable_single_target_still_says_unsupported() {
    // Unchanged contract: an existing file with no adapter is a bad argument,
    // not a missing path. Anchored at the manifest directory so the assertion
    // does not depend on where the test process happens to be running.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let (code, stdout, stderr) = run(&["show", manifest.to_str().unwrap(), "package"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("unsupported file type"),
        "stderr:\n{stderr}"
    );
}
