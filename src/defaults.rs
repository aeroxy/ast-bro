//! Default values that more than one declaration site has to agree on.
//!
//! Most are read twice: by clap for a CLI subcommand (`default_value_t =
//! defaults::LIMIT`) and by serde for the MCP tool that mirrors it (`default =
//! "crate::defaults::limit"`). The MCP schemas interpolate the same constant
//! into the property description a client reads, so the number an agent is told
//! and the number it gets come from one place.
//!
//! Each constant is named for what it bounds rather than for its value. The
//! serde helpers below exist only because `#[serde(default = "…")]` takes a
//! function path where clap takes an expression.
//!
//! The guards hold a value to this module as soon as it appears in an
//! `impl Default`, whether or not a second site reads it yet. Asking how many
//! sites a value has is what the guards cannot do reliably — every constant
//! here has at least two today, and `--preset digest` had exactly one when it
//! drifted.

use std::path::PathBuf;

/// Repo root a command walks when the caller names no path.
pub const ROOT: &str = ".";

/// BFS depth for `callers` and `callees`, in call-graph edges.
pub const CALL_DEPTH: usize = 1;

/// Depth of the transitive cone `impact` reports, in call-graph edges.
pub const IMPACT_DEPTH: usize = 2;

/// BFS depth for `deps` and `reverse-deps`, in import edges.
pub const FILE_DEPTH: usize = 3;

/// Longest call path `trace` searches for, in hops.
pub const TRACE_DEPTH: usize = 12;

/// Display cap on `callers`, `callees`, `impact`, and `reverse-deps`.
///
/// It bounds what is rendered, not what is walked: the reported total counts
/// the whole cone whether or not the rows fit.
pub const LIMIT: usize = 200;

/// Rendered bodies `show` prints when it searched more than one file.
///
/// A bare common name (`new`, `build`) can match a hundred declarations across
/// a repository, each with a full body; the cap keeps that from becoming the
/// whole answer, and the header always reports the true total (issue #32). A
/// single explicit file is never capped.
pub const SHOW_LIMIT: usize = 20;

/// Results `search` and `find-related` return.
pub const TOP_K: usize = 10;

/// Members per type under `map --preset digest`.
pub const MAX_MEMBERS: usize = 50;

/// Doc-comment lines `map` prints per declaration before clipping.
pub const MAX_DOC_LINES: usize = 6;

/// Nesting levels of type headings `digest` renders before flattening.
pub const MAX_HEADING_DEPTH: usize = 3;

/// Token budget `context` fills, counted at roughly four bytes per token.
pub const BUDGET: usize = 8000;

/// Smallest strongly connected component `cycles` reports, in files.
pub const MIN_SIZE: usize = 2;

/// Recursion guard for the re-export chains `surface` follows.
pub const SURFACE_MAX_DEPTH: usize = 16;

/// Section `impact` reports when the caller names no mode.
pub const IMPACT_MODE: &str = "all";

/// File length, in lines, above which the `Read` hook substitutes a map.
///
/// Shared by `hook` and by the `install` flag that writes the hook's
/// configuration, which is why the two agree.
pub const HOOK_MIN_LINES: usize = 200;

/// Serde form of [`ROOT`].
pub fn root() -> PathBuf {
    PathBuf::from(ROOT)
}

/// Serde form of [`CALL_DEPTH`].
pub fn call_depth() -> usize {
    CALL_DEPTH
}

/// Serde form of [`IMPACT_DEPTH`].
pub fn impact_depth() -> usize {
    IMPACT_DEPTH
}

/// Serde form of [`FILE_DEPTH`].
pub fn file_depth() -> usize {
    FILE_DEPTH
}

/// Serde form of [`TRACE_DEPTH`].
pub fn trace_depth() -> usize {
    TRACE_DEPTH
}

/// Serde form of [`LIMIT`].
pub fn limit() -> usize {
    LIMIT
}

/// Serde form of [`SHOW_LIMIT`].
pub fn show_limit() -> usize {
    SHOW_LIMIT
}

/// Serde form of [`TOP_K`].
pub fn top_k() -> usize {
    TOP_K
}

/// Serde form of [`MAX_MEMBERS`].
pub fn max_members() -> usize {
    MAX_MEMBERS
}

/// Serde form of [`BUDGET`].
pub fn budget() -> usize {
    BUDGET
}

/// Serde form of [`MIN_SIZE`].
pub fn min_size() -> usize {
    MIN_SIZE
}

/// Serde form of [`SURFACE_MAX_DEPTH`].
pub fn surface_max_depth() -> usize {
    SURFACE_MAX_DEPTH
}

