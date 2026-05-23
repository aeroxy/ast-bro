//! Claude Code PreToolUse hook protocol shim.
//!
//! Claude Code's PreToolUse event JSON shape:
//!   { "tool_name": "Read",
//!     "tool_input": { "file_path": "...", "offset": ..., "limit": ... } }
//!
//! Response shape is shared with Gemini — see `super::io`.

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
    file_path: Option<PathBuf>,
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
    dispatch(
        ToolCallEvent {
            tool_name: event.tool_name,
            file_path: event.tool_input.file_path,
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
    fn input_event_parses_minimal_shape() {
        let json = r#"{"tool_name":"Read","tool_input":{"file_path":"a.rs"}}"#;
        let e: InputEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.tool_name, "Read");
        assert_eq!(e.tool_input.file_path, Some(PathBuf::from("a.rs")));
    }
}
