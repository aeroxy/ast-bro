//! Shared stdin/stdout plumbing for hook protocols.
//!
//! Both Claude Code and Gemini speak the same response shape:
//!   pass-through: `{"continue": true}`
//!   substitute:   `{"decision": "block", "reason": "<content>"}`
//!
//! Only the input parsing differs (field names, tool-name normalization),
//! which lives in the protocol-specific shims (`claude_code.rs`, `gemini.rs`).

use std::io::{self, Read, Write};

use serde::Serialize;

use super::decide::{decide, DecideOpts};
use super::event::{Decision, ToolCallEvent};

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

pub fn read_stdin() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

pub fn dispatch(event: ToolCallEvent, opts: &DecideOpts) -> i32 {
    match decide(&event, opts) {
        Decision::PassThrough => emit_pass_through(),
        Decision::Substitute { content } => emit_substitute(content),
    }
}

pub fn emit_pass_through() -> i32 {
    let r = PassThroughResponse { cont: true };
    match serde_json::to_string(&r) {
        Ok(json) => match writeln!(io::stdout(), "{}", json) {
            Ok(_) => 0,
            Err(e) => {
                let _ = writeln!(io::stderr(), "hook: stdout write failed: {}", e);
                1
            }
        },
        Err(e) => {
            let _ = writeln!(io::stderr(), "hook: serialization failed: {}", e);
            1
        }
    }
}

pub fn emit_substitute(content: String) -> i32 {
    let r = SubstituteResponse {
        decision: "block",
        reason: content,
    };
    match serde_json::to_string(&r) {
        Ok(json) => match writeln!(io::stdout(), "{}", json) {
            Ok(_) => 0,
            Err(e) => {
                let _ = writeln!(io::stderr(), "hook: stdout write failed: {}", e);
                1
            }
        },
        Err(e) => {
            let _ = writeln!(io::stderr(), "hook: serialization failed: {}", e);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
