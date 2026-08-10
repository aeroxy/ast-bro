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

## CLI and MCP are one feature set

Each MCP tool in `src/mcp/tools.rs` mirrors a CLI subcommand, with underscores where the CLI uses hyphens (`reverse_deps` for `reverse-deps`, `find_related` for `find-related`). Only the maintenance commands (`install`, `uninstall`, `status`, `hook`, `prompt`, `mcp`) are CLI-only.

A flag added or renamed on one surface is unfinished until the other carries it. The CLI declares it once, as a clap derive in `src/lib.rs`. An MCP tool declares it twice: as a property in the JSON schema in `src/mcp/tools.rs`, and as a field of the tool's serde `Args` struct, which for `impact`, `context`, and the search tools sits beside the implementation (`src/impact.rs`, `src/context.rs`, `src/search/mcp.rs`). The structs accept unknown fields, so a property added only to the schema is advertised and ignored. No test compares the argument sets, so change all three in the same commit. State any intended difference in the tool description.

A default lives in `src/defaults.rs`, which both surfaces read. The tests in that module fail when a clap default, a serde default, or a schema description states a literal instead of naming the constant.

The surfaces do not match yet: `--compact` has no MCP property, `show` takes several targets on the CLI and a single `path` on MCP, and several other CLI flags have no property on their tool.
