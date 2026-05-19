# Call graph

Two subcommands — `callers` and `callees` — and a per-repo persistent call-graph cache that rides inside the same on-disk file as the dep graph (`.ast-bro/graph/index.bin`). This page documents the internal architecture. For the user-facing surface see the README. For the file-level import graph that the call-graph resolver leans on for disambiguation see [deps.md](deps.md). For what gets walked see [file-filtering.md](file-filtering.md).

## What it answers

`callers` and `callees` are kind-aware. The same target string returns different results depending on whether the resolved symbol is a callable or a type:

| Target kind | `callers X` | `callees X` |
|---|---|---|
| function / method / constructor | call sites where `X` is invoked (in-edges) | call sites inside `X`'s body (out-edges) |
| class / struct / trait / interface / enum / record | downstream uses — implementors + constructions, including unit-struct receiver patterns (`Foo()`, `Foo::new()`, `Foo {}`, `new Foo()`) | upstream dependencies — ancestor types and the methods they declare, walked transitively via `--depth N` |

Both directions are inverses on their respective graphs. Diamond inheritance is handled. `--include-ambiguous` (callers) and `--external` (callees) surface the noisier results when explicitly requested.

Symbol forms accepted by both subcommands:

```
ast-bro callers TakeDamage
ast-bro callers Player.TakeDamage
ast-bro callers src/Player.cs:TakeDamage
ast-bro callers --file src/Player.cs --symbol TakeDamage
```

The first three are positional; the flag form exists for clients that prefer to avoid string-splitting on `:` / `.`.

## Pipeline

```
build_call_graph(root, deps):
  par_iter(files):                          # rayon
    pass = extract_file(file, lang)         # adapter walks tree-sitter tree:
                                            #   Declaration::calls   (raw CallSites)
                                            #   ParseResult::imports (ImportBindings)
    aggregate(pass) → Vec<FilePass>

  symbol_table = build_symbol_table(passes) # name → Vec<Qn> (terminal segment)

  for each raw edge:
    pass A — same-file:                     # bare name → qn via local
                                            #   defined_names + ImportBindings,
                                            #   resolved through suffix index
    pass B — global symbol table:           # single-match promotion;
                                            #   receiver-bearing calls deferred
                                            #   to pass C (avoids
                                            #   `builder.hidden()` false hits
                                            #   on global homonyms)
    pass C — dep-graph disambiguation:      # filter ambiguous candidates
                                            #   by the caller's transitive
                                            #   forward-dep closure

  → CallGraph {
       forward, callable_meta, types,
       symbol_table, type_by_name,
       implementors, stats,
     }

callers <Sym>:                              # kind-aware:
  if callable: reverse traversal of `forward`
  if type:     implementors ∪ constructions

callees <Sym>:                              # kind-aware:
  if callable: forward traversal of `forward`
  if type:     ancestor walk on `types[*].bases`, depth-limited
```

## Module layout

```
src/calls/
├── mod.rs          orchestrator: build_call_graph(root, &DepGraph) -> CallGraph
├── pass.rs         shared phase-1 IR: FilePass, RawEdge, qn_from, raw_to_edge,
│                   file_rel  (lifted out of build.rs to break the
│                   build ↔ resolve cycle — `ast-bro cycles src/calls/`
│                   was flagging it)
├── build.rs        per-file extraction + FilePass aggregation
├── resolve.rs      three-pass resolver:
│                     run = build_symbol_table → run_with_table
│                   (the split lets the incremental updater resolve a
│                   partial pass set against a precomputed global table)
├── graph.rs        Qn, CallEdge, CallTarget, Confidence, CallableMeta,
│                   TypeMeta, CallGraph, GraphStats
├── traverse.rs     forward / reverse BFS
├── render.rs       text + JSON renderers (palette matches core/surface)
├── cli.rs          run_callers / run_callees + type-aware paths
├── cli_helpers.rs  kind-aware target resolution (Callable vs Type)
└── mcp.rs          MCP server wrappers
```

## IR additions

Three new types in `src/core.rs` plus two new fields on the existing `Declaration` and `ParseResult`:

```rust
pub struct Declaration {
    // ... 18 existing fields ...
    pub calls: Vec<CallSite>,           // direct body only — nested decls own theirs
}

pub struct ParseResult {
    // ... 6 existing fields ...
    pub imports: Vec<ImportBinding>,    // local-name → module spec
}

pub struct CallSite {
    pub name: String,                   // bare name as written: `foo`, `bar`, `baz`
    pub receiver: Option<String>,       // `obj` for `obj.bar()`, `Foo` for `Foo::baz()`
    pub line: u32,
    pub kind: CallKind,
}

pub enum CallKind { Call, Construct, Macro, Super }

pub struct ImportBinding {
    pub local: String,
    pub module: String,
    pub line: u32,
}
```

