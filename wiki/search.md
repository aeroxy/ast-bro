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

Smaller is not better past that. Halving the size target to 750 characters,
the figure semble moved to, measured clearly worse on the five pinned sets:
recall@5 37 → 24 and MRR@10 0.170 → 0.095 over 150 queries (14 up, 34 down),
answered@8k flat (47 → 48), and 1.6–1.9× as many chunks. Semble scores
file-level relevance, where chunk size matters less; these sets score line
spans against methods of 21–150 lines.

Elixir has no declaration kinds to match (`defmodule`, `def` and `test "…" do`
are all `call`s), so it gets its own walk (`chunker/elixir.rs`) that recognises
members by macro name, as the `map` adapter does.

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

A long outline is split at `OUTLINE_MAX_CHARS = 3000`, well under
`MAX_EMBED_CHARS`. An outline above that cap is dropped from the dense index
entirely, and the files with the longest outlines are the large central ones —
leaving them unembedded made phase-one file retrieval miss exactly
`QueryExecutorImpl`, `PgConnection` and `PgResultSet`. Splitting them raised
phase-one file recall@5 from 33% to 60%, and top-1 from 23% to 40%.

A piece is not under the embedder's token window, though: a third to a half of
3000-char pieces run past `MAX_EMBED_TOKENS`, so their last signatures reach
BM25 but not the vector. Sizing pieces to fit, at 1200 or 2000 characters,
measured as noise: 5 up and 5 down, and 7 up and 5 down with recall@5 one
lower.

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
├── chunker/elixir.rs  Elixir member walk, by macro name (no decl node kinds)
├── download.rs    HF probe + hf-mirror fallback, pinned commit + SHA-256s,
│                  size/mtime-stamped manifest
├── embed.rs       safetensors mmap + tokenizer.json + cosine_topk (SIMD via wide)
├── wordpiece.rs   the tokenizer: BERT WordPiece, id for id with `tokenizers`
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
  ids = tokenizer.encode(text, add_special_tokens=False)[:MAX_EMBED_TOKENS]   # 512
  mean = average(embeddings[mapping[id]] for id in ids)                     # unweighted
  return L2_normalize(mean)
```

That's it. Cost is dominated by tokenization (~10–100 µs); embedding lookup is essentially free. Output is always L2-normalized so cosine similarity reduces to a dot product.

### Window and weights: what we measured

That is not quite model2vec's own `encode`, in two ways, both on purpose.
model2vec multiplies each row by the model's per-token `weights` tensor
(punctuation near 0.01, identifiers near 1) and drops `[UNK]`; `encode_one`
takes the unweighted mean. And the 512-token window is `MAX_EMBED_TOKENS` in
`embed.rs`, applied by `encode_one`, not the truncation setting
`tokenizer.json` happens to ship, so a model that ships none cannot quietly
change ranking. `mapping`, model2vec's token-to-row table, is applied; it is
the identity for potion-code-16M.

Measured on all five pinned sets, 150 queries, IoU 0.3. Hybrid search, as
users get it:

| variant | recall@5 | MRR@10 | answered@8k | queries up / down |
| --- | --- | --- | --- | --- |
| **unweighted, 512 tokens (shipped)** | **37** | **0.170** | 47 | – |
| window 256 / 384 / 768 | 37 / 37 / 36 | 0.153 / 0.161 / 0.168 | 46 / 47 / 46 | 12/17, 5/5, 7/5 |
| no window | 34 | 0.163 | 45 | 7 / 9 |
| model2vec's weights | 35 | 0.156 | 43 | 9 / 20 |
| weights, no window (semble's setup) | 35 | 0.147 | 43 | 7 / 18 |

Dense retrieval alone, cosine top-10 with nothing else, says the same about
the weights, so it is not the ranking constants having been tuned to the
unweighted vectors: MRR@10 0.152 unweighted, 0.123 weighted (14 up, 26 down).
Without the window it is 0.152 too. Softening the weights to `weights^0.25`
landed in the noise, and `^0.5` lost.

`meta.json` records the window and the pooling with the model, and `load`
rejects an index built with others, so changing either rebuilds rather than
mixing vectors made two ways. A meta from before they were recorded reads as
512 and `mean`, what built it.

`Embedder::open(model_dir)` mmaps `model.safetensors` (~64 MB) — the matrix stays paged in but never copied. `vocab × 256 × 4 bytes` = ~64 MB regardless of repo size.

`f16` tensors are also accepted and decoded once into an owned `f32` buffer at
open time, so nothing downstream of `Embedder` has to know the on-disk dtype.

### Tokenizer

ast-bro tokenizes with its own WordPiece (`wordpiece.rs`), not the HuggingFace
`tokenizers` crate. It reads the vocabulary and added tokens from the model's
`tokenizer.json` and reproduces `tokenizers` 0.21's `encode` id for id:
`[UNK]` cut from the raw text, `BertNormalizer`, `[PAD]` cut from the
normalized text with its `single_word`, `lstrip` and `rstrip` rules,
`BertPreTokenizer`, then greedy WordPiece. It uses the same Unicode table
crates and the same `aho-corasick` and `regex` matching, so the two agree on
every character. A fixture test against `tokenizers` (kept as a
dev-dependency) holds it to that in CI. Run by hand with the real
`tokenizer.json`, it matched on every file of this repository, semble and
the five pinned corpora: 9,527 files and about 22.8 million tokens. `load`
refuses a `tokenizer.json` describing any other pipeline, and added tokens it
would not match as `tokenizers` does: two that share an id or match as the
same text, or one that starts or ends with whitespace.

The reasons were cost, not behaviour. `Tokenizer::from_file` took 9–14 ms of
every CLI search, against about 5 ms for reading just what WordPiece needs.
Tokenization, which is 95% of embedding CPU, ran about five times faster on
an ILSpy rebuild (embedding 1.0 s → 0.2 s). The binary also drops the
Oniguruma C library and 1.8 MB. `encode` stops once it has
`MAX_EMBED_TOKENS` ids, so a long chunk skips WordPiece work on the words
it would discard. Raw-text splitting and normalization still scan the whole
chunk.

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
    ├── meta.json            # ~2 KB — schema, model (id, dim, token window, pooling), chunk_count, tombstones
    ├── chunks.bin           # bincode Vec<Chunk> (~1.5 KB/chunk × N)
    ├── embeddings.f32       # N × 256 × 4 bytes, header-less, little-endian
    ├── bm25.bin             # bincode Bm25Index (vocab + idf + postings)
    ├── files.bin            # bincode Vec<FileRecord> (path + mtime + size + hash + chunk range)
    └── lock                 # advisory exclusive lock during writes
```

