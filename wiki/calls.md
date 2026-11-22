# Call graph

Three subcommands — `callers`, `callees`, and `trace` — and a per-repo persistent call-graph cache that rides inside the same on-disk file as the dep graph. This page documents the internal architecture. See the README for the user-facing surface. The file-level import graph that the call-graph resolver leans on for disambiguation is covered in [deps.md](deps.md). For what gets walked see [file-filtering.md](file-filtering.md).

## What it answers

`callers` and `callees` are kind-aware. The same target string returns different results depending on whether the resolved symbol is a callable or a type:

| Target kind | `callers X` | `callees X` |
|---|---|---|
| function / method / constructor | call sites where `X` is invoked (in-edges) | call sites inside `X`'s body (out-edges) |
| class / struct / trait / interface / enum / record | downstream uses — implementors + constructions, including unit-struct receiver patterns (`Foo()`, `Foo::new()`, `Foo {}`, `new Foo()`) | upstream dependencies — ancestor types and the methods they declare, walked transitively via `--depth N` |

Both directions are inverses on their respective graphs. Diamond inheritance is handled. Ambiguous callers and unresolved/external callees are shown by default; `--hide-ambiguous` (callers) and `--hide-external` (callees) drop them. `callers` additionally scans for bare edges that *name* the target but were never attributed to a node (`traverse::unattributed_callers`) and lists them as possible callers — without this, a chain like `connection.getCtx().getPrefs().forDate()` whose receiver can't be typed would make `callers forDate` read `0` with nothing to suggest otherwise (issue #31).

That section is only as good as the name it keys on, so three things bound it (all in `traverse.rs` / `render.rs`):

- **Receiver check** — a receiver naming a *project type* other than the target's own enclosing type drops the edge. This is pass A's rule applied one layer later; receivers that are locals, parameters, or external types are kept, because rejecting on an untypeable name would drop the real `connection.close()` sites along with the noise. An Elixir module receiver is compared as the full module name (`MyApp.Repo`), since Elixir types are keyed by it; its last segment alone would match no type and keep the edge.
- **Discrimination gate** — `name_declarers` counts how many project symbols declare the target's terminal name. Past `render::MAX_DECLARERS_TO_LIST` (3) the rows are withheld in favour of a one-line count that states the reason, because the sites are then evidence about every declarer equally: `callers CliError.new` in this repo would otherwise print a sample of the 1020 `Vec::new()` / `String::new()` call sites, and `callers PgConnection.close` on pgjdbc a sample of 1124 (39 symbols declare `close`). JSON carries `unattributed_declarers` and `unattributed_suppressed` so a consumer can't read the total as a caller count either.
- **Own sample cap, strongest first** — the section takes `min(--limit, 25)` rows rather than whatever budget the resolved hits left over (a badly-resolved target would otherwise earn *more* screen), ordered by ambiguity breadth ascending, and each row shows the receiver as written (`recv=os`) — the one triage signal available without type inference.

The remaining work for the fully-resolved case is recording declared types of locals and parameters during the adapter walk (`ResultSet rs = …`, `void f(PgConnection c)`), which is a field, not type inference.

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
    scope receiver:                         # `Foo::Bar.baz()`: only that
                                            #   scope's `baz`, else external
    pass A — same-file:                     # bare name → qn via local
                                            #   defined_names + ImportBindings,
                                            #   resolved through suffix index
    pass B — global symbol table:           # single-match promotion among
                                            #   the caller's language family;
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
               + unattributed bare edges naming <Sym>
  if type:     implementors ∪ constructions

callees <Sym>:                              # kind-aware:
  if callable: forward traversal of `forward`
  if type:     ancestor walk on `types[*].bases`, depth-limited
