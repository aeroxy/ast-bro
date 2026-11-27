# Dependency graph

Four subcommands — `deps`, `reverse-deps`, `cycles`, `graph` — and a per-repo persistent dep-graph cache. This page documents the internal architecture. For the user-facing surface see the README. For what gets walked see [file-filtering.md](file-filtering.md). For the parallel search subsystem (which the dep graph lazily boosts via `find-related`) see [search.md](search.md).

## Pipeline

```
build_graph(root):
  detect_aliases(root)              # tsconfig paths, Cargo crate name, composer PSR-4
  build_suffix_index(root)          # one parallel walk → SuffixIndex { by_suffix, by_file, go_modules }
  par_iter(files):                  # rayon: per-file extract
    imports[file] = extract(file, lang)  # tree-sitter pass; reuses surface/imports.rs where it can
  resolve_all(graph, idx, aliases): # rayon: per-file resolve; shared with the incremental update
    for ri in imports[file]:
      resolve(ri.spec, ctx, idx)    # one resolver, all 9 languages
        → match → DepEdge
        → no match → external bucket
    dedup_edges(graph)              # collapse repeated (src, target) pairs
  → DepGraph { forward, external, imports, stats }

deps <file>:                        forward BFS over `graph.forward`
reverse-deps <file>:                graph.reverse_adjacency() (computed on-demand) + BFS
                                    both report `frontier_truncated` — see below
cycles:                             iterative Tarjan SCC over the compact node-index graph
graph:                              render_graph_text / render_graph_json

find-related (when cache exists):
  semantic top-(K×5) → dep-neighbour boost (1.40× / 1.20×) → top-K
```

## Module layout

```
src/deps/
├── mod.rs           orchestrator: build_graph(root) -> DepGraph
├── options.rs       DepOptions + DepError
├── graph.rs         DepGraph, DepEdge, ImportKind, dedup_edges
├── extract.rs       per-language extract dispatch
├── resolver/
│   ├── mod.rs       re-exports
│   ├── build.rs     build_suffix_index — one parallel walk
│   └── resolve.rs   resolve(spec, ctx, idx) → Option<PathBuf>
├── manifest.rs      go.mod / tsconfig.json / Cargo.toml parsing
├── scc.rs           iterative Tarjan SCC (~80 lines, no petgraph)
├── traverse.rs      forward / reverse (+ *_info) / neighbourhood_depths
├── cache.rs         disk persistence at .ast-bro/deps/graph.bin
├── render.rs        text / JSON renderers
├── cli.rs           run_deps / run_reverse_deps / run_cycles / run_graph
└── mcp.rs           MCP wrappers
```

## Depth cutoff vs. graph end

