---
name: ast-bro
description: Fast code-navigation toolkit for LLM agents. Use to explore codebases without reading whole files — get file shapes, public APIs, dependency graphs, call graphs, blast-radius analysis, and token-budgeted context.
user-invocable: true
---

## Use `sb` (the `ast-bro` toolkit) to explore the code

`sb` is the short alias for the `ast-bro` binary. The legacy `ast-outline` command still works as a thin proxy.

Each command accepts `--json` for a stable, versioned schema (e.g. `ast-bro.map.v1`) and `--compact` to emit single-line JSON instead of pretty-printed.

**Error contract** (uniform across every subcommand): stdout carries results only; every note, hint, and error goes to stderr. Exit `0` = the query ran (even when the answer is legitimately empty), exit `2` = the query could not run as asked (no such path/symbol, unknown flag, missing argument, empty argument list — e.g. a `$(...)` substitution that produced nothing), exit `1` = internal failure. Two deliberate additions: `cycles` exits `3` when cycles exist, and `run` exits `1` when a valid pattern matched nothing (`grep` convention) — a rejected `run` still exits 2 with empty stdout. With `--json`, a rejected call also emits a machine-readable `ast-bro.error.v1` object on stderr. Recovery rule: *stdout empty or exit non-zero → the call was wrong; read stderr; fix the call.* Never silence stderr with `2>/dev/null` — for some commands it carries the only copy of the diagnostic.

**Result caps** (defaults; truncation is always reported, never silent): `--limit 200` on `callers` / `callees` / `impact` / `reverse-deps`, `--limit 20` on a multi-file `show` (a single explicit file is never capped), `--max-members 50` under the digest preset, `--budget 8000` on `context`, `-k 10` on `search`; `--depth` defaults are 1 (`callers`/`callees`), 2 (`impact`), 3 (`deps`), 12 (`trace`); `context` exposes no `--depth` — it walks to depth 2 internally under its `--budget`. When a cap is hit the header on **stdout** carries the true total (`# 113 caller(s) … (showing 3; raise --limit to see the rest)`) — for every section, including `callees` ancestor groups and `callers` type groups — and JSON carries `total` / `truncated`. `--limit` caps the display, not the walk: `callers` / `impact` / `reverse-deps` traverse the full cone to make the total exact, so `--depth` is what costs time.

**`--depth` truncates too, and differently.** A walk that ran out of depth and one that ran out of graph both end quietly, so every depth-bounded command reports which happened: JSON carries `frontier_truncated` — on `callers`, `callees`, `deps`, `reverse-deps`, and `trace` at the top level, and on each `impact` report — and text mode says so on stderr (`# note: --depth 2 reached with unexplored edges beyond it`). Read it before treating a result as exhaustive, because the `--limit` fields will not tell you: `truncated: false` with `frontier_truncated: true` means nothing was cut from the display and `total` itself counts only the part of the cone inside `--depth`. `callers` / `callees` / `impact` print no note at `--depth 1`, where "the callers have callers" is the norm rather than a qualification — the JSON flag is set regardless. On `trace`, `found: false` with `frontier_truncated: true` means "no path within `--depth`", not "no path".

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

1. **Unfamiliar directory** — `sb digest <dir>`: one-page map of every file's types and public methods. (`digest` is an alias for `sb map --preset digest`; it accepts every `map` flag.)
   ```bash
   sb digest src/
   sb digest src/ --glob '*.java' --max-members 8
   ```

2. **One file's shape** — `sb map <file>`: signatures with line ranges, no bodies (5–10× smaller than a full read). Three orthogonal axes: detail (`--detail names|signatures|full`), visibility (`--no-private`, `--no-fields`, `--no-docs`, …), and scope (`--glob`, `--max-members`). `--detail signatures --max-members N` is the middle ground when a directory-wide `map` is too big and `digest`'s bare names are too little.
   ```bash
   sb map src/file_filter.rs
   sb map src/ --detail signatures --max-members 8
   ```