```

## Tracing call paths (`trace`)

`trace <FROM> <TO>` answers "how does `<from>` reach `<to>`?" — the chain of
calls between two symbols, with each hop's source body inlined so a flow
question (request→handler, update→render) is answered in **one** call instead
of the agent manually chaining `callees`.

- **Search**: a multi-source / multi-target BFS over `forward` (the callees
  direction), following only `Resolved` edges, from any qn matching `<from>`
  to any qn matching `<to>`. Returns the shortest path; `--depth N` caps the
  hop count (default 12). Targets are suffix-matched exactly like `callers`
  (`run`, `Type.method`, `src/f.rs:name`).
- **Bodies inlined**: each node on the path is rendered with its source,
  extracted via the same `core::find_symbols` path `show` uses (parsed files
  are cached, so a path through one file parses it once). Output is
  size-capped — `MAX_BODY_CHARS` per symbol, `MAX_TOTAL_CHARS` total — beyond
  which remaining hops are listed header-only.
- **Graceful failure**: when no static path exists — the chain broke at a
  dynamic-dispatch / framework boundary (a callback, trait object, or route
  handler ast-bro's precise resolver won't invent an edge for) — the response
  still inlines both endpoints plus the target file's sibling callables, so
  the agent has somewhere to look. A found path and a resolved-but-no-path are
  both exit 0 (the output is the answer); only an unresolved `<from>` / `<to>`
  is exit 2.
- **JSON** (`--json`): `ast-bro.trace.v1` — `{from, to, found, frontier_truncated,
  hop_count, hops: [{qn, file, line, kind, via, via_line, confidence, body}]}`, or
  `{found: false, frontier_truncated, endpoints, siblings}` on the no-path branch.
- **`found: false` is two findings, not one.** The BFS stops either because it
  exhausted the reachable graph or because it hit `--depth` (default 12) with
  callees still to follow, and only the first justifies blaming dynamic
  dispatch. `frontier_truncated` separates them: the text output says "no
  static call path found within `--depth N`" and names the flag to raise, and
  the JSON field is present on both branches so a consumer reads one thing
  either way. Only an edge into a node the search never visited counts —
  unresolved and external callees are where the static graph ends, so raising
  `--depth` would not walk through them (issue #32).

Lives in `src/calls/trace.rs` — a thin layer over the same `CallGraph.forward`
map `callees` walks; it needs no new IR or cache state.

## Module layout

```
src/calls/
├── mod.rs          orchestrator: build_call_graph(root, &DepGraph) -> CallGraph
├── pass.rs         shared phase-1 IR: FilePass, RawEdge, qn_from, raw_to_edge,
│                   file_rel  (lifted out of build.rs to break the
│                   build ↔ resolve cycle — `ast-bro cycles src/calls/`
│                   was flagging it)
├── build.rs        per-file extraction + FilePass aggregation
├── elixir.rs       Elixir calls without a module receiver, by module
├── resolve.rs      three-pass resolver:
│                     run = build_symbol_table → run_with_table
│                   + resolve_scoped for scope receivers
│                   (the split lets the incremental updater resolve a
│                   partial pass set against a precomputed global table)
├── graph.rs        Qn, CallEdge, CallTarget, Confidence, CallableMeta,
│                   TypeMeta, CallGraph, GraphStats
├── traverse.rs     forward / reverse BFS
├── trace.rs        shortest-path BFS between two symbols, bodies inlined
├── render.rs       text + JSON renderers (palette matches core/surface)
├── cli.rs          run_callers / run_callees / run_trace + type-aware paths
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
    pub receiver_scope: ReceiverScope,  // receiver is a module/class, not a value
}

pub enum CallKind { Call, Construct, Macro, Super }

pub enum ReceiverScope { None, Lexical, Absolute }

