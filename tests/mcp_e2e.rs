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
        assert!(is_error, "unresolved trace target must be isError (json={json}): {resp}");
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
    assert!(is_error, "unknown type must not read as an empty answer: {resp}");
    assert!(text.contains("NoSuchType"), "{text}");

    // A real type with no implementors stays a legitimate 0-match success.
    let resp = call_tool(
        tmp.path(),
        "implements",
        serde_json::json!({"target": "Leaf", "paths": ["."]}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(!is_error, "an empty answer for an existing type is still an answer: {resp}");
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
    assert!(is_error, "json mode must not return a valid-looking empty results list: {resp}");
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
    std::fs::write(root.join("src/lib.rs"), "pub mod a;\npub mod b;\npub mod c;\n").unwrap();
    std::fs::write(root.join("src/a.rs"), "use crate::b::B;\npub struct A(pub B);\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "use crate::c::C;\npub struct B(pub C);\n").unwrap();
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

/// Fills `dir` with enough declarations that a `map` of it clears the 25 KB
/// hint threshold several times over.
fn oversized_fixture(dir: &std::path::Path) {
    for f in 0..20 {
        let mut src = String::new();
        for i in 0..60 {
            src.push_str(&format!(
                "pub fn generated_function_{f}_{i}(argument: usize) -> usize {{ argument }}\n"
            ));
        }
        std::fs::write(dir.join(format!("file_{f}.rs")), src).unwrap();
    }
}

#[test]
fn mcp_map_oversized_directory_carries_the_digest_hint() {
    // #35 on the MCP surface: no stderr to put a note on, so the hint rides
    // the response, and it names a tool call rather than a shell command.
    let tmp = tempfile::tempdir().unwrap();
    oversized_fixture(tmp.path());
    let resp = call_tool(tmp.path(), "map", serde_json::json!({"paths": ["."]}));
    let (_, is_error, text) = result_of(&resp);
    assert!(
        !is_error,
        "a hint qualifies an answer, it does not reject it: {resp}"
    );
    assert!(
        text.starts_with("# hint:") && text.contains("`map` tool"),
        "expected a leading digest hint: {}",
        &text[..text.len().min(200)]
    );
}

#[test]
fn mcp_map_json_carries_the_hint_as_a_field() {
    // A prepended line would break the parser on the other end, so JSON
    // takes the hint the way it takes missing_paths.
    let tmp = tempfile::tempdir().unwrap();
    oversized_fixture(tmp.path());
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["."], "json": true}),
    );
    let (_, is_error, text) = result_of(&resp);
    assert!(!is_error, "{resp}");
    let doc: serde_json::Value =
        serde_json::from_str(text).expect("payload must stay parseable JSON");
    assert_eq!(doc["schema"], "ast-bro.map.v1");
    assert!(
        doc["hint"]
            .as_str()
            .is_some_and(|h| h.contains("`map` tool")),
        "expected a hint field: {}",
        &text[..text.len().min(200)]
    );
}

#[test]
fn mcp_digest_tool_stays_quiet_but_an_inflated_call_does_not() {
    // The digest answer is the resolved axes, not the tool that was called.
    let tmp = tempfile::tempdir().unwrap();
    oversized_fixture(tmp.path());
    let quiet = call_tool(tmp.path(), "digest", serde_json::json!({"paths": ["."]}));
    let (_, _, quiet_text) = result_of(&quiet);
    assert!(
        !quiet_text.contains("# hint:"),
        "the digest answer must not point at itself: {}",
        &quiet_text[..quiet_text.len().min(200)]
    );
    let inflated = call_tool(
        tmp.path(),
        "digest",
        serde_json::json!({"paths": ["."], "include_private": true, "include_fields": true}),
    );
    let (_, _, inflated_text) = result_of(&inflated);
    assert!(
        inflated_text.starts_with("# hint:"),
        "an inflated digest call is not the digest answer: {}",
        &inflated_text[..inflated_text.len().min(200)]
    );
}

