//! Manages a `<!-- ast-bro:begin v=X.Y.Z -->...<!-- ast-bro:end -->`
//! block inside a prose file. Pure string operations — caller does I/O.
//!
//! Also recognizes legacy `<!-- ast-outline:begin` markers for backward
//! compatibility, auto-migrating them to `<!-- ast-bro:begin` on write.

const BEGIN_PREFIX: &str = "<!-- ast-bro:begin";
const OLD_BEGIN_PREFIX: &str = "<!-- ast-outline:begin";
const END_MARKER: &str = "<!-- ast-bro:end -->";
const OLD_END_MARKER: &str = "<!-- ast-outline:end -->";

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Appended,
    Replaced,
    WrappedLegacy,
    UserEditsBlocked(String),
}

pub fn apply(
    file_contents: &str,
    new_block_body: &str,
    expected_block_body: &str,
    legacy_snippet: &str,
    version: &str,
    force: bool,
) -> (String, ApplyOutcome) {
    if let Some((start, end)) = find_block(file_contents) {
        let current_body = &file_contents[start.body_start..end.body_end];
        if !force && current_body.trim() != expected_block_body.trim() {
            let diff = simple_diff(current_body, new_block_body);
            return (file_contents.to_string(), ApplyOutcome::UserEditsBlocked(diff));
        }
        let mut out = String::with_capacity(file_contents.len());
        out.push_str(&file_contents[..start.line_start]);
        out.push_str(&render_block(new_block_body, version));
        out.push_str(&file_contents[end.line_end..]);
        return (out, ApplyOutcome::Replaced);
    }

    if !legacy_snippet.is_empty() {
        if let Some(idx) = file_contents.find(legacy_snippet) {
            let mut out = String::with_capacity(file_contents.len() + 80);
            out.push_str(&file_contents[..idx]);
            out.push_str(&render_block(new_block_body, version));
            out.push_str(&file_contents[idx + legacy_snippet.len()..]);
            return (out, ApplyOutcome::WrappedLegacy);
        }
    }

    let mut out = String::with_capacity(file_contents.len() + new_block_body.len() + 80);
    out.push_str(file_contents);
    if !file_contents.ends_with('\n') && !file_contents.is_empty() {
        out.push('\n');
    }
    if !file_contents.is_empty() {
        out.push('\n');
    }
    out.push_str(&render_block(new_block_body, version));
    (out, ApplyOutcome::Appended)
}

pub fn remove(file_contents: &str) -> (String, bool) {
    if let Some((start, end)) = find_block(file_contents) {
        let mut prefix = file_contents[..start.line_start].to_string();
        // Strip the blank-line separator that `apply` inserts before the block.
        if prefix.ends_with("\n\n") {
            prefix.pop();
        }
        let tail = file_contents[end.line_end..].to_string();
        return (format!("{}{}", prefix, tail), true);
    }
    (file_contents.to_string(), false)
}

/// True if the file looks like it already contains a hand-rolled copy of the
/// ast-bro agent snippet outside any managed marker block. Used to warn
/// before the auto-installer creates a second, marker-wrapped copy alongside
/// the manual one.
///
/// Casual prose mentions of "ast-bro" (e.g. project README references)
/// don't trigger — we look for snippet-shaped patterns: a backticked
/// `\`ast-bro\`` token (markdown code-formatted) or a CLI invocation
/// `ast-bro <subcommand>`. Both are characteristic of pasted instructions
/// and rare in incidental documentation.
pub fn has_unmanaged_brand_content(content: &str) -> bool {
    fn looks_like_snippet(text: &str) -> bool {
        // Backticked code reference: `ast-bro` or `ast-outline`.
        if text.contains("`ast-bro") || text.contains("`ast-outline") {
            return true;
        }
        // CLI invocation: `ast-bro ` or `ast-outline ` followed by a known
        // subcommand. Keep the list narrow — these are the most distinctive
        // shape tokens.
        const SUBCOMMANDS: &[&str] = &[
            "map",
            "digest",
            "show",
            "implements",
            "surface",
            "deps",
            "reverse-deps",
            "cycles",
            "graph",
            "search",
            "find-related",
            "index",
            "prompt",
            "mcp",
            "run",
        ];
        for sub in SUBCOMMANDS {
            let needle_bro = format!("ast-bro {}", sub);
            let needle_old = format!("ast-outline {}", sub);
            if text.contains(&needle_bro) || text.contains(&needle_old) {
                return true;
            }
        }
        false
    }

    if let Some((begin, end)) = find_block(content) {
        looks_like_snippet(&content[..begin.line_start])
            || looks_like_snippet(&content[end.line_end..])
    } else {
        looks_like_snippet(content)
    }
}

