//! Shared stdin/stdout plumbing for hook protocols.
//!
//! Pass-through is one shape everywhere: `{"continue": true}`. A substitution
//! has one shape per [`Channel`]: a `{"decision": "block"}` refusal the host
//! reports as a failed call, a `hookSpecificOutput.updatedToolOutput`
//! replacement it reports as a successful one, and a
//! `hookSpecificOutput.additionalContext` payload for a result no hook may
//! replace.
//!
//! Only the input parsing differs per host (field names, tool-name
//! normalization), which lives in the protocol shims (`claude_code.rs`,
//! `gemini.rs`).

use std::io::{self, Read, Write};

use serde::Serialize;

use super::decide::{decide, DecideOpts};
use super::event::{Channel, Decision, ToolCallEvent};

/// Notice leading a [`Channel::Deny`] payload. The host renders the refusal as
/// a failed tool call, so the first line says outright that nothing failed —
/// a reader who takes the wrapper at face value abandons the answer it holds.
const DENY_NOTICE: &str = "\
# ast-bro substituted this structure map for the full Read: nothing failed.
# The map below is the answer, with line ranges. For more detail, re-read with
# offset/limit, or `ast-bro show <file> <symbol>` for a symbol's body.";

/// Notice leading a [`Channel::Replace`] payload. The read succeeded here, so
/// this one has no failure to disclaim and spends its words on what arrived.
const REPLACE_NOTICE: &str = "\
# ast-bro replaced the file contents with this structure map, with line ranges.
# For more detail, re-read with offset/limit, or `ast-bro show <file> <symbol>`
# for a symbol's body.";

/// Notice leading a [`Channel::Augment`] payload.
///
/// It states only what the hook knows. The event fires for every kind of tool
/// failure and the shim reads no error field, so naming a cause or predicting
/// that a retry fails again would be a guess — and a wrong one whenever the
/// read failed for a reason a retry would clear.
const AUGMENT_NOTICE: &str = "\
# ast-bro supplies this structure map, with line ranges, because the Read did
# not return the file. Read a range with offset/limit, or run
# `ast-bro show <file> <symbol>` for one symbol's body.";

/// The most a notice plus its separators can add to a map in [`payload`].
///
/// `decide` holds this back from its byte ceiling, so the ceiling bounds what is
/// delivered rather than only the map inside it.
pub(super) const MAX_NOTICE_BYTES: usize = widest(
    DENY_NOTICE.len(),
    REPLACE_NOTICE.len(),
    AUGMENT_NOTICE.len(),
) + 3; // payload joins with "\n\n" and ends with "\n"

const fn widest(a: usize, b: usize, c: usize) -> usize {
    let ab = if a > b { a } else { b };
    if ab > c {
        ab
    } else {
        c
    }
}

#[derive(Debug, Serialize)]
struct PassThroughResponse {
    #[serde(rename = "continue")]
    cont: bool,
}

#[derive(Debug, Serialize)]
struct DenyResponse {
    decision: &'static str,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplaceResponse {
    hook_specific_output: PostToolUseOutput,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PostToolUseOutput {
    hook_event_name: &'static str,
    updated_tool_output: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AugmentResponse {
    hook_specific_output: AdditionalContextOutput,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdditionalContextOutput {
    hook_event_name: &'static str,
    additional_context: String,
}

pub fn read_stdin() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

pub fn dispatch(event: ToolCallEvent, opts: &DecideOpts, channel: Channel) -> i32 {
    match decide(&event, opts, channel) {
        Decision::PassThrough => emit_pass_through(),
        Decision::Substitute { map } => emit_substitute(channel, map),
    }
}

pub fn emit_pass_through() -> i32 {
    emit(&PassThroughResponse { cont: true })
}

pub fn emit_substitute(channel: Channel, map: String) -> i32 {
    match channel {
        Channel::Deny => emit(&DenyResponse {
            decision: "block",
            reason: payload(DENY_NOTICE, &map),
        }),
        Channel::Replace => emit(&ReplaceResponse {
            hook_specific_output: PostToolUseOutput {
                hook_event_name: "PostToolUse",
                updated_tool_output: payload(REPLACE_NOTICE, &map),
            },
        }),
        Channel::Augment => emit(&AugmentResponse {
            hook_specific_output: AdditionalContextOutput {
                hook_event_name: "PostToolUseFailure",
                additional_context: payload(AUGMENT_NOTICE, &map),
            },
        }),
    }
}

/// Puts `notice` ahead of `map`, separated by a blank line. The notice leads
/// and appears once: a duplicate at the end is noise on the tool's
/// highest-frequency output path, and a reader who has scrolled that far has
/// already decided what to do with the payload.
fn payload(notice: &str, map: &str) -> String {
    format!("{}\n\n{}\n", notice, map)
}

fn emit<T: Serialize>(response: &T) -> i32 {
    match serde_json::to_string(response) {
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

    /// Every channel, so a new one cannot slip past the notice tests below.
    const ALL_CHANNELS: [Channel; 3] = [Channel::Deny, Channel::Replace, Channel::Augment];

    fn notice_of(channel: Channel) -> &'static str {
        match channel {
            Channel::Deny => DENY_NOTICE,
            Channel::Replace => REPLACE_NOTICE,
            Channel::Augment => AUGMENT_NOTICE,
        }
    }

    #[test]
    fn pass_through_response_serializes_with_continue_true() {
        let r = PassThroughResponse { cont: true };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"continue\":true"));
    }

    #[test]
    fn deny_response_serializes_with_decision_block() {
        let r = DenyResponse {
            decision: "block",
            reason: "x".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"decision\":\"block\""));
        assert!(s.contains("\"reason\":\"x\""));
    }

