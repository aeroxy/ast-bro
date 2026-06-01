//! `ast-bro callers|callees` — symbol-level call-graph analysis.
//!
//! Three-pass pipeline anchored to
//! existing `Declaration` IR + the file-level `DepGraph` for disambiguation:
//!
//! 1. **Extract** (`build`): per-file walk reuses each adapter's
//!    `Declaration::calls` (populated during `parse_file`) and produces
//!    `CallEdge`s with bare-name targets plus a global `symbol_table` of
//!    every callable's qualified name.
//! 2. **Resolve** (`resolve.rs`):
//!    - Pass A — same-file: `defined_names` + `ImportBinding` → qn via the
//!      existing `src/deps/resolver` suffix-index.
//!    - Pass B — global: bare names with exactly one global match promote to
//!      `Resolved`; ambiguous defer to pass C.
//!    - Pass C — disambiguation: filter ambiguous candidates by the source
//!      file's forward-dep closure (loaded from the same unified graph).
//! 3. **Traverse** (`traverse.rs`): forward / reverse BFS over `CallGraph`.
//!
//! Storage rides inside the unified `.ast-bro/deps/graph.bin` cache via
//! `crate::graph_cache`. The call graph is built lazily — the very first
//! `callers`/`callees` query triggers `graph_cache::promote_calls`, which
//! persists the promoted graph for subsequent sessions.

pub mod build;
pub mod cli;
pub mod cli_helpers;
pub mod graph;
pub mod mcp;
pub mod pass;
pub mod render;
pub mod resolve;
pub mod trace;
pub mod traverse;

