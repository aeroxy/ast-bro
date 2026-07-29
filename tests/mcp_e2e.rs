//! End-to-end tests for the MCP server over stdio, focused on the error
//! contract: MCP has no stderr channel and no exit codes, so a rejected
//! call must come back as `isError: true` — never as a valid-looking
//! empty result (#33 on the MCP surface).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

/// Drive `ast-bro mcp` with initialize + one tools/call; return the last
/// response line parsed as JSON. The server exits on stdin EOF; a bounded
/// wait keeps a hung server from wedging the test run, and stderr rides
/// along in every panic message so failures stay debuggable.
fn call_tool(dir: &std::path::Path, name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let mut child = Command::new(bin())
        .arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp server");
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    });
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
        )
        .unwrap();
        writeln!(stdin, "{}", request).unwrap();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("mcp server did not exit within 30s (hung on EOF?)")
        .expect("mcp output");
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last = stdout
        .lines()
        .last()
        .unwrap_or_else(|| panic!("no response line; stderr:\n{stderr}"));
    serde_json::from_str(last)
        .unwrap_or_else(|e| panic!("invalid json-rpc response ({e}): {last}\nstderr:\n{stderr}"))
}

fn result_of(resp: &serde_json::Value) -> (&serde_json::Value, bool, &str) {
    let result = &resp["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    (result, is_error, text)
}

#[test]
fn mcp_map_all_missing_paths_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["/nope/missing-xyz.rs"]}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(is_error, "missing input must not read as an empty answer: {resp}");
    assert!(
        text.contains("resolved 0 of 1"),
        "message should say nothing was inspected: {text}"
    );
}

#[test]
fn mcp_map_all_missing_paths_json_is_an_error_not_empty_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["/nope/missing-xyz.rs"], "json": true}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(
        is_error,
        "json mode must not return a valid-looking empty files list: {resp}"
    );
    assert!(
        !text.contains("\"files\""),
        "no ast-bro.map.v1 payload for a rejected call: {text}"
    );
}

#[test]
fn mcp_map_existing_dir_with_no_parseable_files_stays_success() {
    // The distinction the fix must preserve: paths that exist but contain
    // nothing parseable are a legitimate empty answer.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "not source code\n").unwrap();
    let resp = call_tool(tmp.path(), "map", serde_json::json!({"paths": ["."]}));
    let (_, is_error, text) = result_of(&resp);
    assert!(!is_error, "an empty answer is still an answer: {resp}");
    assert!(text.contains("0 parseable file(s)"), "{text}");
}

#[test]
fn mcp_map_partial_miss_succeeds_with_note() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["lib.rs", "/nope/gone.rs"]}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(!is_error, "one resolved path must still render: {resp}");
    assert!(
        text.contains("# note: path not found: /nope/gone.rs"),
        "partial miss must be noted in the response text: {text}"
    );
    assert!(text.contains("fn f"), "{text}");
}

#[test]
fn mcp_digest_all_missing_paths_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = call_tool(
        tmp.path(),
        "digest",
        serde_json::json!({"paths": ["/nope/missing-dir"]}),
    );
    let (_, is_error, _) = result_of(&resp);
    assert!(is_error, "{resp}");
}
