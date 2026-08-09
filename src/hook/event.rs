use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub tool_name: String,
    pub file_path: Option<PathBuf>,
    pub has_offset_or_limit: bool,
}

#[derive(Debug, Clone)]
pub enum Decision {
    PassThrough,
    /// The map to put where the file contents would have gone, with no notice
    /// wrapped around it. [`Channel`] owns that wording, because what the
    /// notice has to disclaim depends on how the host reports the payload.
    Substitute { map: String },
}

/// How a host lets a hook put its own content where a tool's output would go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Refuse the call and carry the map in the rejection. The read never
    /// runs, and the host renders the refusal as a failed tool call, so the
    /// payload has to open by saying that nothing failed. Gemini's
    /// `BeforeTool` and Claude Code's `PreToolUse` offer only this.
    Deny,
    /// Let the read run and swap its output for the map, so the host reports a
    /// successful tool call rather than a failure that did not happen.
    ///
    /// Claude Code exposes this as `PostToolUse` `updatedToolOutput`, and
    /// `ast-bro install` does **not** register it. Measured on 2.1.223: the
    /// field replaces the result of an MCP tool and is ignored for the built-in
    /// `Read` and `Bash`, so registering it would drop the substitution and
    /// send the whole file to the model. Kept because the shim reaches it from
    /// any hand-written `PostToolUse` entry, and because deleting it would
    /// throw away the measurement. See issue #34.
    Replace,
    /// Deliver the map beside a result the hook cannot replace. Claude Code's
    /// `PostToolUseFailure` offers only `additionalContext`, which is enough
    /// here: the read failed, so there is no file contents for the map to
    /// compete with, and the alternative is an error with no map at all.
    Augment,
}

impl Channel {
    /// True when the payload is added to a result rather than replacing one.
    ///
    /// The gates that keep a substitution from withholding what the caller asked
    /// for do not apply here: the read already ran and returned nothing, so the
    /// map takes the place of nothing. It is also the only channel where a line
    /// count is the wrong question — the host refuses on byte size, so a
    /// 90-line file of 355 KB trips the refusal and no line threshold.
    pub fn is_additive(self) -> bool {
        matches!(self, Channel::Augment)
    }
}