pub struct ImportBinding {
    pub local: String,
    pub module: String,
    pub line: u32,
}
```

`Declaration::calls` is attached to the enclosing declaration so the source/caller relationship is implicit (the caller is the declaration that owns the list). This gives correct nesting semantics for free in languages where one function is defined inside another (Python closures, JS arrows, Rust nested fns).

## Three-pass resolver

The single biggest source of grep noise is homonyms — `helper`, `init`, `parse`, `validate` exist in dozens of files. The three passes attack the problem in increasing order of cost. (Elixir names its call targets statically, so its edges skip them: a module receiver is a [scope receiver](#scope-receivers), and `elixir.rs` looks the other calls up by module. Its types and their bases are indexed by full module name, since `Shop.Repo` and `Ecto.Repo` are two modules; other languages index them by last segment, #77. A one-segment Elixir module and another language's type can share that key, so an Elixir type pairs only with Elixir code as an implementor, ancestor or construction.)

### Pass A — same-file

For each `RawEdge` whose target is a bare name:

1. If the name is in the file's `defined_names` (collected from local `Declaration`s), promote to `Resolved(qn)` with `Confidence::Exact` — **but only when the receiver still points at the enclosing scope** (no receiver, or a self-like keyword: `self`/`Self`/`crate`/`super`/`this`/`$this` — PHP's `self::`/`static::`/`parent::` never reach the resolver as receivers; the adapter normalizes them to `None`, matching case-insensitively since PHP keywords are — `SELF::`/`Static::` too — and the bare words are deliberately absent from the list because `parent`/`static` are common user variable names). An explicit receiver (`connection.getCtx()`) means the target lives on another object, so a same-file homonym must not claim it (issue #31 — the mis-bind used to be tagged `Exact` and silently broke the rest of the chain). Two refinements:
   - Self-like binding prefers the sibling under the caller's own scope: `self.shared()` inside `Greeter::caller` binds `Greeter::shared`, not another same-file class's `shared`.
   - A type-qualified call on a local type (`Foo::bar()`, `Foo.bar()`) still binds `Exact`, but only to a local qn actually scoped as `::Foo::bar`.
   - Rust self-relative prefixes each pick their own anchor, always by anchored equality: `self::P` starts at the caller's enclosing scope and walks up, each `super::` skips one more level first, and `crate::P` anchors at the crate root — which means the arm fires **only when the caller's file *is* `lib.rs` / `main.rs` / a `bin/` target** (`is_crate_root`). In any other file the caller's file segment is not the crate root, and anchoring there would bind `crate::inner::Foo::method()` to a same-file `mod inner` decoy while the real `crate::inner` lives in another file — i.e. the arm would fire exactly where it is wrong. Off the crate root the edge falls through to pass B/C. (Pass C can still pick a same-file decoy over the crate-root module when both are named alike; that is a resolver-level limitation, tagged `Inferred` rather than `Exact`, and wants real module-path resolution to fix.)
   - Everything else falls through to pass B/C, where the dep graph either confirms the relationship (`Inferred`) or the edge stays honestly `Ambiguous`.
2. Else if the name is in the file's `ParseResult::imports` **and the receiver is self-like** (the same gate — no receiver or a self/scope keyword), look up the module → file via the existing `src/deps/resolver/resolve.rs::resolve` and promote to `Resolved("<that file>::<name>")` with `Exact`.

### Pass B — global symbol table

`symbol_table: HashMap<String, Vec<Qn>>` indexes every project declaration's terminal name. For each remaining bare edge, counting only the candidates its language can call (below):

- 0 candidates → leave `Bare`.
- 1 candidate → promote to `Resolved` with `Exact`.
- N candidates → defer to pass C.

**Language gate.** A candidate counts only if the caller's file can call into its language: TypeScript, TSX and JavaScript are one family, Java, Kotlin and Scala another, and every other language is on its own (`can_reach` in `src/calls/resolve.rs`). A Python call never reaches a Ruby method of the same name, however unique that name is in the project, and with no candidate left the edge stays `Bare`. Pass C sees only the candidates that passed, and the unresolved call sites `callers` lists under a target go through the same gate, as does the count of same-named declarers that decides whether to list them (issue #96). Foreign-function bindings do cross languages (a PyO3 function called from Python, JNI, a Node addon), and a call into one now stays `Bare` where a unique name used to bind it here. With fewer candidates, pass B also promotes calls that another language's homonym used to keep ambiguous, and when such a call has no receiver and its one candidate is a method of an unrelated class, the edge is wrong (#109).

**Receiver gate.** Receiver-bearing calls (`obj.bar()`, `self.x()`, `super::foo()`) are deliberately **not** promoted by pass B. With a global single-match it would be too easy for `obj.hidden()` against a generic builder pattern to claim a wildly unrelated `hidden` definition somewhere else in the project. Receiver-bearing edges always go through pass C, which can confirm the relationship via the dep graph. The exact suppression list (`self`, `Self`, `crate`, `super`, `this`, `$this` — see `receiver_is_self_like`) lives in `src/calls/resolve.rs`.

### Pass C — dep-graph disambiguation

For each ambiguous edge with N candidates, load the dep half of the unified graph, compute the caller file's transitive forward-dep closure via `src/deps/traverse.rs::forward`, and filter the candidates to those whose file is in that closure.

- Exactly 1 survives → promote to `Resolved` with `Inferred`.
- More than 1 → keep all in `CallEdge::candidates` and tag `Ambiguous`. The renderer surfaces the count and one canonical choice; ambiguous edges are shown by default (`--hide-ambiguous` drops them).

This mirrors `code-review-graph`'s `resolve_bare_call_targets` but uses the richer ast-bro dep graph instead of just IMPORTS_FROM edges.

### Scope receivers

An adapter sets `CallSite::receiver_scope` when its language makes clear that the receiver is a scope — a module, class or namespace — and not a value. Today that is Ruby's CamelCase constant paths, and Elixir's module receivers, which the adapter expands to the full module; a Ruby all-caps constant may hold a value (`DEFAULT_PARSER.parse`), so it is left out. Passes B and C match by name and ignore the receiver, so they used to bind `File.join()` to whatever project `join` the caller's file can reach. A scope-receiver edge therefore takes this rule instead of passes A and B, whether its scope is in the caller's file or another; pass C only narrows candidates that are already in the scope.

An `Absolute` receiver (`::File.join`) names its path from the top level. A `Lexical` one (`Shop::Pricing.price`) is looked up from the caller's scope outward, one `::` segment of the caller's qualified name at a time, as Ruby and C++ look up a name. Both kinds are matched against every scope that the caller's language declares (`resolve::scope_paths`): each type, each namespace (a Ruby `module`), and each declaration that encloses a callable. A Ruby `User` never binds to a TypeScript `User`. One dotted declaration is one scope: a declaration named `Shop.Cart` declares `Shop::Cart`, not `Shop`. Then:

- The scope defines one callable of that name → `Resolved`, `Exact`.
- It defines several (a scope reopened in several files) → pass C.
- It defines none (an inherited or generated method) → `Bare`.
- The caller's language declares no such scope → `External("File.join")`. These are the only edges the resolver marks external.
- Exception: a `Lexical` receiver whose scope is not found, while the caller's language declares a scope whose path ends with the receiver path (`Jekyll::Utils` for `Utils`) → `Bare`. Ruby also finds a constant through ancestors (`include Jekyll`), which this lookup does not walk, and the method may be inherited, so the scope need not define it.

Ruby and Elixir set a scope marker only on module receivers. Every other adapter sets `None`, so its calls stay name-matched. That includes Java-style `File.join()`, where the grammar cannot tell a type from a value.

The incremental update applies the same `resolve::resolve_scoped` to every scope-receiver edge, resolved and external ones included, because a later delta can define the scope or a nearer one.

### Confidence

Every `CallEdge` carries one of:

| Tag | Meaning |
|---|---|
| `Exact` | Pass A, a scope receiver, or single-candidate pass B promotion. |
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
| Ruby       | `call` (with `method` / `receiver` fields) | `Foo.new` (constant receiver) | tree-sitter-ruby 0.23.1 unifies all call shapes — `obj.method()`, `obj.method "x"`, `puts "hello"`, `Greeter.shout` — under the single `call` kind. Walker deliberately does **not** bail on `block` / `do_block` (closures over the enclosing method's scope, not separate methods). A CamelCase constant receiver sets `receiver_scope` |
| Elixir     | `call` (target `identifier` or `dot`), `\|>` into a bare `identifier`, `&name/arity` captures, `map` with a `struct` | `%Mod{}` (the expanded module, resolved by its full name) | receivers expanded to the full module (`Pricing.sum` → `Shop.Pricing`) and marked `Absolute`; a bare call records the modules imported where it is (an `import` or `alias` ends with its block, `only:`/`except:` select by name and arity, `only: :functions`/`:macros` by kind, a module imported again replaces its earlier import, and `except:` alone filters it per kind, bringing in minus `except:` a kind it did not import); a call from outside a module, through an import or a module receiver, takes only its public definitions, and a `defguard` counts as a macro; a call, bare or remote, takes only a definition that accepts its argument count, default arguments included; a Kernel macro (`if`) is a call where `import Kernel, except:` leaves it to another import; a pattern (`%Mod{} = x`, a clause head) is not a call site, but its guard and a `receive` timeout are; control flow, definitions and directives are not call sites, but their contents are walked; a `quote` contributes only its `unquote`s; `map.key` is a field read |
| SQL        | n/a — no-op by design | — | — |
| Markdown   | n/a — no-op by design | — | — |

Languages still emitting empty `Declaration::calls`: **none.** JavaScript is served by the TypeScript adapter.

### Known per-language limitations

- **Ruby**: bare paren-less arg-less calls (`helper` with no parens, no args) parse as `identifier`, not `call`, so they aren't captured. Inherent to Ruby's grammar — can't disambiguate from a local variable reference at parse time.
- **Ruby**: a class method inherited from a superclass (`Child.create`, defined on `Base`) stays `Bare`: the scope-receiver rule does not walk ancestors.
- **Ruby**: a compact definition (`module Shop::Compact`) is one lexical level in Ruby, but the lookup walks it as two. When both `Shop::Pricing` and a top-level `Pricing` define the method, `Pricing.price` inside it binds to `Shop::Pricing`; Ruby takes the top-level one.
- **Ruby**: a receiver of several segments is looked up whole at each level. Ruby looks up the first segment and then stays under it: with `module Shop; module Tax; end; end`, `Tax::Rule` inside `Shop` is a `NameError` in Ruby, but binds to a top-level `Tax::Rule` here. Following Ruby needs to know which definitions are compact (above): otherwise `class RDoc::Comment` looks up `RDoc` inside `RDoc`, finds `RDoc::RDoc`, and 49 correct `Exact` edges in the Ruby 2.6 stdlib turn unresolved.
- **Ruby**: a constant nested in a superclass is found only when the lexical lookup fails. When a top-level scope of the same name also defines the method, the call binds to the top-level one; Ruby takes the superclass's.
- **Ruby**: the scope marker follows a naming convention, not syntax. A CamelCase constant can hold a value (`TimeoutMutex = Thread::Mutex.new`), and a call on it is then marked like a call on a module.
- **Ruby**: all-caps modules (`JSON.parse`, `CSV.read`, `URI.join`) and `Foo.new` are not marked, so they stay name-matched.
- **Python**: no Jedi-style receiver-type inference. `obj.method()` where `obj`'s type is inferred from runtime flow falls through to pass B/C; the bare-name + import-disambiguation pass gets most callers. Adding Jedi would mean a Python runtime dep — same trade-off doesn't fit a Rust binary.
- **Elixir**: a paren-less zero-argument call (`helper`) is an `identifier`, as in Ruby. Calls injected by `use Foo` and `apply/3` are not seen, and module-level code belongs to no function. A nested `defmodule __MODULE__.Sub` or `defmodule Elixir.Top`, and a `defimpl` inside a module, sit under the enclosing module in their qns, so a call into one from outside stays external. After `import Foo, only: [name: 1]`, an `except:` re-import does not bring in the kind that the list did not name, because the adapter cannot tell a function from a macro by name. In `defimpl P, for: [A, B]`, the body is walked once as `P.A`, so a `__MODULE__.helper(x)` in it binds `P.B`'s call to `P.A.helper`.
- **External-base ancestor walk** (`callees` on a type): capped at depth 1 when a base type doesn't resolve to a project file. Can't walk into types we can't see.

## Unified graph cache

The call graph does **not** get its own cache file. It rides inside a unified `UnifiedGraph { deps, calls: Option<CallGraph> }` at `.ast-bro/deps/graph.bin`. The schema constant is `JSON_SCHEMA_GRAPH_INDEX = "ast-bro.graph-index.v8"`. (The directory keeps its `deps/` name even though it now holds the call graph too — renaming would force every existing user to rebuild from a new path for no benefit.)

### Disk layout

```text
.ast-bro/
├── .gitignore             # auto-written: "*"
├── deps/
│   ├── graph.bin          # bincode CacheFile { schema, graph, files }
│   └── lock               # fs2 advisory exclusive lock during writes
└── index/                 # see search.md
    └── ...
