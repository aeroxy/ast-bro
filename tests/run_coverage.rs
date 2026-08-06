//! `run` with a valid pattern and no matches used to print nothing at all —
//! exit 1 and silence on both streams. Silence cannot distinguish "searched
//! 412 files, the pattern is absent" from "`--glob` narrowed this to three
//! files and the answer covers almost nothing", so the zero now says how much
//! ground it covers.

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

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..3 {
        std::fs::write(
            dir.path().join(format!("f{i}.rs")),
            "fn main() { println!(\"hi\"); }\n",
        )
        .unwrap();
    }
    dir
}

#[test]
fn an_honest_zero_reports_how_many_files_were_scanned() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&[
        "run",
        "-p",
        "zz_absent($$$)",
        "--lang",
        "rust",
        dir.path().to_str().unwrap(),
    ]);
    // grep convention: no matches is exit 1.
    assert_eq!(code, Some(1), "stderr:\n{stderr}");
    assert!(
        stdout.is_empty(),
        "stdout must stay a pure result stream:\n{stdout}"
    );
    assert!(
        stderr.contains("no matches") && stderr.contains("3 file(s) scanned"),
        "the zero must carry its coverage:\n{stderr}"
    );
}

#[test]
fn a_narrowed_glob_shows_up_as_a_small_scan() {
    let dir = fixture();
    let (code, _, stderr) = run(&[
        "run",
        "-p",
        "zz_absent($$$)",
        "--lang",
        "rust",
        "--glob",
        "f0.rs",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("1 file(s) scanned"),
        "a narrowed scope must be visible in the answer:\n{stderr}"
    );
}

#[test]
fn zero_files_keeps_its_own_distinct_diagnosis() {
    // Nothing was searched at all — a different failure with a different
    // repair, so it must not be reworded into a coverage count.
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(&[
        "run",
        "-p",
        "foo($$$)",
        "--lang",
        "rust",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("no source files processed"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn json_carries_files_scanned() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&[
        "run",
        "-p",
        "zz_absent($$$)",
        "--lang",
        "rust",
        "--json",
        "--compact",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["files_scanned"], 3);
    assert_eq!(doc["matches"].as_array().expect("matches array").len(), 0);
}

#[test]
fn a_real_match_is_unaffected() {
    let dir = fixture();
    let (code, stdout, stderr) = run(&[
        "run",
        "-p",
        "println!($$$)",
        "--lang",
        "rust",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("println!"), "stdout:\n{stdout}");
    assert!(
        !stderr.contains("no matches"),
        "a non-empty answer must not claim emptiness:\n{stderr}"
    );
}

#[test]
fn a_zero_over_a_partial_scan_says_it_is_partial() {
    // `attempted_files` counts files we picked up, including ones that then
    // errored out — so a scan that skipped an oversize file still reported
    // "N file(s) scanned" and read as complete. A zero standing on a partial
    // scan has to say so, or it cannot be told from an exhaustive one.
    let dir = fixture();
    let big = dir.path().join("big.rs");
    std::fs::write(&big, "// pad\n".repeat(800_000)).unwrap();

    let (code, _, stderr) = run(&[
        "run",
        "-p",
        "zz_absent($$$)",
        "--lang",
        "rust",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("skipped (size"),
        "the oversize file must be reported:\n{stderr}"
    );
    assert!(
        stderr.contains("1 errored") && stderr.contains("not exhaustive"),
        "the zero must admit the scan was partial:\n{stderr}"
    );
}

#[test]
fn a_clean_zero_does_not_claim_to_be_partial() {
    let dir = fixture();
    let (code, _, stderr) = run(&[
        "run",
        "-p",
        "zz_absent($$$)",
        "--lang",
        "rust",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "stderr:\n{stderr}");
    assert!(
        stderr.contains("3 file(s) scanned") && !stderr.contains("errored"),
        "nothing errored, so nothing should be hedged:\n{stderr}"
    );
}