`Declaration::calls` is attached to the enclosing declaration so the source/caller relationship is implicit (the caller is the declaration that owns the list). This gives correct nesting semantics for free in languages where one function is defined inside another (Python closures, JS arrows, Rust nested fns).

## Three-pass resolver

The single biggest source of grep noise is homonyms — `helper`, `init`, `parse`, `validate` exist in dozens of files. The three passes attack the problem in increasing order of cost:

### Pass A — same-file

For each `RawEdge` whose target is a bare name:

1. If the name is in the file's `defined_names` (collected from local `Declaration`s), promote to `Resolved(qn)` with `Confidence::Exact`.
2. Else if the name is in the file's `ParseResult::imports`, look up the module → file via the existing `src/deps/resolver/resolve.rs::resolve` and promote to `Resolved("<that file>::<name>")` with `Exact`.

### Pass B — global symbol table

`symbol_table: HashMap<String, Vec<Qn>>` indexes every project declaration's terminal name. For each remaining bare edge:

- 0 candidates → leave `Bare`.
- 1 candidate → promote to `Resolved` with `Exact`.
- N candidates → defer to pass C.

**Receiver gate.** Receiver-bearing calls (`obj.bar()`, `self.x()`, `super::foo()`) are deliberately **not** promoted by pass B. With a global single-match it would be too easy for `obj.hidden()` against a generic builder pattern to claim a wildly unrelated `hidden` definition somewhere else in the project. Receiver-bearing edges always go through pass C, which can confirm the relationship via the dep graph. The exact suppression list (`self`, `Self`, `crate`, `super`) lives in `src/calls/resolve.rs`.

### Pass C — dep-graph disambiguation

For each ambiguous edge with N candidates, load the dep half of the unified graph, compute the caller file's transitive forward-dep closure via `src/deps/traverse.rs::forward_bfs`, and filter the candidates to those whose file is in that closure.

- Exactly 1 survives → promote to `Resolved` with `Inferred`.
- More than 1 → keep all in `CallEdge::candidates` and tag `Ambiguous`. The renderer surfaces the count and one canonical choice; `--include-ambiguous` shows every candidate.

This mirrors `code-review-graph`'s `resolve_bare_call_targets` but uses the richer ast-bro dep graph instead of just IMPORTS_FROM edges.

### Confidence

Every `CallEdge` carries one of:

| Tag | Meaning |
|---|---|
| `Exact` | Pass A or single-candidate pass B promotion. |
| `Inferred` | Pass C narrowed multiple candidates to one via dep closure. |
| `Ambiguous` | Pass C left more than one candidate. |

Renderers colour the tag (green / yellow / red) and downstream tooling can filter at the precision level it needs.

## Per-language extraction

Every adapter that emits `Declaration`s now also emits `Declaration::calls`. Each adapter ships an `_extract_call_sites` (or `_walk_calls_in_body`) helper, called from inside its existing function/method/constructor walker. The walker bails on nested type or callable declarations so each `Declaration` owns exactly the calls textually inside its own body.

