# Impact analysis

`impact` is the "one command to size up the blast radius of touching symbol X".
It wraps `callers`, `callees`, file-level `reverse-deps`, file-level `deps`,
and test-file detection into a single structured report.

## What it answers

- **What would break if I change this?** — callers + file reverse-deps + file deps, grouped by section.
- **Which tests exercise this?** — filters the callers list to test files (using the same test-file heuristics as `--tests` / `--exclude-tests`).
- **What does it touch internally?** — callees + file deps.
- **How far does the change propagate?** — transitive callers at configurable depth.

Works for both **callable targets** (functions, methods) and **type targets** (structs, classes, traits, enums, interfaces). Type targets additionally show:

- **Implementors / constructors** in the "called by" section.
- **File-level imports and reverse-deps** of the file containing the type (same as callables — types also have blast radius at the file level).

## CLI surface

```bash
ast-bro impact <symbol>                          # default mode: all sections
ast-bro impact <symbol> --mode deps              # only deps sections
ast-bro impact <symbol> --mode dependents        # only dependents sections
ast-bro impact <symbol> --mode tests             # only affected tests section
ast-bro impact <symbol> --depth 3                # deeper transitive window
ast-bro impact <symbol> --exclude-tests          # drop test files from every section
ast-bro impact <symbol> --tests                  # keep only test files in every section
ast-bro impact <symbol> --hide-ambiguous         # drop Ambiguous call edges
```

## Output

Text mode is rendered section-by-section. Empty sections are dropped by the
renderer, so `--mode tests` on a symbol with no test callers is a clean
single-line target header instead of a noisy report.

```text
⊕ function handleRequest (src/handler.ts::42)

  → callees (6)
    → call db.query  (src/db.ts)
    → call logger.info  (src/logger.ts)
    ...

  → imports (file, 3)
    → use src/db.ts
    → use src/logger.ts
    → use src/auth.ts

  ← called by (4)
    → function route  (src/router.ts)
    → function handler  (src/server.ts)
    ...

  ← imported by (file, 2)
    → bare src/router.ts
    → bare src/server.ts

  ! 3 entities transitively affected (depth 2)
    → function bootstrap  (src/main.ts)
    → function start  (src/app.ts)
    → function listen  (src/server.ts)

  affected tests (2)
    → function test_handle_request  (tests/handler.test.ts)
    → function test_routing  (tests/router.test.ts)
```

JSON schema is `ast-bro.impact.v1`. The envelope carries per-section entry arrays
with `qn`, `file`, `line`, `kind`, `confidence`, and optional `depth`. The report
also surfaces `transitive_count` and `test_count` for downstream filters.

## Internals (src/impact.rs)

`run_impact` resolves the target to one or more `ResolvedTarget`s (kind-aware —
callable or type), then for each candidate builds an `ImpactReport` by calling
these helper functions in priority order:

1. `build_callees_section(c, calls, opts)` — calls one-hop `callees_one_hop`.
2. `build_file_deps_section(file, deps, root)` — `dep_traverse::forward` depth 1.
3. `build_callers_section(c, calls, opts, root)` — `traverse::callers` depth 1,
   plus (for types) implementors via `calls.implementors`.
4. `build_file_reverse_deps_section(file, deps, root, opts)` — `dep_traverse::reverse`
   depth 1, honouring `--tests` / `--exclude-tests`.
5. Transitive collection — re-walks `traverse::callers` at `opts.depth` and
   buckets entries by `h.depth > 1`.
6. Optional `affected tests` section — same callers walk, filtered by
   `is_test_file(h.file, root)`.

Mode gates determine which subset renders. `--exclude-tests` is applied both as
a post-filter on the sections' entries and as a hint that short-circuits the
test-only section.

`build_callers_section` always runs regardless of `SymbolKind` — the `reverse`
graph is callable-keyed, so type dependents appear via their implementors'
callers (the "calls implementor's method" path), not via the type qn directly.
