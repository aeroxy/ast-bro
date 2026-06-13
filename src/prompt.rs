//! Agent prompt content. The source of truth lives in
//! `skills/ast-bro/SKILL.md` (YAML frontmatter + the agent prompt body).
//! This module embeds that file at compile time via `include_str!` and
//! strips the YAML frontmatter on demand so `ast-bro prompt`, the CLI
//! installers, and the manual skill file all stay in lockstep.

/// Raw SKILL.md — frontmatter + prompt body. Embedded at compile time,
/// so any edit to `skills/ast-bro/SKILL.md` triggers a rebuild.
pub const SKILL_MD: &str = include_str!("../skills/ast-bro/SKILL.md");

/// The prompt body: SKILL.md with its YAML frontmatter block removed.
///
/// Computed once on first access and cached in a `OnceLock<&'static str>` —
/// subsequent calls return the memoized slice with no allocation. Exposing
/// this as a function (rather than a const) keeps the compile-time cost
/// free: `include_str!` + const slicing would require a const-eval-friendly
/// parser, whereas this is just eight lines of runtime string splitting.
pub fn agent_prompt() -> &'static str {
    static CACHED: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    CACHED.get_or_init(|| strip_frontmatter(SKILL_MD))
}

/// Strip YAML frontmatter (bounded by two `---` lines) from `s`. Returns
/// the original string unchanged if no `---` opener is found — defensive
/// against a future SKILL.md without frontmatter.
fn strip_frontmatter(s: &'static str) -> &'static str {
    if !s.starts_with("---\n") && !s.starts_with("---\r\n") && s != "---" {
        return s;
    }
    let mut offset = 4;
    let bytes = s.as_bytes();
    while offset < bytes.len() {
        let rest = &s[offset..];
        if rest.starts_with("---\r\n") {
            return skip_one_newline(&s[offset + 5..]);
        }
        if rest.starts_with("---\n") {
            return skip_one_newline(&s[offset + 4..]);
        }
        if rest == "---" {
            return "";
        }
        match rest.find('\n') {
            Some(nl) => offset += nl + 1,
            None => break,
        }
    }
    s
}

/// Skip one leading `\r\n` or `\n` if present after the closing `---`.
#[inline]
fn skip_one_newline(s: &'static str) -> &'static str {
    if let Some(stripped) = s.strip_prefix("\r\n") {
        stripped
    } else if let Some(stripped) = s.strip_prefix('\n') {
        stripped
    } else {
        s
    }
}

/// Returns the complete SKILL.md content (frontmatter + body) as a static
/// string. Used by installers that write the file directly.
pub fn agent_skill_md() -> &'static str {
    SKILL_MD
}

/// YAML frontmatter written at the top of `.claude/agents/Explore.md` on a
/// fresh install. A marker block wrapping the agent-prompt body is appended
/// after it, so Claude Code sees valid frontmatter at file offset 0.
pub const EXPLORE_FRONTMATTER: &str = "---\nname: Explore\ndescription: Fast read-only search agent for locating code. Use it to find files by pattern, grep for symbols or keywords, or answer \"where is X defined / which files reference Y.\" Do NOT use it for code review, design-doc auditing, cross-file consistency checks, or open-ended analysis — it reads excerpts rather than whole files and will miss content past its read window. When calling, specify search breadth: \"quick\" for a single targeted lookup, \"medium\" for moderate exploration, or \"very thorough\" to search across multiple locations and naming conventions.\n---\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_md_starts_with_yaml_frontmatter() {
        // Newline-agnostic: `lines()` strips a trailing \r, so this accepts
        // both LF and CRLF checkouts.
        assert!(
            SKILL_MD.lines().next() == Some("---"),
            "SKILL.md must open with a YAML frontmatter marker"
        );
    }

    #[test]
    fn agent_prompt_strips_frontmatter() {
        let body = agent_prompt();
        assert!(
            !body.starts_with("---"),
            "stripped prompt should not start with `---`"
        );
        assert!(
            body.starts_with("## Use `sb`"),
            "prompt body should start with the H1; got: {:?}",
            &body[..body.len().min(40)]
        );
    }

    #[test]
    fn round_trip_matches_skill_md() {
        let expected = strip_frontmatter(SKILL_MD);
        assert_eq!(agent_prompt(), expected, "memoized value must equal fresh strip");
        assert!(
            SKILL_MD.contains(expected),
            "stripped body must appear verbatim inside SKILL.md"
        );
    }

    #[test]
    fn strip_handles_no_frontmatter() {
        // A string without `---` should be returned unchanged.
        let s: &str = "## Plain content\n\nNo frontmatter here.\n";
        // SAFETY: we pass a static str to the helper; this mirrors what
        // include_str! provides at compile time.
        let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
        assert_eq!(strip_frontmatter(leaked), leaked);
    }
}
