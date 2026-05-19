# Dependency graph

Four subcommands — `deps`, `reverse-deps`, `cycles`, `graph` — and a per-repo persistent dep-graph cache. This page documents the internal architecture. For the user-facing surface see the README. For what gets walked see [file-filtering.md](file-filtering.md). For the parallel search subsystem (which the dep graph lazily boosts via `find-related`) see [search.md](search.md).

## Pipeline

```
build_graph(root):
  detect_aliases(root)              # go.mod module name, tsconfig paths, Cargo crate name
  build_suffix_index(root)          # one parallel walk → SuffixIndex { by_suffix, by_file }
  par_iter(files):                  # rayon: per-file extract + resolve
    raw = extract(file, lang)       # tree-sitter pass; reuses surface/imports.rs where it can
    for ri in raw:
      resolve(ri.spec, ctx, idx)    # one resolver, all 9 languages
        → match → DepEdge
        → no match → external bucket
  dedup_edges(graph)                # collapse repeated (src, target) pairs
  → DepGraph { forward, external, stats }

deps <file>:                        forward BFS over `graph.forward`
reverse-deps <file>:                graph.reverse_adjacency() (computed on-demand) + BFS
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
├── traverse.rs      forward_bfs / reverse_bfs / neighbourhood_depths
├── cache.rs         disk persistence at .ast-bro/deps/graph.bin
├── render.rs        text / JSON renderers
├── cli.rs           run_deps / run_reverse_deps / run_cycles / run_graph
└── mcp.rs           MCP wrappers
```

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

Each `RawImport` carries `spec` (slash-joined module path), `kind`, source `line`, `local_name` (for `as Quux` / `using A = X.Y` aliases), and the original dotted path (`raw_path`).

## Resolution rules

`resolve(spec, ctx, idx)` — one function for everyone, branched by `ctx.lang`:

- **Rust**: `crate::x::y` strips `crate::` → suffix lookup with progressive trailing-segment dropping; `self::` and `super::` resolve relative to the importer's directory; otherwise treats as a bare path.
- **Python**: relative paths (`./x` / `../x`) walk from the importer's parent dir; tries `.py` / `.pyi` / `__init__.py`. Falls back to dropping the trailing imported-name segment when the full path doesn't resolve (so `from .helpers import greet` finds `helpers.py`, not `helpers/greet.py`).
- **TS/JS**: relative `./x` walks the importer's parent; tries extensions in order `.ts → .tsx → .mts → .cts → .d.ts → .js → .jsx → .mjs → .cjs → .json`, then `index.*`. Bare specifiers consult `tsconfig.json` `compilerOptions.paths` aliases via `manifest::parse_tsconfig_paths`. Anything still unresolved → external bucket.
- **Java / Kotlin / Scala / C#**: dot→slash transform, then suffix lookup against the FQN-augmented index. Inner-class fallback: on miss, strip the trailing segment and retry (handles `import com.foo.Bar.Inner` → `com/foo/Bar`).
- **Go**: strips the `go.mod` module-name prefix, then locates any source file inside the resulting directory (directory-as-package semantics). External imports without the module prefix → external bucket.

`pick_closest` resolves ambiguity when multiple files match a suffix — pick the candidate sharing the most leading components with the importer; break ties lexicographically. Prevents false cross-project edges in monorepos.

## Iterative Tarjan SCC

`src/deps/scc.rs` runs Tarjan's strongly-connected-components algorithm with an explicit work stack instead of recursion. ~80 LOC, no `petgraph` dependency. Stack-safe on huge dep chains.

Output: `Vec<Cycle>`. Filter rule:

- SCCs with `len > 1` → kept (real cycles).
- Singleton SCCs → kept iff the node has a self-edge.
- All others dropped.

Cycles sort by member count descending; members within each cycle sort lexicographically — stable for diffs and snapshot tests.

## Caching

Disk format: `.ast-bro/deps/graph.bin` (bincode-serialized `CacheFile = { schema, graph, files }`). Sibling to `.ast-bro/index/`. The `.ast-bro/.gitignore` written by the search subsystem (`*` content) covers it for free.

Refresh strategy: load → run `search::cache::compute_delta` against the recorded `Vec<FileRecord>`. If any file's `(mtime, size)` changed (cheap path) or its `xxhash3-64` differs (expensive path, only on size/mtime mismatch) — full rebuild. Same "any-delta = full rebuild" simplification the search index uses today; partial-rebuild is a v2 swap-in.

Schema constant: `JSON_SCHEMA_DEPS_INDEX = "ast-bro.deps-index.v1"`. Bumped on any `DepGraph` / `DepEdge` shape change to force a rebuild via the schema-mismatch branch in `cache::load_if_fresh`.

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
4. If the language has a manifest-driven module prefix (like Go's `go.mod`), add a parser to `src/deps/manifest.rs` and a branch in `resolve.rs`.
5. Add a fixture under `tests/fixtures/deps/<lang>_<scenario>/` and an integration test in `tests/deps_e2e.rs`.

The four "FQN" languages (Java, Kotlin, Scala, C#) all share the same suffix-index code path — adding a similar one (e.g. F#) is mostly an `extract` pass plus a one-line resolver case.
