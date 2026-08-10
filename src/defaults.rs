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

    /// A struct literal sets a known default field from this module, not from
    /// a number typed in place.
    ///
    /// This is the class `default_value` and `#[serde(default)]` cannot see: a
    /// value assembled in an `impl Default` body or a builder reaches the same
    /// surfaces without passing through either attribute. `DepOptions`,
    /// `SurfaceOptions`, and the `--preset digest` member cap all drifted that
    /// way. Lines after a `#[cfg(test)]` are exempt — a fixture that read the
    /// constant would stop noticing it change.
    #[test]
    fn struct_defaults_come_from_this_module() {
        const FIELDS: [&str; 8] = [
            "max_members", "max_depth", "min_lines", "min_size",
            "min_cycle_size", "top_k", "budget", "limit",
        ];
        let mut hits = Vec::new();
        for path in sources() {
            let src = fs::read_to_string(&path).expect("source is readable");
            let production = src
                .lines()
                .position(|l| l.contains("#[cfg(test)]"))
                .unwrap_or(usize::MAX);
            for (i, line) in src.lines().enumerate().take(production) {
                let trimmed = line.trim_start();
                let Some((field, value)) = trimmed.split_once(": ") else {
                    continue;
                };
                if !FIELDS.contains(&field) {
                    continue;
                }
                let value = value.trim_end_matches(',');
                let literal = value.parse::<usize>().is_ok()
                    || value.starts_with("PathBuf::from(\"");
                if literal {
                    hits.push((path.clone(), i, line.to_string()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "struct fields set from a literal instead of `crate::defaults`; \
             an `impl Default` reaches the CLI and MCP as surely as an \
             attribute does:\n{:#?}",
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
                if !line.contains("\"description\"") || line.contains("format!") {
                    continue;
                }
                // `(default 200)` and `(default \".\")` state a value;
                // `(default: false)` and a bare `(default)` state neither.
                let parenthesized = line.split("(default ").skip(1).any(|rest| {
                    rest.starts_with(|c: char| c.is_ascii_digit()) || rest.starts_with('\\')
                });
                // `max_members=50`, the phrasing the `digest` tool used to
                // describe itself with, which the shape above walks past.
                let assigned = line.as_bytes().windows(3).any(|w| {
                    (w[0].is_ascii_alphabetic() || w[0] == b'_')
                        && w[1] == b'='
                        && w[2].is_ascii_digit()
                });
                let states_a_value = parenthesized || assigned;
                if states_a_value {
                    hits.push((path.clone(), i, line.to_string()));
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