    /// The replacement rides `hookSpecificOutput.updatedToolOutput` under the
    /// host's camelCase spelling. A field the host does not recognize is
    /// ignored in silence, which shows up as the full file reaching the model.
    #[test]
    fn replace_response_serializes_with_updated_tool_output() {
        let r = ReplaceResponse {
            hook_specific_output: PostToolUseOutput {
                hook_event_name: "PostToolUse",
                updated_tool_output: "x".into(),
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"x"}}"#
        );
    }

    /// Every channel's notice leads the payload, appears once, and names both
    /// recovery routes — a substitution the reader cannot act on is a map they
    /// will look up a second way.
    #[test]
    fn every_notice_leads_once_and_names_both_recovery_routes() {
        for channel in ALL_CHANNELS {
            let notice = notice_of(channel);
            let p = payload(notice, "# map body\nfn f() L1-2");
            assert!(p.starts_with("# ast-bro "), "{channel:?}: {p}");
            assert_eq!(
                p.matches("# ast-bro ").count(),
                1,
                "{channel:?}: notice must appear exactly once:\n{p}"
            );
            for phrase in ["structure map", "offset/limit", "ast-bro show"] {
                assert!(
                    p.contains(phrase),
                    "{channel:?}: notice must mention '{phrase}':\n{p}"
                );
            }
            assert!(p.contains("# map body"), "{channel:?}: {p}");
        }
    }

    /// Every notice keeps its recovery command on one line.
    ///
    /// The notices are comment-prefixed and hard-wrapped, so a wrap inside the
    /// backticks puts the next line's `#` between the command's arguments: the
    /// reader copies `ast-bro show <file>` and a comment, and `show` exits 2 for
    /// a missing symbol. Nothing else in the payload would look wrong.
    #[test]
    fn every_notice_keeps_its_command_on_one_line() {
        for channel in ALL_CHANNELS {
            let notice = notice_of(channel);
            let line = notice
                .lines()
                .find(|l| l.contains("ast-bro show"))
                .unwrap_or_else(|| panic!("{channel:?}: no line names `ast-bro show`"));
            assert!(
                line.contains("ast-bro show <file> <symbol>`"),
                "{channel:?}: the command is split across lines: {line}"
            );
        }
    }

    /// Only the refusal disclaims a failure. Saying "nothing failed" on a
    /// channel that reported success invents the very failure issue #34 is
    /// about.
    #[test]
    fn only_the_deny_notice_disclaims_a_failure() {
        assert!(DENY_NOTICE.contains("nothing failed"));
        assert!(!REPLACE_NOTICE.contains("failed"));
    }

    /// The augment notice claims no cause and predicts no retry. The event
    /// fires for every kind of failure and the shim reads no error field, so
    /// either claim would be invented — and wrong whenever a retry would work.
    #[test]
    fn the_augment_notice_asserts_no_cause_and_no_prediction() {
        // Assert the claim, not where it wraps: re-wrapping the notice is a
        // legitimate edit, and `every_notice_keeps_its_command_on_one_line` is
        // the guard that a wrap must not break.
        let flowed = AUGMENT_NOTICE.replace("\n# ", " ");
        assert!(flowed.contains("because the Read did not return the file"), "{flowed}");
        for invented in ["nothing failed", "same way", "will fail", "too large", "exceeds"] {
            assert!(
                !flowed.contains(invented),
                "notice must not claim '{invented}':\n{flowed}"
            );
        }
    }

    /// `additionalContext` is the only channel `PostToolUseFailure` offers, and
    /// it must carry the host's own camelCase spelling or the map is dropped.
    #[test]
    fn augment_response_serializes_with_additional_context() {
        let r = AugmentResponse {
            hook_specific_output: AdditionalContextOutput {
                hook_event_name: "PostToolUseFailure",
                additional_context: "x".into(),
            },
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUseFailure","additionalContext":"x"}}"#
        );
    }
}
