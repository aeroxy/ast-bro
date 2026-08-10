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
