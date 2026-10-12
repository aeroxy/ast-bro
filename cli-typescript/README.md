# @ast-bro/cli

[![npm](https://img.shields.io/npm/v/@ast-bro/cli)](https://www.npmjs.com/package/@ast-bro/cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://github.com/aeroxy/ast-bro/blob/main/LICENSE)

npm installer for [ast-bro](https://github.com/aeroxy/ast-bro) — a fast, AST-based code-navigation toolkit for source files (shape, public API, dep & call graphs, hybrid semantic search, structural rewrite, MCP server). Downloads the pre-built Rust binary on install.

> **Formerly `@ast-outline/cli`.** Same project under a new scope (the `ast-outline` name became overloaded after the tool grew beyond outlining). The `ast-outline` command is still installed as a thin proxy that forwards to `ast-bro`, so existing scripts keep working.

## Install

```bash
npm install -g @ast-bro/cli
```

This installs three commands, all forwarding to the same binary:

- `ast-bro` — canonical name
- `sb` — short alias (same tool, fewer keystrokes)
- `ast-outline` — backward-compat shim

## Usage

```bash
# Map the structure of a file (signatures + line ranges, no bodies)
ast-bro map src/player.rs

# Show the exact source of a specific method
ast-bro show Player.cs TakeDamage

# Compact digest of a whole module
ast-bro digest src/services/

# True public API (resolves pub use / __all__ re-exports)
ast-bro surface .

# Find all implementations of a type
ast-bro implements IDamageable src/

# Dependency graph
ast-bro deps src/auth.rs --depth 2
ast-bro reverse-deps src/auth.rs
ast-bro cycles

# Call graph (AST-accurate)
ast-bro callers TakeDamage
ast-bro callees Player.TakeDamage

# Hybrid BM25 + dense semantic search
ast-bro search "how does login work"

# Find semantically similar code
ast-bro find-related src/auth/login.rs:42

# AST-aware structural search and rewrite (with metavariables)
ast-bro run -p '$FUNC($$$)' -l rust
ast-bro run -p 'foo($A)' -r 'bar($A)' --write    # apply to disk
```

On first run, the CLI downloads the pre-built binary for your platform from [GitHub releases](https://github.com/aeroxy/ast-bro/releases) and caches it locally.

| Platform | Cache directory |
|---|---|
| macOS | `~/Library/Caches/ast-bro-<version>/` |
| Linux | `~/.cache/ast-bro-<version>/` |

## Supported Platforms

| Platform | Status |
|---|---|
| macOS ARM64 | Pre-built binary available |
| Other platforms | Build from source (see below) |

For unsupported platforms, build from source:

```bash
cargo install ast-bro
```

## What is ast-bro?

[ast-bro](https://github.com/aeroxy/ast-bro) is a fast, AST-based code-navigation toolkit built for LLM coding agents and humans. It uses [tree-sitter](https://github.com/tree-sitter/tree-sitter) via [ast-grep](https://github.com/ast-grep/ast-grep) to parse source files and provide:

- **File shape** — `map` / `digest` / `show` for signatures with line ranges (95% token savings vs reading full files)
- **True public API** — `surface` resolves re-export graphs across Rust, Python, TypeScript, and more
- **Dependency graph** — `deps` / `reverse-deps` / `cycles` / `graph` for import analysis
- **Call graph** — `callers` / `callees` with AST accuracy across 14 languages
- **Semantic search** — hybrid BM25 + dense embeddings via `search` and `find-related`
- **Structural rewrite** — `run` for AST-aware pattern matching with metavariables (find + replace)
- **MCP server** — every command exposed as an MCP tool for LLM agents

Supports Rust, Python, TypeScript, JavaScript, Java, C#, C++, Kotlin, Scala, Go, PHP, Ruby, Elixir, SQL, and Markdown.

## Links

- [ast-bro source code](https://github.com/aeroxy/ast-bro)
- [PyPI package](https://pypi.org/project/ast-bro/) (Python installer)
- [crates.io](https://crates.io/crates/ast-bro) (Rust library)

## License

[MIT](https://github.com/aeroxy/ast-bro/blob/main/LICENSE)
