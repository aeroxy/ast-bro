# Context packing

`context` takes a symbol and a token budget and packs "everything an LLM needs
to understand this symbol" into a single structured response — replacing
chains of 4–5 `show` / `callers` / `callees` / `reverse-deps` calls with one
command.

## What it answers

Given a target symbol and a `--budget N`, return the most relevant surrounding
code within that budget:

- The target itself (full body if it fits).
- Direct callees (full bodies while budget permits).
- Direct callers (signatures).
- Transitive callees and callers (signatures only, depth 2).

For **type targets** (structs, classes, traits, enums, interfaces) the walk
is reshaped to fit the use case:

- The type definition (full body if it fits).
- Implementors (full bodies while budget permits — labelled `implementor (body)`).
- Methods of the type (full bodies — labelled `method (body)`).
- Dependents — callers of any of the type's methods (signatures — `dependent (signature)`).

Budget allocation degrades gracefully: once the budget runs out, remaining
entries are silently truncated and the report flags `truncated: true`. If even
the target's body won't fit, it is replaced by its signature and the report
flags `target_omitted: true`. If the target can't be resolved to source (e.g.
external trait), the report carries `body_unavailable: true` and the best
available fallback.

## CLI surface

```bash
ast-bro context <symbol>                     # 8000-token default budget
ast-bro context <symbol> --budget 2000       # tight pack
ast-bro context <symbol> --budget 32000      # big context window
ast-bro context <symbol> --json              # schema: ast-bro.context.v1 (always wrapped)
```

Target form matches `callers`/`callees`/`impact`: bare name, dotted path, or
file-scoped `<file>:<name>` when disambiguation is needed.

## Output

Text mode groups entries under labelled headers (`target`, `direct dependency
(body)`, `implementor (body)`, `dependent (signature)`, ...), each entry
preceded by its `qn`, file, line, kind, and per-entry token count.

```text
context for src/adapters/base.rs::LanguageAdapter (budget: 4000 tokens, used: 167 tokens)

  target:
    trait LanguageAdapter (src/adapters/base.rs:5, ~63 tokens)
      pub trait LanguageAdapter {
          fn language_name(&self) -> &'static str;
          ...
      }

  implementor (body):
    struct CppAdapter (src/adapters/cpp.rs:6, ~6 tokens)
    struct PythonAdapter (src/adapters/python.rs:6, ~7 tokens)
    ...

  method (body):
    method language_name (src/adapters/base.rs:6, ~10 tokens)
    method parse (src/adapters/base.rs:9, ~23 tokens)

  dependent (signature):
    function parse_file_for_hook (src/main_helpers.rs:39, ~93 tokens)
```

Budget-exhausted runs print two advisory lines at the top:

```text
# note: target body omitted to fit budget (show only signature)
# warning: budget exhausted before all transitive context was included
```

JSON mode returns `ast-bro.context.v1` wrapped as `{"schema": "...", "report": {...}}`
so MCP and CLI consumers see the same structure. Every entry carries `tokens`
so the agent can decide whether to re-fetch at a larger budget.

## Internals (src/context.rs)

`build_context(c, calls, root, opts)`:

1. Resolves the target via `resolve_qn_source` (parses file, calls `find_symbols`
   to get body + first-line signature, falls back to `callable_meta` / `types`
   when parse fails).
2. Walks four buckets in priority order:
   - For **callables**: target body → direct callees → direct callers → transitive callees (depth 2) → transitive callers (depth 2).
   - For **types**: type body → implementors → methods (any callable whose qn is `<type_qn>::*`) → callers of each method.
3. Each entry is budget-checked before being pushed — body is added if it
   fits, otherwise the signature is tried, otherwise the bucket is skipped
   and `truncated` is set.
4. An internal `seen_qns: HashSet<String>` prevents the same qn appearing in
   multiple sections (a callee reachable both directly and via a transitive
   path is counted once).

Token estimation uses `bytes.div_ceil(4)` — a rough ~4 bytes per token
heuristic that matches the calibration across Rust / Python / TypeScript
source files. The budget is a hard ceiling; never exceeded, always flagged
when hit.

## MCP integration

MCP consumers get the same payload through the `context` tool — arguments:
`target`, optional `path`, optional `budget` (default 8000), optional `json`
(default false inside MCP — text unless `json: true`). The response is wrapped in the same `{"schema":
"ast-bro.context.v1", "report": ...}` envelope the CLI emits with `--json`,
so downstream tooling that parses the JSON doesn't need a code path split
between MCP and CLI output.
