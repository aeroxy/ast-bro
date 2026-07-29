# Architecture

`ast-bro` is a fast, structurally-aware code-navigation toolkit. It started as a "shape extractor" (signatures with line ranges, no method bodies) and has grown into eight orthogonal subsystems sharing one binary, one filter pipeline, and one walk infrastructure:

1. **`src/adapters/` + `src/core.rs`** — language adapters parse files into a shared `Declaration` IR; renderers turn that into `map` / `digest` / `show` / `implements` output.
2. **`src/surface/`** — resolves the *true public API* of a package (`pub use`, `__all__`, TypeScript barrels, Scala `export`) instead of just listing every public item per file.
3. **`src/deps/`** — file-level dependency graph (`deps`, `reverse-deps`, `cycles`, `graph`) for nine languages. See [deps.md](deps.md).
4. **`src/calls/`** — symbol-level call graph (`callers`, `callees`) for all 14 languages, with a three-pass resolver (same-file → global symbol table → dep-graph disambiguation). See [calls.md](calls.md).
5. **`src/impact.rs`** — cross-file impact analysis (`impact`): callers + callees + file reverse-deps + test detection bundled into one "blast radius" report, with `--mode {deps,dependents,tests,all}` and `--tests` / `--exclude-tests` filters. See [impact.md](impact.md).
6. **`src/context.rs`** — token-budgeted context pack (`context`): greedy knapsack that assembles "everything the agent needs to understand symbol X" into a caller-supplied token budget. Works for both callable and type targets (type targets include implementors, methods, and method dependents). See [context.md](context.md).
7. **`src/search/`** — hybrid BM25 + dense semantic search, plus `find-related`. Cached at `.ast-bro/index/`. See [search.md](search.md).
8. **`src/squeeze/`** — reversible token compression for **logs/text** (`squeeze`): a multi-stage pipeline that shrinks repetitive lines and emits a legend so the output round-trips back to the original. Not a code tool — for code, the "compression" is `map` / `digest` / `show`. See [squeeze.md](squeeze.md).

The dep graph and call graph share one on-disk cache at `.ast-bro/deps/graph.bin` (`UnifiedGraph { deps, calls: Option<CallGraph> }`) and one process-wide `Arc<UnifiedGraph>` registry in `src/graph_cache/` so MCP `tools/call`s reuse a single parsed copy across the whole session. `impact` and `context` both ride on that same unified cache.

