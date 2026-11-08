## Preparing Release

1. Bump the version with `make bump-patch` (or `bump-minor` / `bump-major`) and commit the updated manifests, wrappers, formula URL, and `Cargo.lock`.
2. Push the matching version tag (for example, `4.2.1`, without a `v` prefix). Push the tag first; do not create or publish a GitHub release by hand beforehand.
3. Wait for the `Release packages` workflow to build and test both platforms and attach their ZIPs to a draft release.
4. Run `make update-formula` and commit the formula's checksum. This requires authenticated `gh` access to the draft and hashes its uploaded macOS asset, not a local build. Keep the existing version tag on the tested commit.
5. Review and publish the draft GitHub release, then publish the matching npm and PyPI packages.

See [docs/releasing.md](docs/releasing.md) for local validation and release details. `make release` is for local macOS testing; its ZIP must not supply the published Homebrew checksum.

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