| Language | AST node kinds | `Construct` source | Notes |
|---|---|---|---|
| Rust       | `call_expression`, `macro_invocation`, `struct_expression` | struct literal | `super::` → `CallKind::Super` |
| Python     | `call` | class call (`Foo()`) | receiver from attribute access |
| TypeScript | `call_expression`, `new_expression` | `new T()` | also serves JavaScript |
| Java       | `method_invocation`, `object_creation_expression` | `new T()` | construct type stripped of generics + dotted prefix |
| C#         | `invocation_expression`, `object_creation_expression`, `implicit_object_creation_expression` | `new T()` | callee splitter handles `identifier`, `generic_name`, `member_access_expression`, `qualified_name`, `alias_qualified_name` |
| Kotlin     | `call_expression` | none (no `new`) | navigation_expression receiver via navigation_suffix |
| Scala      | `call_expression`, `instance_expression`, `generic_function` | `new T(...)` | receivers from `field_expression` |
| C++        | `call_expression`, `new_expression` | `new T()` | `qualified_identifier` / `scoped_identifier` split on `::`; `template_function` recurses into `name`; destructor names handled |
| Go         | `call_expression` | none (`new(T)` is just a regular call) | `selector_expression` for receivers |
| PHP        | `function_call_expression`, `member_call_expression`, `nullsafe_member_call_expression`, `scoped_call_expression`, `object_creation_expression` | `new T()` (last `\` segment of qualified type) | `\Foo\bar()` namespace-prefixed free function drops the namespace and emits the bare name with `receiver: None` so pass B promotes it; `self::` / `static::` / `parent::` keywords drop receiver (case-folded by tree-sitter-php's `keyword()` helper); dynamic `$func()` and `new $cls()` return `None` |
| Ruby       | `call` (with `method` / `receiver` fields) | `Foo.new` (constant receiver) | tree-sitter-ruby 0.23.1 unifies all call shapes — `obj.method()`, `obj.method "x"`, `puts "hello"`, `Greeter.shout` — under the single `call` kind. Walker deliberately does **not** bail on `block` / `do_block` (closures over the enclosing method's scope, not separate methods) |
| SQL        | n/a — no-op by design | — | — |
| Markdown   | n/a — no-op by design | — | — |

Languages still emitting empty `Declaration::calls`: **none.** JavaScript is served by the TypeScript adapter.

### Known per-language limitations

- **Ruby**: bare paren-less arg-less calls (`helper` with no parens, no args) parse as `identifier`, not `call`, so they aren't captured. Inherent to Ruby's grammar — can't disambiguate from a local variable reference at parse time.
- **Python**: no Jedi-style receiver-type inference. `obj.method()` where `obj`'s type is inferred from runtime flow falls through to pass B/C; the bare-name + import-disambiguation pass gets most callers. Adding Jedi would mean a Python runtime dep — same trade-off doesn't fit a Rust binary.
- **External-base ancestor walk** (`callees` on a type): capped at depth 1 when a base type doesn't resolve to a project file. Can't walk into types we can't see.

## Unified graph cache

The call graph does **not** get its own cache file. It rides inside a unified `UnifiedGraph { deps, calls: Option<CallGraph> }` at `.ast-bro/graph/index.bin`. The schema constant is `JSON_SCHEMA_GRAPH_INDEX = "ast-bro.graph-index.v1"`.

### Disk layout

```
.ast-bro/
├── .gitignore             # auto-written: "*"
├── graph/
│   ├── index.bin          # bincode UnifiedCacheFile { schema, graph, files }
│   └── lock               # fs2 advisory exclusive lock during writes
└── index/                 # see search.md
    └── ...
```

### Lazy promotion

`deps` / `reverse-deps` / `cycles` / `graph` populate only the `deps` half — `calls` stays `None`. The first `callers` / `callees` invocation triggers `promote_calls`, which builds the call graph from the existing dep graph (no re-walk of the project) and persists the upgraded `UnifiedCacheFile` back to disk. Users who never run `callers` / `callees` never pay the call-graph build cost.

### Process-wide sharing

`src/graph_cache/shared.rs` holds a `OnceLock<RwLock<HashMap<root, Arc<UnifiedGraph>>>>`. Within a single process — the `ast-bro mcp` long-running server is the case that matters — every `tools/call` reuses the same parsed `Arc<UnifiedGraph>`. Zero re-deserialisation, zero re-parse on warm hits. `Arc` swap on promotion means existing readers keep their pre-promotion view safely.

For one-shot CLI invocations the registry initialises, work happens, the process exits — equivalent to today.

### Schema migration

The legacy `.ast-bro/deps/graph.bin` (`deps-index.v1`) was deleted in the v2.1.0 cut. Users with an old cache hit the schema-mismatch branch in `cache::load_with_delta`, the loader returns `LoadOutcome::Missing`, and `load_or_build` rebuilds into `.ast-bro/graph/index.bin`. One-time, transparent.

The schema bump from `graph-index.v1` to `v2` happened mid-development to fix a silent bincode round-trip bug — `#[serde(skip_serializing_if)]` on `DepEdge::local_name` / `raw_path` and `CallEdge::receiver` / `candidates` corrupts bincode's positional encoding (a skipped Option/Vec field shifts every byte that follows). The skip annotations were a JSON-output ergonomics holdover that never applied to the cache; binary positional formats *require* every field to be encoded. Removed the annotations, bumped the schema, and any v1 cache files (which were all corrupt and silently re-cold-built every invocation) get a clean rebuild.

### Per-file invalidation

`load_with_delta` returns a three-armed `LoadOutcome`:

- `Fresh(graph)` — cache + no diff.
- `Stale { graph, delta, prev_records }` — cache + per-file diff to apply.
- `Missing` — schema mismatch, IO error, or decode error → caller rebuilds.

`load_or_build` drives the patch flow: on `Stale`, hand the delta to two sibling patchers in `src/graph_cache/delta.rs`. On patch failure, fall back to full `build_and_save` so a query never sees a half-applied state.

**`apply_delta_to_deps`:**

1. Drop entries for removed + modified files.
2. Re-extract + re-resolve only added + modified files (parallel via rayon, same loop the full build uses).
3. Rebuild the suffix index once — file membership changed.
4. Re-aggregate stats.

**`apply_delta_to_calls`** (more careful since the call graph has cross-file edges):

1. Drop forward entries originating in changed files.
2. Drop changed-file qns from `callable_meta` / `types` / `symbol_table` / `type_by_name` / `implementors`. Prune empty buckets so callers don't see ghost keys.
3. Re-extract changed files via `pass::extract_file`.
4. Splice new qns into the live indices **before** resolving — the resolver's pass A/B for a new edge needs to see qns just added by the same delta.
5. Resolve only the new passes via `resolve::run_with_table` (the split-out resolver entrypoint that takes a prebuilt symbol table instead of constructing one).
6. Validate every `Resolved` edge against the post-update qn set. Edges whose target qn no longer exists (deleted or renamed) get demoted to `Bare` with the original callee name preserved. Edges that *kept* their target — the common case for a modify without a rename — keep their original `Exact` confidence; we deliberately don't blanket-demote everything pointing into changed files because that would burn the high-trust tags for the 99% case.
7. Bare-name re-resolution: for every `Bare` edge, look up its name in the (now updated) symbol table; promote single-match, no-receiver hits to `Resolved/Inferred`. Mirrors the receiver suppression in pass B exactly so cold and warm builds produce identical edge resolutions. Picks up two cases the partial path would otherwise miss: edges demoted in step 6 whose target moved to a different file, and pre-existing `Bare` edges in unchanged files that finally have a target because a *new* file in this delta defines it.
8. Rebuild reverse adjacency + recompute stats. Both are derived; rebuilding fresh is cheaper than incremental maintenance.

### Cost numbers (ast-bro against itself, release build)

| operation                       | before    | after  |
|---------------------------------|-----------|--------|
| deps, cold                      | 2.85 s    | 2.85 s |
| deps, warm (no edits)           | 2.85 s ⚠️ | 8 ms   |
| deps, warm + 1 file modified    | 2.85 s ⚠️ | 22 ms  |
| callers, cold                   | 125 ms    | 125 ms |
| callers, warm (no edits)        | 125 ms ⚠️ | 11 ms  |
| callers, warm + 1 file modified | 125 ms ⚠️ | ~45 ms |

⚠️ = pre-fix "warm" was actually cold every time due to the silent decode bug. The warm-no-edits row is the load-from-cache happy path that never happened until the schema-v2 cut; the warm-with-edits row is the new per-file patch path replacing full rebuild.

For `ast-bro mcp`, where the in-process `Arc<UnifiedGraph>` already shared parsed state across `tools/call`s, the schema-v2 fix recovers the *first* call of each session — which previously reloaded from scratch instead of deserialising the persisted cache — and makes the rest of the session correctly reflect file edits without forcing `--rebuild`.

### Concurrency

Same pattern as the search index and the legacy deps cache: `fs2` advisory exclusive lock at `.ast-bro/graph/lock` during writes; atomic `.tmp` + rename so a SIGKILL mid-write leaves the previous cache intact. Reads use the in-memory `Arc` and don't touch the lock.

## Known gaps

- Pass C is not re-run for surviving `Bare` edges in the partial-update path — only the pass-B-equivalent single-match promotion runs. If a delta introduces a new file with one of multiple homonyms and pass C would have disambiguated, the current path lands it as `Ambiguous`. `--rebuild` recovers; in practice this is the long tail of the long tail.
- The suffix index gets a fresh full walk on every delta. The walk is the cheap part of a cold build (~hundreds of ms even on big repos); the per-file extract+resolve is the expensive part, which we now skip for unchanged files. A surgical suffix-index update would be more code than it saves.

## Adding a new language

If you've already added a `Declaration`-emitting adapter (see [architecture.md](architecture.md)), call-site extraction is one helper and one wiring step:

1. Add an `_extract_call_sites` (or `_walk_calls_in_body`) function in your `src/adapters/<lang>.rs` that walks the function body, bails on nested type/callable declarations, and emits one `CallSite` per recognised AST node kind.
2. Call it from inside each `_function_to_decl` / `_method_to_decl` / `_class_to_decl` builder so the populated `calls` ride along on the returned `Declaration`.
3. If the language has its own import syntax not already covered by `src/deps/extract.rs` and `src/surface/imports.rs`, populate `ParseResult::imports` so pass A can resolve same-file `use` / `import` / `using` bindings.
4. Add an end-to-end test in `tests/calls_e2e.rs` mirroring the existing per-language pairs (`<lang>_callers_finds_intra_file_caller` + `<lang>_callees_lists_construct_and_invocation`). Intra-file scope keeps the failure modes narrow — it exercises pass A without depending on the per-language import resolver.

For languages where AST kind names are case-folded by tree-sitter (PHP's late-binding keywords, Ruby's command unification), pin the assumption with a regression test so future grammar drift surfaces as a test failure instead of silently dropping edges.
