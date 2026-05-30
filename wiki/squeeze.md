# Squeeze (log/text compression)

`sb squeeze` compresses repetitive **log/text** content with a *reversible* legend and emits the squeezed form directly. It ports the pure `Compressor` pipeline from `../logs-tokenizer` (`src/core/mod.rs`) verbatim — same stages, same tuning constants — and wraps it in this repo's CLI/MCP conventions. For internals and the design rationale see [refs/plan_tokenizer.md](../refs/plan_tokenizer.md).

## When to use it (and when not to)

This is a tool for **logs and text**, not code. `squeeze` keeps the full `logs-tokenizer` pipeline because the user invokes it intentionally on log-like input, where 70–80% of the content is genuinely repetitive (timestamps, component tags, near-identical lines).

For **code**, the "compression" is already structural and lives elsewhere — reach for `map` / `digest` / `show` (drop bodies, keep shapes) instead. `squeeze` makes no attempt to compress source code.

## What it does

The engine runs an ordered set of stages, each replacing a recurring substring or pattern with a short tag and recording the mapping in a legend:

1. **Leading-zero trim** — `0x0001` → `0x1`.
2. **Timestamp dictionary** — the most-frequent ISO8601 prefix → `#T#`.
3. **Component / key extraction** — frequent `[Tag]` / `key=` → `#0#`, `#1#`, …
4. **Base62 tag names** — `#10#` → `#a#` to shrink tag width.
5. **BPE** — repeated token sequences → `#n#` (normal) and repeated sequences *of tags* → `!n!` (meta).
6. **Macro templating** — lines differing by a single tag → `&1=…`, referenced as `&1:C`.
7. **Tag-sequence macros** — repeated multi-tag runs → macros.
8. **Dedup** — identical consecutive lines → `… xN`.

Tag assignment is **deterministic** (sort by savings, then first-seen), so output is stable and testable.

## Reversible by design

The **legend** is printed (comment-prefixed) *before* the body so a consumer reads the dictionary first. Because every replacement is recorded, the squeezed output round-trips back to the original text — a downstream model gets the exact content, just smaller.

## Size proxy: chars/bytes, not tokens

The comparison unit is **chars/bytes**, matching `logs-tokenizer` and this repo's stance (see `src/core.rs`) that token estimates mislead because tokenizers vary. The savings line is always printed — `# squeezed 45.0KB → 10.2KB (-77.3%)` — purely because the number is the satisfying/auditable part, not because any decision depends on it.

## The one fallback: the degenerate floor

There is exactly one fallback. If the squeezed **body + legend** ends up **larger** than the raw input (tiny inputs, all-unique lines), `squeeze` falls back to emitting the raw text and notes it:

```text
# app.log  [raw 412B; squeeze would be larger, emitting original]
---
<raw body>
```

The size comparison **must include legend bytes** in the squeezed total — otherwise the floor would lie. This single `if` is essentially free and prevents "squeeze made it bigger."

There is no dual-render, no "show both," no pick-smaller feature: the user ran `squeeze` because they want it squeezed.

## Usage

```bash
sb squeeze <file> [from:to] [--raw] [--json] [--compact]
```

- `<file>` — path to read. A single explicit file is read directly (`read_to_string`); `squeeze` does not walk a directory or use the `file_filter` pipeline.
- `[from:to]` — optional 1-indexed inclusive line range, clamped to file bounds. Also accepts `from`, `from:`, `:to`. Useful for squeezing a slice of a huge log.
- `--raw` — escape hatch: skip compression and behave like `cat`-with-header, for diffing or inspecting the original.
- `--json` / `--compact` — structured output under schema `ast-bro.squeeze.v1` (`pretty = !compact`). The legend is always included (empty when the raw fallback fires).

### Text output (normal case)

```text
# app.log  [squeezed 45.0KB → 10.2KB, -77.3%]
# legend:
#   #T# = 2026-05-30T11:54:
#   #0# = [WinFocusMonitor]
#   #1# = hwnd=
#   &1  = #T#19.557 #0##@##8##81# #90#
---
<squeezed body>
```

### Edge cases

- **Unreadable file** — `read_to_string` failures are surfaced directly: directories note `path is a directory`, and other I/O failures print `could not read ...`.
- **Range out of bounds** — clamped; a start past EOF yields an empty body plus a note.
- **Empty / tiny slice** — legend overhead guarantees a loss, so the degenerate floor emits raw.