It is written natively in Rust, relying heavily on the [tree-sitter](https://tree-sitter.github.io/tree-sitter/) parsing framework via the excellent [`ast-grep`](https://ast-grep.github.io/) ecosystem bindings, achieving incredibly fast speeds while still taking advantage of `rayon` for massive multithreading across directories. The walking subsystems all share `src/file_filter.rs` for what gets walked (see [file-filtering.md](file-filtering.md)) — adding a feature in one subsystem doesn't change what files the others see. (`squeeze` is the exception: it reads one explicit file path directly, so it neither walks nor touches the filter pipeline.) `file_filter.rs` also hosts `is_test_file` (test-file heuristics used by `impact`, `callers`, `reverse-deps`) and `detect_language` (shebang-based language detection for explicitly-passed extensionless files).

## Core Flow (shape commands)

1. **Routing (`src/main.rs`)**: `ast-bro` iterates through files using the `ignore` crate (which handles `.gitignore` automatically in parallel). Each file extension is identified by `ast-grep`'s `SupportLang::from_path(path)`.
2. **Parsing (`src/adapters/*`)**: The raw source string is handed to `ast-grep` which returns a tree of `ast_grep_core::Node`. A language-specific adapter (e.g. `rust.rs`, `python.rs`) performs a highly tailored AST traversal over these nodes.
3. **IR Generation (`src/core.rs`)**: The traversal emits a canonical `Declaration` tree. This is the Intermediate Representation (IR) shared across every language. It encapsulates `kind`, `name`, `signature`, `docs`, `visibility`, etc.
4. **Rendering (`src/core.rs`)**:
   - `map` iterates the declarations to print a hierarchical file breakdown.
   - `digest` squashes the tree into a concise module-level API map.
   - `show` walks the tree for a specific suffix match and extracts the raw string boundaries.
   - `implements` performs a generic Breadth-First-Search across the IR trees of the entire repository to find inheritance hierarchies.
   - `--json` is the fifth rendering mode: any of the above commands accepts `--json` to serialise the same `Declaration` IR directly via `serde_json` into a versioned JSON schema, instead of formatting it as text. Add `--compact` for single-line output.

The `surface`, `deps`, `calls`, and `search` subsystems each have their own walk + render pipeline but use the same `Declaration` IR (and the same `file_filter`) under the hood. The call graph in particular extends `Declaration` with a `calls: Vec<CallSite>` field and `ParseResult` with `imports: Vec<ImportBinding>` so adapters can populate raw call-sites and import bindings during their existing tree walk; the resolver lives in `src/calls/resolve.rs`. See the dedicated wiki pages for their internals.

## CLI structure and the error contract

Every operation is an explicit subcommand — there's no implicit-default form. Bare `ast-bro` (no arguments at all) prints help to stdout and exits 0 — an orientation request, not a failed query.

Everything else follows one contract (issues #33/#36), implemented in `src/cli_error.rs` and applied by every subcommand:

- **Channel** — stdout carries results only. Every rejection, note, hint, and warning goes to stderr, which keeps `--json` output parseable without preprocessing and makes "stdout is empty" a reliable signal on its own.
- **Exit code** — `0`: the query ran; the answer may be legitimately empty ("this symbol genuinely has no callers"), and any qualification — a path that didn't resolve, a display cap, a depth cutoff — is reported as a `# note:` on stderr, so read the notes before treating exit-0 output as exhaustive. `2`: the query could not run as asked — no such path, no such symbol, unknown flag, missing argument, no paths after shell expansion — with nothing on stdout. `1`: internal failure (parse crash, unreadable cache).
- **Machine-readable form** — with `--json`, a rejection also emits an `ast-bro.error.v1` object on stderr (`{schema, command, kind, detail, hint}`, `kind` ∈ `no_input | path_not_found | symbol_not_found | unknown_flag | bad_argument | index_error`), so a consumer needs exactly one check instead of a per-subcommand table.
- **Unknown flags** — exit 2 with clap's error on stderr; when the flag exists on a sibling subcommand the message says which one (`--glob is a map flag`). Help on stdout is reserved for `--help`, which is a request for help — a rejected argument list is not a result.
- **Notes beside a real result** — qualifications of a delivered answer (partial path misses, truncation notes, ambiguity counts) stay exit 0, message on stderr, result on stdout.

The recovery rule an agent needs is one sentence: *stdout empty or exit non-zero → the call was wrong; read stderr; fix the call.*

Relatedly, capped output is never silent (issue #32): `callers`/`callees`/`impact`/`reverse-deps` headers report the true total when `--limit` trims the display, JSON carries `total`/`truncated` (and `frontier_truncated` when `--depth` stopped a walk that still had edges), and `map --max-members` prints a `+N more` line for what it cut.

`map` and `digest` are one command (issue #37): same walk, same `Declaration` IR, byte-identical `--json`. `map` exposes three orthogonal axes — detail (`--detail names|signatures|full`), visibility (`--no-private`, `--no-fields`, …), and scope (`--glob`, `--max-members`) — and `digest` is an alias for `map --preset digest` (`--detail names --no-private --no-fields --max-members 50`), with explicit flags overriding the preset. Detail below `full` also sheds doc comments from the JSON payload, which is routinely a third of its weight.

## MCP Server (`src/mcp/`)

`ast-bro mcp` runs the binary as a [Model Context Protocol](https://modelcontextprotocol.io) server so coding agents can invoke the same operations as native tools. The implementation is intentionally tiny:

- **Transport**: line-delimited JSON-RPC 2.0 on stdin/stdout, fully synchronous — no tokio, no extra dependencies. The cost is ~600 KB of binary (~1%) and zero overhead on the regular CLI commands, since none of the MCP code runs unless you invoke the `mcp` subcommand.
- **`src/mcp/protocol.rs`**: serde types for `Request`/`Response`/`RpcError` and the standard JSON-RPC error codes.
- **`src/mcp/tools.rs`**: declares nineteen tool schemas (`map`, `digest`, `show`, `implements`, `callers`, `callees`, `trace`, `surface`, `impact`, `context`, `squeeze`, `deps`, `reverse_deps`, `cycles`, `graph`, `search`, `find_related`, `index`, `run`) and dispatches `tools/call` into the existing `core::render_*` / `calls::*` / `surface::*` / `impact::*` / `context::*` / `deps::*` / `search::*` functions. Each tool maps 1:1 to a CLI subcommand and reuses its render logic byte-for-byte, so the JSON schemas are shared with the CLI's `--json` output.
- **`src/mcp/mod.rs`**: read loop, method routing (`initialize`, `ping`, `tools/list`, `tools/call`, `resources/list`, `prompts/list`), and panic-safe tool dispatch (panics are surfaced as `-32603 internal error` instead of taking the server down).

Tools are exposed in their text form by default — that's what the agent prompt is built around — with `json: true` available for any client that wants the structured payload.

## Adding a New Language

Adding a new language is incredibly straightforward due to the foundation provided by `ast-grep-language`.

1. Identify the target language from the `SupportLang` enum in `ast-grep` (e.g. `SupportLang::Cpp`). If not present, you'll need a native fallback — Markdown does this via `MarkdownLang` in `src/adapters/markdown.rs`, and SQL skips tree-sitter entirely with a regex parser in `src/adapters/sql.rs`.
2. Create a new `src/adapters/mylang.rs` file and `pub mod mylang;` it from [`src/adapters/mod.rs`](../src/adapters/mod.rs).
3. Implement the `LanguageAdapter` trait.
4. Write a `_walk_top` function to perform depth-first traversal of the `ast_grep_core::Node` children.
5. Identify AST kinds by matching `node.kind()` and retrieve source values using `node.field("name")` or slicing `src[node.range().start .. node.range().end]`.
6. Convert them to generic `Declaration` objects representing Classes, Functions, Fields, Interfaces, etc.
7. Wire your new adapter into the `parse_file_for_hook` routing match block in [`src/main_helpers.rs`](../src/main_helpers.rs). Languages that bypass `ast-grep` (Markdown, SQL) get a pre-`SupportLang` extension check at the top of that function.
