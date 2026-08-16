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

/// Dropped `implements` edges listed one per line before the note counts
/// the rest.
///
/// The note qualifies an answer rather than being one, so it stays short: a
/// repository where one colliding name is written in two hundred files
/// would otherwise bury the result under its own caveat. The count is
/// always the true total, as everywhere else a cap applies (issue #32).
pub const AMBIGUOUS_SHOWN: usize = 10;

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
        let value = value.trim().trim_end_matches([',', ' ', '}']).trim();
        (!field.is_empty() && !value.is_empty()).then_some((field, value))
    }

    /// Whether a value is a number or a path typed in place.
    fn is_literal_field(line: &str) -> bool {
        field_and_value(line)
            .is_some_and(|(_, v)| v.parse::<usize>().is_ok() || v.starts_with("PathBuf::from(\""))
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
        assert_eq!(
            field_and_value("            max_depth: 16,"),
            Some(("max_depth", "16"))
        );
        assert_eq!(
            field_and_value("        Self { max_depth: 16 }"),
            Some(("max_depth", "16"))
        );
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
        assert!(
            shared.len() > 5,
            "expected several shared fields, found {shared:?}"
        );

        // Pass two: the same names, set from a literal instead.
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            for (i, line) in production_lines(&src).into_iter().enumerate() {
                let Some((field, _)) = field_and_value(line) else {
                    continue;
                };
                if shared.iter().any(|f| f == field) && is_literal_field(line) {
                    hits.push((path.clone(), i, line.to_string()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "fields set from a literal whose name reads `crate::defaults` \
             elsewhere; an `impl Default` or a struct literal reaches the CLI \
             and MCP as surely as an attribute does:\n{:#?}",
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
                if is_literal_field(trimmed) {
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
             schema all reach it:\n{:#?}",
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
}
