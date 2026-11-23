# Network & Security

`ast-bro` makes outbound HTTPS requests in exactly one situation: the first time you run `ast-bro search` (or `find-related`), it downloads the embedding model used for semantic search. Everything else — outline, digest, show, implements, MCP — is fully offline.

This page explains *what* it downloads, *where* the bytes come from, and *why* the TLS defaults are unusual.

## What gets downloaded

A single embedding model:

| Item | Source | Size |
|---|---|---|
| `config.json` | `https://huggingface.co/minishlab/potion-code-16M` | 97 B |
| `tokenizer.json` | same | ~1 MB |
| `model.safetensors` | same | ~64 MB |

All three come from one pinned commit of that repository, not from `main`, and ast-bro carries the SHA-256 each file has there (`ModelInfo::potion_code_16m` in `src/search/download.rs`). Every install therefore reads the same bytes, and a re-upload upstream cannot change the model under existing indexes.

Stored under `~/.cache/ast-bro/models/potion-code-16M/` (see [cache layout](#cache-layout) below).

The model is `minishlab/potion-code-16M` — a static model2vec embedding (no neural net inference, just a vocab × 256 float32 lookup table). It runs on CPU in microseconds and stays mmap'd in memory while ast-bro is running.

## Mirror fallback

`huggingface.co` is blocked by many corporate networks. To keep things working:

1. ast-bro first probes `https://huggingface.co/.../config.json` with a 3-second timeout.
2. If the probe fails (timeout, DNS failure, content-length anomaly from a captive portal), it transparently falls back to `https://hf-mirror.com/.../config.json`.
3. `hf-mirror.com` is a community-run URL-rewrite mirror of HuggingFace — it serves the exact same bytes from a different domain.

Both are checked against the same pinned SHA-256s, so either path produces a bit-identical local cache.

## TLS verification policy (the unusual part)

**TLS certificate verification is DISABLED by default for model downloads.**

This is intentional. The reason:

> Many regulated environments — banks, financial institutions, large enterprises, government — install a corporate "TLS-intercepting" proxy on every employee laptop. The proxy man-in-the-middles every outbound HTTPS connection: it terminates the user's TLS session, presents its own self-signed certificate signed by an internal CA, makes its own outbound TLS session to the real server, and hands the decrypted/re-encrypted bytes back to the user. This is non-negotiable in those environments — compliance requires that all outbound traffic be inspectable.

Strict TLS verification fails universally there, because the corporate CA isn't in any standard trust store. We could refuse to work in those environments, or we could accept the trade-off. We picked the trade-off because:

1. **Search is opt-in** — users who never run `search` never make any outbound request, so this policy has zero impact on them.
2. **Integrity is enforced by pinned SHA-256s, not TLS** — each file streams to a temporary file and is checked against the SHA-256 ast-bro carries for it before it enters the cache. A proxy or network attacker can block the download, but cannot substitute the model, including on the first download.
3. **The bytes are reproducible** — the pinned commit and hashes are in the source, and HuggingFace publishes the same hashes for that commit.

A loud stderr warning fires on every download in non-strict mode:

```text
ast-bro: TLS certificate verification is DISABLED for model downloads
(works through corp MITM proxies). Set AST_BRO_TLS_STRICT=1 to enforce
full chain verification. Every file is still checked against the SHA-256
pinned in ast-bro before it is used.
```

### Opting back into strict TLS

Two env vars let you tighten the policy:

| Env var | Effect |
|---|---|
| `AST_BRO_TLS_STRICT=1` | Enforce full TLS chain verification. Will fail behind a corp MITM proxy unless the corp CA is in the OS trust store. |
| `AST_BRO_CA_BUNDLE=/path/to/ca.pem` | Add extra root CAs from a PEM file. Compatible with either strict or non-strict mode — most useful with strict, when you want to validate the corp MITM cert chain. |

For a security-conscious user behind a corp proxy:

```bash
export AST_BRO_TLS_STRICT=1
export AST_BRO_CA_BUNDLE=/usr/local/share/ca-certificates/corp-mitm.crt
ast-bro search "anything" .   # validates against the corp CA
```

Every `AST_BRO_*` variable on this page also answers to its pre-rename `AST_OUTLINE_*` name.

## Cache layout

```text
~/.cache/ast-bro/models/potion-code-16M/
├── config.json           # model config
├── tokenizer.json        # huggingface tokenizers serialized form
├── model.safetensors     # ~64 MB of f32 weights, mmap'd at runtime
└── manifest.json         # { "sha256": {...}, "stamps": {...}, "source": "hf" | "hf-mirror" | "custom" | "local" }
```

`stamps` records the size and mtime each file had when it was last verified. A load trusts a file whose stamp still matches and hashes anything else against its pinned SHA-256, so a truncated, partly written or edited file is caught without hashing 64 MB on every load. That hash used to run on every search and took most of a warm CLI search's time. A manifest from before stamps existed is hashed once and then gains them.

Cache root is `dirs::cache_dir()` (XDG-respecting) joined with `ast-bro/models`. Override with `AST_BRO_MODEL_DIR=/some/path`. For an offline machine, copy the three files into `<that path>/potion-code-16M/`; they are hashed against the pinned values on first use, so no manifest is needed.

## Pinning the source

If you don't want the auto-probe (e.g. CI where you know HF is reachable, or a closed network where only the mirror works), pin it:

| `AST_BRO_MODEL_SOURCE` value | Effect |
|---|---|
| *(unset)* | Default — probe HF, fall back to hf-mirror.com |
| `hf` | Always use `https://huggingface.co` |
| `hf-mirror` | Always use `https://hf-mirror.com` |
| `https://your.mirror/` | Use a custom HF-compatible base URL. It must serve `<id>/resolve/<commit>/<file>`, since ast-bro asks for the pinned commit rather than `main`. |

## What does NOT make network requests

For clarity:

- `ast-bro` (default outline command) — no network
- `ast-bro digest` — no network
- `ast-bro show` — no network
- `ast-bro implements` — no network
- `ast-bro mcp` (server) — no network at startup; the `search`/`find_related`/`index` MCP tools download the model on first call (same path as the CLI)
- `ast-bro install` / `uninstall` / `status` — no network

Only `search`, `find-related`, and `index` touch the network, and only on first run (or when the cache is invalid).
