# Network & Security

`ast-bro` makes outbound HTTPS requests in exactly one situation: the first time you run `ast-bro search` (or `find-related`), it downloads the embedding model used for semantic search. Everything else — outline, digest, show, implements, MCP — is fully offline.

This page explains *what* it downloads, *where* the bytes come from, and *why* the TLS defaults are unusual.

## What gets downloaded

A single embedding model:

| Item | Source | Size |
|---|---|---|
| `config.json` | `https://huggingface.co/minishlab/potion-code-16M` | ~600 B |
| `tokenizer.json` | same | ~2 MB |
| `model.safetensors` | same | ~64 MB |

Stored under `~/.cache/ast-bro/models/potion-code-16M/` (see [cache layout](#cache-layout) below).

The model is `minishlab/potion-code-16M` — a static model2vec embedding (no neural net inference, just a vocab × 256 float32 lookup table). It runs on CPU in microseconds and stays mmap'd in memory while ast-bro is running.

## Mirror fallback

`huggingface.co` is blocked by many corporate networks. To keep things working:

1. ast-bro first probes `https://huggingface.co/.../config.json` with a 3-second timeout.
2. If the probe fails (timeout, DNS failure, content-length anomaly from a captive portal), it transparently falls back to `https://hf-mirror.com/.../config.json`.
3. `hf-mirror.com` is a community-run URL-rewrite mirror of HuggingFace — it serves the exact same bytes from a different domain.

Both attempts share the same SHA-256 integrity check, so the fallback path produces a bit-identical local cache.

## TLS verification policy (the unusual part)

**TLS certificate verification is DISABLED by default for model downloads.**

This is intentional. The reason:

> Many regulated environments — banks, financial institutions, large enterprises, government — install a corporate "TLS-intercepting" proxy on every employee laptop. The proxy man-in-the-middles every outbound HTTPS connection: it terminates the user's TLS session, presents its own self-signed certificate signed by an internal CA, makes its own outbound TLS session to the real server, and hands the decrypted/re-encrypted bytes back to the user. This is non-negotiable in those environments — compliance requires that all outbound traffic be inspectable.

Strict TLS verification fails universally there, because the corporate CA isn't in any standard trust store. We could refuse to work in those environments, or we could accept the trade-off. We picked the trade-off because:

1. **Search is opt-in** — users who never run `search` never make any outbound request, so this policy has zero impact on them.
2. **Integrity is enforced via SHA-256, not TLS** — after first download, ast-bro writes a `manifest.json` with a SHA-256 hash of every file. Subsequent loads verify each cached file against the manifest before using it. A network attacker who tampers with the *first* download will succeed; tampering with subsequent loads will be detected.
3. **The bytes are reproducible** — anyone can verify the model files against HuggingFace's published hashes if they're paranoid about a first-time MITM.

A loud stderr warning fires on every download in non-strict mode:

```
ast-bro: TLS certificate verification is DISABLED for model downloads
(works through corp MITM proxies). Set AST_OUTLINE_TLS_STRICT=1 to enforce
full chain verification. Integrity is checked via SHA-256 on subsequent loads.
```

### Opting back into strict TLS

Two env vars let you tighten the policy:

| Env var | Effect |
|---|---|
| `AST_OUTLINE_TLS_STRICT=1` | Enforce full TLS chain verification. Will fail behind a corp MITM proxy unless the corp CA is in the OS trust store. |
| `AST_OUTLINE_CA_BUNDLE=/path/to/ca.pem` | Add extra root CAs from a PEM file. Compatible with either strict or non-strict mode — most useful with strict, when you want to validate the corp MITM cert chain. |

For a security-conscious user behind a corp proxy:

```bash
export AST_OUTLINE_TLS_STRICT=1
export AST_OUTLINE_CA_BUNDLE=/usr/local/share/ca-certificates/corp-mitm.crt
ast-bro search "anything" .   # validates against the corp CA
```

## Cache layout

```
~/.cache/ast-bro/models/potion-code-16M/
├── config.json           # model config
├── tokenizer.json        # huggingface tokenizers serialized form
├── model.safetensors     # ~64 MB of f32 weights, mmap'd at runtime
└── manifest.json         # { "sha256": {...}, "source": "hf" | "hf-mirror" }
```

Cache root is `dirs::cache_dir()` (XDG-respecting) joined with `ast-bro/models`. Override with `AST_OUTLINE_MODEL_DIR=/some/path`.

## Pinning the source

If you don't want the auto-probe (e.g. CI where you know HF is reachable, or a closed network where only the mirror works), pin it:

| `AST_OUTLINE_MODEL_SOURCE` value | Effect |
|---|---|
| *(unset)* | Default — probe HF, fall back to hf-mirror.com |
| `hf` | Always use `https://huggingface.co` |
| `hf-mirror` | Always use `https://hf-mirror.com` |
| `https://your.mirror/` | Use a custom HF-compatible base URL |

## What does NOT make network requests

For clarity:

- `ast-bro` (default outline command) — no network
- `ast-bro digest` — no network
- `ast-bro show` — no network
- `ast-bro implements` — no network
- `ast-bro mcp` (server) — no network at startup; the `search`/`find_related`/`index` MCP tools download the model on first call (same path as the CLI)
- `ast-bro install` / `uninstall` / `status` — no network

Only `search`, `find-related`, and `index` touch the network, and only on first run (or when the cache is invalid).
