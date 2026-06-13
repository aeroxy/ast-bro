---
name: ast-bro
description: Fast code-navigation toolkit for LLM agents. Use to explore codebases without reading whole files — get file shapes, public APIs, dependency graphs, call graphs, blast-radius analysis, and token-budgeted context.
user-invocable: true
---

## Use `sb` (the `ast-bro` toolkit) to explore the code

`sb` is the short alias for the `ast-bro` binary. The legacy `ast-outline` command still works as a thin proxy.

Each command accepts `--json` for a stable, versioned schema (e.g. `ast-bro.map.v1`) and `--compact` to emit single-line JSON instead of pretty-printed. Pass an unknown flag or no command and the help text prints automatically — there is no "default" command.

Read structure with `sb` before opening full contents. Pull method bodies only once you know which ones you need.

**A typical task done two ways:**

```bash
# Without ast-bro: 4 file reads + 2 greps to find what calls TakeDamage
Read Player.cs            # 1200 lines
Read DamageSystem.cs      # 400 lines
grep "TakeDamage" src/    # noisy, string-match false positives

# With ast-bro: one call, AST-accurate
sb callers Player.TakeDamage
```

Stop at the step that answers the question:

1. **Unfamiliar directory** — `sb digest <dir>`: one-page map of every file's types and public methods.
   ```
   sb digest src/
   ```

2. **One file's shape** — `sb map <file>`: signatures with line ranges, no bodies (5–10× smaller than a full read).
   ```
   sb map src/file_filter.rs
   ```

3. **One symbol's source** — `sb show <file> <Symbol>`: suffix matching, multiple at once. Explicitly-passed extensionless files fall back to shebang detection (`#!/usr/bin/env python3` → Python, `#!/usr/bin/env node` → TypeScript, etc.) — useful for CLI scripts in `bin/` or `~/.local/bin/`. Directory walks skip extensionless files to keep the walk fast.
   ```
   sb show src/main_helpers.rs parse_file_for_hook
   sb show Player.cs TakeDamage Heal Die
   ```

4. **Who implements a type** — `sb implements <Type> <dir>`: AST-accurate (skip `grep`), transitive by default with `[via Parent]` tags. Add `--direct` for level-1 only.
   ```
   sb implements LanguageAdapter src/
   ```

5. **You don't know the file or symbol name** — `sb search "<query>"`: bare identifiers lean BM25 (`HandlerStack`), full sentences lean semantic ("how does login work"). First call builds the index at `.ast-bro/index/`.
   ```
   sb search "token-budgeted context"
   ```

6. **Code similar to a chunk you already have** — `sb find-related <file>:<line>`: pastes directly from `search` output (`path:start-end`).
   ```
   sb find-related src/context.rs:144
   ```

7. **The actual published API of a package** — `sb surface <dir>`: resolves `pub use` (Rust), `__all__` (Python), barrel files (TS/JS), `export` (Scala). `--tree` for hierarchy, `--include-chain` for re-export paths.
   ```
   sb surface .
   ```

8. **File-level deps** — `sb deps <file>`: forward BFS of what `<file>` imports. Footer lists unresolved imports tagged `[external]` so you see what the file tries to pull in from outside the project.
   ```
   sb deps src/impact.rs
   ```

9. **Who imports a file** — `sb reverse-deps <file>`: backward BFS, with `--tests` / `--exclude-tests` to filter by test-file heuristics. Blast radius before a refactor.
   ```
   sb reverse-deps src/impact.rs --exclude-tests
   ```

10. **Import cycles** — `sb cycles [<dir>]`: Tarjan SCC; exits non-zero when cycles exist (CI gate).
    ```
    sb cycles
    ```

11. **The full dependency graph** — `sb graph [<dir>] [--hide-external]`: external imports shown by default (tagged `[external]`); `--json` for `ast-bro.graph.v1`.
    ```
    sb graph . --json
    ```

12. **Who calls X / what X calls / how A reaches B** — symbol-level call graph (shares the dep-graph cache with steps 8–11).

    Edges are tagged `Exact` / `Inferred` / `Ambiguous` by a three-pass resolver (same-file → global symbol table → dep-graph disambiguation). **Ambiguous callers and unresolved/external callees are shown by default** (red/cyan) so you see the full set without re-running. Pass `--hide-ambiguous` (callers) or `--hide-external` (callees) to drop them when you want the cleaner bucket.

    - `sb callers <Symbol>`: in-edges. Kind-aware: a function gets call-sites; a type gets implementors / constructions / ancestors.
      ```
      sb callers run_impact
      sb callers --tests run_impact         # test-file callers only
      sb callers LanguageAdapter            # implementors + constructions
      ```
    - `sb callees <Symbol>`: out-edges.
      ```
      sb callees run_impact --hide-external
      ```
    - `sb trace <FROM> <TO>`: shortest static call path, each hop's body inlined. No-path fallback to both endpoints + target file siblings.
      ```
      sb trace run_impact build_context
      ```

13. **Blast radius of touching a symbol** — `sb impact <Symbol>`: combines callers + callees + file `deps` + `reverse-deps` + test detection; for types, includes implementors and file-level reverse-deps. Four modes: `--mode all|deps|dependents|tests`. **Prefer `impact` over separate calls.** Schema: `ast-bro.impact.v1`.
    ```
    sb impact LanguageAdapter --mode tests
    sb impact run_impact --exclude-tests --depth 3
    ```

14. **Token-budgeted context** — `sb context <Symbol>`: target body + direct callees (bodies→signatures) + callers + transitive at depth 2 (signatures only). Types walk: target → implementors → methods → method callers. Flags `truncated` / `target_omitted` when budget runs short. **Prefer over chains of show + callers + callees.** Default `--budget 8000`. Schema: `ast-bro.context.v1`.
    ```
    sb context LanguageAdapter --budget 2000
    ```

15. **Find or rewrite by AST pattern** — `sb run -p '<pattern>' [-r '<rewrite>'] [--write] [--lang <lang>]`: metavariable patterns (`$VAR`, `$$$` for splats). `--write` mutates files — always dry-run first.
    ```
    sb run -p 'println!($$$)' --lang rust
    ```

16. **Compress a repetitive log/text file** — `sb squeeze <file> [from:to]`: for **logs/text, not code**. Replaces repeated timestamps/tags with short tags plus a reversible legend; falls back to raw when it wouldn't help. `--raw` skips compression. Schema: `ast-bro.squeeze.v1`.
    ```
    sb squeeze app.log
    ```

Path / argument expectations:
- `deps`, `reverse-deps` → expect a file path
- `graph`, `cycles` → expect a directory (repo root)
- `callers`, `callees`, `impact`, `context` → expect a symbol name (function or type), not a path
- `trace` → expects two symbol names (FROM then TO), optional repo root
- `run` → expects a `-p <pattern>` flag, optionally `-r <rewrite>` and `--write`
- `squeeze` → expects a file path, optionally a `from:to` line range

Maintenance commands (not usually called directly — use `sb install` once and rely on `sb prompt` / `sb mcp` for ongoing integration):

```bash
sb index          Build, refresh, or inspect the per-repo search index
sb prompt         Print the agent prompt snippet (for hand-copying into AGENTS.md)
sb install        Install ast-bro into a coding-agent CLI
sb uninstall      Remove ast-bro from a coding-agent CLI
sb status         Report what's installed where
sb mcp            Run as an MCP (Model Context Protocol) server over stdio
sb hook           Internal: read a tool-call event from stdin and respond
```
