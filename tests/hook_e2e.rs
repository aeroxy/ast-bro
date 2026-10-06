use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

/// A file comfortably over the 200-line threshold every test here passes.
fn big_file(dir: &TempDir) -> std::path::PathBuf {
    let p = dir.path().join("big.rs");
    let mut src = String::new();
    for i in 0..300 {
        src.push_str(&format!("fn f{}() {{}}\n", i));
    }
    std::fs::write(&p, &src).unwrap();
    p
}

/// A markdown document over the same threshold, at `p` — the kind of file
/// `keep_source` exists for, and one the hook outlines unless told not to.
fn big_doc(p: &Path) -> PathBuf {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut src = String::new();
    for i in 0..60 {
        src.push_str(&format!("## Step {i}\n\nDo the thing.\nThen check it.\n\n"));
    }
    std::fs::write(p, src).unwrap();
    p.to_path_buf()
}

/// A repository root `find_root_for` settles on, with `.ast-bro.toml` in it.
fn repo_with_config(config: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(dir.path().join(".ast-bro.toml"), config).unwrap();
    dir
}

/// A Claude Code `Read` of `p`, as delivered under `hook_event_name`.
fn read_event(hook_event_name: &str, p: &Path) -> String {
    format!(
        r#"{{"hook_event_name":"{hook_event_name}","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    )
}

fn run_hook(protocol: &str, stdin: &str, args: &[&str]) -> (String, String, i32) {
    spawn_hook(Command::new(binary()), protocol, stdin, args)
}

/// [`run_hook`] with `HOME` pointed at `home`, so the user-level config the
/// hook reads is the test's and never the developer's.
fn run_hook_with_home(
    home: &Path,
    protocol: &str,
    stdin: &str,
    args: &[&str],
) -> (String, String, i32) {
    let mut cmd = Command::new(binary());
    cmd.env("HOME", home);
    spawn_hook(cmd, protocol, stdin, args)
}

fn spawn_hook(mut cmd: Command, protocol: &str, stdin: &str, args: &[&str]) -> (String, String, i32) {
    cmd.arg("hook")
        .arg("--protocol")
        .arg(protocol)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn ast-bro");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn claude_code_pass_through_for_small_file() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("small.rs");
    std::fs::write(&p, "fn main() {}\n").unwrap();
    let stdin = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"continue\":true"), "stdout: {}", stdout);
}

#[test]
fn claude_code_substitutes_for_big_file() {
    let dir = TempDir::new().unwrap();
    let p = big_file(&dir);
    let stdin = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"decision\":\"block\""));
    assert!(stdout.contains("ast-bro substituted"));
}