/// Serde form of [`IMPACT_MODE`].
pub fn impact_mode() -> String {
    IMPACT_MODE.to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Every `.rs` file under `src/`, except this one.
    ///
    /// The walk is what makes the guards below survive a new file: an MCP
    /// tool's arguments do not all live in `mcp/tools.rs` — `impact`,
    /// `context`, and `search` declare theirs beside their implementation —
    /// so a list of files to check would go stale the next time one moves.
    fn sources() -> Vec<PathBuf> {
        // This module holds the guards' own patterns as string literals, so it
        // is the one exemption. Matching the full path rather than the
        // basename keeps a future `src/search/defaults.rs` under the guards.
        let this = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/defaults.rs");
        fn walk(dir: &Path, this: &Path, out: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).expect("src/ is readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, this, out);
                } else if path.extension().is_some_and(|e| e == "rs") && path != this {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src").as_path(),
            &this,
            &mut out,
        );
        assert!(out.len() > 20, "walk found only {} files", out.len());
        out
    }

    /// The file's lines up to its trailing `#[cfg(test)] mod …`.
    ///
    /// Fixtures below that line are exempt: one that read a constant would
    /// stop noticing it change. The cut is the gated *module*, never a gated
    /// item — `src/search/shared.rs` gates a single `pub fn` among production
    /// code, and cutting there would blind the guards to the rest of the file.
    fn production_lines(src: &str) -> Vec<&str> {
        let lines: Vec<&str> = src.lines().collect();
        let end = (0..lines.len())
            .find(|&i| {
                lines[i].contains("#[cfg(test)]")
                    && lines[i + 1..]
                        .iter()
                        .find(|n| !n.trim().is_empty())
                        .is_some_and(|n| n.trim_start().starts_with("mod "))
            })
            .unwrap_or(lines.len());
        lines.into_iter().take(end).collect()
    }

    /// The `field: value` pair a line assigns, if it assigns one.
    ///
    /// A one-line `Self { max_depth: 16 }` is split the same way a multi-line
    /// body is: the field is the last segment before the colon, so the brace
    /// and the type name in front of it do not become part of the name, and
    /// the trailing brace does not become part of the value.
    fn field_and_value(line: &str) -> Option<(&str, &str)> {
        let (field, value) = line.split_once(": ")?;
        let field = field.rsplit(['{', '(']).next()?.trim();
        // A trailing comment is not part of the value. Leaving it attached made
        // `0 } // note` unparseable, so `is_literal_field` said "not a literal"
        // and the guards skipped the line — any comment at all bought silence,
        // whether or not it claimed anything.
        let value = code_of(value);
        let value = value.trim().trim_end_matches([',', ' ', '}']).trim();
        (!field.is_empty() && !value.is_empty()).then_some((field, value))
    }

    /// The text with Rust comments removed, strings left intact.
    ///
    /// Rust, and only Rust: `sources()` yields `*.rs` and nothing else, so
    /// `//` and `/*` are the whole comment syntax this ever meets. The
    /// hard-coded pair is the language these guards read, not an assumption
    /// about source files in general — the adapters that parse *other*
    /// languages live in `src/adapters/` and share nothing with this.
    ///
    /// Block comments nest in Rust, so the close is matched by depth rather
    /// than by the first `*/`. Taking the first one left the tail of
    /// `/* a /* b */ arg(default_value = 5) */` outside the comment, which is
    /// the forged-attribute hole this function exists to close. An unterminated
    /// comment runs to the end of the accumulated text, as it does in the
    /// compiler.
    fn strip_comments(text: &str) -> String {
        let masked = strip_string_literals(text);
        let mut out = String::with_capacity(text.len());
        let mut rest = 0usize;
        while rest < text.len() {
            let line = masked[rest..].find("//").map(|i| rest + i);
            let block = masked[rest..].find("/*").map(|i| rest + i);
            let Some(cut) = line.into_iter().chain(block).min() else {
                out.push_str(&text[rest..]);
                break;
            };
            out.push_str(&text[rest..cut]);
            if Some(cut) == line {
                break;
            }
            let Some(end) = block_comment_end(&masked, cut) else {
                break;
            };
            rest = end;
        }
        out
    }

    /// Where the block comment opening at `start` closes, counting nesting.
    fn block_comment_end(masked: &str, start: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut scan = start;
        while scan < masked.len() {
            let open = masked[scan..].find("/*").map(|i| scan + i);
            let close = masked[scan..].find("*/").map(|i| scan + i);
            match (open, close) {
                (Some(o), Some(c)) if o < c => {
                    depth += 1;
                    scan = o + 2;
                }
                (_, Some(c)) => {
                    depth -= 1;
                    scan = c + 2;
                    if depth == 0 {
                        return Some(scan);
                    }
                }
                _ => return None,
            }
        }
        None
    }

    /// Whether an attribute body is clap's own `arg(…)` or `clap(…)`.
    ///
    /// The name has to be matched at a boundary, not as a substring:
    /// `#[my_arg(default_value = 5)]` contains `arg(` and prints nothing, so
    /// a `contains` test failed the build on the doc comment above someone
    /// else's macro. `#[cfg_attr(unix, arg(default_value = …))]` is why the
    /// match cannot simply be anchored at the start.
    fn names_clap_attribute(body: &str) -> bool {
        ["arg(", "clap(", "command("].iter().any(|name| {
            body.match_indices(name).any(|(i, _)| {
                let before = body[..i].chars().next_back();
                before.is_none_or(|c| !c.is_alphanumeric() && c != '_')
            })
        })
    }

    /// The text with every `"…"` literal blanked out.
    ///
    /// Brackets and markers inside a string are data, not syntax. Escapes are
    /// honoured so `"a\"b"` does not end early; raw strings are not, because
    /// no attribute in this tree uses one.
    fn strip_string_literals(text: &str) -> String {
        // Byte-for-byte: every position in the result is the same position in
        // the input, so a match found here can be sliced out of the original.
        // `code_of` and `comment_of` need that, and having one function serve
        // both them and the bracket balance is what keeps the two from
        // disagreeing about where a string ends.
        let mut out = String::with_capacity(text.len());
        let mut in_string = false;
        let mut escaped = false;
        for ch in text.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                out.extend(std::iter::repeat_n(' ', ch.len_utf8()));
                if !in_string {
                    // Restore the closing quote itself so the balance below
                    // still sees a well-formed pair.
                    out.pop();
                    out.push('"');
                }
                continue;
            }
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
        out
    }

    /// Where a line's trailing comment starts, ignoring `//` inside a string.
    ///
    /// A path value spelling `"// defaults-ok: x"` used to open a comment as
    /// far as this was concerned, so the value exempted itself — the bypass
    /// the marker exists to replace.
    fn comment_start(line: &str) -> Option<usize> {
        let outside = strip_string_literals(line);
        [outside.find("//"), outside.find("/*")]
            .into_iter()
            .flatten()
            .min()
    }

    /// The part of a line before its trailing comment.
    fn code_of(line: &str) -> &str {
        comment_start(line).map_or(line, |i| &line[..i])
    }

    /// The part of a line inside its trailing comment, if it has one.
    fn comment_of(line: &str) -> &str {
        comment_start(line).map_or("", |i| &line[i..])
    }

    /// Whether a line is a literal default the guards should reject.
    ///
    /// The composition is what needs testing, not the three predicates under
    /// it: `field_and_value` decides what the value is, `is_literal_field`
    /// whether it is a literal, and `is_exempt` whether the author claimed it
    /// deliberately. Checking them one at a time is how a defect survived where
    /// a trailing comment made the value unparseable — every commented line was
    /// exempt, marked or not, while the marker's own test passed.
    fn is_unmarked_literal(line: &str) -> bool {
        is_literal_field(line) && !is_exempt(line)
    }

    /// Whether a value is a number or a path typed in place.
    ///
    /// `_` separators are stripped before parsing, so `budget: 8_000` is as
    /// much a literal as `budget: 8000`. The doc-comment rule learned that
    /// spelling first, and for a while the two halves of this module disagreed
    /// about it: a comment describing `8_000` failed the build while the
    /// literal itself walked past both guards. `src/calls/trace.rs` and
    /// `src/lib.rs` both write numbers that way.
    fn is_literal_field(line: &str) -> bool {
        field_and_value(line).is_some_and(|(_, v)| {
            v.replace('_', "").parse::<usize>().is_ok() || v.starts_with("PathBuf::from(\"")
        })
    }

    /// Whether a line opts out of the guards, and says why.
    ///
    /// Two shapes are correct code the guards cannot recognise on their own: a
    /// counter whose default is `0`, and a literal that deliberately departs
    /// from the defaults (`DepOptions { max_depth: 1, ..Default::default() }`).
    /// Both were first handled by inferring intent from the source text —
    /// exempting every zero, and tracking brace frames to find `..`. Both
    /// inferences were wrong in ways that mattered: the zero rule let a real
    /// zero default through, and the brace scan counted braces inside comments
    /// and strings, so it blindly exempted 24 lines across five files while
    /// missing the two `..Type::default()` spellings in the tree.
    ///
    /// Intent that cannot be read off the syntax is asked for instead. The
    /// reason after the colon is required: it is what a reviewer reads, and
    /// demanding it keeps the marker from becoming a reflex.
    ///
    /// ```text
    /// Self { hits: 0 } // defaults-ok: a counter's starting point, not a setting
    /// ```
    fn is_exempt(line: &str) -> bool {
        // Only a comment can carry the claim. Scanning the whole line let a
        // string value containing the marker exempt itself, which is the
        // silent bypass the marker exists to replace.
        comment_of(line)
            .split_once("defaults-ok:")
            .is_some_and(|(_, reason)| !reason.trim().is_empty())
    }

    /// Field names that read a constant from this module anywhere in the tree.
    ///
    /// Derived rather than listed: a name earns its place by being set from
    /// `crate::defaults` somewhere, which is what a hand-written allowlist got
    /// wrong — it omitted `max_members_per_type`, the very field the class was
    /// found through.
    fn shared_field_names() -> Vec<String> {
        let mut shared: Vec<String> = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for line in production_lines(&src) {
                if let Some((field, value)) = field_and_value(line) {
                    if value.starts_with("crate::defaults::")
                        || value.starts_with("PathBuf::from(crate::defaults::")
                    {
                        shared.push(field.to_string());
                    }
                }
            }
        }
        shared.sort();
        shared.dedup();
        shared
    }

    /// Reports each offending line as `path:line: text`, relative to `src/`.
    fn offenders(hits: Vec<(PathBuf, usize, String)>) -> Vec<String> {
        hits.into_iter()
            .map(|(path, line, text)| {
                let short = path
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                format!("{short}:{}: {}", line + 1, text.trim())
            })
            .collect()
    }

    /// The line splitter reads a one-line body the same as a multi-line one.
    ///
    /// The guards are textual, so this is where their parser is checked
    /// rather than through a mutation the compiler would reject: a
    /// single-field `Self { .. }` literal has to yield the bare field name
    /// and a value with no trailing brace, or a literal hides in plain sight.
    #[test]
    fn field_splitter_reads_one_line_bodies() {
        assert_eq!(field_and_value("            max_depth: 16,"), Some(("max_depth", "16")));
        assert_eq!(field_and_value("        Self { max_depth: 16 }"), Some(("max_depth", "16")));
        assert_eq!(
            field_and_value("    root: PathBuf::from(\".\"),"),
            Some(("root", "PathBuf::from(\".\")"))
        );
        assert_eq!(
            field_and_value("    limit: crate::defaults::LIMIT,"),
            Some(("limit", "crate::defaults::LIMIT"))
        );
        assert_eq!(field_and_value("    let x = 3;"), None);

        assert!(is_literal_field("        Self { max_depth: 16 }"));
        assert!(is_literal_field("    root: PathBuf::from(\".\"),"));
        assert!(!is_literal_field("    limit: crate::defaults::LIMIT,"));
        assert!(!is_literal_field("    output: OutputMode::Text,"));
        // A zero is a literal like any other. Whether it is a *default* is a
        // question about intent, which the marker answers and the text cannot.
        assert!(is_literal_field("        Self { hits: 0 }"));
    }

    /// The guards' verdict on a line, over every shape a line can take.
    ///
    /// This is the test the round-2 review said was missing, and it is missing
    /// in a specific way worth naming: `is_exempt` had a passing test of its
    /// own while being unreachable, because a trailing comment left the value
    /// unparseable and `is_literal_field` answered "not a literal" first. Each
    /// link was green and the chain was broken, so the assertions here are on
    /// the composed verdict rather than on any one predicate.
    #[test]
    fn the_guards_verdict_over_every_shape_of_line() {
        // (line, rejected?)
        let cases = [
            ("        Self { max_depth: 3 }", true),
            ("    limit: 200,", true),
            ("    root: PathBuf::from(\".\"),", true),
            // A comment does not excuse anything on its own — this is the
            // defect: every one of these used to pass.
            ("        Self { max_depth: 3 } // TODO: revisit", true),
            ("        Self { max_depth: 3 } // defaults-ok:", true),
            ("        Self { max_depth: 3 } // defaults-ok:    ", true),
            // A claim with a reason does.
            ("        Self { max_depth: 3 } // defaults-ok: fixture, not shipped", false),
            ("    limit: 200, // defaults-ok: the protocol fixes this one", false),
            // The marker only counts inside a comment, or a value could
            // exempt itself — including a value that spells a comment.
            ("    root: PathBuf::from(\"defaults-ok: not a claim\"),", true),
            ("    root: PathBuf::from(\"// defaults-ok: not a claim\"),", true),
            ("    root: PathBuf::from(\"/* defaults-ok: x */\"),", true),
            // `_` separators are how this tree writes large numbers, so they
            // are literals here as much as in a doc comment.
            ("    budget: 8_000,", true),
            ("    budget: 8_000, // defaults-ok: the format fixes this", false),
            // Values that were never literals stay uninteresting.
            ("    limit: crate::defaults::LIMIT,", false),
            ("    output: OutputMode::Text,", false),
        ];
        let mut rejected = 0;
        for (line, expected) in cases {
            let verdict = is_unmarked_literal(line);
            assert_eq!(verdict, expected, "wrong verdict for: {line}");
            // Count what the code answered, not what the table declared.
            // Summing `expected` made this 7 by construction — a number that
            // holds however the predicate behaves, which is the shape of an
            // assertion that cannot fail.
            rejected += usize::from(verdict);
        }
        assert!(
            rejected >= 7,
            "only {rejected} shapes reached the rejecting branch"
        );
    }

    /// A commented-out clap attribute is not a clap attribute.
    ///
    /// The compiler drops the comment before it means anything, so the guard
    /// has to as well, or someone else's `#[serde(…)]` can be made to look
    /// like clap's and fail the build on the doc comment above it.
    #[test]
    fn commented_out_text_cannot_forge_a_clap_attribute() {
        let forged = vec![
            "        /// Cap results (default 5).",
            "        #[serde(/* arg(default_value = 5) */ rename = \"limit\")]",
            "        limit: usize,",
        ];
        assert!(
            docs_restating_a_default(&forged).is_empty(),
            "commented-out text must not count as clap syntax"
        );

        // The real thing beside a comment still counts.
        let real = vec![
            "        /// Cap results (default 200).",
            "        #[arg(long, /* historical */ default_value_t = crate::defaults::LIMIT)]",
            "        limit: usize,",
        ];
        assert_eq!(docs_restating_a_default(&real).len(), 1);

        // Rust nests block comments, so the close has to be matched by depth.
        // Taking the first `*/` left `arg(default_value = 5)` outside the
        // comment and forged the match all over again.
        let nested = vec![
            "        /// Cap results (default 5).",
            "        #[serde(/* a /* b */ arg(default_value = 5) */ rename = \"limit\")]",
            "        limit: usize,",
        ];
        assert!(
            docs_restating_a_default(&nested).is_empty(),
            "a nested block comment is still a comment"
        );
    }

    /// The attribute scan says so when it loses the thread.
    ///
    /// The bound is the one mechanism here that fires only on input nothing in
    /// the tree produces, which is exactly the kind of alarm that rots: move
    /// the counter reset above the increment and every other test still
    /// passes while the guard silently returns to checking nothing. A char
    /// literal holding a quote is the shape that reaches it today.
    #[test]
    #[should_panic(expected = "attribute scan never closed")]
    fn an_attribute_that_never_closes_is_an_error_not_a_silence() {
        let mut lines = vec!["        #[arg(value_delimiter = '\"', long)]"];
        lines.extend(std::iter::repeat_n("        more: usize,", 30));
        let _ = docs_restating_a_default(&lines);
    }

    /// The opt-out marker is exact, and it costs a reason.
    ///
    /// The composed test above covers the verdict; this one covers the marker
    /// itself, so a change to its spelling fails here with a name that says
    /// what broke rather than inside a table of line shapes.
    #[test]
    fn the_exemption_marker_demands_a_reason() {
        assert!(is_exempt("    Self { hits: 0 } // defaults-ok: a counter's start"));
        assert!(is_exempt(
            "    DepOptions { max_depth: 1, ..Default::default() } // defaults-ok: caller wants 1"
        ));

        // No reason, no exemption.
        assert!(!is_exempt("    Self { hits: 0 } // defaults-ok:"));
        assert!(!is_exempt("    Self { hits: 0 } // defaults-ok:   "));
        // And nothing is exempt by accident.
        assert!(!is_exempt("    Self { hits: 0 }"));
        assert!(!is_exempt("    // the defaults are ok as they stand"));
    }

    /// The doc-block reader survives a multi-line `#[arg(…)]`.
    ///
    /// `default_value` is often not the first argument, and the continuation
    /// lines of an attribute look like ordinary code. Reading forwards and
    /// balancing the brackets is what keeps the doc block attached to the
    /// declaration it belongs to.
    #[test]
    fn default_value_is_found_behind_a_multi_line_attribute() {
        assert!(states_a_default_value("/// Repository root (default: \".\").").is_some());
        assert!(states_a_default_value("/// Cap results (default 200).").is_some());
        // Mentioning the word without naming a value is what a reader needs.
        assert!(states_a_default_value("/// Shown by default (tagged `[external]`).").is_none());
        assert!(states_a_default_value("/// The default; pass it to say so.").is_none());
        assert!(states_a_default_value("/// The `default` mode is the widest.").is_none());
        // A number the sentence carries on past is not a stated value, or
        // ordinary prose about a defaulted argument would fail the build.
        assert!(states_a_default_value("/// The default 64-bit mode is portable.").is_none());
        assert!(states_a_default_value("/// Uses the default 2x scale factor.").is_none());
        assert!(states_a_default_value("/// Falls back to the default 2 levels up.").is_none());
        // …but a number the sentence ends on is one, in every spelling a
        // default is actually written in.
        assert!(states_a_default_value("/// Cap results (default 200).").is_some());
        assert!(states_a_default_value("/// Depth, default 3, in hops.").is_some());
        assert!(states_a_default_value("/// Budget (default 8_000).").is_some());
        assert!(states_a_default_value("/// Timeout (default 200ms).").is_some());
        assert!(states_a_default_value("/// Chunk (default 4KB).").is_some());

        // A non-clap attribute that happens to spell `default_value` prints
        // nothing, so the doc comment above it duplicates nothing.
        let other_macro = vec![
            "        /// Uses default 5.",
            "        #[my_macro(default_value = 5)]",
            "        field: usize,",
        ];
        assert!(
            docs_restating_a_default(&other_macro).is_empty(),
            "only clap's own attribute makes a doc comment a duplicate"
        );

        // …and the name is matched at a boundary, so someone else's macro
        // whose name merely ends in `arg` is not clap either.
        let arg_suffixed = vec![
            "        /// Uses default 5.",
            "        #[my_arg(default_value = 5)]",
            "        field: usize,",
        ];
        assert!(
            docs_restating_a_default(&arg_suffixed).is_empty(),
            "`my_arg(` is not `arg(`"
        );

        // A conditional clap attribute still is clap.
        let conditional = vec![
            "        /// Repo root (default: \".\").",
            "        #[cfg_attr(unix, arg(default_value = crate::defaults::ROOT))]",
            "        root: PathBuf,",
        ];
        assert_eq!(
            docs_restating_a_default(&conditional).len(),
            1,
            "cfg_attr-wrapped clap attributes still print a default"
        );

        // A bracket inside a string must not latch the attribute scan open,
        // which would silently stop checking the rest of the file.
        let bracket_in_string = vec![
            "        /// First (default: \".\").",
            "        #[arg(long, help = \"index [0..n\", default_value = crate::defaults::ROOT)]",
            "        first: PathBuf,",
            "        /// Second (default: \".\").",
            "        #[arg(long, default_value = crate::defaults::ROOT)]",
            "        second: PathBuf,",
        ];
        assert_eq!(
            docs_restating_a_default(&bracket_in_string).len(),
            2,
            "a `[` inside a string must not swallow the rest of the file"
        );

        // A doc block separated from its attributes by an ordinary comment is
        // still that declaration's doc block.
        let commented = vec![
            "        /// Repo root (default: \".\").",
            "        // clap needs the value_name here, see #12",
            "        #[arg(long, default_value = crate::defaults::ROOT)]",
            "        root: PathBuf,",
        ];
        assert_eq!(
            docs_restating_a_default(&commented).len(),
            1,
            "a comment between the docs and the attribute must not clear the block"
        );

        // `default_value` last, behind a multi-line attribute: the shape the
        // backwards walk missed, because `long,` reads as ordinary code.
        let spread = vec![
            "        /// Repo root to scan (default: \".\").",
            "        #[arg(",
            "            long,",
            "            value_name = \"PATH\",",
            "            default_value = crate::defaults::ROOT",
            "        )]",
            "        root: PathBuf,",
        ];
        assert_eq!(
            docs_restating_a_default(&spread).len(),
            1,
            "a doc block behind a multi-line attribute must still be read"
        );

        // The same doc text on a field clap does not default is not a
        // duplicate of anything, so it passes.
        let no_default = vec![
            "        /// Repo root to scan (default: \".\").",
            "        #[arg(long)]",
            "        root: PathBuf,",
        ];
        assert!(docs_restating_a_default(&no_default).is_empty());

        // A doc block belongs to the declaration that follows it, not to the
        // next one along.
        let two_fields = vec![
            "        /// Repo root to scan (default: \".\").",
            "        #[arg(long)]",
            "        root: PathBuf,",
            "        /// Cap results.",
            "        #[arg(long, default_value_t = crate::defaults::LIMIT)]",
            "        limit: usize,",
        ];
        assert!(
            docs_restating_a_default(&two_fields).is_empty(),
            "the first field's docs must not be judged against the second field's attribute"
        );
    }

    /// A clap default names a constant from this module, never a literal.
    #[test]
    fn clap_defaults_come_from_this_module() {
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for (i, line) in src.lines().enumerate() {
                let Some(rest) = line
                    .split_once("default_value_t = ")
                    .or_else(|| line.split_once("default_value = "))
                    .map(|(_, rest)| rest)
                else {
                    continue;
                };
                if !rest.starts_with("crate::defaults::") {
                    hits.push((path.clone(), i, line.to_string()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "clap defaults that do not read `crate::defaults`; add the value \
             there and name it for what it bounds:\n{:#?}",
            offenders(hits)
        );
    }

    /// A serde default names a helper from this module, never a local one.
    ///
    /// `#[serde(default)]` with no path is untouched — that is the type's own
    /// `Default`, not a value the CLI also declares.
    #[test]
    fn serde_defaults_come_from_this_module() {
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for (i, line) in src.lines().enumerate() {
                let Some((_, rest)) = line.split_once("serde(default = \"") else {
                    continue;
                };
                if !rest.starts_with("crate::defaults::") {
                    hits.push((path.clone(), i, line.to_string()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "serde defaults that do not read `crate::defaults`; a value the CLI \
             also declares belongs there, so the two cannot drift:\n{:#?}",
            offenders(hits)
        );
    }

    /// A field that reads a constant anywhere reads it everywhere.
    ///
    /// This is the class `default_value` and `#[serde(default)]` cannot see: a
    /// value assembled in an `impl Default` body or a struct literal reaches
    /// the same surfaces without passing through either attribute. `DepOptions`,
    /// `SurfaceOptions`, the `--preset digest` member cap, and both of
    /// `MapOptions`' render caps drifted that way.
    ///
    /// The set of guarded field names is derived, not listed: a name earns its
    /// place by being set from `crate::defaults` somewhere in the tree. That is
    /// what a hand-written allowlist got wrong — it omitted
    /// `max_members_per_type`, the very field the class was found through.
    #[test]
    fn struct_defaults_come_from_this_module() {
        // Pass one: every field name that already reads a constant.
        let shared = shared_field_names();
        assert!(
            shared.len() > 5,
            "expected several shared fields, found {shared:?}"
        );

        // Pass two: the same names, set from a literal instead — unless the
        // line says why it departs from the default on purpose.
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for (i, line) in production_lines(&src).into_iter().enumerate() {
                let Some((field, _)) = field_and_value(line) else {
                    continue;
                };
                if is_exempt(line) {
                    continue;
                }
                if shared.iter().any(|f| f == field) && is_unmarked_literal(line) {
                    hits.push((path.clone(), i, line.to_string()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "fields set from a literal whose name reads `crate::defaults` \
             elsewhere; an `impl Default` or a struct literal reaches the CLI \
             and MCP as surely as an attribute does. If the value is a \
             deliberate departure rather than a default, say so on the line: \
             `// defaults-ok: <why>`:\n{:#?}",
            offenders(hits)
        );
    }

    /// An `impl Default` body holds no bare values.
    ///
    /// The derived rule above cannot see the *first* site of a name: revert
    /// `max_members_per_type` in `core.rs` and the name stops reading a
    /// constant anywhere, so it drops out of the set that would have caught
    /// it. This pass does not ask what a field is called — inside an
    /// `impl Default`, every value is by definition a default, and its place
    /// is this module.
    #[test]
    fn impl_default_bodies_hold_no_literals() {
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            let mut depth = 0usize;
            let mut inside = false;
            let mut opened = false;
            for (i, line) in production_lines(&src).into_iter().enumerate() {
                let trimmed = line.trim_start();
                // `impl<T> Default for …` and `impl Default for …` alike; the
                // body may open on this line or the next, so the block ends
                // only once a brace has actually been seen and closed.
                if !inside && trimmed.starts_with("impl") && trimmed.contains(" Default for ") {
                    inside = true;
                    depth = 0;
                    opened = false;
                }
                if !inside {
                    continue;
                }
                depth += line.matches('{').count();
                if depth > 0 {
                    opened = true;
                }
                depth = depth.saturating_sub(line.matches('}').count());
                if is_unmarked_literal(trimmed) {
                    hits.push((path.clone(), i, line.to_string()));
                }
                if opened && depth == 0 {
                    inside = false;
                }
            }
        }
        assert!(
            hits.is_empty(),
            "literal values inside an `impl Default`; a default belongs in \
             `crate::defaults`, where the CLI, the serde args, and the MCP \
             schema all reach it. If the value is not a tunable default — a \
             counter's starting point, a fixture — say so on the line: \
             `// defaults-ok: <why>`:\n{:#?}",
            offenders(hits)
        );
    }

    /// An MCP description states its default by interpolating the constant.
    ///
    /// A hard-coded `(default 200)` in the schema is the copy an agent cannot
    /// check: it reads the number, applies it, and has no way to notice the
    /// tool now uses another one.
    #[test]
    fn schema_descriptions_interpolate_their_default() {
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for (i, line) in src.lines().enumerate() {
                if !line.contains("\"description\"") {
                    continue;
                }
                // One rule over every phrasing: wherever the word `default`
                // introduces a value, that value must be the `{}` a `format!`
                // fills. `(default 200)`, `(default: 200)`, `default=200` and
                // `defaults to 200` all state a number; `(default: false)` and
                // a bare `(default)` state none, and `crate::defaults::LIMIT`
                // is the interpolation itself.
                let lowered = line.to_ascii_lowercase();
                for rest in lowered.split("default").skip(1) {
                    let value = rest
                        .trim_start_matches([':', '=', ' '])
                        .trim_start_matches("s to ")
                        .trim_start_matches([':', '=', ' ']);
                    // A quoted value must be the placeholder itself: `\"{}\"`
                    // passes, `\".\"` does not. An unquoted one fails on a
                    // digit, which lets `(default: false)` through.
                    let states_a_value = match value.strip_prefix("\\\"") {
                        Some(quoted) => !quoted.starts_with("{}"),
                        None => value.starts_with(|c: char| c.is_ascii_digit()),
                    };
                    if states_a_value {
                        hits.push((path.clone(), i, line.to_string()));
                        break;
                    }
                }
            }
        }
        assert!(
            hits.is_empty(),
            "tool descriptions stating a default as literal text; use \
             `format!(\"… (default {{}}).\", crate::defaults::CONST)`:\n{:#?}",
            offenders(hits)
        );
    }

    /// A clap doc comment leaves the default to clap.
    ///
    /// The sibling guard above covers the MCP schema, where a description is a
    /// runtime `String` and the repair is to interpolate the constant. A clap
    /// doc comment cannot interpolate anything — it is an attribute, fixed at
    /// compile time — so the rule here is the other one: say nothing, because
    /// clap already appends `[default: .]` from the `default_value` beside it.
    /// Stating it too renders the value twice and gives it a second place to
    /// drift from:
    ///
    /// ```text
    /// [PATH]  Repository root to scan (default: ".") [default: .]
    /// ```
    ///
    /// Phrasings that mention the word without naming a value — "shown by
    /// default", "the default; pass it to say so" — are what the reader needs
    /// and are left alone.
    #[test]
    fn clap_doc_comments_leave_the_default_to_clap() {
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for (i, text) in docs_restating_a_default(&production_lines(&src)) {
                hits.push((path.clone(), i, text));
            }
        }
        assert!(
            hits.is_empty(),
            "clap doc comments naming a default clap already prints; drop the \
             value and let `[default: …]` say it once:\n{:#?}",
            offenders(hits)
        );
    }

    /// The doc comments that restate a default their `default_value` prints.
    ///
    /// Reads forwards, accumulating the doc block and the attributes after it,
    /// and judges both when the declaration arrives. Walking backwards from
    /// the `default_value` line was the obvious shape and the wrong one:
    /// `#[arg(` may span lines, and its continuation lines (`long,`,
    /// `value_enum,`) look like ordinary code, so the walk stopped short of
    /// the doc block whenever `default_value` was not the first argument.
    fn docs_restating_a_default(lines: &[&str]) -> Vec<(usize, String)> {
        let mut hits = Vec::new();
        let mut docs: Vec<(usize, String)> = Vec::new();
        let mut attrs = String::new();
        let mut in_attr = false;
        let mut attr_lines = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t.starts_with("///") {
                docs.push((i, t.to_string()));
                continue;
            }
            if t.starts_with("#[") || in_attr {
                // Comments inside an attribute are not attribute syntax. The
                // compiler drops `#[serde(/* arg(default_value = 5) */ …)]`
                // before it means anything, so reading it raw let commented-out
                // text forge a clap match.
                attrs.push_str(&strip_comments(t));
                // The attribute ends where its brackets balance. Brackets
                // inside a string are not syntax: `help = "index [0..n"` once
                // latched this open for the rest of the file, and a guard that
                // silently checks nothing reports success.
                let outside_strings = strip_string_literals(&attrs);
                in_attr =
                    outside_strings.matches('[').count() > outside_strings.matches(']').count();
                attr_lines = if in_attr { attr_lines + 1 } else { 0 };
                // A bound rather than another special case. The balance is
                // computed textually, so some spelling this does not model —
                // a char literal holding a quote, a raw string — can latch it
                // open, and the failure mode is the worst one available: the
                // guard checks nothing and reports success. No attribute in
                // this tree spans close to this many lines, so crossing it
                // means the scan is lost, and saying so is better than going
                // quiet.
                assert!(
                    attr_lines < 20,
                    "attribute scan never closed near line {}; the bracket \
                     balance is lost, so this guard is checking nothing:\n{}",
                    i + 1,
                    attrs
                );
                continue;
            }
            // A blank line or an ordinary comment sits between a doc block and
            // its attributes often enough — a `// SAFETY:` note, a commented-out
            // attribute — and neither ends the declaration. Treating them as
            // one would clear the doc block and check nothing.
            if t.is_empty() || t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') {
                continue;
            }
            // A declaration line: judge it against what preceded it.
            // Only clap prints `[default: …]`, so only clap's attribute makes a
            // doc comment a duplicate. `#[my_macro(default_value = 5)]` names
            // the same words and prints nothing, and failing the build on the
            // doc comment above it would be a rule about the wrong thing.
            let clap_default = strip_string_literals(&attrs).split("#[").any(|a| {
                a.contains("default_value") && names_clap_attribute(a)
            });
            if clap_default {
                for (n, doc) in &docs {
                    if let Some(text) = states_a_default_value(doc) {
                        hits.push((*n, text));
                    }
                }
            }
            docs.clear();
            attrs.clear();
        }
        hits
    }

    /// The doc-comment text, when it names a concrete default value.
    ///
    /// Same shape as the schema rule, minus the interpolation escape: a digit
    /// or a quoted value after the word `default` is the copy, while a bare
    /// `(default)` or `by default (tagged …)` names nothing and passes.
    fn states_a_default_value(line: &str) -> Option<String> {
        let lowered = line.to_ascii_lowercase();
        for rest in lowered.split("default").skip(1) {
            let value = rest
                .trim_start_matches([':', '=', ' '])
                .trim_start_matches("s to ")
                .trim_start_matches([':', '=', ' ']);
            // A quoted value is always the copy. A number is the copy only
            // when it stands on its own: `(default 200)` states one, while
            // "the default 64-bit mode" is a sentence about a mode, and
            // failing the build on it would cost a real sentence to catch a
            // hypothetical one. A backtick is left alone for the same reason
            // — prose says "the `default` mode".
            if value.starts_with('"') {
                return Some(line.to_string());
            }
            // The token is the number with the spellings a default actually
            // uses — `8_000`, `200ms`, `4KB` — and it is a stated value only
            // when the sentence ends there. "the default 2 levels up" carries
            // on into a noun, and failing the build on it would cost ordinary
            // prose; `(default 200)` and `default 3,` do not.
            let token: String = value
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '_' || c.is_ascii_alphabetic())
                .collect();
            if token.starts_with(|c: char| c.is_ascii_digit()) {
                let after = value[token.len()..].chars().next();
                let ends_there = after.is_none_or(|c| matches!(c, ')' | '.' | ',' | ';'));
                if ends_there {
                    return Some(line.to_string());
                }
            }
        }
        None
    }
}
