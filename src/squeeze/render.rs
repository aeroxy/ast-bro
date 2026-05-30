//! Renderers + emit-decision for `sb squeeze`.
//!
//! This module is the single source of truth for the squeeze emit decision and
//! output formatting. Both `render_text` and `render_json` run the same pipeline:
//!
//! 1. Call [`crate::squeeze::squeeze`] on the (already line-sliced) `raw` text.
//! 2. Assemble the squeezed output as `legend block + body` and measure it in bytes.
//! 3. Apply the **degenerate floor**: if `--raw` was requested, OR the squeezed
//!    total (legend block **and** its `# legend:` marker **included**) is `>=` the
//!    raw byte length, emit the raw input instead. Every squeezed-only byte MUST
//!    be counted or the floor lies about being smaller.
//! 4. Format as text or JSON.

use colored::Colorize;
use serde::Serialize;

use crate::squeeze::{squeeze, Squeezed};

/// Stable JSON schema identifier — bump on breaking changes.
pub const JSON_SCHEMA_SQUEEZE: &str = "ast-bro.squeeze.v1";

/// Everything the renderers need. `raw` is already line-sliced by the caller
/// (CLI / MCP) via [`slice_lines`]; the renderers operate on it directly.
pub struct SqueezeReport<'a> {
    pub path: &'a str,
    /// 1-indexed inclusive range, as resolved. `None` => whole file.
    pub range: Option<(usize, usize)>,
    /// Raw text, ALREADY line-sliced.
    pub raw: &'a str,
    /// `--raw` => never compress (escape hatch).
    pub raw_requested: bool,
}

/// What the decision logic resolved to. Internal to this module.
struct Emit {
    /// `true` if the squeezed form won and is being emitted.
    squeezed: bool,
    /// The body to print (squeezed body, or the raw text on fallback).
    body: String,
    /// Legend pairs (tag, original) — empty when emitting raw.
    legend: Vec<(String, String)>,
    /// Byte length of the raw input.
    raw_bytes: usize,
    /// Byte length of what we actually emit (legend block + body when squeezed;
    /// raw length when not).
    emitted_bytes: usize,
}

/// The marker line printed immediately before the legend in text output. It is
/// emitted ONLY on the squeezed branch, so it must be counted in the
/// degenerate-floor size comparison (see [`decide`]) and in `emitted_bytes`.
const LEGEND_MARKER: &str = "# legend:\n";

/// Render the legend lines exactly as they appear in the text output (each
/// line `#   <tag> = <value>` plus a trailing newline). Used both for display
/// and — critically — for the byte accounting that drives the degenerate floor.
fn legend_block(legend: &[(String, String)]) -> String {
    let mut s = String::new();
    for (tag, value) in legend {
        s.push_str("#   ");
        s.push_str(tag);
        s.push_str(" = ");
        s.push_str(value);
        s.push('\n');
    }
    s
}

/// The core emit decision. Single source of truth shared by text + JSON.
fn decide(r: &SqueezeReport) -> Emit {
    let raw_bytes = r.raw.len();

    // --raw escape hatch: never compress.
    if r.raw_requested {
        return Emit {
            squeezed: false,
            body: r.raw.to_string(),
            legend: Vec::new(),
            raw_bytes,
            emitted_bytes: raw_bytes,
        };
    }

    let Squeezed { body, legend } = squeeze(r.raw);

    // Degenerate floor: the squeezed *total* must include every byte that the
    // squeezed branch emits beyond the raw branch — that's the legend block AND
    // its `# legend:` marker line (the `---` separator is common to both branches
    // so it cancels). Omitting the marker lets a sub-marker-sized gain report a
    // "win" while the emitted payload is actually larger than raw.
    let legend_bytes = LEGEND_MARKER.len() + legend_block(&legend).len();
    let squeezed_total_bytes = legend_bytes + body.len();

    // An empty legend means nothing was actually substituted — any apparent
    // "savings" comes only from the pipeline's lossy whitespace/dedup trimming,
    // not a real reversible compression. Emitting that as a win would be both
    // misleading and lossy (a dropped trailing newline can read as "smaller"),
    // so treat it as the degenerate case and fall back to raw.
    if legend.is_empty() || squeezed_total_bytes >= raw_bytes {
        Emit {
            squeezed: false,
            body: r.raw.to_string(),
            legend: Vec::new(),
            raw_bytes,
            emitted_bytes: raw_bytes,
        }
    } else {
        Emit {
            squeezed: true,
            body,
            legend,
            raw_bytes,
            emitted_bytes: squeezed_total_bytes,
        }
    }
}

