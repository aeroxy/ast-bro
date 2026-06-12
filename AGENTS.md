## Preparing Release

1. Bump the version: `make bump-patch` (or `bump-minor` / `bump-major`)
2. Build and zip the release binary: `make release`
3. Update `Formula/ast-bro.rb` with the new SHA256: `make update-formula`

## WIKI

@wiki/architecture.md

The architecture page links to eight deeper wiki files — read them on-demand when your work touches that subsystem:

- [wiki/deps.md](wiki/deps.md) — dependency-graph internals (deps / reverse-deps / cycles / graph)
- [wiki/calls.md](wiki/calls.md) — call-graph internals (callers / callees / trace, three-pass resolver, unified graph cache, per-language extraction)
- [wiki/impact.md](wiki/impact.md) — cross-file impact analysis (`impact`): combines callers + callees + file deps + test detection in one command
- [wiki/context.md](wiki/context.md) — token-budgeted context (`context`): greedy knapsack over call graph for LLM-friendly context packs
- [wiki/search.md](wiki/search.md) — semantic search internals (BM25 + dense, chunking, on-disk format)
- [wiki/squeeze.md](wiki/squeeze.md) — log/text token compression (squeeze): multi-stage pipeline, reversible legend, degenerate fallback
- [wiki/network-security.md](wiki/network-security.md) — model download, TLS policy, mirror fallback
- [wiki/file-filtering.md](wiki/file-filtering.md) — what gets walked, ignore layers, escape hatches, shebang detection, test-file heuristics
