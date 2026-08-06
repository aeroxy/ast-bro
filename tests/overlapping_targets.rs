//! Overlapping walk roots name one file once.
//!
//! `run -p … src src/hot`, `map . ./src`, `implements T src ./src` are normal
//! spellings, but `ignore::WalkBuilder` visits each root independently and has
//! no idea the second is inside (or is) the first. Every command that shares
//! `walk_paths` / `walk_and_parse` therefore reported the shared files twice:
//! duplicated `map` entries and `implements` hits, doubled `run` match counts
//! and `files_scanned`, and — the reason this is a correctness fix rather than
//! a cosmetic one — `run --write` re-reading its own output and applying a
//! re-matchable rewrite a second time.
//!
//! Two spellings of one root (`sub` and `./sub`) count as overlapping too: a
//! walk roots its results at the spelling it was given, so comparing those
//! strings never noticed. `double_rewrite_is_the_severe_case` is the case that
//! corrupted a file.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

/// Run with `cwd` set: a leading `./` only exists relative to a cwd, and cwd
/// is process-global, so the spelling is driven through the subprocess rather
/// than `set_current_dir`, which would race the rest of the suite.
fn run_in(cwd: &Path, args: &[&str]) -> (Option<i32>, String, String) {
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

/// A `sub/` holding one Rust file with a `log!` call and one trait impl.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(
        dir.path().join("sub").join("a.rs"),
        "fn main() { log!(\"a\"); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("sub").join("t.rs"),
        "trait T {}\nstruct S;\nimpl T for S {}\n",
    )
    .unwrap();
    dir
}

#[test]
fn double_rewrite_is_the_severe_case() {
    // `log!($$$A)` -> `log!(tag, $$$A)` still matches after it is applied, so
    // a file visited twice was read back after its own rewrite and rewritten
    // again, yielding `log!(tag, tag, "a")` on disk. A duplicate count is
    // wrong; a duplicate edit is destructive.
    let dir = fixture();
    let file = dir.path().join("sub").join("a.rs");

    let (code, _, stderr) = run_in(
        dir.path(),
        &[
            "run",
            "-p",
            "log!($$$A)",
            "-r",
            "log!(tag, $$$A)",
            "--lang",
            "rust",
            "sub",
            "./sub",
            "--write",
        ],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");

    let after = std::fs::read_to_string(&file).unwrap();
    assert_eq!(
        after, "fn main() { log!(tag, \"a\"); }\n",
        "the rewrite must be applied exactly once"
    );
}

#[test]
fn run_counts_a_shared_file_once() {
    let dir = fixture();
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &[
            "run",
            "-p",
            "log!($$$A)",
            "--lang",
            "rust",
            "sub",
            "./sub",
            "--json",
            "--compact",
        ],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        doc["matches"].as_array().expect("matches").len(),
        1,
        "one call site, not two:\n{stdout}"
    );
    assert_eq!(doc["files_scanned"], 2, "sub/ holds two files, not four");
}

#[test]
fn run_counts_a_file_reached_by_root_and_by_name_once() {
    // The nested form: a directory root plus an explicit file inside it.
    let dir = fixture();
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &[
            "run",
            "-p",
            "log!($$$A)",
            "--lang",
            "rust",
            "sub",
            "sub/a.rs",
            "--json",
            "--compact",
        ],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        doc["matches"].as_array().expect("matches").len(),
        1,
        "stdout:\n{stdout}"
    );
    // The count too, not just the matches: a file reached by both spellings
    // could still be scanned twice while matching once.
    assert_eq!(doc["files_scanned"], 2, "sub/ holds two files, not three");
}

#[test]
fn map_lists_a_shared_file_once() {
    let dir = fixture();
    let (code, stdout, stderr) = run_in(dir.path(), &["map", "sub", "./sub"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert_eq!(
        stdout.matches("a.rs").count(),
        1,
        "one file, one entry:\n{stdout}"
    );
}

#[test]
fn the_shortest_spelling_is_the_one_displayed() {
    // Two walk roots are equally "the" root, so the survivor's spelling is a
    // free choice; shortest is the one that reads best. Roots are given
    // long-first so passing order cannot be what decides it.
    let dir = fixture();
    let (code, stdout, stderr) = run_in(dir.path(), &["map", "./sub", "sub"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("# sub/a.rs"),
        "expected the bare spelling:\n{stdout}"
    );
    assert!(
        !stdout.contains("./sub/a.rs"),
        "the `./` spelling is the longer one:\n{stdout}"
    );
}

#[test]
fn an_absolute_root_yields_to_a_relative_one() {
    let dir = fixture();
    let absolute = dir.path().join("sub");
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &[
            "map",
            absolute.to_str().unwrap(),
            "sub",
            "--json",
            "--compact",
        ],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let paths: Vec<&str> = doc["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    assert_eq!(paths.len(), 2, "sub/ holds two files: {paths:?}");
    assert!(
        paths.iter().all(|p| !Path::new(p).is_absolute()),
        "the relative spelling is the shorter one: {paths:?}"
    );
}

#[test]
fn map_json_has_no_duplicate_file_entries() {
    let dir = fixture();
    let (code, stdout, stderr) = run_in(dir.path(), &["map", "sub", "sub", "--json", "--compact"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let files = doc["files"].as_array().expect("files array");
    assert_eq!(
        files.len(),
        2,
        "the identical spelling twice is still one walk:\n{stdout}"
    );
}

#[test]
fn implements_reports_an_implementor_once() {
    let dir = fixture();
    let (code, stdout, stderr) = run_in(dir.path(), &["implements", "T", "sub", "./sub"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("1 match(es)"),
        "one implementor, counted once:\n{stdout}"
    );
    assert_eq!(stdout.matches("struct S").count(), 1, "stdout:\n{stdout}");
}

#[test]
fn a_single_root_is_unchanged() {
    // The dedup must not eat distinct files that merely share a basename.
    let dir = fixture();
    std::fs::create_dir(dir.path().join("other")).unwrap();
    std::fs::write(
        dir.path().join("other").join("a.rs"),
        "fn main() { log!(\"b\"); }\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run_in(
        dir.path(),
        &[
            "run",
            "-p",
            "log!($$$A)",
            "--lang",
            "rust",
            ".",
            "--json",
            "--compact",
        ],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        doc["matches"].as_array().expect("matches").len(),
        2,
        "two different a.rs files are two files:\n{stdout}"
    );
}
