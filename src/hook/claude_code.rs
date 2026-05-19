use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::decide::{decide, DecideOpts};
use super::event::{Decision, ToolCallEvent};

#[derive(Debug, Deserialize)]
struct InputEvent {
    tool_name: String,
    #[serde(default)]
    tool_input: ToolInput,
}

#[derive(Debug, Default, Deserialize)]
struct ToolInput {
    #[serde(default)]
    file_path: Option<PathBuf>,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Debug, Serialize)]
struct PassThroughResponse {
    #[serde(rename = "continue")]
    cont: bool,
}

#[derive(Debug, Serialize)]
struct SubstituteResponse {
    decision: &'static str,
    reason: String,
}

pub fn run(opts: DecideOpts) -> i32 {
    let mut buf = String::new();
    if io::stdin().read_to_string(&mut buf).is_err() {
        return emit_pass_through();
    }
    let event: InputEvent = match serde_json::from_str(&buf) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ast-bro hook: bad stdin json: {}", e);
            return emit_pass_through();
        }
    };
    let event = ToolCallEvent {
        tool_name: event.tool_name,
        file_path: event.tool_input.file_path,
        has_offset_or_limit: event.tool_input.offset.is_some()
            || event.tool_input.limit.is_some(),
    };
    match decide(&event, &opts) {
        Decision::PassThrough => emit_pass_through(),
        Decision::Substitute { content } => emit_substitute(content),
    }
}

fn emit_pass_through() -> i32 {
    let r = PassThroughResponse { cont: true };
    let _ = writeln!(io::stdout(), "{}", serde_json::to_string(&r).unwrap());
    0
}

fn emit_substitute(content: String) -> i32 {
    let r = SubstituteResponse {
        decision: "block",
        reason: content,
    };
    let _ = writeln!(io::stdout(), "{}", serde_json::to_string(&r).unwrap());
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn pass_through_response_serializes_with_continue_true() {
        let r = PassThroughResponse { cont: true };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"continue\":true"));
    }

    #[test]
    fn substitute_response_serializes_with_decision_block() {
        let r = SubstituteResponse {
            decision: "block",
            reason: "x".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"decision\":\"block\""));
        assert!(s.contains("\"reason\":\"x\""));
    }

    #[test]
    fn input_event_parses_minimal_shape() {
        let json = r#"{"tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
        let e: InputEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.tool_name, "Read");
        assert_eq!(e.tool_input.file_path, Some(PathBuf::from("a.rs")));
    }
}
