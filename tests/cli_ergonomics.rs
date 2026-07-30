//! The error contract (issues #33 / #36):
//!   - stdout carries results only; every rejection goes to stderr
//!   - exit 0 = the query ran (even if the answer is empty)
//!     exit 2 = the query could not run as asked (nothing on stdout)
//!     exit 1 = internal failure
//!   - with `--json`, rejections also emit an `ast-bro.error.v1` envelope
//!     on stderr

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

// ---------------------------------------------------------------------------
// Rejected calls: exit 2, stderr, empty stdout
// ---------------------------------------------------------------------------

#[test]
fn map_missing_path_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["map", "/tmp/ast-bro-does-not-exist-xyz"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("error:") && stderr.contains("0 of 1"),
        "expected resolved-0-of-1 error:\n{stderr}"
    );
}

#[test]
fn map_no_paths_exits_2_with_no_input_error() {
    let (code, stdout, stderr) = run(&["map"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("received no paths"),
        "expected no-input error:\n{stderr}"
    );
}

#[test]
fn map_no_paths_json_emits_error_envelope_on_stderr() {
    let (code, stdout, stderr) = run(&["map", "--json"]);
    assert_eq!(code, Some(2));
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert_eq!(doc["schema"], "ast-bro.error.v1");
    assert_eq!(doc["command"], "map");
    assert_eq!(doc["kind"], "no_input");
}

#[test]
fn map_partial_paths_exit_0_with_stderr_note() {
    let (code, stdout, stderr) = run(&["map", "src/core.rs", "/tmp/ast-bro-nope-xyz"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(!stdout.is_empty(), "the resolved path must still render");
    assert!(
        stderr.contains("# note: path not found:"),
        "expected partial-miss note on stderr:\n{stderr}"
    );
}

#[test]
fn digest_missing_path_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["digest", "/tmp/ast-bro-does-not-exist-xyz"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
}

#[test]
fn show_missing_path_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["show", "/tmp/ast-bro-does-not-exist-xyz", "Foo"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(stderr.contains("path not found"), "stderr:\n{stderr}");
}

#[test]
fn show_missing_symbol_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["show", "src/core.rs", "ZzNonexistentSymbolZz"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("no symbol matching"),
        "expected symbol-not-found on stderr:\n{stderr}"
    );
}

#[test]
fn show_missing_symbol_json_emits_error_envelope() {
    let (code, stdout, stderr) = run(&["show", "src/core.rs", "ZzNonexistentSymbolZz", "--json"]);
    assert_eq!(code, Some(2));
    assert!(stdout.is_empty(), "no valid-looking empty payload:\n{stdout}");
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert_eq!(doc["kind"], "symbol_not_found");
}

#[test]
fn show_unsupported_file_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["show", "Cargo.toml", "package"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("unsupported file type"), "stderr:\n{stderr}");
}

#[test]
fn implements_unknown_type_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["implements", "ZzNoSuchTypeZz", "src/"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("no type named"),
        "expected symbol-not-found on stderr:\n{stderr}"
    );
}

#[test]
fn implements_existing_type_with_zero_impls_exits_0() {
    // A real type with no implementations is a legitimately empty answer:
    // exit 0, result on stdout. Fixture-local so changes to the repo's own
    // sources can never invalidate the premise.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lonely.rs"), "pub struct Lonely;\n").unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let (code, stdout, stderr) = run(&["implements", "Lonely", &root]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("0 match(es)"), "stdout:\n{stdout}");
}