pub fn installed_version(file_contents: &str) -> Option<String> {
    let (start, _) = find_block(file_contents)?;
    let header = &file_contents[start.line_start..start.body_start];
    let prefix = "v=";
    let v_at = header.find(prefix)? + prefix.len();
    let rest = &header[v_at..];
    let end = rest.find(' ').or_else(|| rest.find("-->"))?;
    Some(rest[..end].trim().to_string())
}

struct BeginPos {
    line_start: usize,
    body_start: usize,
}
struct EndPos {
    body_end: usize,
    line_end: usize,
}

fn find_block(contents: &str) -> Option<(BeginPos, EndPos)> {
    // Try new marker first, then legacy ast-outline marker. The end marker
    // we look for must match the family of the begin marker we found —
    // legacy begin paired with new end (or vice versa) shouldn't match.
    let (begin_offset, end_marker) = match contents.find(BEGIN_PREFIX) {
        Some(off) => (off, END_MARKER),
        None => (contents.find(OLD_BEGIN_PREFIX)?, OLD_END_MARKER),
    };
    let begin_line_end = contents[begin_offset..].find('\n')? + begin_offset + 1;
    let line_start = contents[..begin_offset]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);

    let end_offset = contents[begin_line_end..].find(end_marker)? + begin_line_end;
    let end_line_start = contents[..end_offset]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let end_line_end = contents[end_offset..]
        .find('\n')
        .map(|i| end_offset + i + 1)
        .unwrap_or(contents.len());

    Some((
        BeginPos {
            line_start,
            body_start: begin_line_end,
        },
        EndPos {
            body_end: end_line_start,
            line_end: end_line_end,
        },
    ))
}

fn render_block(body: &str, version: &str) -> String {
    let mut s = String::with_capacity(body.len() + 80);
    s.push_str(&format!("<!-- ast-bro:begin v={} -->\n", version));
    s.push_str(body.trim_end_matches('\n'));
    s.push('\n');
    s.push_str(END_MARKER);
    s.push('\n');
    s
}

