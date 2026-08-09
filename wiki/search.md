# Semantic search

Two new subcommands — `search` and `find-related` — and a per-repo persistent index. This page documents the internal architecture. For the user-facing surface see the README. For network/TLS behaviour see [network-security.md](network-security.md). For what gets indexed see [file-filtering.md](file-filtering.md).

Every number quoted here comes from [tests/eval](../tests/eval/README.md), a
30-query set over pgjdbc at a pinned revision, with symbol-level ground truth.
Measure a change there before adopting it — several that looked obviously right
measured flat, one that looked wrong measured well, and the harness is what
caught the ranking non-determinism described under [Determinism](#determinism).

Scores are quoted at `--min-iou 0.3`: a result counts only when its line range
resembles the answer's. Bare overlap instead rewards coarse chunking, since a
result spanning a whole class overlaps every symbol in it by construction — the
pre-hierarchy chunker leads on that metric and collapses to 3% on this one.

## Pipeline

```
search "<query>":
  parse_query(query)                   peel lang:/path:/name: filters; rest = text
    → mask = language ∧ path-substring ∧ name-substring ∧ query_scope ∧ live
  tokenize(text)
    ├─ BM25.get_scores(tokens, mask)  → 100 candidates (fixed, see below)
    └─ encode_one(text) → cosine_topk(embeddings, mask)  → same count
  RRF normalize each (k = 60)
  combine(alpha-weighted)              alpha auto: 0.3 symbol query, 0.5 NL
  boost_multi_chunk_files(...)         file coherence (+20% × file_sum/max_file_sum)
  file_prior(...)                      phase one: rank files by their Outline
                                       chunks alone; chunks in a liked file are
                                       multiplied by 1 + 1.0 × prior
  apply_query_boost(...)               definition (3×), embedded symbol (1.5×),
                                       file-stem matches for NL queries
  rerank_topk(top_k, penalise_paths=True)
                                       chunk kind: Enclosing 0.7×, Outline 0.8×;
                                       test files 0.3×, compat dirs 0.3×,
                                       generated files 0.3×, __init__.py 0.5×,
                                       .d.ts 0.7×, file-saturation decay (0.5^extra);
                                       an Enclosing chunk drops when one of its
                                       own Parts is also a candidate

find-related <file>:<line>:
  resolve_chunk(file, line)             prefer chunks where start ≤ line < end
  encode chunk.content
  cosine_topk(embeddings, mask)         mask: same language, exclude self
  return top_k
```

The candidate count is fixed at 100, not derived from `top_k`. Deriving it made
`top_k` change the *order* of results and not only how many come back, since
the reranker sees a different candidate set. A *floor* of 100 over `top_k × 5`
was not enough: it only moved the coupling past `k = 20`, where the multiplier
overtakes the floor again — eight of thirty eval queries still had a
`k`-dependent top-3 and three a `k`-dependent top-1. With the pool fixed it is
zero of thirty. It moves neither recall@5 nor MRR@10, and costs one wider
selection over scores both retrievers compute for every chunk anyway.

## Chunk hierarchy

Chunking emits overlapping levels, not a partition. `ChunkKind` says which:

| Kind | What it covers |
| --- | --- |
| `Source` | A slice of the file packed at member boundaries |
| `Enclosing` | A member too large for one chunk, kept whole |
| `Part` | A body-level slice of an `Enclosing` member |
| `Outline` | Synthesized signature list, no body text |

`Source` and `Enclosing` tile the file exactly once. `Part` chunks re-cover the
inside of a large member on purpose, so a query can match the statement or the
method containing it; ranking prefers the `Part` and drops the `Enclosing` when
both are candidates.

Splitting descends through *type containers* — class bodies, `impl` blocks,
inline modules — but never into a callable. That is what keeps a chunk boundary
off the middle of an expression. Descending into anything oversized instead put
`list.forEach` and `(item -> {` in different chunks, and separated a nested
class header from its body.

"Callable" has to include constructors and destructors, which are named
`*_declaration` rather than anything containing `method`. Leaving them out made
them containers: descent entered the body, and `local_variable_declaration`
carries the `_declaration` suffix too, so it became a split point in the middle
of a constructor. `is_member_kind` rejects `local_*` for the same reason — they
are statements wearing a declaration suffix.

The effect is largest where a language forces one top-level declaration per
file. Splitting only at top level left a whole Java class as a single chunk: on
pgjdbc, 85% of indexed characters sat in chunks above the size target, and
`PgResultSet.java` was one 139 KB chunk of 4397 lines. Per-member splitting
brings that to 41%, and p90 chunk height from 196 lines to 57. Rust gains less
(53% → 39%), since it already has many top-level items and only `impl` blocks
are affected. Go and C barely change.

### Breadcrumbs

Every chunk carries the declaration path that encloses it (`QueryExecutorBase >
sendQueryCancel`), rendered in results and present in `--json`. It also goes
into the text handed to the embedder, which is not the obvious choice —
`potion-code-16M` is static and never saw such headers in training. It measures
well anyway: dropping it costs recall@5 23% → 17% and MRR@10 0.226 → 0.148.
Averaging is likely why it works, since a body-level chunk that never names its
own method gets those tokens pulled into its mean. BM25 keeps the breadcrumb
too, where it carries the effect alone but adds nothing on top of the embedder.

### Outline documents

One synthesized chunk per file, listing member signatures with no bodies. A
signature ends where the AST says the body starts, not at the first `{` or
newline: a brace can sit inside a parameter list — `fn unpack(Config { host,
port }: Config)`, or any destructured TypeScript prop — and stopping at the
newline dropped the parameters of every signature wrapped across lines.
Declarations with no body at all, such as interface methods, keep their whole
text.

Outlines answer "which file exposes this API" for queries that name a type or
method, where body-level chunks each match only weakly. They sit beside the
source chunks rather than replacing them.

A wrapper node is transparent, detected by shape rather than by kind name: a
node holding a member that ends where it ends. Left opaque, `export_statement`
hid every exported declaration, so a TypeScript outline listed exactly the ones
that were *not* part of the public API, and Python's `decorated_definition`
listed `@decorator` as a member of its own.

Markdown gets them too, built from its headings — a heading is to a document
what a signature is to a class. Heading detection follows CommonMark where it
is cheap to: at most three spaces of indentation (four makes it a code block),
setext underlines as well as `#`, nothing inside a fenced block, and no YAML
front matter — whose closing `---` otherwise reads as a setext underline and
promoted the last metadata line into a heading. Skipping it was not neutral: the phase-one
prior can only *raise* a file's score, so a file with no outline could not
compete with one that had them, and documents lost to code on queries the
document answered. The file type was deciding the ranking, not the relevance.

A long outline is split at `OUTLINE_MAX_CHARS = 3000`, well under the
embedder's own cap. An outline above that cap is dropped from the dense index
entirely, and the files with the longest outlines are the large central ones —
leaving them unembedded made phase-one file retrieval miss exactly
`QueryExecutorImpl`, `PgConnection` and `PgResultSet`. Splitting them raised
phase-one file recall@5 from 33% to 60%, and top-1 from 23% to 40%.

The pieces do not overlap. Carrying signatures across the boundary measured
worse at every setting tried: an outline is a list of independent signatures
rather than prose, so no evidence straddles the cut, and overlap only
duplicates terms and makes near-identical pieces compete.

## Two-phase retrieval

`file_prior` ranks *files* using only their `Outline` chunks, then phase two
multiplies each chunk by `1 + 1.0 × prior[file]`. The prior only ever promotes:
a hard filter would zero out recall for every query whose file the outlines
miss, and phase one finds the right file in roughly half of them.

Measured on the pinned pgjdbc set: MRR@10 0.204 → 0.226 at unchanged recall@5,
with two queries improving and none regressing. Anything from 0.5 to 2.0
performs about the same, so the weight sits on a plateau. Latency is unchanged
— phase one is a masked pass over embeddings already in memory.

## Field-qualified queries

`parse_query` (in `query.rs`) peels structured filters out of the raw query
before retrieval, so an agent can narrow a search inline instead of via
separate flags. Given:

```text
lang:rust path:src/auth name:login how is the token refreshed
```

it splits into `lang=rust`, path-substring `src/auth`, name-substring
`login`, and the free text `how is the token refreshed`. The filters become a
post-filter mask (composed with the existing `--lang` flag and the
path-derived `query_scope`); the free text is what BM25 + the embedder
actually score, over the narrowed set.

- `lang:` / `language:` — chunk language; **unions** with the `--lang` flag.
- `path:` — case-insensitive substring of the chunk's repo-relative path.
- `name:` — case-insensitive substring of the file's name. The index is
  chunk-based (no per-symbol name), so `name:` matches the *file*, not a
  symbol.

Repeated fields OR together (`lang:rust lang:go` → either). Quoted values
keep spaces (`path:"src/some path"`). An unknown prefix (`TODO:`,
`http://x`) falls through to the free text, so a literal colon still
searches. Filters are applied in `build_combined_mask` (`index.rs`) — the
same masking path as language/scope/tombstone filtering.

## Module layout

```
src/search/
├── tokens.rs      identifier extraction + camel/snake split
├── bm25.rs        sparse BM25 (lucene variant), get_scores(tokens, mask)
├── chunker.rs     chunking: ast-grep decls, tree-sitter-md sections (markdown),
│                  blank-line paragraphs (toml / powershell)
├── download.rs    HF probe + hf-mirror fallback + sha256 manifest
├── embed.rs       safetensors mmap + tokenizer.json + cosine_topk (SIMD via wide)
├── fusion.rs      RRF (k=60) + alpha resolver
├── ranking.rs     boosting + penalties + greedy top-k
├── cache.rs       mtime + xxhash3 delta detection
├── query.rs       field-qualified query parser (lang:/path:/name:)
├── index.rs       orchestrator: build / open / search / find_related / persist
├── format.rs      text + JSON renderers (shared by CLI and MCP)
├── cli.rs         clap-side handlers — called from main.rs
└── mcp.rs         MCP-side handlers — called from mcp/tools.rs
```

The `cli.rs` / `mcp.rs` shims keep dispatch in [main.rs](../src/main.rs) and [mcp/tools.rs](../src/mcp/tools.rs) thin: each subcommand is a one-line forward into the shared `Index::search` / `Index::find_related`.

## Embedding model: model2vec / potion-code-16M

A "static" embedder — no neural-net inference, just a `vocab × 256` float32 lookup table:

```
encode_one(text):
  ids = tokenizer.encode(text, add_special_tokens=False)
  mean = average(embeddings[id] for id in ids)
  return L2_normalize(mean)
```

That's it. Cost is dominated by tokenization (~10–100 µs); embedding lookup is essentially free. Output is always L2-normalized so cosine similarity reduces to a dot product.

`Embedder::open(model_dir)` mmaps `model.safetensors` (~64 MB) — the matrix stays paged in but never copied. `vocab × 256 × 4 bytes` = ~64 MB regardless of repo size.

`f16` tensors are also accepted and decoded once into an owned `f32` buffer at
open time, so nothing downstream of `Embedder` has to know the on-disk dtype.

### Tokenizer

WordPiece with `lowercase: true`, vocabulary ~62 k. Note the order: the
normalizer lowercases *before* WordPiece splits, so `sendQueryCancel` becomes
`sendquerycancel` and the case boundaries — the only signal of where the words
are — are gone by the time splitting happens. `send`, `query` and `cancel` are
all in the vocabulary; the model just cannot reach them.

Feeding the word-split form alongside the identifier is therefore a real
mechanism, and it measures small: +1 query on recall@5 over 30, with MRR@10
inside run-to-run noise, and one regression where splitting diluted the rare
term `SASL` across many chunks. Not enabled. The lexical half already splits
identifiers via `split_identifier`, which is most of why the dense-side gain is
so thin.

## Cosine top-k: brute-force SIMD

`cosine_topk(query, embeddings, mask, k)` walks every row of the chunk-embedding matrix:

- Pre-loads the query into 32 × `wide::f32x8` SIMD lanes (256 dims = 32 chunks of 8).
- Each row is a `&[f32; 256]` — one cache-friendly slice from the contiguous matrix.
- Dot product per row: 32 × 8-lane FMA + horizontal sum.
- For matrices ≥ 4096 rows, parallelizes via rayon over row-blocks of 256.
- Top-k via `select_nth_unstable_by` on indices, then sort the prefix.

Bench: ~25 ms single-threaded on a 10k-chunk repo, ~5 ms across 8 cores.

No HNSW or other ANN structure. At repo scale (≤100k chunks for monorepos), brute-force SIMD is faster than ANN setup time and trivial to maintain.

## BM25: hand-rolled lucene variant

We use `bm25s.BM25(method="lucene")`'s exact formula:

```text
idf(t) = ln(1 + (N - df(t) + 0.5) / (df(t) + 0.5))
score(d, q) = Σ idf(t) · tf(t,d) · (k1+1)
                          / (tf(t,d) + k1 · (1 - b + b · |d| / avgdl))
                          k1 = 1.5, b = 0.75
```

`get_scores(tokens, mask)` returns one f32 per chunk. The mask is a *post-filter score multiplier* (matches `bm25s`'s `weight_mask` semantics) — not a slice — so IDF normalization over the full corpus is preserved when filtering by language.

The hand-roll is ~150 lines and lets us own the mask semantics. The `bm25` crate doesn't expose them.

## RRF + ranking

Combining BM25 and dense scores by raw magnitude doesn't work — they're on different scales. RRF (`1 / (k + rank)` with `k = 60`) normalizes both into the same band before alpha-weighted blending.

Then four boosting / penalty passes:

1. **`boost_multi_chunk_files`** — files with multiple high-scoring chunks get their top chunk lifted by `0.2 × max_score × (file_sum / max_file_sum)`.
2. **Symbol queries** trigger `_boost_symbol_definitions`: chunks that *define* the queried name get `3× max_score` (1.5× multiplier if the file stem matches the symbol). Also scans non-candidate chunks whose file stem matches.
3. **NL queries** trigger `_boost_stem_matches` (file/dir name overlap with query keywords) + `_boost_embedded_symbols` (PascalCase / camelCase identifiers in the query, half-strength definition boost).
4. **`rerank_topk`** applies the chunk-kind penalty (`Enclosing` 0.7×, `Outline` 0.8×), multiplicative path penalties (test files 0.3×, compat/legacy dirs 0.3×, examples 0.3×, generated files 0.3×, `.d.ts` 0.7×, `__init__.py` / `package-info.java` 0.5×) and greedy file-saturation decay (2nd chunk from the same file × 0.5, 3rd × 0.25, ...).

### Determinism

Ranking must not depend on `HashMap` iteration order, which Rust randomizes per
process. Three places did, and together they made the same query over the same
index return different results run to run. Five identical runs on the pinned
corpus spanned recall@5 23–27% and MRR@10 0.196–0.223 — wide enough to hide or
invent a change worth adopting.

- `boost_multi_chunk_files` accumulated a per-file score sum in map order.
  Float addition is not associative, so the sum differed each run; on an exact
  tie the "best chunk" of a file also went to whichever id came first.
- `file_prior` folded a file's several `Outline` pieces into one entry, letting
  the last piece in iteration order set the file's prior.
- `rerank_topk` sorted by score with no tiebreak, and ties are common because
  RRF maps scores onto the small discrete set `1 / (k + rank)`.

All three now walk ids in order or break ties on id. `file_coherence_boost_is_
independent_of_map_order` guards the first by rebuilding the map in reverse
insertion order and demanding bit-identical scores.

**Generated-file down-ranking.** `generated_file_re` (`ranking.rs`) classifies machine-generated paths by suffix/marker: protobuf & gRPC stubs (`.pb.go`, `_grpc.pb.go`, `_pb2.py`, `.pb.{cc,h,dart,ts}`, …), Dart/Flutter codegen (`.g.dart`, `.freezed.dart`), C# designer output (`.Designer.cs`, `.g.cs`), gomock files (`_mock(s).go`, `mock_*.go`), minified bundles (`.min.js`, `.bundle.js`), and anything under a `generated/` / `__generated__/` directory. Suffixes are anchored to `$` and the directory marker requires an exact path component, so look-alikes (`general.rs`, `genesis.rs`, `codegen.rs`) never match. This stops a query like `Send` against a protobuf-heavy repo from burying the hand-written implementation under a dozen generated stubs.

## On-disk format

```
.ast-bro/
├── .gitignore               # auto-written: "*"
└── index/
    ├── meta.json            # ~2 KB — schema, model id+revision, chunk_count, tombstones
    ├── chunks.bin           # bincode Vec<Chunk> (~1.5 KB/chunk × N)
    ├── embeddings.f32       # N × 256 × 4 bytes, header-less, little-endian
    ├── bm25.bin             # bincode Bm25Index (vocab + idf + postings)
    ├── files.bin            # bincode Vec<FileRecord> (path + mtime + size + hash + chunk range)
    └── lock                 # advisory exclusive lock during writes
```

Loader refuses if `meta.json.schema != "ast-bro.search-index.v2"`, model id mismatches, or `chunks.len() × 256 × 4 != len(embeddings.f32)`. Each binary is read via bincode with `serde::Deserialize`. Embeddings are read into memory (mmap is a later swap that won't change the format).

Schema v2 adds `breadcrumb` and `kind` to `Chunk`, which changes the bincode
layout of `chunks.bin`. A v1 index cannot be decoded, so the loader rejects it
outright — including the pre-rename `ast-outline.*` names — and the caller
rebuilds. There is no migration path and there does not need to be: the index
is a cache.

A chunk above `MAX_EMBED_CHARS = 6000` gets a non-finite embedding rather than a
real one, so `cosine_topk` drops it and only BM25 can retrieve it. A zero vector
would not do: its dot product is a finite `0.0`, which outranks every negative
similarity and enters the pool whenever the corpus is smaller than the pool.
`potion-code-16M` encodes a chunk as the mean of its token vectors, so a 40 KB
class drifts toward the centroid of the language and matches everything weakly —
past that size the vector carries less signal than the noise it adds. The
content stays reachable densely through the member's `Part` chunks. On pgjdbc
this affects 1.4% of chunks.

`embeddings.f32` is row-major so a single chunk's vector is one cache-friendly slice — friendly to both the in-memory and future-mmap paths.

The format carries the fields incremental updates need:

- `meta.json.tombstones: Vec<u32>` — chunk ids logically deleted (a removed or modified file's old chunks) but not yet compacted away.
- `FileRecord.chunk_start` / `chunk_end` — per-file `[start, end)` into `chunks.bin` so a delta can drop one file's chunks without rewriting the rest.

On `Index::open`, a non-empty delta is applied incrementally (`apply_delta`): the changed files' old chunks are tombstoned, the added/modified files are re-chunked, re-embedded, and appended, and BM25 is rebuilt over the live set — the untouched corpus is never re-embedded. A full rebuild happens only as a fallback when `apply_delta` errors, or as compaction once tombstones exceed `AST_BRO_COMPACTION_RATIO` (default 30%) of all chunk slots. The cheap detection path (mtime + size, only hashing on mismatch) keeps the per-open check affordable.

## Concurrency

`fs2` advisory lock at `.ast-bro/index/lock` — exclusive during writes. Two `search` calls at the same instant during a rebuild serialize; the loser sees the winner's update on its next read. All writes use `.tmp` + atomic rename so a SIGKILL mid-write leaves the previous index intact.

## Adding a new model

`ModelInfo::potion_code_16m()` is the only model wired in. To add another:

1. Add a constructor to `download::ModelInfo` listing its files (config.json, tokenizer.json, model.safetensors).
2. Verify the embedding tensor inside the safetensors is named `embeddings` and is `f32` or `f16` (model2vec convention). `f16` tensors are decoded to `f32` once at open time (`decode_f16_le` in `embed.rs`); any other dtype is rejected.
3. If the dimension differs from 256, the `DIM` constant in [`src/search/embed.rs`](../src/search/embed.rs) needs to follow. Most of the code is generic over `DIM`, but the const is the single source of truth — bumping it requires re-indexing existing repos (the schema check in `Meta::model.dim` will catch this and force a rebuild).

Measure the swap on [tests/eval](../tests/eval/README.md) before taking it. A
better public benchmark is not enough on its own: `potion-code-16M-v2` shares
this model's dimension and teacher and scores higher on CoIR (NDCG@10 39.08 vs
37.05), yet dropped recall@5 from 27% to 17% and MRR@10 from 0.223 to 0.117 on
the pinned pgjdbc set. CoIR leans on docstring-to-function tasks, and these
queries describe behaviour over a real codebase.

The `AST_OUTLINE_MODEL_SOURCE` env var lets ops point at a custom HF-compatible mirror without code changes.
