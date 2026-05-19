//! Gemini CLI hook protocol shim.
//!
//! Gemini's BeforeTool event JSON shape per the official docs:
//!   { "tool_name": "read_file",
//!     "tool_input": { "absolute_path": "...", "offset": ..., "limit": ... } }
//!
//! Response shape matches Claude Code's:
//!   pass-through: { "continue": true }
//!   substitute:   { "decision": "block", "reason": "\<content\>" }

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
    absolute_path: Option<PathBuf>,
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
    // decide() keys on "Read"; Gemini sends "read_file".
    let normalized_tool = if event.tool_name == "read_file" {
        "Read".to_string()
    } else {
        event.tool_name
    };
    let event = ToolCallEvent {
        tool_name: normalized_tool,
        file_path: event.tool_input.absolute_path,
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
    fn input_event_parses_gemini_shape() {
        let json = r#"{"tool_name":"read_file","tool_input":{"absolute_path":"/x/a.rs"}}"#;
        let e: InputEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.tool_name, "read_file");
        assert_eq!(
            e.tool_input.absolute_path,
            Some(PathBuf::from("/x/a.rs"))
        );
    }
}
