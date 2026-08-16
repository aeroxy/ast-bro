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
    // Select the tools/call response by its id, not by line position — a
    // stray notification or reordered write must fail loudly here rather
    // than let every downstream assertion check the initialize response.
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["id"] == 2)
        .unwrap_or_else(|| panic!("no response with id 2; stdout:\n{stdout}\nstderr:\n{stderr}"))
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
    assert!(
        is_error,
        "missing input must not read as an empty answer: {resp}"
    );
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

#[test]
fn mcp_trace_unresolved_target_is_an_error() {
    // An endpoint that matches no symbol is a rejected call (#36) — it must
    // ride the error channel, not come back as a valid-looking trace doc.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "pub fn a() { b(); }\npub fn b() {}\n",
    )
    .unwrap();
    for json in [false, true] {
        let resp = call_tool(
            tmp.path(),
            "trace",
            serde_json::json!({"from": "a", "to": "no_such_symbol_xyz", "json": json}),
        );
        let (_, is_error, text) = result_of(&resp);
        assert!(
            is_error,
            "unresolved trace target must be isError (json={json}): {resp}"
        );
        assert!(
            text.contains("no_such_symbol_xyz"),
            "diagnostic must name the missing symbol: {text}"
        );
    }
}

#[test]
fn mcp_implements_unknown_type_is_an_error() {
    // Same gate as the CLI (#36): 0 matches for a type that exists is an
    // answer; 0 matches because the type exists nowhere is a rejection.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "pub trait Base {}\npub struct Leaf;\nimpl Base for Leaf {}\n",
    )
    .unwrap();
    let resp = call_tool(
        tmp.path(),
        "implements",
        serde_json::json!({"target": "NoSuchType", "paths": ["."]}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(
        is_error,
        "unknown type must not read as an empty answer: {resp}"
    );
    assert!(text.contains("NoSuchType"), "{text}");

    // A real type with no implementors stays a legitimate 0-match success.
    let resp = call_tool(
        tmp.path(),
        "implements",
        serde_json::json!({"target": "Leaf", "paths": ["."]}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(
        !is_error,
        "an empty answer for an existing type is still an answer: {resp}"
    );
    assert!(text.contains("0 match(es)"), "{text}");
}

#[test]
fn mcp_find_related_unknown_location_is_an_error_in_json_mode_too() {
    // The JSON empty-envelope special case said "nothing is similar to this
    // chunk" for a location that was never indexed — a different and wrong
    // claim (#33). Both output modes must reject.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
    // No index exists and the file was never indexed; use a path that does
    // not exist so the lookup misses without a model download.
    let resp = call_tool(
        tmp.path(),
        "find_related",
        serde_json::json!({"path": "nope/missing.rs", "line": 1, "root": ".", "json": true}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(
        is_error,
        "json mode must not return a valid-looking empty results list: {resp}"
    );
    assert!(
        !text.contains("\"results\""),
        "no ast-bro.related.v1 payload for a rejected call: {text}"
    );
    // Nothing was built on the way to the rejection (no model download).
    assert!(
        !tmp.path().join(".ast-bro/index").exists(),
        "a rejected location must not trigger an index build"
    );
}

#[test]
fn mcp_callees_limit_truncates_display_but_keeps_the_true_total() {
    // `limit` bounds the payload, not the walk (issue #32): the JSON must
    // carry the exact pre-cap `total` and `truncated: true` so a consumer
    // can tell a capped answer from a complete one.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "pub fn f0() {}\npub fn f1() {}\npub fn f2() {}\npub fn hub() { f0(); f1(); f2(); }\n",
    )
    .unwrap();
    let resp = call_tool(
        tmp.path(),
        "callees",
        serde_json::json!({"target": "hub", "limit": 2, "json": true}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(!is_error, "{resp}");
    let doc: serde_json::Value = serde_json::from_str(text).expect("payload parses");
    assert_eq!(doc["total"], 3, "true pre-cap total: {text}");
    assert_eq!(doc["truncated"], true, "{text}");
    assert_eq!(
        doc["matches"].as_array().map(|m| m.len()),
        Some(2),
        "display capped at limit: {text}"
    );
}

#[test]
fn mcp_json_partial_miss_carries_missing_paths() {
    // JSON responses can't prepend a note; the unresolved originals ride
    // as a machine-readable `missing_paths` field instead, for map and
    // the digest alias alike.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "pub fn f() {}\n").unwrap();
    for tool in ["map", "digest"] {
        let resp = call_tool(
            tmp.path(),
            tool,
            serde_json::json!({"paths": ["lib.rs", "/nope/gone.rs"], "json": true}),
        );
        let (_, is_error, text) = result_of(&resp);
        assert!(!is_error, "partial miss is a qualified success: {resp}");
        let doc: serde_json::Value = serde_json::from_str(text).expect("payload parses");
        assert_eq!(
            doc["missing_paths"],
            serde_json::json!(["/nope/gone.rs"]),
            "{tool} payload must name the unresolved input: {text}"
        );
        assert_eq!(
            doc["files"].as_array().map(|f| f.len()),
            Some(1),
            "the resolved path still renders: {text}"
        );
    }

    // Fully-resolved requests keep the untouched payload — no field.
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["lib.rs"], "json": true}),
    );
    let (_, _, text) = result_of(&resp);
    let doc: serde_json::Value = serde_json::from_str(text).expect("payload parses");
    assert!(
        doc.get("missing_paths").is_none(),
        "no phantom field when everything resolved: {text}"
    );
}

#[test]
fn mcp_depth_cutoff_reports_the_frontier_on_every_walk() {
    // MCP has no stderr channel, so the note the CLI prints there rides in
    // the response text instead — and the JSON form carries the flag (#32).
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub mod a;\npub mod b;\npub mod c;\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/a.rs"),
        "use crate::b::B;\npub struct A(pub B);\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/b.rs"),
        "use crate::c::C;\npub struct B(pub C);\n",
    )
    .unwrap();
    std::fs::write(root.join("src/c.rs"), "pub struct C;\n").unwrap();

    for (tool, file) in [("deps", "src/a.rs"), ("reverse_deps", "src/c.rs")] {
        let resp = call_tool(
            root,
            tool,
            serde_json::json!({"file": file, "depth": 1, "json": true, "rebuild": true}),
        );
        let (_, is_error, text) = result_of(&resp);
        assert!(!is_error, "{resp}");
        let doc: serde_json::Value = serde_json::from_str(text).expect("payload parses");
        assert_eq!(doc["frontier_truncated"], true, "{tool}: {text}");

        let resp = call_tool(root, tool, serde_json::json!({"file": file, "depth": 1}));
        let (_, _, text) = result_of(&resp);
        assert!(
            text.contains("raise --depth"),
            "{tool} text mode must carry the note: {text}"
        );
    }
}

#[test]
fn mcp_impact_depth_cutoff_reports_the_frontier() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "pub fn target() {}\npub fn c1() { target(); }\npub fn c2() { c1(); }\npub fn c3() { c2(); }\n",
    )
    .unwrap();

    let resp = call_tool(
        tmp.path(),
        "impact",
        serde_json::json!({"target": "target", "depth": 2, "json": true}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(!is_error, "{resp}");
    let doc: serde_json::Value = serde_json::from_str(text).expect("payload parses");
    assert_eq!(doc["impacts"][0]["frontier_truncated"], true, "{text}");

    let resp = call_tool(
        tmp.path(),
        "impact",
        serde_json::json!({"target": "target", "depth": 2}),
    );
    let (_, _, text) = result_of(&resp);
    assert!(text.contains("raise --depth"), "{text}");
}

/// The `[group]` marker is documented in README.md and SKILL.md, neither
/// of which an MCP client receives. `tools/list` is the only place a
/// client learns what a marker in `map`'s output means, so the sentence
/// has to live there too — and say the same thing, or the two drift.
#[test]
fn mcp_map_description_explains_the_group_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = list_tools(tmp.path());
    let desc = tools["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .find(|t| t["name"] == "map")
        .expect("map tool")["description"]
        .as_str()
        .expect("description");

    // What must not drift is the operational half — which JSON field holds
    // a block's comment, and that a member's own stays in `docs`. Asserting
    // those facts rather than a byte-identical passage leaves each surface
    // free to spell `--json` and `json: true` its own way, does not care
    // where in the description the passage sits, and cannot be satisfied by
    // a truncation that keeps the opening clause.
    let skill = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/ast-bro/SKILL.md"),
    )
    .unwrap();
    for (surface, text) in [
        ("the MCP map description", desc),
        ("SKILL.md", skill.as_str()),
    ] {
        for needle in [
            "A `[group]` line is a comment on the enclosing block, not on the symbol below it",
            "never as that member's",
            "`group`",
            "start_line",
            "end_line",
            "`docs`",
            "projected.docs: false",
        ] {
            assert!(
                text.contains(needle),
                "{surface} no longer states {needle:?}, so an agent reading it \
                 cannot tell a block's comment from a member's:\n{text}"
            );
        }
    }
}

/// Drive `ast-bro mcp` with initialize + `tools/list`, mirroring
/// `call_tool`'s bounded wait and id-based response selection.
fn list_tools(dir: &std::path::Path) -> serde_json::Value {
    let mut child = Command::new(bin())
        .arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp server");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        )
        .unwrap();
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
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["id"] == 2)
        .unwrap_or_else(|| panic!("no tools/list response; stdout:\n{stdout}\nstderr:\n{stderr}"))
}