/// Human-readable byte size: `412B`, `45.0KB`, `1.2MB`. Decimal (1000) units,
/// e.g. `45.0KB → 10.2KB`.
fn fmt_bytes(n: usize) -> String {
    const KB: f64 = 1000.0;
    const MB: f64 = 1000.0 * 1000.0;
    let f = n as f64;
    if f < KB {
        format!("{}B", n)
    } else if f < MB {
        format!("{:.1}KB", f / KB)
    } else {
        format!("{:.1}MB", f / MB)
    }
}

/// Percentage saved going from `raw` to `emitted`, e.g. `77.3`. Positive means
/// smaller. Guards against div-by-zero on empty input.
fn savings_pct(raw_bytes: usize, emitted_bytes: usize) -> f64 {
    if raw_bytes == 0 {
        return 0.0;
    }
    (1.0 - (emitted_bytes as f64 / raw_bytes as f64)) * 100.0
}

/// Slice `text` by an optional 1-indexed inclusive line range, clamped to
/// bounds. `None` => the whole text. Shared by CLI + MCP so the slice they hand
/// to a [`SqueezeReport`] is identical to what the renderers assume.
///
/// Out-of-bounds is clamped; a `start` past EOF yields an empty string. The
/// returned slice preserves the original line terminators where present (a
/// trailing newline on the last selected line is kept iff it existed in the
/// source), so round-tripping a full-file slice is byte-identical to the input.
pub fn slice_lines(text: &str, range: Option<(usize, usize)>) -> String {
    let (start, end) = match range {
        None => return text.to_string(),
        Some(r) => r,
    };
    let start = start.max(1);
    if end < start {
        return String::new();
    }

    text.split_inclusive('\n')
        .skip(start - 1)
        .take(end - start + 1)
        .collect()
}

/// Text rendering: muted-truecolor header, then the comment-prefixed
/// legend, then a `---` separator, then the body.
pub fn render_text(r: &SqueezeReport) -> String {
    let e = decide(r);

    // Header line. Colored exactly like map/show line suffixes (truecolor 150).
    let header_plain = if e.squeezed {
        let pct = savings_pct(e.raw_bytes, e.emitted_bytes);
        format!(
            "# {}  [squeezed {} -> {}, {:.1}%]",
            r.path,
            fmt_bytes(e.raw_bytes),
            fmt_bytes(e.emitted_bytes),
            -pct, // print as a negative delta, e.g. -77.3%
        )
    } else if r.raw_requested {
        // --raw escape hatch.
        format!("# {}  [raw {}]", r.path, fmt_bytes(e.raw_bytes))
    } else {
        // Degenerate fallback: squeeze would have been larger.
        format!(
            "# {}  [raw {}; squeeze would be larger, emitting original]",
            r.path,
            fmt_bytes(e.raw_bytes),
        )
    };
    let header = header_plain.truecolor(150, 150, 150).to_string();

    let mut out = String::new();
    out.push_str(&header);
    out.push('\n');

    // Legend block (only when squeezed and non-empty). Uncolored so it copies
    // cleanly. Prefixed with the `# legend:` marker.
    if e.squeezed && !e.legend.is_empty() {
        out.push_str(LEGEND_MARKER);
        out.push_str(&legend_block(&e.legend));
    }

    // Separator + body. The body already carries its own line terminators; a
    // single newline before it keeps the `---` on its own line.
    out.push_str("---\n");
    out.push_str(&e.body);
    out
}

/// JSON document for `ast-bro.squeeze.v1`.
#[derive(Serialize)]
struct JsonSqueezeDoc<'a> {
    schema: &'static str,
    path: &'a str,
    range: Option<JsonRange>,
    raw_bytes: usize,
    squeezed_bytes: usize,
    savings_pct: f64,
    emitted: &'static str,
    legend: Vec<JsonLegendEntry<'a>>,
    body: &'a str,
}

#[derive(Serialize)]
struct JsonRange {
    start: usize,
    end: usize,
}

#[derive(Serialize)]
struct JsonLegendEntry<'a> {
    tag: &'a str,
    value: &'a str,
}