```

### Lazy promotion

`deps` / `reverse-deps` / `cycles` / `graph` populate only the `deps` half — `calls` stays `None`. The first `callers` / `callees` invocation triggers `promote_calls`, which builds the call graph from the existing dep graph (no re-walk of the project) and persists the upgraded `UnifiedCacheFile` back to disk. Users who never run `callers` / `callees` never pay the call-graph build cost.

### Process-wide sharing

`src/graph_cache/shared.rs` holds a process-wide
`OnceLock<RwLock<HashMap<root, Entry>>>`, where each `Entry` pairs the parsed
`Arc<UnifiedGraph>` with the `FileRecord` fingerprints it was built from.
Within a single process — the `ast-bro mcp` long-running server is the case
that matters — every `tools/call` goes through `get_or_init`, which:

- **re-validates** the cached graph against the working tree on every call (a
  stat-only `compute_delta` against the in-memory fingerprints in the steady
  state — no `graph.bin` re-read); an unchanged tree returns the memoised
  `Arc` with zero re-parse,
- on a detected edit, patches the in-memory graph via the same
  `apply_delta_*` machinery the cold load uses and swaps the `Arc`, so a
  long-lived session reflects edits **without** `--rebuild` (this is the fix
  for the prior "graph frozen after first query" behaviour), and
- serialises the slow load / patch / rebuild path behind a process-wide load
  lock with a double-check, so concurrent callers can't both load a stale
  graph and race on the on-disk write.

`Arc` swap on promotion or refresh means existing readers keep their prior
view safely. For one-shot CLI invocations the registry initialises, work
happens, the process exits.

### Schema migration

The legacy `deps-index.v1` cache (at `.ast-bro/deps/graph.bin`) was retired in the v2.1.0 cut. Users with an old cache hit the schema-mismatch branch in `cache::load_with_delta`, the loader returns `LoadOutcome::Missing`, and `load_or_build` rebuilds in place at `.ast-bro/deps/graph.bin` — the schema bumped (`deps-index.v1` → `graph-index.v1`), the path stayed. One-time, transparent.

The schema bump from `ast-outline.graph-index.v1` to `v2` happened mid-development to fix a silent bincode round-trip bug — `#[serde(skip_serializing_if)]` on `DepEdge::local_name` / `raw_path` and `CallEdge::receiver` / `candidates` corrupts bincode's positional encoding (a skipped Option/Vec field shifts every byte that follows). The skip annotations were a JSON-output ergonomics holdover that never applied to the cache; binary positional formats *require* every field to be encoded. Removed the annotations, bumped the schema, and any v1 cache files (which were all corrupt and silently re-cold-built every invocation) get a clean rebuild.

