use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
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
    let p = dir.path().join("big.rs");
    let mut s = String::new();
    for i in 0..300 {
        s.push_str(&format!("fn f{}() {{}}\n", i));
    }
    std::fs::write(&p, &s).unwrap();
    let stdin = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}"}}}}"#,
        p.display()
    );
    let (stdout, _, code) = run_hook("claude-code", &stdin, &["--min-lines", "200"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"decision\":\"block\""));
    assert!(stdout.contains("ast-bro substituted"));
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
    let p = dir.path().join("big.rs");
    let mut s = String::new();
    for i in 0..300 {
        s.push_str(&format!("fn f{}() {{}}\n", i));
    }
    std::fs::write(&p, &s).unwrap();
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
    let p = dir.path().join("big.rs");
    let mut s = String::new();
    for i in 0..300 {
        s.push_str(&format!("fn f{}() {{}}\n", i));
    }
    std::fs::write(&p, &s).unwrap();
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