/// A `PostToolUse` event replaces the delivered result instead of refusing the
/// call, so the host reports a success and the payload has no failure to
/// disclaim (issue #34).
#[test]
fn claude_code_post_tool_use_replaces_the_result() {
    let dir = TempDir::new().unwrap();
    let p = big_file(&dir);
    let stdin = format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains(r#""hookEventName":"PostToolUse""#),
        "stdout: {}",
        stdout
    );
    assert!(stdout.contains(r#""updatedToolOutput""#), "stdout: {}", stdout);
    assert!(stdout.contains("ast-bro replaced the file contents"));
    assert!(!stdout.contains("\"decision\""), "stdout: {}", stdout);
    assert!(!stdout.contains("nothing failed"), "stdout: {}", stdout);
}

/// An entry still registered under `PreToolUse` keeps the refusal channel: the
/// read has not run there, so a map can only arrive inside a rejection.
#[test]
fn claude_code_pre_tool_use_still_denies() {
    let dir = TempDir::new().unwrap();
    let p = big_file(&dir);
    let stdin = format!(
        r#"{{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"decision\":\"block\""), "stdout: {}", stdout);
    assert!(stdout.contains("nothing failed"));
}

/// `PostToolUse` never fires for a read the host rejected, so the map has to
/// ride `PostToolUseFailure`. That event cannot replace a result, only add
/// context — which is all that is needed when the result carries no file.
#[test]
fn claude_code_post_tool_use_failure_augments_with_context() {
    let dir = TempDir::new().unwrap();
    let p = big_file(&dir);
    let stdin = format!(
        r#"{{"hook_event_name":"PostToolUseFailure","tool_name":"Read","tool_input":{{"file_path":"{}"}},"error":"File content (30000 tokens) exceeds maximum allowed tokens (25000)."}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains(r#""hookEventName":"PostToolUseFailure""#),
        "stdout: {}",
        stdout
    );
    assert!(stdout.contains(r#""additionalContext""#), "stdout: {}", stdout);
    assert!(stdout.contains("because the Read did"), "stdout: {}", stdout);
    // The notice must not repeat the host's error back as a diagnosis: the shim
    // reads no error field, and the event fires for failures a retry would clear.
    assert!(!stdout.contains("exceeds maximum"), "stdout: {}", stdout);
    // No replacement field: PostToolUseFailure does not honor one, and sending
    // it anyway would claim a capability the event does not have.
    assert!(!stdout.contains("updatedToolOutput"), "stdout: {}", stdout);
    assert!(!stdout.contains("\"decision\""), "stdout: {}", stdout);
}

#[test]
fn claude_code_post_tool_use_passes_through_small_file() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("small.rs");
    std::fs::write(&p, "fn main() {}\n").unwrap();
    let stdin = format!(
        r#"{{"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"continue\":true"), "stdout: {}", stdout);
}

#[test]
fn claude_code_pass_through_for_png() {
    let stdin = r#"{"tool_name":"Read","tool_input":{"file_path":"image.png"}}"#;
    let (stdout, _, code) = run_hook("claude-code", stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"continue\":true"));
}

#[test]
fn claude_code_pass_through_when_offset_set() {
    let dir = TempDir::new().unwrap();
    let p = big_file(&dir);
    let stdin = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}","offset":10,"limit":20}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"continue\":true"));
}

#[test]
fn gemini_substitutes_for_big_file() {
    let dir = TempDir::new().unwrap();
    let p = big_file(&dir);
    let stdin = format!(
        r#"{{"tool_name":"read_file","tool_input":{{"absolute_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("gemini", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"decision\":\"block\""));
}

#[test]
fn bad_stdin_json_pass_through_with_warning() {
    let (stdout, stderr, code) = run_hook("claude-code", "not json", &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"continue\":true"));
    assert!(stderr.contains("bad stdin json"));
}

#[test]
fn unknown_protocol_exits_nonzero() {
    let (_, stderr, code) = run_hook("zzz-not-real", "{}", &[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown --protocol"));
}

/// `keep_source` in a project's `.ast-bro.toml` hands a listed file over whole
/// on both channels that would otherwise put a map in its place — `--always`
/// included, since that flag is about size and this policy is not. An unlisted
/// file of the same shape is still mapped, so the pass-through is the
/// policy's doing and not the fixture's.
#[test]
fn keep_source_in_a_project_config_delivers_the_file_whole() {
    let home = TempDir::new().unwrap();
    let repo = repo_with_config("[hook]\nkeep_source = [\"docs/runbooks/**\"]\n");
    let runbook = big_doc(&repo.path().join("docs/runbooks/deploy.md"));
    let readme = big_doc(&repo.path().join("docs/README.md"));

    for event in ["PreToolUse", "PostToolUse"] {
        for args in [&["--min-lines", "200"][..], &["--always"][..]] {
            let (stdout, _, code) =
                run_hook_with_home(home.path(), "claude-code", &read_event(event, &runbook), args);
            assert_eq!(code, 0);
            assert!(
                stdout.contains("\"continue\":true"),
                "{event} {args:?}: a listed file must arrive whole: {stdout}"
            );
            let (stdout, _, _) =
                run_hook_with_home(home.path(), "claude-code", &read_event(event, &readme), args);
            assert!(
                !stdout.contains("\"continue\":true"),
                "{event} {args:?}: an unlisted file must still be mapped: {stdout}"
            );
        }
    }
}

/// A read the host refused still gets its map beside the error. There is no
/// delivered file there for a map to stand in for, and withholding it would
/// leave a bare error — the one exception the README documents.
#[test]
fn keep_source_still_augments_a_read_the_host_refused() {
    let home = TempDir::new().unwrap();
    let repo = repo_with_config("[hook]\nkeep_source = [\"docs/runbooks/**\"]\n");
    let runbook = big_doc(&repo.path().join("docs/runbooks/deploy.md"));
    let (stdout, _, code) = run_hook_with_home(
        home.path(),
        "claude-code",
        &read_event("PostToolUseFailure", &runbook),
        &["--min-lines", "200"],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains(r#""additionalContext""#), "stdout: {stdout}");
}

/// The user-level config, read through a `$HOME` that is a symlink.
///
/// This is the case the key was written for — a skill under the home
/// directory, in no repository — and the shape that once left the policy
/// inert: `dirs::home_dir()` hands `$HOME` over as spelled while the file side
/// was canonicalized, so the two never compared equal and nothing said so. The
/// unit tests reach `add` with a root they chose; only a real process resolves
/// the home the way an installed hook does. The link is explicit so this fails
/// on Linux too, where the temp directory is not behind one.
#[cfg(unix)]
#[test]
fn keep_source_in_the_user_config_applies_through_a_linked_home() {
    let real = TempDir::new().unwrap();
    let links = TempDir::new().unwrap();
    let home = links.path().join("home");
    std::os::unix::fs::symlink(real.path(), &home).unwrap();
    let config = home.join(".config/ast-bro/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "[hook]\nkeep_source = [\"**/SKILL.md\"]\n").unwrap();
    let skill = big_doc(&home.join(".claude/skills/sweep/SKILL.md"));
    let notes = big_doc(&home.join(".claude/skills/sweep/notes.md"));

    let args = ["--min-lines", "200"];
    let (stdout, _, code) =
        run_hook_with_home(&home, "claude-code", &read_event("PreToolUse", &skill), &args);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\"continue\":true"),
        "a user pattern must reach a skill under a linked home: {stdout}"
    );
    let (stdout, _, _) =
        run_hook_with_home(&home, "claude-code", &read_event("PreToolUse", &notes), &args);
    assert!(
        stdout.contains("\"decision\":\"block\""),
        "and must not spill onto its neighbours: {stdout}"
    );
}

/// A `keep_source` of the wrong shape says so instead of passing for a policy
/// that applied. A string where the array belongs protects nothing, so the
/// file is still mapped; a non-string entry is skipped while the valid
/// pattern beside it keeps working.
#[test]
fn a_wrongly_typed_keep_source_says_so() {
    let home = TempDir::new().unwrap();
    let args = ["--min-lines", "200"];

    let repo = repo_with_config("[hook]\nkeep_source = \"docs/runbooks/**\"\n");
    let runbook = big_doc(&repo.path().join("docs/runbooks/deploy.md"));
    let (stdout, stderr, _) =
        run_hook_with_home(home.path(), "claude-code", &read_event("PreToolUse", &runbook), &args);
    assert!(
        stderr.contains(".ast-bro.toml: keep_source is not an array of strings"),
        "stderr: {stderr}"
    );
    assert!(stdout.contains("\"decision\":\"block\""), "stdout: {stdout}");

    let repo = repo_with_config("[hook]\nkeep_source = [42, \"docs/runbooks/**\"]\n");
    let runbook = big_doc(&repo.path().join("docs/runbooks/deploy.md"));
    let (stdout, stderr, _) =
        run_hook_with_home(home.path(), "claude-code", &read_event("PreToolUse", &runbook), &args);
    assert!(
        stderr.contains(".ast-bro.toml: keep_source entry 42 is not a string"),
        "stderr: {stderr}"
    );
    assert!(
        stdout.contains("\"continue\":true"),
        "the valid pattern must still apply: {stdout}"
    );
}

/// A config that exists but cannot be read or parsed says so, rather than
/// reading as one that was never written. One Latin-1 byte in a comment is
/// enough for the first; an unclosed table header for the second.
#[test]
fn a_config_that_cannot_be_read_or_parsed_says_so() {
    let home = TempDir::new().unwrap();
    for (config, expected) in [
        (
            &b"# caf\xe9\n[hook]\nkeep_source = [\"docs/**\"]\n"[..],
            ".ast-bro.toml could not be read",
        ),
        (
            &b"[hook\nkeep_source = [\"docs/**\"]\n"[..],
            ".ast-bro.toml could not be parsed",
        ),
    ] {
        let repo = TempDir::new().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".ast-bro.toml"), config).unwrap();
        let runbook = big_doc(&repo.path().join("docs/deploy.md"));
        let (_, stderr, code) = run_hook_with_home(
            home.path(),
            "claude-code",
            &read_event("PreToolUse", &runbook),
            &["--min-lines", "200"],
        );
        assert_eq!(code, 0);
        assert!(stderr.contains(expected), "stderr: {stderr}");
    }
}
