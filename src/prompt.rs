pub const AGENT_PROMPT: &str = r#"## Use `ast-bro` to explore the code

Usage: ast-bro <COMMAND> [OPTIONS]

Commands:
  map           Map files or directories — signatures with line ranges, no method bodies
  show          Extract source of a symbol
  digest        One-page module map
  implements    Find subclasses / implementations
  callers       Who calls this function/method, or constructs/implements this type
  callees       What does this function/method call, or which ancestors does this type extend
  run           AST-aware search and rewrite with metavariable patterns
  prompt        Print the agent prompt snippet
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
  index         Build, refresh, or inspect the per-repo search index

Each command has `--json` for stable schemas and `--compact` for single-line JSON. Pass an unknown flag or no command and the help text prints automatically — there's no "default" command, every operation is explicit.

Read structure with `ast-bro` before opening full contents. Pull method bodies only once you know which ones you need.

Stop at the step that answers the question:

1. **Unfamiliar directory** — `ast-bro digest <dir>`: one-page map of every file's types and public methods.

2. **One file's shape** — `ast-bro map <file>`: signatures with line ranges, no bodies (5–10× smaller than a full read).

3. **One method, class, or markdown section** — `ast-bro show <file> <Symbol>`. Suffix matching: `TakeDamage`, or `Player.TakeDamage` when ambiguous. Multiple at once: `ast-bro show Player TakeDamage Heal Die`. For markdown, the symbol is the heading text.

4. **Who implements/extends a type** — `ast-bro implements <Type> <dir>`: AST-accurate (skip `grep`), transitive by default with `[via Parent]` tags on indirect matches. Add `--direct` for level-1 only.

5. **You don't know the file or symbol name** — `ast-bro search "<query>"`: hybrid BM25 + dense semantic search over the repo. Use bare identifiers for symbol lookup (`HandlerStack`, `Sinatra::Base` — auto-leans BM25), full sentences for behaviour search ("how does login work" — auto-balances semantic + BM25). First call builds an index at `.ast-bro/index/` (~seconds for typical repos); subsequent calls reuse it and refresh incrementally.

6. **Find code similar to a chunk you already have** — `ast-bro find-related <file>:<line>`: returns chunks semantically similar to the one containing that line. Useful for "what else looks like this?" or finding alternative implementations. Pastes directly from `search` output (which prints results as `path:start-end`).

7. **The actual published API of a package** — `ast-bro surface <dir>`: resolves `pub use` re-exports (Rust) and `__all__` (Python) so you see exactly what a downstream user can reach, not the union of every `pub`/non-underscore item. Falls back to visibility-filtered output for Java/C#/Go/Kotlin (no real re-export concept). Use `--tree` for hierarchy, `--include-chain` to see the re-export path each entry took.

8. **Who calls / what does this call?** — symbol-level call graph, AST-accurate, no `grep` noise. Backed by the unified graph cache at `.ast-bro/graph/index.bin` (built lazily, per-file invalidated, shared across MCP `tools/call`s).
   - `ast-bro callers <Symbol> [<dir>]`: in-edges. For a function/method: the call sites that invoke it. For a type: implementors and constructions (covers `Foo()`, `new Foo()`, `Foo {}`, `Foo::new()`).
   - `ast-bro callees <Symbol> [<dir>]`: out-edges. For a function/method: what it calls. For a type: ancestor types and the methods they declare (use `--depth N` for transitive).
   - Symbol forms: bare suffix (`TakeDamage`), dotted (`Player.TakeDamage`), file-scoped (`src/Player.cs:TakeDamage`), or flag form (`--file src/Player.cs --symbol TakeDamage`).
   - Edges carry `Exact` / `Inferred` / `Ambiguous` confidence. Add `--include-ambiguous` (callers) or `--external` (callees) when you want the noisier bucket.

9. **What does this file pull in / who depends on it / are there cycles?** — file-level dep-graph commands. Same unified cache as `callers`/`callees`.
   - `ast-bro deps <file> [--depth N]`: forward — what `<file>` imports (transitively).
   - `ast-bro reverse-deps <file> [--depth N]`: backward — who imports `<file>`. Use before refactoring to know the blast radius.
   - `ast-bro cycles [<dir>]`: import cycles via Tarjan SCC. Exits non-zero when cycles exist (CI gate).
   - `ast-bro graph [<dir>]`: emit the full graph (text). Add `--json` for `ast-bro.graph.v1`.

10. **AST-aware pattern search and rewrite** — `ast-bro run` uses metavariable patterns ($FUNC, $ARG, $$$BODY) for structural code matching and transformation.
    - `ast-bro run -p '$FUNC($$$)' -l rust`: find all function calls in Rust files.
    - `ast-bro run -p 'foo($A)' -r 'bar($A)' -l py`: dry-run rewrite foo→bar in Python.
    - `ast-bro run -p 'foo($A)' -r 'bar($A)' --write`: apply the rewrite to disk.

**Path type expectations:**
- `deps`, `reverse-deps` → expect a **file** path
- `graph`, `cycles` → expect a **directory** (repo root)
- `callers`, `callees` → symbol first, optional **directory** (defaults to `.`)
- `run` → optional **file** or **directory** paths (defaults to `.`)
"#;

/// YAML frontmatter for the Claude Code skill file. Concatenated with
/// `AGENT_PROMPT` at install time to produce `SKILL.md`. Description is
/// what Claude Code uses for skill discovery / routing — keep it
/// action-oriented and accurate.
pub const SKILL_FRONTMATTER: &str = "---\nname: ast-bro\ndescription: Fast AST-based code-navigation toolkit. Use to explore unfamiliar directories, list a file's symbols without reading bodies, jump to a specific function/class, find subclasses or implementations, search a repo by symbol or behaviour, extract a package's true public API, analyze file-level dependencies, or perform AST-aware pattern search and rewrite. Prefer this over reading whole files when you only need shape.\nuser-invocable: true\n---\n\n";

pub fn agent_skill_md() -> String {
    format!("{}{}", SKILL_FRONTMATTER, AGENT_PROMPT)
}

/// YAML frontmatter written at the top of `.claude/agents/Explore.md` on a
/// fresh install. The marker block (and AGENT_PROMPT body) is appended after
/// it, so Claude Code sees valid frontmatter at file offset 0.
pub const EXPLORE_FRONTMATTER: &str = "---\nname: Explore\ndescription: Fast read-only search agent for locating code. Use it to find files by pattern, grep for symbols or keywords, or answer \"where is X defined / which files reference Y.\" Do NOT use it for code review, design-doc auditing, cross-file consistency checks, or open-ended analysis — it reads excerpts rather than whole files and will miss content past its read window. When calling, specify search breadth: \"quick\" for a single targeted lookup, \"medium\" for moderate exploration, or \"very thorough\" to search across multiple locations and naming conventions.\n---\n";