`--depth` (default 3) bounds both walks, and a walk that ran out of depth looks
exactly like one that ran out of graph — which is how `reverse-deps src/core.rs
--depth 1` came to report `33 total` for a file with 54 importers (issue #32).
`forward_info` / `reverse_info` return a `DepTraversal { hits,
frontier_truncated }`; the plain `forward` / `reverse` wrappers stay for the
callers that only want the hits.

The flag is set when a file popped at the cutoff still has an edge into a file
the walk never `seen`. Two consequences worth keeping straight:

- **An edge back into visited territory does not count.** Otherwise the
  raise-`--depth` hint would be a lie on any cyclic import graph.
- **Predicate-rejected edges do count.** `--exclude-tests` filters what gets
  *reported*, not what gets *expanded* — a test file at the cutoff can still
  lead to a non-test importer deeper — so the frontier question is about
  traversal, not about the display.

`frontier_truncated` is orthogonal to `truncated`, which is what `--limit` sets:
`truncated: false` with `frontier_truncated: true` says nothing was cut from the
display and `total` itself counts only part of the cone. Text mode prints
`render::frontier_note` on stderr; MCP, which has one channel, prepends the same
string to the response body. Unlike `callers` / `callees`, the note is *not*
suppressed at low depth: those default to 1, where "the callers have callers" is
the norm, while an import cone running past three hops is a fact about the file.

## Suffix-index resolver

Single shared resolver with per-call language hints — much smaller than nine per-language resolvers and just as accurate.

For every source file `a/b/c.<ext>` we index every path-suffix of the file (without extension) → absolute path:

```
src/deps/cli.rs   →   indexes "src/deps/cli", "deps/cli", "cli"
```

Plus three language-specific augmentations:

| Language | Extra index entries |
|---|---|
| Python `__init__.py` | parent dir (`pkg/__init__.py` → `pkg`, `pkg/__init__`) so `import a.b` finds `a/b/__init__.py` |
| Java / Kotlin / Scala / C# | `<package>/<TypeName>` for every top-level type, parsed from each file's `package` / `namespace` declaration |

Storage: `HashMap<String, SmallVec<[PathBuf; 2]>>`. Multi-value because suffix collisions happen (`utils.py` may exist in two packages); `pick_closest` breaks ties by max common-prefix length with the importer. Built once per `build_graph` call (single parallel walk via `WalkBuilder` + rayon).

## Per-language extract

`src/deps/extract.rs` dispatches on the file's `Lang` and produces `Vec<RawImport>`. Where possible it reuses extractors that already exist for the `surface` subsystem (Rust / Python / TS / Scala in `src/surface/imports.rs`); the four newer languages get their own tree-sitter passes inline.

| Lang | AST nodes walked | Source |
|---|---|---|
| Rust | `use_declaration`, `mod_item` (`#[path]` aware) | `surface::imports::extract_rust_imports` |
| Python | `import_from_statement`, `import_statement`, `__all__` | `surface::imports::extract_python_imports` + bare-`import` pass |
| TS/JS | `import_statement` (with `import_clause`/`namespace_import`/`named_imports`), `export_statement`, top-level `require()` calls | new pass in `extract.rs` |
| Scala | `import_declaration` with selector lists `{a => b, _}`, descends into bodies | new pass in `extract.rs` |
| Java | `import_declaration` (regular + `static`, `.*` glob, inner-class trailing-segment) | new pass in `extract.rs` |
| Kotlin | `import_header` / `import_directive` (with `as Quux`, `.*` glob) | new pass in `extract.rs` |
| C# | `using_directive` (`using A = X.Y;` aliases, `using static`, namespace bodies) | new pass in `extract.rs` |
| Go | grouped `import (...)` + single `import_spec`; `go.mod` `module` directive | new pass in `extract.rs` |
| Elixir | `alias` / `import` / `require` / `use` (tuple form, `as:`, `__MODULE__`, earlier aliases expanded), a `defimpl`'s protocol as an `impl` edge, then every other module name the code uses (remote-call targets, structs, modules as values, `for:` types) as `ref` edges; typespecs and `quote` bodies (except their `unquote`s) skipped | `adapters::elixir::scan_modules` |

Each `RawImport` carries `spec` (slash-joined module path), `kind`, source `line`, `local_name` (for `as Quux` / `using A = X.Y` aliases), and the original dotted path (`raw_path`).

## Resolution rules

`resolve(spec, ctx, idx)` — one function for everyone, branched by `ctx.lang`:

- **Rust**: `crate::x::y` strips `crate::` → suffix lookup with progressive trailing-segment dropping; `self::` and `super::` resolve relative to the importer's directory; otherwise treats as a bare path.
- **Python**: relative paths (`./x` / `../x`) walk from the importer's parent dir; tries `.py` / `.pyi` / `__init__.py`. Falls back to dropping the trailing imported-name segment when the full path doesn't resolve (so `from .helpers import greet` finds `helpers.py`, not `helpers/greet.py`), for an absolute `from a import real` as well as a relative one. An absolute import matches Python files only: the suffix index holds every language's files, and `import requests` is not `web/requests.ts` (#127).
- **TS/JS**: relative `./x` walks the importer's parent; tries extensions in order `.ts → .tsx → .mts → .cts → .d.ts → .js → .jsx → .mjs → .cjs → .json`, then `index.*`. Bare specifiers consult `compilerOptions.paths` aliases, then `compilerOptions.baseUrl` (`components/Button` under `"baseUrl": "src"` is `src/components/Button.ts`). A `baseUrl` candidate is tried in the same extension and `index.*` order as a relative path, but looked up in the suffix index's `by_file` map rather than on disk: most bare specifiers are packages, and every miss used to stat up to 21 paths on every update. Before the probe, `may_name_an_indexed_file` checks the suffix keys a hit would need (`rel`, `rel.d`, `rel/index`, `rel/index.d`, or `rel` without its own extension); when none exists, the candidate is a miss without building any path. Pass A and the incremental update resolve the import of every call through it, so on a project with `"baseUrl": "."` and 20k calls into packages this check took the cold call graph from 350 ms to 255 ms and the update from 183 ms to 94 ms. The index holds what a cold build walks, so a `.json` or an ignored file under `baseUrl` is not a target. Anything still unresolved → external bucket: a bare specifier names a package, not whichever project file's path ends in it (#127). The settings come from the root `tsconfig.json`, or from `jsconfig.json` when there is none, merged with every config it `extends` through a relative path inside the root (a string or an array, `.json` optional; a later config overrides an earlier one, and a package name is not followed). A config two parents extend is read under each, as `tsc` merges each parent's effective settings: the later parent's inherited `baseUrl` wins over the earlier parent's own. Only a config already on the current `extends` chain stops the walk, which ends a cycle. `baseUrl` and `paths` resolve against the directory of the config that sets them, and both a `paths` target (`../src/*` from `configs/`) and a `baseUrl` import (`lib/../util`) are normalised to the index key (`src/`, `util`). `manifest::strip_jsonc` turns the JSONC into JSON: `//` and `/* */` comments and trailing commas go, strings stay byte for byte.
- **Java / Kotlin / Scala / C#**: dot→slash transform, then suffix lookup against the FQN-augmented index. Inner-class fallback: on miss, strip the trailing segment and retry (handles `import com.foo.Bar.Inner` → `com/foo/Bar`).
- **Elixir**: modules are named, not paths. `build_suffix_index` parses each `.ex`/`.exs` into `elixir_modules` (module name → defining files, nested modules included), and a spec is looked up there by exact name. Stdlib, Erlang and dependency modules go to the external bucket. Elixir files are not in the path-suffix index: a module name is not a path, and `import json` in Python must not reach `lib/app/json.ex`.
- **Go**: strips the `go.mod` module-name prefix, then locates any source file inside the resulting directory (directory-as-package semantics). An import equal to the module path names the package in the module directory itself — the layout of most single-package libraries — and resolves there; the remainder must start at a `/` so `example.com/dupfoo` is not read as a subpackage of `example.com/dup`. External imports without the module prefix → external bucket. The prefixes come from every `go.mod` under the root, not just `<root>/go.mod`: the graph root is the repository, and a repository may keep its module in a subdirectory or declare several. `build_suffix_index` collects them during the walk it already makes, and `resolve` ranks the candidates three ways. The innermost module directory *holding the importer* comes first: that is the module the importer is part of, and Go resolves within it before looking anywhere else — a module elsewhere in the tree is a separate build even when it declares a longer path that also matches, which is what `testdata/`, a nested checkout, or a vendored copy leaves lying around. Longest module path then decides among the candidates that hold the importer in none of their directories, and there a nested module wins over the one containing it. Equal prefixes are then ranked by distance from the importer, the same rule `pick_closest` applies to suffix hits: one root can hold two copies of a module, and an import must not cross into the other copy. A candidate whose directory holds no matching package is skipped rather than ending the search, since a nested module's path need not mirror its directory.

`pick_closest` resolves ambiguity when multiple files match a suffix — pick the candidate sharing the most leading components with the importer; break ties lexicographically. Prevents false cross-project edges in monorepos.

## Iterative Tarjan SCC

`src/deps/scc.rs` runs Tarjan's strongly-connected-components algorithm with an explicit work stack instead of recursion. ~80 LOC, no `petgraph` dependency. Stack-safe on huge dep chains.

Output: `Vec<Cycle>`. Filter rule:

- SCCs with `len > 1` → kept (real cycles).
- Singleton SCCs → kept iff the node has a self-edge.
- All others dropped.

`ref` edges (an Elixir module named without a directive, mostly a runtime call) are left out unless `--include-refs` is passed: runtime cycles are normal in Elixir and would fail every CI gate on an Elixir tree, while a cycle through `alias` / `import` / `require` / `use` / `impl` still counts. When leaving them out changes the answer, `cycles` says so: a `# note:` with both counts (on stderr in the CLI, in front of the text on MCP), and a `cycles_with_refs` count in the JSON on both surfaces. `scc::report` makes that decision once for both.

Cycles sort by member count descending; members within each cycle sort lexicographically — stable for diffs and snapshot tests.

## Caching

Disk format: `.ast-bro/deps/graph.bin` (bincode-serialized `CacheFile = { schema, graph, files }`). Sibling to `.ast-bro/index/`. The `.ast-bro/.gitignore` written by the search subsystem (`*` content) covers it for free.

Refresh strategy: load → run `graph_cache::cache::compute_graph_delta` against the recorded `Vec<FileRecord>`. It tracks what the graph depends on, in two kinds: the *contents* of every file `deps_walk` + `is_deps_input` yield (the walk and rule `build_suffix_index` uses — hidden files included, no size cap, each `go.mod`) and of every file `detect_aliases` reads (`manifest::alias_sources`: the `manifest::ROOT_MANIFESTS` that exist and the configs a root `tsconfig.json` extends, one that does not parse included, so the edit that fixes it is seen); and the *existence* of every other walked file, because a relative import may name any file and the resolver checks that it exists (`./data.json`, `./logo.svg`). An existence-only file gets a placeholder record and is never hashed: adding or deleting it is a change, editing it is not. The search corpus walk is not reused: it skips hidden and oversized files, so changes the graph depends on went unseen (#123); both walks compare through `search::cache::delta_from_files`. The fingerprints are taken before the build or the patch reads any file, so a file saved while the graph reads it shows up as a change on the next check instead of being recorded against the old contents. A file changed when its `(mtime, size)` changed (cheap path) and its `xxhash3-64` differs (expensive path, only on size/mtime mismatch). `graph_cache::delta::apply_delta_to_deps` then parses only the added and modified files, and resolves *every* file again against a fresh suffix index, from the imports the cache keeps per file (`DepGraph::imports`). Resolving only the changed files is not enough (#93): a deleted target, a new file that satisfies an external import or sits nearer than the old target, or an Elixir module that moves between two edited files all change where an *unchanged* file's import lands. `build_graph` and the update share `deps::resolve_all`, so the two cannot drift.

Schema constant: the cache file is stamped with `JSON_SCHEMA_GRAPH_INDEX` (`src/core.rs`), which is bumped on any `DepGraph` / `DepEdge` shape change to force a rebuild via the schema-mismatch branch in `cache::load_with_delta`. `JSON_SCHEMA_DEPS_INDEX` only stamps `DepGraph::schema`.

Concurrency: `fs2` advisory exclusive lock at `.ast-bro/deps/lock` during writes; atomic `.tmp` + rename so a SIGKILL mid-write leaves the previous cache intact. Same pattern as the search index.

`--rebuild` on any of the four CLI subcommands forces a fresh build regardless of staleness.

## find-related dep boost

Wired into `src/search/index.rs::find_related_opts` (called by the public `find_related` with `dep_boost = true, dep_depth = 2` defaults).

Flow:

1. Resolve source chunk + build language mask (existing).
2. Pull a wider candidate window (`top_k × 5`) so the boost can promote items that wouldn't be in the top-k by raw similarity.
3. `cosine_topk` → candidates (existing).
4. **Lazily** load the dep graph: `Index::dep_graph_cached()` consults the on-disk cache and memoises the result in an `RwLock<Option<Option<DepGraph>>>` for the lifetime of the `Index` struct. Never triggers a build; if no cache exists, the boost is silently skipped.
5. `traverse::neighbourhood_depths(graph, source_file, dep_depth)` → `HashMap<PathBuf, usize>` (BFS over forward + reverse-on-demand adjacency).
6. For each candidate, multiply its score by **1.40×** (depth 1) or **1.20×** (depth 2). Other depths unchanged.
7. Re-sort, truncate to `top_k`, return.

Disable per-call with `--no-dep-boost`. Configurable depth with `--dep-depth N`.

## On-disk format

```
.ast-bro/
├── .gitignore             # auto-written: "*"
├── deps/
│   ├── graph.bin          # bincode CacheFile { schema, graph, files }
│   └── lock               # fs2 advisory exclusive lock during writes
└── index/                 # see search.md
    └── ...
```

Loader refuses if `CacheFile.schema != JSON_SCHEMA_DEPS_INDEX`.

## Adding a new language

1. Add a variant to `Lang` in `src/deps/resolver/build.rs` and a case in `Lang::from_path` for its file extensions.
2. Add an arm to the `match` in `src/deps/extract.rs::extract` returning `Vec<RawImport>` for the new language.
3. If the language has FQN-based imports (`com.foo.Bar`), add it to the `Lang::Java | Lang::Kotlin | Lang::Scala | Lang::CSharp` branches in `extract_package_and_types` (build.rs) and the resolver (resolve.rs).
4. If the language has a manifest-driven module prefix (like Go's `go.mod`), add a parser to `src/deps/manifest.rs` and a branch in `resolve.rs`. A manifest that can appear anywhere in the tree — not only at the root — is collected in `build_suffix_index`'s walk, the way `go_modules` is.
5. Add a fixture under `tests/fixtures/deps/<lang>_<scenario>/` and an integration test in `tests/deps_e2e.rs`.

The four "FQN" languages (Java, Kotlin, Scala, C#) all share the same suffix-index code path — adding a similar one (e.g. F#) is mostly an `extract` pass plus a one-line resolver case.