The `ast-bro.` name restarted at `graph-index.v1`. `ast-bro.graph-index.v2` adds `CallEdge::receiver_scope` and `CallGraph::namespaces`, and an older cache rebuilds once. `v3` adds Elixir files to the dep graph: a v2 cache already fingerprints `.ex` files, so without the bump nothing reads as changed and every Elixir file stays out until `--rebuild`. `v4` adds Elixir files to the call graph, for the same reason: a v3 cache holds them with no call sites. `v5` adds `Arity::is_callback`, so that a `@callback` takes no call: bincode encodes fields by position, so a v4 cache cannot be read with the new layout. `v6` keeps a bare call to its language family (issue #96): a v5 cache holds cross-language `Exact` edges, and an incremental update re-resolves only bare edges, so it would never revisit them. `v7` adds the definitions in module-level blocks, from user `def…` macros and in ExUnit tests: a v6 cache holds the old declarations of every Elixir file that did not change. `v8` keys a module defined in an ExUnit `describe` under its module, not under the `describe`: a v7 cache keeps the old key for every test file that did not change.

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
7. Bare-name re-resolution: for every `Bare` edge, look up its name in the (now updated) symbol table and keep the candidates its language can call (pass B's language gate). With none left, the edge goes back to what a cold build leaves, `Bare` with no candidates, rather than keeping candidates that may since have been deleted. Single-match, no-receiver hits promote to `Resolved/Exact` — pass B's rule *and* pass B's confidence, since parity covers the tag as well as the target (receiver suppression included); everything else goes through `resolve::disambiguate` — the *same* function pass C uses — so candidates are filtered to what the caller's file can reach through the dep graph, with a memoized per-file closure. Picks up two cases the partial path would otherwise miss: edges demoted in step 6 whose target moved to a different file, and pre-existing `Bare` edges in unchanged files that finally have a target because a *new* file in this delta defines it.

   The invariant is parity — a partial update must produce the graph a cold build of the same content produces. This step used to assign the *unfiltered* symbol-table entry as an edge's candidate set, which broke that: it ran over the whole graph, so one edit to an unrelated file gave every bare edge in the project candidates the dep filter had ruled out. Measured on ast-bro itself, `callers CliError.new` reported 1023 unresolved sites cold and 1073 after appending a comment to `src/adapters/sql.rs` — every `Vec::new()` in the tree newly counted as a possible caller. Reusing pass C's own decision function is what keeps the two paths honest; `tests/calls_e2e.rs::incremental_update_matches_a_cold_build_of_the_same_content` pins it.
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

Same pattern as the search index: `fs2` advisory exclusive lock at `.ast-bro/deps/lock` during writes; atomic `.tmp` + rename so a SIGKILL mid-write leaves the previous cache intact. Reads use the in-memory `Arc` and don't touch the lock.

## Known gaps

- The suffix index gets a fresh full walk on every delta. The walk is the cheap part of a cold build (~hundreds of ms even on big repos); the per-file extract+resolve is the expensive part, which we now skip for unchanged files. A surgical suffix-index update would be more code than it saves.

## Adding a new language

If you've already added a `Declaration`-emitting adapter (see [architecture.md](architecture.md)), call-site extraction is one helper and one wiring step:

1. Add an `_extract_call_sites` (or `_walk_calls_in_body`) function in your `src/adapters/<lang>.rs` that walks the function body, bails on nested type/callable declarations, and emits one `CallSite` per recognised AST node kind.
2. Call it from inside each `_function_to_decl` / `_method_to_decl` / `_class_to_decl` builder so the populated `calls` ride along on the returned `Declaration`.
3. If the language has its own import syntax not already covered by `src/deps/extract.rs` and `src/surface/imports.rs`, populate `ParseResult::imports` so pass A can resolve same-file `use` / `import` / `using` bindings.
4. Set `CallSite::receiver_scope` only where the syntax tells a scope receiver from an object, and only if the [scope-receiver rule](#scope-receivers) matches how the language looks up that name. Check the lookup first: C++ also finds names through `using` and namespace aliases, and PHP through `use` imports, and PHP does not search enclosing namespaces. A language whose lookup differs needs its own rule, or the marker produces wrong `Exact` edges.
5. Add an end-to-end test in `tests/calls_e2e.rs` mirroring the existing per-language pairs (`<lang>_callers_finds_intra_file_caller` + `<lang>_callees_lists_construct_and_invocation`). Intra-file scope keeps the failure modes narrow — it exercises pass A without depending on the per-language import resolver.

For languages where AST kind names are case-folded by tree-sitter (PHP's late-binding keywords, Ruby's command unification), pin the assumption with a regression test so future grammar drift surfaces as a test failure instead of silently dropping edges.
