# Architecture

`ast-bro` is a fast, structurally-aware code-navigation toolkit. It started as a "shape extractor" (signatures with line ranges, no method bodies) and has grown into six orthogonal subsystems sharing one binary, one filter pipeline, and one walk infrastructure:

1. **`src/adapters/` + `src/core.rs`** — language adapters parse files into a shared `Declaration` IR; renderers turn that into `map` / `digest` / `show` / `implements` output.
2. **`src/surface/`** — resolves the *true public API* of a package (`pub use`, `__all__`, TypeScript barrels, Scala `export`) instead of just listing every public item per file.
3. **`src/deps/`** — file-level dependency graph (`deps`, `reverse-deps`, `cycles`, `graph`) for nine languages. See [deps.md](deps.md).
4. **`src/calls/`** — symbol-level call graph (`callers`, `callees`) for all 14 languages, with a three-pass resolver (same-file → global symbol table → dep-graph disambiguation). See [calls.md](calls.md).
5. **`src/search/`** — hybrid BM25 + dense semantic search, plus `find-related`. Cached at `.ast-bro/index/`. See [search.md](search.md).
6. **`src/squeeze/`** — reversible token compression for **logs/text** (`squeeze`): a multi-stage pipeline that shrinks repetitive lines and emits a legend so the output round-trips back to the original. Not a code tool — for code, the "compression" is `map` / `digest` / `show`. See [squeeze.md](squeeze.md).

The dep graph and call graph share one on-disk cache at `.ast-bro/deps/graph.bin` (`UnifiedGraph { deps, calls: Option<CallGraph> }`) and one process-wide `Arc<UnifiedGraph>` registry in `src/graph_cache/` so MCP `tools/call`s reuse a single parsed copy across the whole session.

It is written natively in Rust, relying heavily on the [tree-sitter](https://tree-sitter.github.io/tree-sitter/) parsing framework via the excellent [`ast-grep`](https://ast-grep.github.io/) ecosystem bindings, achieving incredibly fast speeds while still taking advantage of `rayon` for massive multithreading across directories. The five walking subsystems all share `src/file_filter.rs` for what gets walked (see [file-filtering.md](file-filtering.md)) — adding a feature in one subsystem doesn't change what files the others see. (`squeeze` is the exception: it reads one explicit file path directly, so it neither walks nor touches the filter pipeline.)

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

## CLI structure (1.0)

Every operation is an explicit subcommand — there's no implicit-default form. Bare `ast-bro` (or `ast-bro --wrong`, or any unknown subcommand) prints help to stdout and exits 0, so an agent that mistypes gets a self-contained correction without a separate `--help` round-trip. The handler lives at the top of `main()` in `src/main.rs` and intercepts clap errors before they hit stderr.

## MCP Server (`src/mcp/`)

`ast-bro mcp` runs the binary as a [Model Context Protocol](https://modelcontextprotocol.io) server so coding agents can invoke the same operations as native tools. The implementation is intentionally tiny:

- **Transport**: line-delimited JSON-RPC 2.0 on stdin/stdout, fully synchronous — no tokio, no extra dependencies. The cost is ~600 KB of binary (~1%) and zero overhead on the regular CLI commands, since none of the MCP code runs unless you invoke the `mcp` subcommand.
- **`src/mcp/protocol.rs`**: serde types for `Request`/`Response`/`RpcError` and the standard JSON-RPC error codes.
- **`src/mcp/tools.rs`**: declares fifteen tool schemas (`map`, `digest`, `show`, `implements`, `callers`, `callees`, `surface`, `squeeze`, `deps`, `reverse_deps`, `cycles`, `graph`, `search`, `find_related`, `index`) and dispatches `tools/call` into the existing `core::render_*` / `calls::*` / `surface::*` / `deps::*` / `search::*` functions. Each tool maps 1:1 to a CLI subcommand and reuses its render logic byte-for-byte, so the JSON schemas are shared with the CLI's `--json` output.
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