3. **One symbol's source** — `sb show <target>... <Symbol>...`: suffix matching, multiple symbols at once. A target is a **file, a directory, or a quoted glob** — pass a directory when you know the symbol but not the file, and skip the search-then-read round trip. Multi-file answers carry a coverage header (`# 3 match(es) for 'greet' in 2 of 47 file(s) searched`) and cap rendered bodies at `--limit 20` with the true total still reported. An unquoted glob is recovered rather than misread: the shell expands `sb show src/*.cs Widget` before `sb` sees it, and the extra files are taken as targets, not as symbol names. An argument that reads as a path but isn't there (`src/typo.rs`) is rejected as a path, never searched for as a symbol; if a symbol genuinely shares its name with a parseable file *or a directory* (`tests`, `core`, `utils`), qualify it (`Type.method`) or put it last — the final argument stays a symbol, so only a colliding name in the middle of the list is read as a target. Explicitly-passed extensionless files fall back to shebang detection (`#!/usr/bin/env python3` → Python, `#!/usr/bin/env node` → TypeScript, etc.) — useful for CLI scripts in `bin/` or `~/.local/bin/`; directory walks skip extensionless files to keep the walk fast. For markdown the symbol is a heading or `frontmatter` (a leading `---` YAML block; `+++` TOML is not surfaced). Schema: `ast-bro.show.v2`.
   ```bash
   sb show src/main_helpers.rs parse_file_for_hook
   sb show Player.cs TakeDamage Heal Die
   sb show src/ TakeDamage              # don't know the file
   sb show 'src/**/*.cs' TakeDamage     # quote the glob
   sb show tasks/ frontmatter           # every card's YAML block, one call
   ```

4. **Who implements a type** — `sb implements <Type> <dir>`: AST-accurate (skip `grep`), transitive by default with `[via Parent]` tags. Add `--direct` for level-1 only.
   ```bash
   sb implements LanguageAdapter src/
   ```

5. **You don't know the file or symbol name** — `sb search "<query>"`: bare identifiers lean BM25 (`HandlerStack`), full sentences lean semantic ("how does login work"). First call builds the index at `.ast-bro/index/`.
   ```bash
   sb search "token-budgeted context"
   ```

6. **Code similar to a chunk you already have** — `sb find-related <file>:<line>`: pastes directly from `search` output (`path:start-end`).
   ```bash
   sb find-related src/context.rs:144
   ```

7. **The actual published API of a package** — `sb surface <dir>`: resolves `pub use` (Rust), `__all__` (Python), barrel files (TS/JS), `export` (Scala). `--tree` for hierarchy, `--include-chain` for re-export paths.
   ```bash
   sb surface .
   ```

8. **File-level deps** — `sb deps <file>`: forward BFS of what `<file>` imports. Footer lists unresolved imports tagged `[external]` so you see what the file tries to pull in from outside the project.
   ```bash
   sb deps src/impact.rs
   ```

9. **Who imports a file** — `sb reverse-deps <file>`: backward BFS, with `--tests` / `--exclude-tests` to filter by test-file heuristics. Blast radius before a refactor.
   ```bash
   sb reverse-deps src/impact.rs --exclude-tests
   ```

10. **Import cycles** — `sb cycles [<dir>]`: Tarjan SCC; exits non-zero when cycles exist (CI gate).
    ```bash
    sb cycles
    ```

11. **The full dependency graph** — `sb graph [<dir>] [--hide-external]`: external imports shown by default (tagged `[external]`); `--json` for `ast-bro.graph.v1`.
    ```bash
    sb graph . --json
    ```

