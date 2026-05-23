---
name: ast-outline
description: Fast AST-based structural outline for source code. Use to explore unfamiliar directories, list a file's symbols without reading bodies, jump to a specific function/class, find subclasses or implementations, search a repo by symbol or behaviour, or extract a package's true public API, or analyze file-level dependencies. Prefer this over reading whole files when you only need shape.
user-invocable: true
---

## Use `sb` (the `ast-bro` toolkit; formerly `ast-outline`) to explore the code

`sb` is the short alias for the `ast-bro` binary — same tool, fewer keystrokes. The legacy `ast-outline` command is still installed as a thin proxy, so either name works; prefer `sb` in your commands.

Usage: sb <COMMAND> [OPTIONS]

Commands:
  map           Map files or directories — signatures with line ranges, no method bodies
  show          Extract source of a symbol
  digest        One-page module map
  implements    Find subclasses / implementations
  prompt        Print the agent prompt snippet
  install       Install ast-bro into a coding-agent CLI
  uninstall     Remove ast-bro from a coding-agent CLI
  status        Report what's installed where
  hook          Internal: read a tool-call event from stdin and respond
  mcp           Run as an MCP (Model Context Protocol) server over stdio
  search        Hybrid BM25 + dense semantic search over the repo
  find-related  Find chunks semantically similar to a given file:line
  surface       True public API surface — resolves `pub use` / `__all__` re-exports
  deps          Forward import-graph traversal: what does this file import (transitively)?
  reverse-deps  Reverse import-graph: who imports this file (transitively)?
  cycles        Find import cycles via Tarjan SCC
  graph         Emit the dep graph (text or JSON)
  callers       Find callers of a symbol — AST-accurate, no grep noise
  callees       What does this symbol call? — AST-accurate forward call traversal
  index         Build, refresh, or inspect the per-repo search index
  run           AST-aware search and rewrite using pattern matching with metavariables

Each command has `--json` for stable schemas and `--compact` for single-line JSON. Pass an unknown flag or no command and the help text prints automatically — there's no "default" command, every operation is explicit.

Read structure with `sb` before opening full contents. Pull method bodies only once you know which ones you need.

Stop at the step that answers the question:

1. **Unfamiliar directory** — `sb digest <dir>`: one-page map of every file's types and public methods.

2. **One file's shape** — `sb map <file>`: signatures with line ranges, no bodies (5–10× smaller than a full read).

3. **One method, class, or markdown section** — `sb show <file> <Symbol>`. Suffix matching: `TakeDamage`, or `Player.TakeDamage` when ambiguous. Multiple at once: `sb show Player TakeDamage Heal Die`. For markdown, the symbol is the heading text.

4. **Who implements/extends a type** — `sb implements <Type> <dir>`: AST-accurate (skip `grep`), transitive by default with `[via Parent]` tags on indirect matches. Add `--direct` for level-1 only.

5. **You don't know the file or symbol name** — `sb search "<query>"`: hybrid BM25 + dense semantic search over the repo. Use bare identifiers for symbol lookup (`HandlerStack`, `Sinatra::Base` — auto-leans BM25), full sentences for behaviour search ("how does login work" — auto-balances semantic + BM25). First call builds an index at `.ast-bro/index/` (~seconds for typical repos); subsequent calls reuse it and refresh incrementally.

6. **Find code similar to a chunk you already have** — `sb find-related <file>:<line>`: returns chunks semantically similar to the one containing that line. Useful for "what else looks like this?" or finding alternative implementations. Pastes directly from `search` output (which prints results as `path:start-end`).

7. **The actual published API of a package** — `sb surface <dir>`: resolves `pub use` re-exports (Rust) and `__all__` (Python) so you see exactly what a downstream user can reach, not the union of every `pub`/non-underscore item. Falls back to visibility-filtered output for Java/C#/Go/Kotlin (no real re-export concept). Use `--tree` for hierarchy, `--include-chain` to see the re-export path each entry took.

8. **What does this file pull in / who depends on it / are there cycles?** — file-level dep-graph commands. First call builds a graph at `.ast-bro/deps/graph.bin` (~hundreds of ms for typical repos); subsequent calls reuse it.
   - `sb deps <file> [--depth N]`: forward — what `<file>` imports (transitively).
   - `sb reverse-deps <file> [--depth N]`: backward — who imports `<file>`. Use before refactoring to know the blast radius.
   - `sb cycles [<dir>]`: import cycles via Tarjan SCC. Exits non-zero when cycles exist (CI gate).
   - `sb graph [<dir>]`: emit the full graph (text). Add `--json` for `ast-bro.graph.v1`.

9. **Who calls X / what does X call?** — symbol-level call graph (shares the dep-graph cache).
   - `sb callers <Symbol>`: AST-accurate callers, no `grep` false positives on overloaded names, comments, or string literals. Kind-aware: a function gets call-sites; a type gets implementors / constructions / ancestors.
   - `sb callees <Symbol>`: forward — what `<Symbol>` itself calls.
   - Edges are tagged `Exact` / `Inferred` / `Ambiguous` by a three-pass resolver (same-file → global symbol table → dep-graph disambiguation); filter by precision when the resolver isn't sure.

10. **Find or rewrite by AST pattern** — `sb run -p '<pattern>'`: structural search with metavariables (`$VAR`, `$$$` for splats). Add `-r '<rewrite>'` for a dry-run diff, or `-r '<rewrite>' --write` to apply in-place. Pass `--lang <lang>` to pre-compile the pattern (faster across many files; also fails fast on an invalid pattern). Use when you need a structural shape (`foo($$$)` regardless of args) rather than a text match.

**Path / argument expectations:**
- `deps`, `reverse-deps` → expect a **file** path
- `graph`, `cycles` → expect a **directory** (repo root)
- `callers`, `callees` → expect a **symbol name** (function or type), not a path
- `run` → expects a `-p <pattern>` flag, optionally `-r <rewrite>` and `--write`

**Legacy paths:** if a repo still has `.ast-outline/` or `.ast-outline-ignore`, `sb` auto-migrates them to `.ast-bro/` and `.ast-bro-ignore` on first run.
