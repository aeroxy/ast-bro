//! End-to-end tests for `ast-bro squeeze`.
//!
//! These shell out to the built `ast-bro` binary (the same path users hit;
//! `sb` is a thin compatibility shim that execs `ast-bro`). Each test writes a
//! temp fixture file and asserts invariants on the output rather than full
//! snapshots — snapshots are brittle to colour/whitespace changes.
//!
//! Harness: `std::process::Command` against `env!("CARGO_BIN_EXE_ast-bro")`,
//! `tempfile::tempdir()` for fixtures — mirroring `tests/calls_e2e.rs` and
//! `tests/surface_e2e.rs`. (No `assert_cmd` dev-dependency is present, so we
//! use std process spawning like the other e2e suites.)

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    // CARGO_BIN_EXE_<bin name> is set by cargo for integration tests.
    // The canonical CLI binary in Cargo.toml's `[[bin]]` list is `ast-bro`.
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

/// Run `ast-bro <args...>` with colour disabled. Returns (exit_code, stdout, stderr).
fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    let code = out.status.code().unwrap_or(-1);
    (code, stdout, stderr)
}

fn run_ok(args: &[&str]) -> String {
    let (code, stdout, stderr) = run(args);
    assert!(
        code == 0,
        "expected exit 0, got {code}\nargs: {args:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

fn write(p: &Path, body: &str) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

/// A long, highly-repetitive log: every line shares an ISO8601 timestamp
/// prefix and a repeated `[Tag]` component, plus repeated `key=` tokens — the
/// exact shape the squeeze pipeline (timestamp dict + component extraction +
/// BPE) is built to collapse. 200 lines guarantees the legend overhead is
/// amortised and the squeezed form is a real win.
fn repetitive_log() -> String {
    let mut s = String::new();
    for i in 0..200 {
        s.push_str(&format!(
            "2026-05-30T11:54:{:02}.557 [WinFocusMonitor] hwnd=0x{:04x} pid=1234 event=focus_changed state=active title=window\n",
            i % 60,
            i
        ));
    }
    s
}

#[test]
fn repetitive_log_is_squeezed_with_legend() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("app.log");
    write(&path, &repetitive_log());
    let path_str = path.to_str().unwrap();

    let out = run_ok(&["squeeze", path_str]);

    // Text format: a legend block must be present for the squeezed case.
    assert!(
        out.contains("# legend:"),
        "expected a legend block in squeezed output:\n{out}"
    );
    // Header advertises the squeezed transition with a savings figure.
    assert!(
        out.contains("[squeezed"),
        "expected '[squeezed ...]' header marker:\n{out}"
    );
    // Body is separated from the header/legend by a `---` rule.
    assert!(out.contains("---"), "expected '---' separator:\n{out}");

    // The whole point: the squeezed emission is smaller than a raw `--raw`
    // run of the very same file.
    let raw_out = run_ok(&["squeeze", path_str, "--raw"]);
    assert!(
        out.len() < raw_out.len(),
        "squeezed output ({}) should be smaller than raw output ({}):\n--- squeezed ---\n{out}\n--- raw ---\n{raw_out}",
        out.len(),
        raw_out.len()
    );
}

#[test]
fn tiny_nonrepetitive_file_falls_back_to_raw() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    // Short, all-unique content: legend overhead would make a squeeze larger,
    // so the degenerate safety floor must emit raw instead.
    let content = "alpha one\nbeta two\ngamma three\n";
    write(&path, content);
    let path_str = path.to_str().unwrap();

    let out = run_ok(&["squeeze", path_str]);

    // No squeeze claim should be made on the degenerate fallback.
    assert!(
        !out.contains("[squeezed"),
        "tiny input should NOT claim to be squeezed:\n{out}"
    );
    // Raw fallback emits no legend (the body is verbatim original).
    assert!(
        !out.contains("# legend:"),
        "raw fallback should not print a legend:\n{out}"
    );
    // The original text comes through verbatim.
    assert!(
        out.contains("alpha one") && out.contains("gamma three"),
        "expected original lines in raw fallback:\n{out}"
    );
}