12. **Who calls X / what X calls / how A reaches B** — symbol-level call graph (shares the dep-graph cache with steps 8–11).

    Edges are tagged `Exact` / `Inferred` / `Ambiguous` by a three-pass resolver (same-file → global symbol table → dep-graph disambiguation). **Ambiguous callers and unresolved/external callees are shown by default** (red/cyan) so you see the full set without re-running. Pass `--hide-ambiguous` (callers) or `--hide-external` (callees) to drop them when you want the cleaner bucket. `callers` additionally reports **unresolved call sites naming the target** (call chains whose receiver couldn't be typed) in a separate section — treat those as possible extra callers when costing a rename; the resolved count alone can undercount. Read that section's header before quoting its number: when several project symbols share the target's terminal name (`close`, `new`, `build`), the sites are evidence about all of them, the header says so, the rows are withheld, and JSON sets `unattributed_suppressed` with `unattributed_declarers`. When the name is specific the rows are listed strongest-first with the receiver as written (`recv=os`), capped at `min(--limit, 25)` — its own cap rather than the resolved list's leftover budget, so `--limit 5` yields at most 5 rows and raising `--limit` past 25 does not widen the sample.

    - `sb callers <Symbol>`: in-edges. Kind-aware: a function gets call-sites; a type gets implementors / constructions / ancestors.
      ```bash
      sb callers run_impact
      sb callers --tests run_impact         # test-file callers only
      sb callers LanguageAdapter            # implementors + constructions
      ```
    - `sb callees <Symbol>`: out-edges.
      ```bash
      sb callees run_impact --hide-external
      ```
    - `sb trace <FROM> <TO>`: shortest static call path, each hop's body inlined. No-path fallback to both endpoints + target file siblings.
      ```bash
      sb trace run_impact build_context
      ```

13. **Blast radius of touching a symbol** — `sb impact <Symbol>`: combines callers + callees + file `deps` + `reverse-deps` + test detection; for types, includes implementors and file-level reverse-deps. Four modes: `--mode all|deps|dependents|tests`. **Prefer `impact` over separate calls.** Schema: `ast-bro.impact.v1`.
    ```bash
    sb impact LanguageAdapter --mode tests
    sb impact run_impact --exclude-tests --depth 3
    ```

14. **Token-budgeted context** — `sb context <Symbol>`: target body + direct callees (bodies→signatures) + callers + transitive at depth 2 (signatures only). Types walk: target → implementors → methods → method callers. Flags `truncated` / `target_omitted` when budget runs short. **Prefer over chains of show + callers + callees.** Default `--budget 8000`. Schema: `ast-bro.context.v1`.
    ```bash
    sb context LanguageAdapter --budget 2000
    ```

15. **Find or rewrite by AST pattern** — `sb run -p '<pattern>' [-r '<rewrite>'] [--write] [--lang <lang>]`: metavariable patterns (`$VAR`, `$$$` for splats). `--write` mutates files — always dry-run first. A zero result states its own coverage (`no matches for "foo($$$)" (412 file(s) scanned)`, `files_scanned` in `--json`) — read that number before trusting the zero, since a stray `--glob` or `--lang` can shrink the scan to almost nothing. Zero *files* scanned is a different message and a different fix (`no source files processed`).
    ```bash
    sb run -p 'println!($$$)' --lang rust
    ```

16. **Compress a repetitive log/text file** — `sb squeeze <file> [from:to]`: for **logs/text, not code**. Replaces repeated timestamps/tags with short tags plus a reversible legend; falls back to raw when it wouldn't help. `--raw` skips compression. Schema: `ast-bro.squeeze.v1`.
    ```bash
    sb squeeze app.log
    ```

**Wrong-path recovery**: a path that isn't where you said it was is rejected with exit 2, and the stderr hint offers a repair when one is verifiable on disk — the same file name found elsewhere in the tree, or the quoted form when an unquoted path with spaces got split into several arguments. No hint means nothing on disk corroborated a guess, so re-read the path rather than retrying blind.

Path / argument expectations:
- `show` → expects a file, directory, or quoted glob, then one or more symbol names
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