fn simple_diff(old: &str, new: &str) -> String {
    use similar::TextDiff;
    TextDiff::from_lines(old, new)
        .unified_diff()
        .header("installed", "new")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "## Hello\nLine.\n";
    const NEW_BODY: &str = "## Hello\nUpdated line.\n";

    #[test]
    fn appends_to_empty_file() {
        let (out, outcome) = apply("", BODY, BODY, "", "1.0.0", false);
        assert_eq!(outcome, ApplyOutcome::Appended);
        assert!(out.contains("<!-- ast-bro:begin v=1.0.0 -->"));
        assert!(out.contains("<!-- ast-bro:end -->"));
        assert!(out.contains("Line."));
    }

    #[test]
    fn appends_with_blank_line_after_existing_content() {
        let (out, outcome) = apply("Existing.\n", BODY, BODY, "", "1.0.0", false);
        assert_eq!(outcome, ApplyOutcome::Appended);
        assert!(out.starts_with("Existing.\n\n<!-- ast-bro:begin"));
    }

    #[test]
    fn replaces_existing_block_in_place() {
        let initial = format!(
            "Top.\n\n<!-- ast-bro:begin v=1.0.0 -->\n{}<!-- ast-bro:end -->\nBottom.\n",
            BODY
        );
        let (out, outcome) = apply(&initial, NEW_BODY, BODY, "", "2.0.0", false);
        assert_eq!(outcome, ApplyOutcome::Replaced);
        assert!(out.contains("v=2.0.0"));
        assert!(out.contains("Updated line."));
        assert!(out.starts_with("Top.\n\n"));
        assert!(out.ends_with("Bottom.\n"));
    }

    #[test]
    fn refuses_when_user_edited_block_without_force() {
        let initial =
            "<!-- ast-bro:begin v=1.0.0 -->\n## Hello\nUSER EDITED.\n<!-- ast-bro:end -->\n"
                .to_string();
        let (out, outcome) = apply(&initial, NEW_BODY, BODY, "", "2.0.0", false);
        assert_eq!(out, initial);
        match outcome {
            ApplyOutcome::UserEditsBlocked(diff) => {
                assert!(diff.contains("USER EDITED"));
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn force_overrides_user_edits() {
        let initial =
            "<!-- ast-bro:begin v=1.0.0 -->\n## Hello\nUSER EDITED.\n<!-- ast-bro:end -->\n"
                .to_string();
        let (out, outcome) = apply(&initial, NEW_BODY, BODY, "", "2.0.0", true);
        assert_eq!(outcome, ApplyOutcome::Replaced);
        assert!(out.contains("Updated line."));
        assert!(!out.contains("USER EDITED"));
    }

    #[test]
    fn wraps_legacy_snippet_in_place() {
        let legacy = "## Code exploration\nUse ast-bro.\n";
        let initial = format!("Top.\n\n{}\nBottom.\n", legacy);
        let (out, outcome) = apply(&initial, NEW_BODY, BODY, legacy, "1.0.0", false);
        assert_eq!(outcome, ApplyOutcome::WrappedLegacy);
        assert!(out.contains("<!-- ast-bro:begin v=1.0.0 -->"));
        assert!(!out.contains("## Code exploration\nUse ast-bro."));
    }

    #[test]
    fn idempotent_when_block_matches() {
        let (out1, _) = apply("", BODY, BODY, "", "1.0.0", false);
        let (out2, outcome) = apply(&out1, BODY, BODY, "", "1.0.0", false);
        assert_eq!(out1, out2);
        assert_eq!(outcome, ApplyOutcome::Replaced);
    }

    #[test]
    fn remove_strips_block_and_trailing_blank() {
        let (with_block, _) = apply("Top.\n", BODY, BODY, "", "1.0.0", false);
        let (out, removed) = remove(&with_block);
        assert!(removed);
        assert_eq!(out, "Top.\n");
    }

    #[test]
    fn remove_noop_when_absent() {
        let (out, removed) = remove("Just text.\n");
        assert!(!removed);
        assert_eq!(out, "Just text.\n");
    }

    #[test]
    fn installed_version_extracts_v_tag() {
        let (with_block, _) = apply("", BODY, BODY, "", "1.2.3", false);
        assert_eq!(installed_version(&with_block), Some("1.2.3".to_string()));
    }

    #[test]
    fn has_unmanaged_brand_content_flags_backticked_reference() {
        assert!(has_unmanaged_brand_content("Use `ast-bro` to explore.\n"));
    }

    #[test]
    fn has_unmanaged_brand_content_flags_cli_invocation() {
        assert!(has_unmanaged_brand_content("Run `ast-bro map src/`.\n"));
        assert!(has_unmanaged_brand_content("ast-bro digest .\n"));
    }

    #[test]
    fn has_unmanaged_brand_content_ignores_casual_prose_mention() {
        // A plain prose mention ("we use ast-bro") is not a pasted snippet
        // and shouldn't block the install.
        assert!(!has_unmanaged_brand_content(
            "Our team uses ast-bro and other tools.\n"
        ));
        assert!(!has_unmanaged_brand_content("Just normal docs.\n"));
        assert!(!has_unmanaged_brand_content(""));
    }

    #[test]
    fn has_unmanaged_brand_content_ignores_content_inside_block() {
        // Mention only inside marker block → managed, not loose.
        let (with_block, _) = apply(
            "",
            "Use `ast-bro` here.\n",
            "Use `ast-bro` here.\n",
            "",
            "1.0.0",
            false,
        );
        assert!(!has_unmanaged_brand_content(&with_block));
    }

    #[test]
    fn has_unmanaged_brand_content_finds_snippet_outside_block() {
        let (with_block, _) = apply(
            "I already pasted `ast-bro` instructions manually here.\n",
            BODY,
            BODY,
            "",
            "1.0.0",
            false,
        );
        assert!(has_unmanaged_brand_content(&with_block));
    }
}
