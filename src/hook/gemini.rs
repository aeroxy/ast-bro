//! Gemini CLI hook protocol shim.
//!
//! Gemini's BeforeTool event JSON shape per the official docs:
//!   { "tool_name": "read_file",
//!     "tool_input": { "absolute_path": "...", "offset": ..., "limit": ... } }
//!
//! Response shape is shared with Claude Code — see `super::io`.

use std::path::PathBuf;

use serde::Deserialize;

use super::decide::DecideOpts;
use super::event::ToolCallEvent;
use super::io::{dispatch, emit_pass_through, read_stdin};

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

pub fn run(opts: DecideOpts) -> i32 {
    let buf = match read_stdin() {
        Ok(b) => b,
        Err(_) => return emit_pass_through(),
    };
    let event: InputEvent = match serde_json::from_str(&buf) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ast-bro hook: bad stdin json: {}", e);
            return emit_pass_through();
        }
    };
    // decide() keys on "Read"; Gemini sends "read_file".
    let tool_name = if event.tool_name == "read_file" {
        "Read".to_string()
    } else {
        event.tool_name
    };
    dispatch(
        ToolCallEvent {
            tool_name,
            file_path: event.tool_input.absolute_path,
            has_offset_or_limit: event.tool_input.offset.is_some()
                || event.tool_input.limit.is_some(),
        },
        &opts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
