//! Claude Code hook protocol shim, for both events that can carry a map.
//!
//! Claude Code's tool-call event JSON shape:
//!   { "hook_event_name": "PostToolUse",
//!     "tool_name": "Read",
//!     "tool_input": { "file_path": "...", "offset": ..., "limit": ... } }
//!
//! `hook_event_name` picks the response channel, so one command serves an
//! entry registered under either event and `ast-bro install` can move the
//! entry without changing the command string. Response shapes live in
//! `super::io`.

use std::path::PathBuf;

use serde::Deserialize;

use super::decide::DecideOpts;
use super::event::{Channel, ToolCallEvent};
use super::io::{dispatch, emit_pass_through, read_stdin};

#[derive(Debug, Deserialize)]
struct InputEvent {
    tool_name: String,
    /// The event that fired, absent on a host too old to send it. Only
    /// `PostToolUse` can replace a delivered result and only
    /// `PostToolUseFailure` reaches a read the host rejected, so everything
    /// else — including a missing value — falls back to refusing the call.
    #[serde(default)]
    hook_event_name: Option<String>,
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
    let channel = match event.hook_event_name.as_deref() {
        Some("PostToolUse") => Channel::Replace,
        Some("PostToolUseFailure") => Channel::Augment,
        _ => Channel::Deny,
    };
    dispatch(
        ToolCallEvent {
            tool_name: event.tool_name,
            file_path: event.tool_input.file_path,
            has_offset_or_limit: event.tool_input.offset.is_some()
                || event.tool_input.limit.is_some(),
        },
        &opts,
        channel,
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
        assert_eq!(e.hook_event_name, None);
    }

    #[test]
    fn input_event_parses_hook_event_name() {
        let json = r#"{"hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{}}"#;
        let e: InputEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.hook_event_name.as_deref(), Some("PostToolUse"));
    }
}