#[test]
fn find_related_bad_target_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["find-related", "no-colon-here"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("FILE>:<LINE"),
        "expected format hint on stderr:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Unknown flags / subcommands: exit 2, stderr — never help-on-stdout-exit-0
// ---------------------------------------------------------------------------

#[test]
fn unknown_flag_exits_2_on_stderr() {
    let (code, stdout, stderr) = run(&["show", "src/core.rs", "find_symbols", "--glob", "*.rs"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stdout.is_empty(),
        "help text must not masquerade as a result:\n{stdout}"
    );
    assert!(stderr.contains("--glob"), "stderr:\n{stderr}");
}

#[test]
fn unknown_flag_hints_at_sibling_subcommand() {
    // `--glob` exists on `map` (among others); the `show` rejection
    // should say so.
    let (_, _, stderr) = run(&["show", "src/core.rs", "find_symbols", "--glob", "*.rs"]);
    assert!(
        stderr.contains("`map`"),
        "expected cross-subcommand hint naming `map`:\n{stderr}"
    );
}

#[test]
fn digest_accepts_map_scope_flags() {
    // #37: the digest alias shares map's full flag set.
    let (code, stdout, stderr) = run(&["digest", "src/", "--glob", "*.rs", "--max-members", "3"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(!stdout.is_empty());
}

#[test]
fn map_preset_digest_json_matches_digest_json() {
    // #37: `digest` is exactly `map --preset digest`.
    let (_, a, _) = run(&["map", "src/core.rs", "--preset", "digest", "--json", "--compact"]);
    let (_, b, _) = run(&["digest", "src/core.rs", "--json", "--compact"]);
    assert_eq!(a, b, "digest alias must be byte-identical to the preset");
}

#[test]
fn unknown_flag_json_emits_error_envelope() {
    let (code, stdout, stderr) = run(&["digest", "src/", "--json", "--not-a-flag"]);
    assert_eq!(code, Some(2));
    assert!(stdout.is_empty(), "stdout:\n{stdout}");
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert_eq!(doc["schema"], "ast-bro.error.v1");
    assert_eq!(doc["kind"], "unknown_flag");
}

#[test]
fn unknown_subcommand_exits_2_on_stderr() {
    let (code, stdout, _stderr) = run(&["totally-bogus-command"]);
    assert_eq!(code, Some(2));
    assert!(
        stdout.is_empty(),
        "help text must not masquerade as a result:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Requests for help stay exit 0 on stdout
// ---------------------------------------------------------------------------

#[test]
fn no_command_prints_help_exits_zero() {
    let (code, stdout, _stderr) = run(&[]);
    assert_eq!(code, Some(0), "bare invocation is an orientation request");
    assert!(
        stdout.contains("Commands:"),
        "expected help text on stdout:\n{stdout}"
    );
}

#[test]
fn explicit_help_exits_zero() {
    let (code, stdout, _) = run(&["--help"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("Commands:"));
}

// ---------------------------------------------------------------------------
// Happy paths and legitimately empty answers stay exit 0
// ---------------------------------------------------------------------------

#[test]
fn happy_path_map_works() {
    let (code, stdout, stderr) = run(&["map", "src/core.rs"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(!stdout.is_empty(), "expected non-empty map output");
}

#[test]
fn map_json_stdout_is_pure_json_even_on_partial_miss() {
    let (code, stdout, _) = run(&[
        "map",
        "src/core.rs",
        "/tmp/ast-bro-nope-xyz",
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0));
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must parse as pure JSON");
    assert_eq!(doc["schema"], "ast-bro.map.v1");
}

/// A directory whose `map` output clears the 25 KB hint threshold several
/// times over. Generated rather than borrowed from `src/` so neither test
/// below depends on how large this repository happens to be today.
fn oversized_map_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for f in 0..20 {
        let mut src = String::new();
        for i in 0..60 {
            src.push_str(&format!(
                "pub fn generated_function_{f}_{i}(argument: usize) -> usize {{ argument }}\n"
            ));
        }
        std::fs::write(dir.path().join(format!("file_{f}.rs")), src).expect("write fixture");
    }
    dir
}

#[test]
fn map_on_large_directory_hints_at_digest_preset() {
    // #35: a directory-wide map past the size threshold points at the
    // digest preset on stderr — a hint beside the result, never a redirect.
    let dir = oversized_map_fixture();
    let (code, stdout, stderr) = run(&["map", dir.path().to_str().unwrap()]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        stderr.contains("# hint:") && stderr.contains("--preset digest"),
        "expected digest-preset hint on stderr:\n{stderr}"
    );
}

#[test]
fn map_on_small_input_stays_quiet() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("tiny.rs"),
        "pub fn one() {}\npub fn two() {}\n",
    )
    .expect("write fixture");
    let (code, stdout, stderr) = run(&["map", dir.path().to_str().unwrap()]);
    assert_eq!(code, Some(0));
    assert!(stdout.len() < 25_000, "fixture must stay under the threshold");
    assert!(
        !stderr.contains("# hint:"),
        "small directories must not nag:\n{stderr}"
    );
}

#[test]
fn implements_function_name_exits_2() {
    // Only *type* declarations validate an `implements` target: a function
    // named like the query must not produce an authoritative "0 match(es)"
    // (PR #38 review).
    let (code, stdout, stderr) = run(&["implements", "find_symbols", "src/"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout:\n{stdout}");
    assert!(stderr.contains("no type named"), "stderr:\n{stderr}");
}

// ---------------------------------------------------------------------------
// PR #38 review round 2 — the contract must hold where it claims to.
// ---------------------------------------------------------------------------

#[test]
fn implements_declares_paths_required_so_help_and_runtime_agree() {
    // `require_paths` rejected a call that `--help` advertised as valid, and
    // the message blamed a shell substitution for a plainly missing argument.
    // Declaring it required in clap fixes both at once.
    let (code, stdout, _) = run(&["implements", "--help"]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("<PATHS>..."),
        "help must show PATHS as required, not [PATHS]...:\n{stdout}"
    );

    let (code, stdout, stderr) = run(&["implements", "SomeType"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("required arguments were not provided") && stderr.contains("<PATHS>"),
        "clap must name the missing argument:\n{stderr}"
    );
    assert!(
        !stderr.contains("substitution"),
        "the message must describe the error that happened:\n{stderr}"
    );
}

#[test]
fn missing_required_argument_envelope_names_the_argument() {
    let (code, _, stderr) = run(&["implements", "SomeType", "--json"]);
    assert_eq!(code, Some(2));
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert!(
        doc["detail"].as_str().unwrap_or("").contains("<PATHS>"),
        "envelope detail must name the missing argument, got: {doc}"
    );
}

#[test]
fn run_unresolved_path_is_a_rejection_not_an_empty_result() {
    // Was: exit 1 (which means internal failure) with a valid-looking empty
    // `ast-bro.run.v1` payload on stdout.
    let (code, stdout, stderr) = run(&[
        "run",
        "-p",
        "fn $A(){$$$}",
        "--json",
        "/tmp/ast-bro-does-not-exist-xyz",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert_eq!(doc["kind"], "path_not_found");
    assert_eq!(doc["command"], "run");
}

#[test]
fn run_with_no_paths_still_defaults_to_the_current_directory() {
    // The path list is optional for `run` — only a *wrong* list is rejected.
    let (code, stdout, stderr) = run(&["run", "-p", "fn require_path($$$) { $$$ }"]);
    assert!(
        code == Some(0) || code == Some(1),
        "an empty path list must default to `.`, not reject: code={code:?}\n{stderr}"
    );
    assert!(
        !stdout.contains("error"),
        "stdout carries results only:\n{stdout}"
    );
}

#[test]
fn search_unresolved_path_is_a_rejection() {
    let (code, stdout, stderr) = run(&["search", "zzz", "./ast-bro-no-such-dir-xyz", "--json"]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert_eq!(doc["kind"], "path_not_found");
}

#[test]
fn find_related_unresolved_path_is_a_rejection() {
    let (code, stdout, stderr) = run(&[
        "find-related",
        "f.rs:3",
        "./ast-bro-no-such-dir-xyz",
        "--json",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
}

#[test]
fn find_related_missing_chunk_rejects_in_json_mode_too() {
    // Was: exit 0 with `results: []` on stdout under --json, which claims
    // "nothing is similar" for a location the index never saw.
    let (code, stdout, stderr) = run(&[
        "find-related",
        "no/such/file/anywhere.rs:3",
        ".",
        "--json",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    let envelope = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    let doc: serde_json::Value = serde_json::from_str(envelope).expect("valid json");
    assert_eq!(doc["command"], "find-related");
}

#[test]
fn projected_payload_says_which_keys_it_dropped() {
    // `digest --json` sheds `docs` because its preset implies
    // `--detail names` — the caller never asked, so the payload has to carry
    // the signal rather than leave an absence to infer.
    let (code, stdout, stderr) = run(&["digest", "src/cli_error.rs", "--json", "--compact"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["projected"]["docs"], false, "{doc}");
    assert_eq!(doc["projected"]["line_numbers"], true, "{doc}");

    // Nothing projected → no key at all, so the unprojected payload stays
    // byte-identical to what consumers already parse.
    let (code, stdout, _) = run(&["map", "src/cli_error.rs", "--json", "--compact"]);
    assert_eq!(code, Some(0));
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert!(doc.get("projected").is_none(), "{doc}");
}