Loader refuses if `meta.json.schema != "ast-bro.search-index.v4"`, the model id, token window or pooling mismatches, `ast_bro_version` differs from the binary, `meta.json.chunk_count != chunks.len()`, or `chunks.len() × 256 × 4 != len(embeddings.f32)`. Each binary is read via bincode with `serde::Deserialize`. Embeddings are read into memory (mmap is a later swap that won't change the format).

Schema v2 adds `breadcrumb` and `kind` to `Chunk`, which changes the bincode
layout of `chunks.bin`. A v1 index cannot be decoded, so the loader rejects it
outright — including the pre-rename `ast-outline.*` names — and the caller
rebuilds. There is no migration path and there does not need to be: the index
is a cache.

Schema v3 chunks Elixir per definition. The layout is unchanged, but a refresh
re-chunks only the files that changed, so a v2 index would keep every unchanged
Elixir module as one chunk until `--rebuild`.

The loader also rejects an index that another version wrote: `meta.json`
records `ast_bro_version`, and a chunker change reaches every file with the
next release (issue #81). So the schema changes only with the layout now. Two
builds of one version share an index; `ast-bro index --rebuild` covers that
during review.

Schema v4 marks that check. The layout is unchanged, but a binary from before
it checks only the schema: after a downgrade it would read a v3 index that a
newer version chunked, and keep those chunks for every file that did not
change. A v3 index from a `main` build carries the same version as the next
release, so the bump also rebuilds it once.

A rebuild of a rejected index keeps the corpus that its `meta.json` records,
widened to the request the way `ast-bro index` widens it. The meta decodes
when the binaries do not. Without this, the first `search <dir>` after a schema
bump rebuilt only `<dir>`, and later searches from the root missed every other
file.

A chunk above `MAX_EMBED_CHARS = 6000` gets a non-finite embedding rather than a
real one, so `cosine_topk` drops it and only BM25 can retrieve it. A zero vector
would not do: its dot product is a finite `0.0`, which outranks every negative
similarity and enters the pool whenever the corpus is smaller than the pool.
The cap counts UTF-8 **bytes** of the chunk content plus its breadcrumb and the
newline after it — not characters or tokens — so e.g. 2001 copies of `界` are
already over it. This byte cutoff is separate from the 512-token window below.
Below the cap, a chunk's vector averages its first 512 tokens, so a long chunk
is represented by its head; 512 tokens is often 1.5–2 KB of code. Past the
cap, content stays reachable densely only where the member's `Part` chunks
re-cover it, and only within each part's window. A chunk with no parts, such
as a paragraph of a file with no grammar (a TOML table with no blank line in
it), is reachable only through BM25. On
pgjdbc the cap affects 1.4% of chunks. Whether it still helps under the window
is unmeasured; with every chunk embedded in full and weighted, dropping it cost
answered@8k 43 → 41 and @32k 48 → 46.

`embeddings.f32` is row-major so a single chunk's vector is one cache-friendly slice — friendly to both the in-memory and future-mmap paths.

The format carries the fields incremental updates need:

- `meta.json.tombstones: Vec<u32>` — chunk ids logically deleted (a removed or modified file's old chunks) but not yet compacted away.
- `FileRecord.chunk_start` / `chunk_end` — per-file `[start, end)` into `chunks.bin` so a delta can drop one file's chunks without rewriting the rest.

On `Index::open`, a non-empty delta is applied incrementally (`apply_delta`): the changed files' old chunks are tombstoned, the added/modified files are re-chunked, re-embedded, and appended, and BM25 is rebuilt over the live set — the untouched corpus is never re-embedded. A full rebuild happens only as a fallback when `apply_delta` errors, or as compaction once tombstones exceed `AST_BRO_COMPACTION_RATIO` (default 30%) of all chunk slots. The cheap detection path (mtime + size, only hashing on mismatch) keeps the per-open check affordable.

## Concurrency

`fs2` advisory lock at `.ast-bro/index/lock` — exclusive during writes. Two `search` calls at the same instant during a rebuild serialize; the loser sees the winner's update on its next read. Each file is written to `.tmp` and renamed into place, so no file is ever half-written, but the five are not replaced together. `meta.json` goes last, so a SIGKILL or a failed write between two renames leaves the previous meta beside newer files; the loader rejects that mix when the encoder settings or the chunk count changed, and the next open rebuilds. A mix that keeps both still loads, for instance new chunks beside the previous build's vectors (issue #119).

## Adding a new model

`ModelInfo::potion_code_16m()` is the only model wired in. To add another:

1. Add a constructor to `download::ModelInfo` naming the upstream commit to pin and the SHA-256 of each file at that commit (config.json, tokenizer.json, model.safetensors). The Hub API lists the LFS hashes (`/api/models/<id>/tree/<commit>`); hash the small non-LFS files yourself, from both huggingface.co and hf-mirror.com.
2. Verify the embedding tensor inside the safetensors is named `embeddings` and is `f32` or `f16` (model2vec convention). `f16` tensors are decoded to `f32` once at open time (`decode_f16_le` in `embed.rs`); any other dtype is rejected. Check for model2vec's side tensors too: `mapping` is applied, and `weights` is ignored on purpose (see [Window and weights](#window-and-weights-what-we-measured)), so measure a model that ships them before assuming it reads as intended. Check the tokenizer's own truncation as well; `MAX_EMBED_TOKENS` overrides it.
3. If the dimension differs from 256, the `DIM` constant in [`src/search/embed.rs`](../src/search/embed.rs) needs to follow. Most of the code is generic over `DIM`, but the const is the single source of truth — bumping it requires re-indexing existing repos (the schema check in `Meta::model.dim` will catch this and force a rebuild).
4. Load the candidate's `tokenizer.json` with `WordPiece::load` before benchmarking anything. It refuses any pipeline but BERT WordPiece with potion-code-16M's settings (another normalizer, pre-tokenizer or a post-processor, or added tokens that disagree with the vocabulary), and such a model needs a compatible export or a tokenizer change first. If it loads, run the ignored `network_matches_tokenizers_on_real_text` test with `AST_BRO_PARITY_TOKENIZER` set to its path, to confirm id-for-id parity with `tokenizers`; without that variable the test checks the pinned model. The test skips non-UTF-8 files and files over 2 MB, printing the paths of the latter — check that list before trusting a pass.
5. Select the candidate at every site that currently calls `ModelInfo::potion_code_16m()` — the full build, both load-path calls (the id check and the embedder open), and `ModelMeta::current()` — and rebuild the evaluation indexes with it. Parity only proves the tokenizer agrees; a benchmark run against stock weights measures the wrong model.

Measure the swap on [tests/eval](../tests/eval/README.md) before taking it. A
better public benchmark is not enough on its own: `potion-code-16M-v2` shares
this model's dimension and teacher and scores higher on CoIR (NDCG@10 39.08 vs
37.05), yet dropped recall@5 from 27% to 17% and MRR@10 from 0.223 to 0.117 on
the pinned pgjdbc set. CoIR leans on docstring-to-function tasks, and these
queries describe behaviour over a real codebase. That comparison read v2 as
designed, since v2 bakes its weighting into the rows and ships no `weights`
tensor, and v1 without its `weights`; reading v1 with them only scores lower.

The `AST_BRO_MODEL_SOURCE` env var lets ops point at a custom HF-compatible mirror without code changes; the mirror has to serve the pinned commit. See [network-security.md](network-security.md).