#[test]
fn json_output_matches_squeeze_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("app.log");
    write(&path, &repetitive_log());
    let path_str = path.to_str().unwrap();

    let out = run_ok(&["squeeze", path_str, "--json"]);

    // JSON must carry the versioned schema id and an `emitted` discriminator.
    assert!(
        out.contains("ast-bro.squeeze.v1"),
        "missing JSON schema id:\n{out}"
    );
    assert!(out.contains("\"emitted\""), "missing 'emitted' field:\n{out}");

    // It must be valid JSON. We avoid a hard dep on a JSON crate's typed model
    // and instead do a lenient structural parse with serde_json::Value, which
    // is already a transitive dependency of the project.
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\n{out}"));
    assert_eq!(
        v.get("schema").and_then(|s| s.as_str()),
        Some("ast-bro.squeeze.v1"),
        "schema field mismatch in JSON:\n{out}"
    );
    assert!(
        v.get("emitted").and_then(|s| s.as_str()).is_some(),
        "emitted field should be a string:\n{out}"
    );
    // A repetitive log should report the squeezed emission.
    assert_eq!(
        v.get("emitted").and_then(|s| s.as_str()),
        Some("squeezed"),
        "repetitive log should emit 'squeezed':\n{out}"
    );
}

#[test]
fn json_compact_is_single_line() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("app.log");
    write(&path, &repetitive_log());
    let path_str = path.to_str().unwrap();

    let out = run_ok(&["squeeze", path_str, "--json", "--compact"]);
    assert!(
        out.contains("ast-bro.squeeze.v1"),
        "compact JSON missing schema id:\n{out}"
    );
    // Compact JSON should still parse.
    let _v: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("compact stdout not valid JSON: {e}\n{out}"));
    // pretty = !compact, so compact output is a single line (sans trailing newline).
    let line_count = out.trim_end_matches('\n').lines().count();
    assert_eq!(line_count, 1, "compact JSON should be one line, got {line_count}:\n{out}");
}

#[test]
fn raw_flag_emits_original_without_legend() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("app.log");
    let content = repetitive_log();
    write(&path, &content);
    let path_str = path.to_str().unwrap();

    let out = run_ok(&["squeeze", path_str, "--raw"]);

    // --raw prints a `[raw ...]` header and NO legend / no squeeze claim.
    assert!(out.contains("[raw"), "expected '[raw ...]' header:\n{}", &out[..out.len().min(200)]);
    assert!(
        !out.contains("# legend:"),
        "--raw must not print a legend:\n{}",
        &out[..out.len().min(200)]
    );
    assert!(
        !out.contains("[squeezed"),
        "--raw must not claim to squeeze:\n{}",
        &out[..out.len().min(200)]
    );
    // The original body is present verbatim (sample a distinctive line).
    assert!(
        out.contains("[WinFocusMonitor] hwnd=0x0000"),
        "expected verbatim original content under --raw"
    );
}

#[test]
fn line_range_limits_considered_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ranged.log");
    // Five clearly-distinguishable lines; we ask for 2:4 (1-indexed, inclusive).
    write(
        &path,
        "LINE_ALPHA\nLINE_BRAVO\nLINE_CHARLIE\nLINE_DELTA\nLINE_ECHO\n",
    );
    let path_str = path.to_str().unwrap();

    // Use --raw so the body is emitted verbatim and the slice is unambiguous
    // (no compression mangling the marker strings).
    let out = run_ok(&["squeeze", path_str, "2:4", "--raw"]);

    // Only lines 2..=4 should be considered.
    assert!(out.contains("LINE_BRAVO"), "range should include line 2:\n{out}");
    assert!(out.contains("LINE_CHARLIE"), "range should include line 3:\n{out}");
    assert!(out.contains("LINE_DELTA"), "range should include line 4:\n{out}");
    // Lines 1 and 5 are outside the range and must be excluded from the body.
    assert!(!out.contains("LINE_ALPHA"), "line 1 should be excluded by range 2:4:\n{out}");
    assert!(!out.contains("LINE_ECHO"), "line 5 should be excluded by range 2:4:\n{out}");
}

#[test]
fn open_ended_json_range_clamps_end_to_eof() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("open-ended.log");
    write(&path, "ONE\nTWO\nTHREE\n");
    let path_str = path.to_str().unwrap();

    let out = run_ok(&["squeeze", path_str, "2:", "--json"]);
    let v: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\n{out}"));
    let range = v
        .get("range")
        .and_then(|r| r.as_object())
        .unwrap_or_else(|| panic!("expected object range in JSON:\n{out}"));

    assert_eq!(range.get("start").and_then(|n| n.as_u64()), Some(2));
    assert_eq!(range.get("end").and_then(|n| n.as_u64()), Some(3));
}
