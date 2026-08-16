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

/// The `ast-bro.error.v1` envelope carried on stderr beside the human text.
/// First line that starts with `{` — the human message and any hint come
/// first, and only the envelope is JSON.
fn envelope(stderr: &str) -> serde_json::Value {
    let line = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .expect("json envelope on stderr");
    serde_json::from_str(line).expect("valid json")
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
    let doc = envelope(&stderr);
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
    let doc = envelope(&stderr);
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
    // #37: `digest` is exactly `map --preset digest`. Both calls must have
    // *run* — two identical failures would otherwise satisfy the comparison.
    let (code_a, a, err_a) = run(&["map", "src/core.rs", "--preset", "digest", "--json", "--compact"]);
    let (code_b, b, err_b) = run(&["digest", "src/core.rs", "--json", "--compact"]);
    assert_eq!(code_a, Some(0), "map --preset digest failed:\n{err_a}");
    assert_eq!(code_b, Some(0), "digest failed:\n{err_b}");
    assert!(!a.is_empty(), "map --preset digest produced nothing");
    assert!(!b.is_empty(), "digest produced nothing");
    assert_eq!(a, b, "digest alias must be byte-identical to the preset");
}

#[test]
fn unknown_flag_json_emits_error_envelope() {
    let (code, stdout, stderr) = run(&["digest", "src/", "--json", "--not-a-flag"]);
    assert_eq!(code, Some(2));
    assert!(stdout.is_empty(), "stdout:\n{stdout}");
    let doc = envelope(&stderr);
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

/// A single file whose `map` output clears the 25 KB hint threshold. The
/// hint is about directories, so this is the fixture that proves size alone
/// does not trigger it.
fn oversized_map_file(dir: &std::path::Path) -> PathBuf {
    let mut src = String::new();
    for i in 0..800 {
        src.push_str(&format!(
            "pub fn generated_function_{i}(argument: usize) -> usize {{ argument }}\n"
        ));
    }
    let path = dir.join("one_big_file.rs");
    std::fs::write(&path, src).expect("write fixture");
    path
}

#[test]
fn map_hint_names_map_even_when_entered_through_the_digest_alias() {
    // #35 + #37: `map` and `digest` are one command, so the suggestion is
    // always spelled `map` — `digest … --preset digest` would be nonsense.
    let dir = oversized_map_fixture();
    let (code, _stdout, stderr) =
        run(&["digest", dir.path().to_str().unwrap(), "--detail", "full"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("`ast-bro map "),
        "hint must suggest the canonical `map` form:\n{stderr}"
    );
    assert!(
        !stderr.contains("digest --preset digest"),
        "hint must not suggest digest through the digest alias:\n{stderr}"
    );
}

#[test]
fn map_hint_fires_when_a_directory_rides_along_with_a_file() {
    // The trigger is "a directory was inspected", not "the sole argument was
    // a directory": mixing a file in still buries a package in the output.
    let dir = oversized_map_fixture();
    let file = dir.path().join("file_0.rs");
    let (code, _stdout, stderr) =
        run(&["map", file.to_str().unwrap(), dir.path().to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("# hint:") && stderr.contains(dir.path().to_str().unwrap()),
        "expected the hint to name the directory argument:\n{stderr}"
    );
}

#[test]
fn map_on_large_single_file_stays_quiet() {
    // One file is what `map` is for at any size — there is nothing smaller
    // to point at, and `digest` would answer a different question.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = oversized_map_file(dir.path());
    let (code, stdout, stderr) = run(&["map", file.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        !stderr.contains("# hint:"),
        "a single file must not be nagged about its size:\n{stderr}"
    );
}

#[test]
fn map_hint_keeps_the_scope_and_format_flags_of_the_call() {
    // The hint claims to answer the same question. Dropping `--glob` would
    // name a broader one, and dropping `--compact` would answer in a
    // different shape.
    let dir = oversized_map_fixture();
    let (code, _stdout, stderr) = run(&[
        "map",
        dir.path().to_str().unwrap(),
        "--glob",
        "*.rs",
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("--glob='*.rs'")
            && stderr.contains("--json")
            && stderr.contains("--compact"),
        "hint must carry the call's own scope and format flags:\n{stderr}"
    );
}

#[test]
fn map_detail_names_json_is_hinted_because_it_is_not_the_digest_answer() {
    // `--detail names` sheds doc comments and nothing else: the JSON payload
    // keeps private members, fields, and an uncapped member list, so it is
    // exactly the oversized answer the hint exists for.
    let dir = oversized_map_fixture();
    let (code, stdout, stderr) = run(&[
        "map",
        dir.path().to_str().unwrap(),
        "--detail",
        "names",
        "--json",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        stderr.contains("# hint:"),
        "a names-detail JSON map is not the digest answer and must be hinted:\n{stderr}"
    );
}

#[test]
fn map_detail_names_is_hinted_in_text_too() {
    // Text and JSON answer the same question about the same call, so the
    // hint cannot depend on which renderer produced the bytes.
    let dir = oversized_map_fixture();
    let (code, stdout, stderr) = run(&["map", dir.path().to_str().unwrap(), "--detail", "names"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        stderr.contains("# hint:"),
        "names detail in text is not the digest answer either:\n{stderr}"
    );
}

#[test]
fn an_inflated_digest_call_is_hinted_like_the_map_call_it_equals() {
    // `digest <dir> --include-private --include-fields` renders exactly what
    // `map <dir> --detail names` renders. Exempting one and hinting the
    // other would make the rule about the spelling rather than the answer.
    let dir = oversized_map_fixture();
    let path = dir.path().to_str().unwrap();
    let (code, inflated, stderr) = run(&[
        "digest",
        path,
        "--include-private",
        "--include-fields",
        "--json",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let (_, spelled_as_map, _) = run(&["map", path, "--detail", "names", "--json"]);
    assert_eq!(
        inflated, spelled_as_map,
        "the two spellings must render the same payload for this test to mean anything"
    );
    assert!(
        stderr.contains("# hint:"),
        "an inflated digest call is not the digest answer:\n{stderr}"
    );
}

#[test]
fn map_hint_repeats_every_path_it_was_given() {
    // The suggestion replaces the call it qualifies, so dropping a root
    // would answer a narrower question than the one that was asked.
    let dir = oversized_map_fixture();
    let second = tempfile::tempdir().expect("tempdir");
    std::fs::write(second.path().join("extra.rs"), "pub fn extra() {}\n").expect("write fixture");
    let (code, _stdout, stderr) = run(&[
        "map",
        dir.path().to_str().unwrap(),
        second.path().to_str().unwrap(),
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains(dir.path().to_str().unwrap())
            && stderr.contains(second.path().to_str().unwrap()),
        "hint must name every root it was given:\n{stderr}"
    );
}

#[test]
fn map_hint_keeps_a_tighter_member_cap_the_caller_asked_for() {
    // --max-members is a scope axis. Suggesting 8 to a caller who asked for
    // 1 would answer a broader question than the one being qualified.
    let dir = oversized_map_fixture();
    let (code, _stdout, stderr) = run(&[
        "map",
        dir.path().to_str().unwrap(),
        "--max-members",
        "1",
        "--json",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("--max-members 1"),
        "a tighter cap must survive into the suggestion:\n{stderr}"
    );
}

/// Runs `args` from `cwd`, so a test can pass a relative path.
///
/// The dash hazard only exists for a relative path: an absolute one starts
/// with `/`, and clap never mistakes it for a flag.
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

/// Runs the command the hint suggests, from `cwd`, and returns its exit code.
///
/// The hint is documented as pasteable, so the assertion that matters is
/// that a shell can run it as printed: quoting alone is not enough when clap
/// reads a leading dash as a flag.
fn run_suggested_command(cwd: &std::path::Path, stderr: &str) -> (Option<i32>, String) {
    let line = stderr
        .lines()
        .find(|l| l.starts_with("# hint:"))
        .expect("hint on stderr");
    let command = line
        .split('`')
        .nth(1)
        .expect("suggestion between backticks")
        .replacen("ast-bro", bin().to_str().expect("utf8 path"), 1);
    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .output()
        .expect("run the suggested command");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Runs the suggestion and asserts it both works and terminates.
///
/// A suggestion that hints again is a loop: the builder and the
/// `already_digest` test are computed in separate places, and nothing but
/// this assertion forces them to agree.
fn assert_suggestion_runs_and_settles(cwd: &std::path::Path, stderr: &str) {
    let hint = stderr
        .lines()
        .find(|l| l.starts_with("# hint:"))
        .expect("hint on stderr");
    assert!(
        hint.contains("`ast-bro map "),
        "this helper runs the whole command form; the counted form has none:\n{hint}"
    );
    let (code, suggested_stderr) = run_suggested_command(cwd, stderr);
    assert_eq!(
        code,
        Some(0),
        "the suggested command must run as printed:\n{stderr}"
    );
    assert!(
        !suggested_stderr.contains("# hint:"),
        "following the suggestion must land on a quiet call, got:\n{suggested_stderr}"
    );
}

/// Fills `dir` with enough declarations to clear the hint threshold.
fn fill_past_threshold(dir: &std::path::Path) {
    for f in 0..20 {
        let mut src = String::new();
        for i in 0..60 {
            src.push_str(&format!(
                "pub fn generated_function_{f}_{i}(argument: usize) -> usize {{ argument }}\n"
            ));
        }
        std::fs::write(dir.join(format!("file_{f}.rs")), src).expect("write fixture");
    }
}

#[test]
fn the_suggested_command_runs_for_a_dash_prefixed_path() {
    // A relative directory named `-generated` has to reach clap behind a
    // `--`, or the command the hint says to paste exits 2.
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = parent.path().join("-generated");
    std::fs::create_dir(&dir).expect("mkdir");
    fill_past_threshold(&dir);
    let (code, _stdout, stderr) = run_in(parent.path(), &["map", "--", "-generated"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains(" -- -generated"),
        "a dash-prefixed path must move behind a separator:\n{stderr}"
    );
    assert_suggestion_runs_and_settles(parent.path(), &stderr);
}

#[test]
fn the_suggested_command_runs_for_a_dash_prefixed_glob() {
    // Same hazard on the other axis: `--glob -big*.rs` is read as a flag, so
    // the suggestion has to use the attached `--glob=` form.
    let dir = tempfile::tempdir().expect("tempdir");
    for f in 0..20 {
        let mut src = String::new();
        for i in 0..60 {
            src.push_str(&format!(
                "pub fn generated_function_{f}_{i}(argument: usize) -> usize {{ argument }}\n"
            ));
        }
        std::fs::write(dir.path().join(format!("-big{f}.rs")), src).expect("write fixture");
    }
    let (code, stdout, stderr) = run_in(
        dir.path(),
        &["map", ".", "--glob=-big*.rs", "--detail", "signatures"],
    );
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        stderr.contains("--glob='-big*.rs'"),
        "a dash-prefixed glob needs the attached form:\n{stderr}"
    );
    assert_suggestion_runs_and_settles(dir.path(), &stderr);
}

#[test]
fn map_hint_repeats_a_path_that_did_not_resolve() {
    // The suggestion stands in for the whole call. Dropping the missing path
    // would return a clean answer and lose the note saying it was missing.
    let dir = oversized_map_fixture();
    let (code, _stdout, stderr) = run(&["map", dir.path().to_str().unwrap(), "/nope/gone-xyz.rs"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let hint = stderr
        .lines()
        .find(|l| l.starts_with("# hint:"))
        .unwrap_or("");
    assert!(
        hint.contains("/nope/gone-xyz.rs"),
        "the unresolved path must survive into the suggestion:\n{stderr}"
    );
}

#[test]
fn map_hint_keeps_display_filters_the_caller_turned_off() {
    // Re-enabling attributes and line ranges would render something other
    // than what was asked for, and usually more of it.
    let dir = oversized_map_fixture();
    let (code, _stdout, stderr) = run(&[
        "map",
        dir.path().to_str().unwrap(),
        "--no-attrs",
        "--no-lines",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("--no-attrs") && stderr.contains("--no-lines"),
        "display filters must survive into the suggestion:\n{stderr}"
    );
}

#[test]
fn a_glob_that_expands_to_directories_is_hinted() {
    // `map 'src/*'` is the largest directory answer there is, and testing
    // the argument as typed left exactly that call silent.
    let parent = tempfile::tempdir().expect("tempdir");
    for pkg in ["alpha", "beta"] {
        let dir = parent.path().join(pkg);
        std::fs::create_dir(&dir).expect("mkdir");
        fill_past_threshold(&dir);
    }
    let (code, stdout, stderr) = run_in(parent.path(), &["map", "*"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        stderr.contains("# hint:"),
        "a glob over directories is a directory input:\n{stderr}"
    );
}

#[test]
fn a_glob_over_files_is_hinted_like_the_directory_it_equals() {
    // `map alpha` and `map 'alpha/*.rs'` render the same bytes. Hinting one
    // and not the other would make the rule about the spelling.
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = parent.path().join("alpha");
    std::fs::create_dir(&dir).expect("mkdir");
    fill_past_threshold(&dir);
    let (_, as_dir, dir_stderr) = run_in(parent.path(), &["map", "alpha"]);
    let (code, as_glob, glob_stderr) = run_in(parent.path(), &["map", "alpha/*.rs"]);
    assert_eq!(code, Some(0), "stderr:\n{glob_stderr}");
    assert_eq!(
        as_dir, as_glob,
        "the two spellings must render the same answer for this test to mean anything"
    );
    assert!(dir_stderr.contains("# hint:"), "{dir_stderr}");
    assert!(
        glob_stderr.contains("# hint:"),
        "a glob that inspects the same files is the same question:\n{glob_stderr}"
    );
}

#[test]
fn explicitly_named_files_are_hinted_like_the_glob_they_equal() {
    // The trigger is what was inspected, so every spelling of the same file
    // set gets the same answer. Exempting an enumerated list would put the
    // decision back on the spelling.
    let dir = oversized_map_fixture();
    let files: Vec<String> = (0..20)
        .map(|f| {
            dir.path()
                .join(format!("file_{f}.rs"))
                .to_str()
                .expect("utf8")
                .to_string()
        })
        .collect();
    let mut args = vec!["map"];
    args.extend(files.iter().map(String::as_str));
    let (code, listed, stderr) = run(&args);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let (_, as_dir, dir_stderr) = run(&["map", dir.path().to_str().unwrap()]);
    assert_eq!(
        listed, as_dir,
        "the two spellings must render the same answer for this test to mean anything"
    );
    assert!(dir_stderr.contains("# hint:"), "{dir_stderr}");
    assert!(
        stderr.contains("# hint:"),
        "an enumerated list of the same files is the same question:\n{stderr}"
    );
}

#[test]
fn a_glob_matching_one_directory_is_hinted() {
    // `map 'sr*'` and `map src` inspect the same files. Asking the argument
    // how many entries it expanded to answered one and not the other.
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = parent.path().join("alpha");
    std::fs::create_dir(&dir).expect("mkdir");
    fill_past_threshold(&dir);
    let (_, as_dir, dir_stderr) = run_in(parent.path(), &["map", "alpha"]);
    let (code, as_glob, glob_stderr) = run_in(parent.path(), &["map", "alph*"]);
    assert_eq!(code, Some(0), "stderr:\n{glob_stderr}");
    assert_eq!(
        as_dir, as_glob,
        "the two spellings must render the same answer"
    );
    assert!(dir_stderr.contains("# hint:"), "{dir_stderr}");
    assert!(
        glob_stderr.contains("# hint:"),
        "a glob matching one directory inspects the same files:\n{glob_stderr}"
    );
}

#[test]
fn a_directory_holding_one_file_stays_quiet() {
    // "multi-file answer" has to be true when it is printed: one file is one
    // file however it was reached, and there is nothing smaller to suggest.
    let dir = tempfile::tempdir().expect("tempdir");
    oversized_map_file(dir.path());
    let (code, stdout, stderr) = run(&["map", dir.path().to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        !stderr.contains("# hint:"),
        "one file in a directory is still one file:\n{stderr}"
    );
}

#[test]
fn a_long_path_list_is_counted_rather_than_echoed() {
    // A shell expands `map src/**/*.rs` into a hundred arguments. Repeating
    // them would make the hint rival the answer it is warning about.
    let dir = oversized_map_fixture();
    let files: Vec<String> = (0..20)
        .map(|f| {
            dir.path()
                .join(format!("file_{f}.rs"))
                .to_str()
                .expect("utf8")
                .to_string()
        })
        .collect();
    let mut args = vec!["map"];
    args.extend(files.iter().map(String::as_str));
    let (code, _stdout, stderr) = run(&args);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let hint = stderr
        .lines()
        .find(|l| l.starts_with("# hint:"))
        .expect("hint on stderr");
    assert!(
        hint.contains("the same 20 paths"),
        "a long list must be counted, not echoed:\n{hint}"
    );
    assert!(
        hint.len() < 200,
        "the hint must stay cheaper to read than the answer, got {} bytes:\n{hint}",
        hint.len()
    );
}

/// Writes `count` oversized files into a fresh directory and maps them by
/// name, returning the hint line.
fn hint_for_named_files(count: usize) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut src = String::new();
    for i in 0..300 {
        src.push_str(&format!(
            "pub fn generated_function_{i}(argument: usize) -> usize {{ argument }}\n"
        ));
    }
    let files: Vec<String> = (0..count)
        .map(|f| {
            let path = dir.path().join(format!("file_{f}.rs"));
            std::fs::write(&path, &src).expect("write fixture");
            path.to_str().expect("utf8").to_string()
        })
        .collect();
    let mut args = vec!["map"];
    args.extend(files.iter().map(String::as_str));
    let (code, _stdout, stderr) = run(&args);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    stderr
        .lines()
        .find(|l| l.starts_with("# hint:"))
        .unwrap_or("")
        .to_string()
}

#[test]
fn the_echo_cap_switches_forms_where_it_says_it_does() {
    // The boundary itself, so the comparison operator is observed rather
    // than assumed: four paths are spelled out, five are counted.
    let at_cap = hint_for_named_files(4);
    assert!(
        at_cap.contains("`ast-bro map "),
        "four paths must still be a pasteable command:\n{at_cap}"
    );
    let past_cap = hint_for_named_files(5);
    assert!(
        past_cap.contains("the same 5 paths"),
        "five paths must be counted:\n{past_cap}"
    );
    assert!(
        !past_cap.contains("`ast-bro map "),
        "the counted form must not look like a whole command:\n{past_cap}"
    );
}

#[test]
fn a_path_that_cannot_be_written_on_one_line_falls_back_to_the_counted_form() {
    // The spelled-out form is one backtick-delimited line. A backtick closes
    // that delimiter from inside the shell quotes that correctly protect it,
    // and a control character splits the line — and every reader of a hint
    // is line-oriented, this file included. Neither reaches the counted
    // form, which names no paths.
    for name in ["we`ird", "we\nird", "we\tird"] {
        let parent = tempfile::tempdir().expect("tempdir");
        let dir = parent.path().join(name);
        std::fs::create_dir(&dir).expect("mkdir");
        fill_past_threshold(&dir);
        let (code, _stdout, stderr) = run(&["map", dir.to_str().expect("utf8")]);
        assert_eq!(code, Some(0), "stderr:\n{stderr}");
        let hint = stderr
            .lines()
            .find(|l| l.starts_with("# hint:"))
            .unwrap_or_else(|| panic!("hint on stderr for {name:?}:\n{stderr}"));
        assert!(
            hint.contains("running `") && hint.contains("on the same path"),
            "{name:?} must fall back to the counted form:\n{hint}"
        );
        assert_eq!(
            hint.matches('`').count(),
            2,
            "{name:?}: exactly one delimited span, closed where it opened:\n{hint}"
        );
        assert!(
            stderr.lines().filter(|l| l.starts_with("# hint:")).count() == 1
                && !hint.contains('\n'),
            "{name:?}: the hint must occupy one line:\n{stderr}"
        );
    }
}

#[test]
fn the_counted_form_settles_like_the_spelled_out_one() {
    // The spelled-out form is a fresh command and cannot loop. The counted
    // form describes one too — phrased as an addition to the command already
    // typed it would loop forever here, where the caller has set every axis
    // the preset would change and adding it alters nothing.
    let dir = oversized_map_fixture();
    let files: Vec<String> = (0..20)
        .map(|f| {
            dir.path()
                .join(format!("file_{f}.rs"))
                .to_str()
                .expect("utf8")
                .to_string()
        })
        .collect();
    let pinned = [
        "--detail",
        "names",
        "--include-private",
        "--include-fields",
        "--max-members",
        "8",
    ];
    let mut args = vec!["map"];
    args.extend(files.iter().map(String::as_str));
    args.extend(pinned);
    let (code, stdout, stderr) = run(&args);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.len() > 25_000, "fixture must exceed the threshold");
    let hint = stderr
        .lines()
        .find(|l| l.starts_with("# hint:"))
        .expect("hint on stderr");
    assert!(
        hint.contains("running `"),
        "the counted form must prescribe a call, not an addition:\n{hint}"
    );

    // Follow it: the same paths under the prescribed flags, and nothing else.
    let flags = hint.split('`').nth(1).expect("flag fragment");
    let mut followed = vec!["map"];
    followed.extend(files.iter().map(String::as_str));
    followed.extend(flags.split_whitespace());
    let (code, _stdout, stderr) = run(&followed);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("# hint:"),
        "following the counted form must settle, got:\n{stderr}"
    );

    // And the reading the wording rules out. Appending the preset to what
    // was typed changes nothing, because explicit flags beat it on every
    // axis, so the same hint prints again — and appending the fragment
    // verbatim is worse still, since the repeated `--max-members` is a
    // clap rejection. Neither is a call worth prescribing.
    let mut appended = args.clone();
    appended.push("--preset");
    appended.push("digest");
    let (code, _stdout, stderr) = run(&appended);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("# hint:"),
        "the additive reading is the one that loops; if it settles, this test has stopped explaining the wording:\n{stderr}"
    );
    let mut verbatim = args.clone();
    verbatim.extend(flags.split_whitespace());
    let (code, _stdout, _stderr) = run(&verbatim);
    assert_eq!(
        code,
        Some(2),
        "appending the fragment verbatim repeats --max-members, which clap rejects"
    );
}

/// Builds a two-file fixture whose `map` stdout is exactly `target` bytes.
///
/// The renderer prints each name once, so one character of padding in a
/// function name moves stdout by one byte, which is what makes an exact
/// landing possible at all. Returns the directory and the stderr of the run
/// that hit the target.
fn fixture_with_stdout_of_exactly(target: usize) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bulk: String = (0..300)
        .map(|i| {
            format!("pub fn generated_function_{i}(argument: usize) -> usize {{ argument }}\n")
        })
        .collect();
    std::fs::write(dir.path().join("bulk.rs"), &bulk).expect("write fixture");
    let mut pad = 1usize;
    for _ in 0..40 {
        std::fs::write(
            dir.path().join("pad.rs"),
            format!("pub fn {}() {{}}\n", "p".repeat(pad)),
        )
        .expect("write fixture");
        let (code, stdout, stderr) = run(&["map", dir.path().to_str().expect("utf8")]);
        assert_eq!(code, Some(0), "stderr:\n{stderr}");
        match stdout.len().cmp(&target) {
            std::cmp::Ordering::Equal => return (dir, stderr),
            std::cmp::Ordering::Less => pad += target - stdout.len(),
            std::cmp::Ordering::Greater => {
                let over = stdout.len() - target;
                assert!(
                    pad > over,
                    "cannot shrink below a one-character name; overshoot {over}"
                );
                pad -= over;
            }
        }
    }
    panic!("could not land stdout on exactly {target} bytes");
}

#[test]
fn the_threshold_is_measured_on_what_was_written_not_on_the_string() {
    // `println!` appends a newline the size check used to miss, which moved
    // the firing point by one byte. Both sides of the boundary are pinned
    // here because the middle case is the one a fixture never lands on by
    // accident: every other test in this file clears the bar by 50 KB.
    let (_dir, quiet) = fixture_with_stdout_of_exactly(25_000);
    assert!(
        !quiet.contains("# hint:"),
        "25 000 emitted bytes do not clear a 25 000-byte bar:\n{quiet}"
    );
    let (_dir, hinted) = fixture_with_stdout_of_exactly(25_001);
    assert!(
        hinted.contains("# hint:"),
        "25 001 emitted bytes clear it, and the newline is one of them:\n{hinted}"
    );
}

#[test]
fn an_unspellable_glob_withholds_the_hint() {
    // The glob rides inside the same backtick span as the command, and
    // unlike a path it has no fallback: the counted form carries it too, and
    // dropping it would prescribe a broader question.
    let dir = oversized_map_fixture();
    // Both spellings still match every file, so the answer stays over the
    // threshold and the missing hint is the guard rather than a small answer.
    for glob in ["{*.rs,zz`zz}", "{*.rs,zz\nzz}"] {
        let (code, stdout, stderr) = run(&["map", dir.path().to_str().unwrap(), "--glob", glob]);
        assert_eq!(code, Some(0), "stderr:\n{stderr}");
        assert!(stdout.len() > 25_000, "fixture must exceed the threshold");
        assert!(
            !stderr.contains("# hint:"),
            "{glob:?} cannot be written into the hint, so there is none:\n{stderr}"
        );
    }
    // The ordinary glob still reaches the suggestion.
    let (_, _, stderr) = run(&["map", dir.path().to_str().unwrap(), "--glob", "*.rs"]);
    assert!(
        stderr.contains("--glob='*.rs'"),
        "a spellable glob must survive into the suggestion:\n{stderr}"
    );
}

#[test]
fn map_hint_quotes_a_path_a_shell_would_split() {
    // The hint exists to be pasted. A directory named `my code` has to come
    // back as one argument.
    let parent = tempfile::tempdir().expect("tempdir");
    let dir = parent.path().join("my code");
    std::fs::create_dir(&dir).expect("mkdir");
    for f in 0..20 {
        let mut src = String::new();
        for i in 0..60 {
            src.push_str(&format!(
                "pub fn generated_function_{f}_{i}(argument: usize) -> usize {{ argument }}\n"
            ));
        }
        std::fs::write(dir.join(format!("file_{f}.rs")), src).expect("write fixture");
    }
    let (code, _stdout, stderr) = run(&["map", dir.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains(&format!("'{}'", dir.display())),
        "a path with a space must be quoted in the suggestion:\n{stderr}"
    );
}

#[test]
fn digest_preset_json_stays_quiet() {
    // The suppression rule has to hold in the renderer where it is the only
    // thing standing between the digest answer and a hint pointing at it.
    let dir = oversized_map_fixture();
    let (code, stdout, stderr) = run(&["digest", dir.path().to_str().unwrap(), "--json"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        stdout.len()
    );
    assert!(
        !stderr.contains("# hint:"),
        "the digest preset must not point at itself:\n{stderr}"
    );
}

#[test]
fn digest_alias_with_full_detail_is_still_hinted() {
    // Wearing the alias is not being the digest answer: `--detail full`
    // brings back signatures and doc comments, and the preset is smaller.
    let dir = oversized_map_fixture();
    let (code, _stdout, stderr) =
        run(&["digest", dir.path().to_str().unwrap(), "--detail", "full"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("# hint:"),
        "a full-detail answer must be hinted whichever alias asked for it:\n{stderr}"
    );
}

#[test]
fn digest_preset_on_large_directory_stays_quiet() {
    // The digest preset is what the hint points at, so it has nothing left
    // to suggest however large its own output grows.
    let dir = oversized_map_fixture();
    let (code, stdout, stderr) = run(&["digest", dir.path().to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(!stdout.is_empty(), "digest must still answer");
    assert!(
        !stderr.contains("# hint:"),
        "the digest preset must not point at itself:\n{stderr}"
    );
}

#[test]
fn map_json_on_large_directory_hints_without_polluting_stdout() {
    // A JSON caller pays the same size for the same question, and stdout
    // stays pure JSON because the hint rides the error channel.
    let dir = oversized_map_fixture();
    let (code, stdout, stderr) = run(&["map", dir.path().to_str().unwrap(), "--json"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must parse as pure JSON");
    assert_eq!(doc["schema"], "ast-bro.map.v1");
    assert!(
        stderr.contains("# hint:") && stderr.contains("--json"),
        "the JSON hint must keep the caller in JSON:\n{stderr}"
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
    assert!(
        stdout.len() < 25_000,
        "fixture must stay under the threshold"
    );
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
    let doc = envelope(&stderr);
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
    let doc = envelope(&stderr);
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
    let doc = envelope(&stderr);
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
    //
    // Runs against a throwaway repo rather than this checkout: pointing at a
    // root with no `.ast-bro/index` makes `Index::open` fall back to a full
    // build — model download included — and the location check now happens
    // before that, so the rejection needs neither an index nor a network.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("present.rs"), "pub fn f() {}\n").expect("write fixture");
    let root = dir.path().to_str().expect("utf8 path");
    let (code, stdout, stderr) = run(&[
        "find-related",
        "no/such/file/anywhere.rs:3",
        root,
        "--json",
    ]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    let doc = envelope(&stderr);
    assert_eq!(doc["command"], "find-related");
    assert_eq!(doc["kind"], "path_not_found");
    // Nothing was built on the way to the rejection.
    assert!(
        !dir.path().join(".ast-bro/index").exists(),
        "a rejected location must not trigger an index build"
    );
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
