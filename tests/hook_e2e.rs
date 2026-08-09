use std::io::Write;
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

fn run_hook(protocol: &str, stdin: &str, args: &[&str]) -> (String, String, i32) {
    let mut cmd = Command::new(binary());
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
