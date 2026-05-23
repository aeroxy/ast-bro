# ast-bro

Fast, AST-based **code-navigation toolkit** for source files — surface the *shape* of a file (signatures with line numbers, no method bodies), the *true public API* of a package, the *dependency graph* between files, the *call graph* between symbols, and search the repo by *symbol* or *behaviour*. Fourteen subcommands, one binary, built for LLM coding agents and humans who’d rather not waste tokens reading every file just to understand a codebase.

[ast-bro](https://github.com/aeroxy/ast-bro) is written in Rust and uses [ast-grep](https://github.com/ast-grep/ast-grep)’s incredibly fast [tree-sitter](https://github.com/tree-sitter/tree-sitter) bindings. Thanks to [rayon](https://github.com/rayon-rs/rayon), it parses your entire workspace concurrently—often in milliseconds. For Google- or ByteDance-scale monorepos, [ast-bro](https://github.com/aeroxy/ast-bro) benefits from the additional abstraction layer provided by [repolayer](https://github.com/zhousiyao03-cyber/repolayer).

[![crates.io](https://img.shields.io/crates/v/ast-bro.svg)](https://crates.io/crates/ast-bro)
[![npm](https://img.shields.io/npm/v/ast-bro)](https://www.npmjs.com/package/ast-bro)
[![PyPI](https://img.shields.io/pypi/v/ast-bro)](https://pypi.org/project/ast-bro/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/aeroxy/ast-bro)

> **Renamed from `ast-outline` (v2.1.x and earlier).** The project outgrew "outline" — it now ships dep graphs, call graphs, hybrid semantic search, true public API resolution, and a structural search/rewrite engine, not just structural outlining. The old name also collided with an unrelated [VS Code extension](https://marketplace.visualstudio.com/items?itemName=cancerberosgx.vscode-typescript-ast-outline) and an [npm package](https://www.npmjs.com/package/ast-outline) — too generic to keep.
>
> **Upgrading from `ast-outline`?** Run any `ast-bro` command once and it will auto-migrate `.ast-outline/` → `.ast-bro/` (cache), `.ast-outline-ignore` → `.ast-bro-ignore` (per-repo filter), `~/.cache/ast-outline/` → `~/.cache/ast-bro/` (model cache), and any `ast-outline` entries in your MCP config → `ast-bro`. The legacy `ast-outline` binary is still installed as a thin proxy that execs into `ast-bro`, so existing scripts keep working.

---

## Purpose

**[ast-bro](https://github.com/aeroxy/ast-bro) exists to make LLM coding agents faster, cheaper, and smarter
when navigating unfamiliar code.**

Modern agentic coding tools explore codebases by reading files directly. That's reliable but has a massive cost: on a 1000-line file, the agent pays for 1000 lines of tokens just to answer *"what methods exist here?"* — and reading is only one of several questions an agent has. *"Who imports this?"* *"What's the public API?"* *"Are there cycles?"* *"Where in the repo is the login flow?"* — each one historically required dozens of file reads or noisy `grep`s.

[ast-bro](https://github.com/aeroxy/ast-bro) collapses each of those questions into a single command:

1. **Shape over bytes.** `map` / `digest` / `show` give you signatures and line ranges instead of method bodies — typically a **95% token saving** vs reading the file. `implements` finds subclasses with AST accuracy, no `grep` false positives.
2. **Published API in one call.** `surface` resolves `pub use` re-exports (Rust), `__all__` (Python), barrel files (TypeScript), `export` clauses (Scala) so you see the surface a downstream user actually sees — not the union of every public item per file.
3. **Dependency graph for free.** `deps` / `reverse-deps` / `cycles` / `graph` build a file-level import graph (Rust, Python, TS/JS, Java, C#, Kotlin, Scala, Go) cached at `.ast-bro/graph/`. Use `reverse-deps` before refactoring to know the blast radius. `cycles` exits non-zero — wire it into a CI gate. `graph` emits the full dependency graph (text by default, `--json` for JSON).
4. **Symbol-level call graph.** `callers` / `callees` answer "who calls X" and "what does X call" with AST accuracy across all 14 languages — no `grep` false positives on overloaded names, comments, or string literals. Both are kind-aware: ask for a function and you get call-sites; ask for a type and you get implementors / constructions / ancestors. A three-pass resolver (same-file → global symbol table → dep-graph disambiguation) tags every edge `Exact` / `Inferred` / `Ambiguous` so you can filter by precision. Same on-disk cache as the dep graph.
5. **Hybrid semantic search.** `search` runs BM25 + dense embeddings via [`potion-code-16M`](https://huggingface.co/minishlab/potion-code-16M) (a static, no-inference model — ~64 MB, runs on CPU in microseconds). `find-related` returns chunks structurally similar to one you already have, with a dep-graph-aware boost when a graph cache exists.
6. **Fourteen native MCP tools.** Every CLI command is also exposed as an MCP tool — `ast-bro install --mcp <agent>` wires it into Claude Code, Cursor, Gemini, Codex, or VS Code Copilot in one line.

### The workflow

**Before [ast-bro](https://github.com/aeroxy/ast-bro):**

```
Agent: Read Player.cs            # 1200 lines of tokens
Agent: Read Enemy.cs             # 800 lines of tokens
Agent: Read DamageSystem.cs      # 400 lines of tokens
Agent: grep "IDamageable" src/   # noisy, lots of false matches
...
```

**With [ast-bro](https://github.com/aeroxy/ast-bro):**

```console
Agent: ast-bro surface .                  # one-page true public API of the crate/package
Agent: ast-bro digest src/Combat          # ~100 lines, whole module's structure
Agent: ast-bro implements IDamageable     # precise list, no grep noise
Agent: ast-bro search "damage handling"   # hybrid BM25 + dense semantic, ranked
Agent: ast-bro show Player.cs TakeDamage  # just the method body
Agent: ast-bro reverse-deps Player.cs     # who imports this — blast radius before refactor
Agent: ast-bro callers Player.TakeDamage  # AST-accurate call sites — no grep false positives
Agent: ast-bro callees Player.TakeDamage  # what TakeDamage itself calls
Agent: ast-bro cycles src/                # find import cycles via Tarjan SCC
```

Result: **same understanding, a fraction of the tokens, a fraction of the round-trips.**
For "what does this package actually expose?" — historically the most expensive question, since the answer was "read every file" — `surface` resolves the re-export graph and gives you the answer directly, often replacing dozens of file reads with a single call. For "what would break if I change this method?" — `callers` gives you the AST-accurate set of call sites in one shot, instead of `grep`-ing a homonym across the repo.

---

## Supported languages

| Language | Extensions |
| --- | --- |
| Rust       | `.rs` |
| C#         | `.cs` |
| C++        | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh` |
| Python     | `.py`, `.pyi` |
| TypeScript | `.ts`, `.tsx` |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` |
| Java       | `.java` |
| Kotlin     | `.kt`, `.kts` |
| Scala      | `.scala`, `.sc` |
| Go         | `.go` |
| PHP        | `.php` |
| Ruby       | `.rb` |
| SQL        | `.sql`, `.ddl`, `.dml` |
| Markdown   | `.md`, `.markdown`, `.mdx`, `.mdown` |

*More coming soon! Adding another language is a single new adapter file leveraging the massive `ast-grep` language ecosystem.*

---

## What gets walked

[ast-bro](https://github.com/aeroxy/ast-bro) skips a lot of files when walking a directory — by design. Filters apply uniformly across every subcommand.

1. **`.gitignore` and friends** — every level's `.gitignore`, your global gitignore, `.git/info/exclude`, and `.ignore` files (the [`ignore`](https://crates.io/crates/ignore) crate's convention used by `ripgrep`/`fd`).
2. **Hardcoded denylist** — directories almost no one wants walked, even if `.gitignore` doesn't list them: `.git`, `node_modules`, `target`, `dist`, `build`, `__pycache__`, `.venv`, `venv`, `.cache`, `.idea`, `.vscode`, `.next`, `.nuxt`, `.turbo`, `.parcel-cache`, `.gradle`, `.tox`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache`, `.eggs`, `.ast-bro`, and a few others.
3. **`.ast-bro-ignore`** — per-repo escape hatch. Same syntax as `.gitignore`. Useful for excluding paths from [ast-bro](https://github.com/aeroxy/ast-bro) that you *don't* want excluded from git itself, e.g. test fixtures or vendored corpora:

   ```gitignore
   # .ast-bro-ignore
   tests/fixtures/large_corpus/
   benches/data/
   *.generated.rs
   ```
4. **Extension allowlist** — files are only opened if their extension is one ast-bro knows how to parse (the table above for map/digest/show/implements; a broader set for the search commands).

Want to see exactly what ast-bro walks? Compare `ast-bro digest some/dir` with `rg --files some/dir` — anything in `rg` but not the digest is being filtered by one of the layers above.

---

## Install

### Homebrew (macOS)

```bash
brew install aeroxy/tap/ast-bro
```

### npm

```bash
npm install -g ast-bro
```

### pip

```bash
pip install ast-bro
```

### Cargo

```bash
cargo install ast-bro
```

This installs the [ast-bro](https://github.com/aeroxy/ast-bro) CLI globally into `~/.cargo/bin` — make sure that's on your `PATH`.

### Nix

You can run [ast-bro](https://github.com/aeroxy/ast-bro) directly with Nix without installing:

```bash
nix run github:aeroxy/ast-bro
```

Or add it as a dependency in your Nix flake:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    ast-bro.url = "github:aeroxy/ast-bro";
  };

  outputs = { self, nixpkgs, ast-bro }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.${system}.default = pkgs.mkShell {
        buildInputs = [ ast-bro.packages.${system}.default ];
      };
    };
}
```

---

## Quick start

```bash
# Map the structure of one file
ast-bro map path/to/Player.rs
ast-bro map path/to/user_service.py

# Map a whole directory (recurses supported extensions in parallel)
ast-bro map src/

# Print the exact source of one specific method
ast-bro show Player.cs TakeDamage

# Compact public-API map of a whole module
ast-bro digest src/Services

# True public surface (resolves `pub use` / `__all__`, not every `pub` item)
ast-bro surface .                  # auto-detect Cargo.toml / pyproject.toml / __init__.py
ast-bro surface --tree --include-chain mycrate/

# Every class that inherits/implements a given type
ast-bro implements IDamageable src/

# Dependency graph: forward, reverse, cycles, full
ast-bro deps src/auth.rs --depth 2  # what auth.rs imports (transitively)
ast-bro reverse-deps src/auth.rs    # who imports auth.rs (refactor blast radius)
ast-bro cycles                      # find import cycles via Tarjan SCC
ast-bro graph .                     # full dependency graph (text)
ast-bro graph . --json              # same, as JSON (ast-bro.graph.v1)

# Call graph: who calls X, what does X call (AST-accurate, all 14 langs)
ast-bro callers TakeDamage              # function/method: in-edges
ast-bro callers Player                  # type: implementors + constructions
ast-bro callees Player.TakeDamage       # function/method: out-edges
ast-bro callees Player --depth 2        # type: ancestor walk (transitive)
ast-bro callers src/Player.cs:TakeDamage --include-ambiguous

# Hybrid BM25 + dense semantic search (builds an index on first call)
ast-bro search "how does login work"
ast-bro search "HandlerStack" -k 5

# Find code semantically similar to a given file:line
ast-bro find-related src/auth/login.rs:42

# Build / refresh / inspect the per-repo search index
ast-bro index            # build or refresh
ast-bro index --stats    # show chunk count, model, etc.
ast-bro index --rebuild  # drop cache and rebuild

# Output a prompt snippet to steer LLM agents
ast-bro prompt >> AGENTS.md

# Machine-readable JSON (stable schema, great for tooling)
ast-bro map src/player.rs --json
ast-bro digest src/ --json
ast-bro show Player.cs TakeDamage --json
ast-bro implements IDamageable src/ --json
ast-bro search "json rendering" --json
```

---

## Using with LLM coding agents

This is the main use case. The fastest path is `ast-bro install`,
which writes the agent prompt snippet (and, where supported, a real
`Read`-interceptor hook) into your coding agent's config.

```bash
# Install into every supported CLI it can detect on your system.
ast-bro install --all

# Or pick a single target.
ast-bro install --target claude-code
ast-bro install --target gemini --min-lines 150

# See exactly what would change before writing.
ast-bro install --all --dry-run

# Per-repo install (default is global).
ast-bro install --target claude-code --local

# Remove everything we wrote.
ast-bro uninstall --all

# Quick visibility.
ast-bro status
```

Supported targets: `claude-code`, `gemini`, `tabnine`, `cursor`,
`aider`, `codex`, `copilot`. Claude Code, Gemini, and Tabnine also get
a tool-call hook that intercepts `Read` on supported source files when
they exceed `--min-lines` (default 200) and substitutes the map output.
The other targets receive the prompt only.

### Claude Code subagent shadowing

Claude Code has isolated subagents (Explore, Plan, general-purpose) that run in
their own context and cannot see the main `CLAUDE.md`. `ast-bro install` 
automatically shadows these subagents with `.claude/agents/<Name>.md` files 
containing the full ast-bro prompt.

When you run `ast-bro install --target claude-code`, you get:
- `CLAUDE.md` — main agent prompt (global or local per-repo)
- `.claude/settings.json` — `Read` tool hook
- `.claude/agents/Explore.md` — Explore subagent with the prompt injected

This solves the "why doesn't my subagent use ast-bro?" problem — subagents
now get the prompt automatically. Legacy manual `~/.claude/agents/Explore.md` files
are wrapped in marker blocks in-place (non-breaking).

### Skills for manual installation

A `skills/` folder is included in the repo for users who prefer manual setup:

```bash
# Clone or download the repo
git clone https://github.com/aeroxy/ast-bro.git
cd ast-bro

# Copy the skill to your user skills directory
cp -r skills/ast-bro ~/.claude/skills/ast-bro

# Then manually invoke from Claude Code
/ast-bro
```

This works alongside `ast-bro install` — the skill definition tells Claude Code
how to invoke the [ast-bro](https://github.com/aeroxy/ast-bro) CLI with proper tool schemas and documentation.

Manual install via `ast-bro prompt` (e.g. project-level):

```bash
ast-bro prompt >> AGENTS.md
ast-bro prompt | pbcopy   # macOS clipboard
```

### Works with

- Claude Code (+ custom subagents like `Explore`, `codebase-scout`)
- Cursor agent mode
- Aider
- Copilot Chat / Workspace
- Any custom agent on the Claude / OpenAI / Gemini APIs
- Humans (the colored terminal format is highly readable; `show` is a nice alternative to `grep -A 20`)

---

## Output format

The format is designed to be **LLM-friendly**: Python-style indentation,
line-number suffixes in `L<start>-<end>` form, doc-comments preserved.
The header summarises scale and flags partial parses.

When you run it yourself, you'll see a gorgeous ANSI-colored output. Don't worry, the terminal colors are automatically stripped when piped to a file or consumed by an agent's shell hook!

### Rust

```
# src/core.rs (490 lines, 3 types, 12 methods, 5 fields)
pub struct Declaration  L10-120
    pub kind: DeclarationKind  L12
    pub name: String  L15
    pub fn lines_suffix(&self) -> String  L30-48
```

### `show` with ancestor context

`ast-bro show <file> <Symbol>` prints a `# in: ...` breadcrumb
between the header and the body so you know what the extracted code is
nested inside, without a second `map` call:

```
# Player.cs:30-48  Game.Player.PlayerController.TakeDamage  (method)
# in: namespace Game.Player → public class PlayerController : MonoBehaviour, IDamageable
/// <summary>Apply damage.</summary>
public void TakeDamage(int amount) { ... }
```

---

## JSON output

Add `--json` to any command to get the full symbol graph as stable,
structured JSON instead of formatted text — ideal for editors, language
servers, CI tooling, or any script that needs to consume the data
programmatically.

```bash
ast-bro map src/player.rs --json        # per-file map
ast-bro digest src/ --json              # digest view
ast-bro show Player.cs TakeDamage --json
ast-bro implements IDamageable src/ --json
ast-bro map src/ --json --compact       # single-line (no pretty-print)
```

Every JSON document includes a `schema` field that is bumped on breaking
changes, so downstream tooling can guard on it:

```json
{
  "schema": "ast-bro.outline.v1",
  "files": [
    {
      "path": "src/player.rs",
      "language": "rust",
      "line_count": 312,
      "error_count": 0,
      "declarations": [
        {
          "kind": "struct",
          "name": "Player",
          "signature": "pub struct Player",
          "visibility": "pub",
          "start_line": 10,
          "end_line": 40,
          "children": [ ... ]
        }
      ]
    }
  ]
}
```

| Schema | Command |
|--------|----------|
| `ast-bro.outline.v1` | default outline, `digest --json` |
| `ast-bro.show.v1` | `show --json` |
| `ast-bro.implements.v1` | `implements --json` |
| `ast-bro.surface.v1` | `surface --json` |
| `ast-bro.deps.v1` | `deps --json` |
| `ast-bro.reverse-deps.v1` | `reverse-deps --json` |
| `ast-bro.cycles.v1` | `cycles --json` |
| `ast-bro.graph.v1` | `graph --json` |
| `ast-bro.callers.v1` | `callers --json` |
| `ast-bro.callees.v1` | `callees --json` |
| `ast-bro.search.v1` | `search --json` |
| `ast-bro.related.v1` | `find-related --json` |
| `ast-bro.index-stats.v1` | `index --stats --json` |

---

## MCP server

Run [ast-bro](https://github.com/aeroxy/ast-bro) as a [Model Context Protocol](https://modelcontextprotocol.io)
server over stdio so any MCP-aware coding agent can call the same operations
as native tools — no shell parsing required:

```bash
ast-bro mcp
```

The server speaks line-delimited JSON-RPC 2.0 on stdin/stdout and exposes fourteen
tools that map 1:1 to the CLI commands:

| Tool | Equivalent CLI | Returns |
|------|----------------|---------|
| `map`          | `ast-bro map <paths>`                | text, or `ast-bro.map.v1` with `json: true` |
| `digest`       | `ast-bro digest <paths>`             | text, or `ast-bro.map.v1` with `json: true` |
| `show`         | `ast-bro show <path> <syms>`         | text, or `ast-bro.show.v1` with `json: true` |
| `implements`   | `ast-bro implements <type> <paths>`  | text, or `ast-bro.implements.v1` with `json: true` |
| `callers`      | `ast-bro callers <symbol>`           | text, or `ast-bro.callers.v1` with `json: true` |
| `callees`      | `ast-bro callees <symbol>`           | text, or `ast-bro.callees.v1` with `json: true` |
| `surface`      | `ast-bro surface [path]`             | text, or `ast-bro.surface.v1` with `json: true` |
| `deps`         | `ast-bro deps <file>`                | text, or `ast-bro.deps.v1` with `json: true` |
| `reverse_deps` | `ast-bro reverse-deps <file>`        | text, or `ast-bro.reverse-deps.v1` with `json: true` |
| `cycles`       | `ast-bro cycles [path]`              | text, or `ast-bro.cycles.v1` with `json: true` |
| `graph`        | `ast-bro graph [path]`               | text by default; `json: true` for `ast-bro.graph.v1` |
| `search`       | `ast-bro search "<query>"`           | text, or `ast-bro.search.v1` with `json: true` |
| `find_related` | `ast-bro find-related <file>:<line>` | text, or `ast-bro.related.v1` with `json: true` |
| `index`        | `ast-bro index`                      | text, or `ast-bro.index-stats.v1` with `json: true` |

Wire it into a client by pointing at the binary:

```jsonc
{
  "mcpServers": {
    "ast-bro": { "command": "ast-bro", "args": ["mcp"] }
  }
}
```

The server is fully synchronous, has no extra runtime dependencies, and adds
roughly 1% to the binary size. The CLI itself is unaffected — none of the MCP
code runs unless you invoke the `mcp` subcommand.

---

## Semantic search

`ast-bro search` runs hybrid retrieval over a per-repo index:

- **BM25** for exact identifier matches and keyword density.
- **Dense embeddings** via [`minishlab/potion-code-16M`](https://huggingface.co/minishlab/potion-code-16M) — a static (no inference) `vocab × 256` model that runs on CPU in microseconds.
- **Reciprocal Rank Fusion** (k = 60) blends the two; alpha auto-resolves to 0.3 for symbol queries (`HandlerStack`, `Sinatra::Base` — lean BM25) and 0.5 for natural language ("how does login work" — balanced).
- A ranking pass adds definition boosts (3× for chunks that *define* a queried symbol), file-coherence boosts (multi-chunk hits in the same file lift the top chunk), file-stem matches for NL queries, and path-based penalties (test files 0.3×, `.d.ts` stubs 0.7×, `__init__.py` 0.5×).

`ast-bro find-related <file>:<line>` is the same engine in semantic-only mode, language-filtered, with the source chunk excluded — useful for "what else is structured like this?"

```bash
ast-bro search "request validation" -k 5
ast-bro search "HandlerStack" --json
ast-bro find-related src/auth/login.rs:42 -k 3
```

### How indexing works

First call to `search` / `find-related` builds an index at `.ast-bro/index/`:

```text
.ast-bro/
  .gitignore           # auto-written, contents: "*"
  index/
    meta.json          # schema + model + chunk_count
    chunks.bin         # per-chunk content + line range + language
    embeddings.f32     # chunk_count × 256 little-endian f32, mmap-friendly
    bm25.bin           # vocab + idf + postings
    files.bin          # per-file mtime + xxhash + chunk range
    lock               # advisory lock for concurrent writers
```

Subsequent calls walk the tree, compare `(mtime, size)` against `files.bin`, and only hash files where the cheap check fails. If anything changed, the index rebuilds automatically (a v2 will support partial updates against the same on-disk format). Steady-state cost on an unchanged 10k-file repo: ~30 ms of stat syscalls.

The model is downloaded once (~64 MB) on first use to `~/.cache/ast-bro/models/`. It tries HuggingFace first, falls back to `hf-mirror.com` if blocked. **TLS verification is disabled by default** so corporate MITM proxies don't break setup; integrity is enforced via SHA-256 on every cached file. Set `AST_OUTLINE_TLS_STRICT=1` to enforce strict TLS.

For more on what gets indexed (the five filter layers, `.ast-bro-ignore` syntax) see the "What gets walked" section above. For the security trade-offs around the TLS default, see the [network-security wiki page](https://github.com/aeroxy/ast-bro/blob/main/wiki/network-security.md) on GitHub.

`find-related` quietly benefits from the dep graph too — when one is cached, results are reranked so files within depth 2 of the source (importer or importee) get a multiplicative boost. Disable with `--no-dep-boost`.

---

## Dependency graph

`ast-bro deps`, `reverse-deps`, `cycles`, and `graph` build a file-level import graph for the project and answer different questions on it:

```bash
ast-bro deps src/auth.rs --depth 2          # what does auth.rs pull in?
ast-bro reverse-deps src/auth.rs            # who imports auth.rs? (refactor blast radius)
ast-bro cycles                              # find import cycles via Tarjan SCC (exit 3 if any)
ast-bro graph .                              # full dependency graph (text)
ast-bro graph . --json                      # same, as JSON (ast-bro.graph.v1)
```

All four commands share one cache at `.ast-bro/graph/index.bin` (a unified `UnifiedGraph { deps, calls: Option<CallGraph> }` — same file used by `callers` / `callees`, see below). First call builds the dep half (~hundreds of ms for typical repos via the same `ignore`-respecting walk used by search); subsequent calls reuse it via per-file delta detection, with `--rebuild` to force a fresh build. Inside `ast-bro mcp`, every `tools/call` shares one in-memory `Arc<UnifiedGraph>` so the second invocation in a session is a memory read, not a disk read.

Resolution is per-language but shares one suffix-index resolver:

- **Rust**: `use crate::*` / `use super::*` / `mod foo;` (with `#[path]` attribute support).
- **Python**: relative imports (`from .x import y`), `__init__.py` packages, bare `import a.b`.
- **TypeScript / JavaScript**: relative paths with extension probing (`.ts → .tsx → .mts → .cts → .d.ts → .js → ... → .json`), `index.*` fallback, `tsconfig.json` `paths` aliases.
- **Java / Kotlin / Scala / C#**: FQN suffix index built from each file's `package` / `namespace` declaration. Inner classes resolve via strip-and-retry.
- **Go**: `go.mod` `module` prefix is stripped; `import "mymod/pkg/foo"` resolves to `pkg/foo/*.go` (directory-as-package).

The four commands are also exposed as MCP tools for agents. For internals (suffix index, Tarjan SCC, per-file invalidation, find-related dep boost) see the [deps wiki page](https://github.com/aeroxy/ast-bro/blob/main/wiki/deps.md) on GitHub.

---

## Call graph

`ast-bro callers` and `ast-bro callees` answer "who calls X" and "what does X call" with AST accuracy across all 14 languages. They replace `grep` for refactor blast-radius assessment — no false positives on overloaded names, comments, or string literals.

```bash
ast-bro callers TakeDamage              # function/method: in-edges
ast-bro callees TakeDamage              # function/method: out-edges
ast-bro callers Player                  # type: implementors + constructions
ast-bro callees Player --depth 2        # type: ancestor walk (transitive)
ast-bro callers Player.TakeDamage --include-ambiguous --json
```

Both commands are **kind-aware**:

| Target kind | `callers X` | `callees X` |
|---|---|---|
| function / method / constructor | call sites that invoke `X` | call sites inside `X`'s body |
| class / struct / trait / interface / enum / record | implementors + constructions (covers `Foo()`, `new Foo()`, `Foo {}`, `Foo::new()`) | ancestor types and the methods they declare (transitive via `--depth N`) |

Symbol forms accepted by both: bare suffix (`TakeDamage`), dotted (`Player.TakeDamage`), file-scoped (`src/Player.cs:TakeDamage`), or flag form (`--file src/Player.cs --symbol TakeDamage`).

**Three-pass resolver.** Bare names are disambiguated in three increasing-cost passes:

1. **Same-file** — local definitions + per-file `import` / `use` / `using` bindings.
2. **Global symbol table** — single-match promotion across the project. Receiver-bearing calls (`obj.bar()`) skip this pass to avoid `builder.hidden()`-style false positives on global homonyms.
3. **Dep-graph disambiguation** — for ambiguous matches, filter candidates by the caller's transitive forward-dep closure.

Every edge carries a `Confidence` tag — `Exact` (passes A/B), `Inferred` (pass C narrowed to one), or `Ambiguous` (multiple candidates survive). `--include-ambiguous` (callers) and `--external` (callees) surface the noisier results when explicitly requested.

**Cache.** Same `.ast-bro/graph/index.bin` as the dep graph, lazily promoted — users who only run `deps` / `cycles` never pay the call-graph build cost. Per-file invalidation: edit one file, only that file gets re-extracted and re-resolved.

For internals (per-language node-kind tables, the call-shape pitfalls each adapter handles, the per-file patch path, cost numbers) see the [calls wiki page](https://github.com/aeroxy/ast-bro/blob/main/wiki/calls.md) on GitHub.

---

## Architecture & Development

See the [wiki](https://github.com/aeroxy/ast-bro/blob/main/wiki/architecture.md) on GitHub for details on how [ast-bro](https://github.com/aeroxy/ast-bro) leverages `ast-grep` internally and how you can add new language adapters.

### Getting started

```bash
git clone https://github.com/aeroxy/ast-bro.git
cd ast-bro

# With Cargo
cargo run -- digest src/

# With Nix flake
nix develop        # Enter development shell
nix build          # Build the project
nix flake check    # Run all checks (tests, clippy, formatting)
```

Contributions welcome.

---

## License

[MIT](./LICENSE)