#[test]
fn mcp_map_of_a_single_file_stays_quiet_however_large() {
    // Size alone is not the trigger. This file clears the threshold on its
    // own, and one file is what `map` is for at any size.
    let tmp = tempfile::tempdir().unwrap();
    let mut src = String::new();
    for i in 0..800 {
        src.push_str(&format!(
            "pub fn generated_function_{i}(argument: usize) -> usize {{ argument }}\n"
        ));
    }
    std::fs::write(tmp.path().join("one_big_file.rs"), src).unwrap();
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["one_big_file.rs"]}),
    );
    let (_, _, text) = result_of(&resp);
    assert!(
        text.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        text.len()
    );
    assert!(
        !text.contains("# hint:"),
        "one file has nothing smaller to point at: {}",
        &text[..text.len().min(200)]
    );
}

#[test]
fn mcp_hint_carries_the_call_s_own_arguments() {
    // The suggestion replaces the call it qualifies, so naming only the tool
    // would answer a different question — the same defect the CLI hint had
    // when it dropped --glob.
    let tmp = tempfile::tempdir().unwrap();
    oversized_fixture(tmp.path());
    let resp = call_tool(
        tmp.path(),
        "map",
        serde_json::json!({"paths": ["."], "glob": "*.rs", "max_members": 3}),
    );
    let (_, _, text) = result_of(&resp);
    let hint = text.lines().next().unwrap_or("");
    assert!(hint.starts_with("# hint:"), "{text}");
    assert!(
        hint.contains("\"glob\":\"*.rs\""),
        "the caller's glob must survive into the suggestion: {hint}"
    );
    assert!(
        hint.contains("\"max_members\":3"),
        "a tighter cap must not be loosened to 8: {hint}"
    );
}

#[test]
fn mcp_directory_holding_one_file_stays_quiet() {
    // The case maintainer fix #2 changed: one file is one file however it
    // was reached, so "multi-file answer" must not be printed.
    let tmp = tempfile::tempdir().unwrap();
    let mut src = String::new();
    for i in 0..800 {
        src.push_str(&format!(
            "pub fn generated_function_{i}(argument: usize) -> usize {{ argument }}\n"
        ));
    }
    std::fs::write(tmp.path().join("only.rs"), src).unwrap();
    let resp = call_tool(tmp.path(), "map", serde_json::json!({"paths": ["."]}));
    let (_, _, text) = result_of(&resp);
    assert!(
        text.len() > 25_000,
        "fixture must exceed the threshold, got {} bytes",
        text.len()
    );
    assert!(
        !text.contains("# hint:"),
        "one file in a directory is still one file: {}",
        &text[..text.len().min(200)]
    );
}

#[test]
fn mcp_following_the_suggestion_lands_on_a_quiet_call() {
    // The suggestion's contract is that it is the digest answer, so sending
    // it back must not produce a second hint. The call names many paths on
    // purpose: an argument object that omits any of them is not a call the
    // tool accepts, and an object is what an agent copies whole.
    let tmp = tempfile::tempdir().unwrap();
    oversized_fixture(tmp.path());
    let named: Vec<String> = (0..20).map(|f| format!("file_{f}.rs")).collect();
    let resp = call_tool(tmp.path(), "map", serde_json::json!({"paths": named}));
    let (_, _, text) = result_of(&resp);
    let hint = text.lines().next().expect("hint line");
    let args: serde_json::Value = serde_json::from_str(
        &hint[hint.find('{').expect("argument object")..=hint.rfind('}').expect("argument object")],
    )
    .expect("the suggested arguments must parse");
    let again = call_tool(tmp.path(), "map", args);
    let (_, is_error, followed) = result_of(&again);
    assert!(!is_error, "the suggested call must be accepted: {again}");
    assert!(
        !followed.contains("# hint:"),
        "following the suggestion must settle: {}",
        &followed[..followed.len().min(200)]
    );
}