/// JSON rendering. `pretty = !compact`. Legend always present (empty when
/// emitting raw). `savings_pct` rounded to one decimal to match the text line.
pub fn render_json(r: &SqueezeReport, pretty: bool) -> String {
    let e = decide(r);
    let pct = (savings_pct(e.raw_bytes, e.emitted_bytes) * 10.0).round() / 10.0;

    let doc = JsonSqueezeDoc {
        schema: JSON_SCHEMA_SQUEEZE,
        path: r.path,
        range: r.range.map(|(start, end)| JsonRange { start, end }),
        raw_bytes: e.raw_bytes,
        squeezed_bytes: e.emitted_bytes,
        savings_pct: pct,
        emitted: if e.squeezed { "squeezed" } else { "raw" },
        legend: e
            .legend
            .iter()
            .map(|(tag, value)| JsonLegendEntry { tag, value })
            .collect(),
        body: &e.body,
    };

    let res = if pretty {
        serde_json::to_string_pretty(&doc)
    } else {
        serde_json::to_string(&doc)
    };
    res.unwrap_or_else(|err| serde_json::json!({ "error": err.to_string() }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny / non-repetitive input has no compressible structure, so the
    /// legend overhead guarantees the squeezed total is >= raw: must fall back.
    #[test]
    fn tiny_input_falls_back_to_raw() {
        let raw = "hi\n";
        let r = SqueezeReport {
            path: "tiny.log",
            range: None,
            raw,
            raw_requested: false,
        };
        let txt = render_text(&r);
        assert!(
            txt.contains("squeeze would be larger") || txt.contains("[raw "),
            "tiny input should report raw fallback, got:\n{txt}"
        );
        // The body is the original text, byte-for-byte.
        assert!(txt.ends_with("---\nhi\n"));
    }

    /// Regression: an input whose true compression gain is smaller than the
    /// squeezed-only `# legend:` marker must NOT be reported as a win. Before the
    /// marker was counted, this reported `[squeezed 39B -> 37B]` while the real
    /// emitted payload was larger than the 39-byte input.
    #[test]
    fn marginal_gain_does_not_falsely_claim_squeezed() {
        let raw = "xy12 xy12 zz\n".repeat(3);
        let r = SqueezeReport {
            path: "marginal.log",
            range: None,
            raw: &raw,
            raw_requested: false,
        };
        let json = render_json(&r, false);
        assert!(
            !json.contains("\"emitted\":\"squeezed\""),
            "marginal input falsely claimed a squeeze win: {json}"
        );
    }

    /// `--raw` always emits the original with no legend, regardless of content.
    #[test]
    fn raw_requested_skips_compression() {
        let raw = "ab\n".repeat(500);
        let r = SqueezeReport {
            path: "forced.log",
            range: None,
            raw: &raw,
            raw_requested: true,
        };
        let json = render_json(&r, false);
        assert!(json.contains("\"emitted\":\"raw\""));
        assert!(json.contains("\"legend\":[]"));
        let txt = render_text(&r);
        assert!(txt.contains("[raw "));
        assert!(!txt.contains("# legend:"));
    }

    /// A highly repetitive input must compress: emitted=="squeezed" and the
    /// emitted body is shorter than the raw input.
    #[test]
    fn repetitive_input_emits_squeezed() {
        // Many identical, structured log lines — lots for the pipeline to fold.
        let line = "2026-05-30T11:54:19.557 [WinFocusMonitor] hwnd=0x1234 focus changed event\n";
        let raw = line.repeat(200);
        let r = SqueezeReport {
            path: "app.log",
            range: None,
            raw: &raw,
            raw_requested: false,
        };

        let txt = render_text(&r);
        assert!(
            txt.contains("[squeezed "),
            "repetitive input should squeeze, got header in:\n{}",
            txt.lines().next().unwrap_or("")
        );
        assert!(txt.contains("# legend:"), "squeezed output must have a legend");

        // JSON view confirms the decision + that the emitted body shrank.
        let json = render_json(&r, false);
        assert!(json.contains("\"emitted\":\"squeezed\""));
        assert!(json.contains(JSON_SCHEMA_SQUEEZE));
        assert!(
            json.contains("ast-bro.squeeze.v1"),
            "JSON must carry the schema id"
        );
    }

    /// The schema id is present in JSON regardless of the emit branch.
    #[test]
    fn json_always_contains_schema() {
        let r = SqueezeReport {
            path: "x.log",
            range: Some((1, 1)),
            raw: "only one line, no newline",
            raw_requested: false,
        };
        let pretty = render_json(&r, true);
        assert!(pretty.contains(JSON_SCHEMA_SQUEEZE));
        // Pretty output is multi-line; compact is single-line.
        let compact = render_json(&r, false);
        assert!(!compact.contains('\n'));
        assert!(pretty.contains('\n'));
        // Range serialized as an object, not null.
        assert!(compact.contains("\"range\":{\"start\":1,\"end\":1}"));
    }

    #[test]
    fn slice_lines_clamps_and_is_inclusive() {
        let text = "a\nb\nc\nd\n";
        // Whole file when None.
        assert_eq!(slice_lines(text, None), text);
        // 1-indexed inclusive.
        assert_eq!(slice_lines(text, Some((2, 3))), "b\nc\n");
        // Clamped past EOF.
        assert_eq!(slice_lines(text, Some((3, 999))), "c\nd\n");
        // Start past EOF => empty.
        assert_eq!(slice_lines(text, Some((10, 20))), "");
        // Last line without trailing newline is preserved as-is.
        assert_eq!(slice_lines("x\ny\nz", Some((3, 3))), "z");
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(412), "412B");
        assert_eq!(fmt_bytes(45_000), "45.0KB");
        assert_eq!(fmt_bytes(1_200_000), "1.2MB");
    }
}
