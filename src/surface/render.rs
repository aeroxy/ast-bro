//! Text / tree / JSON renderers for `Vec<SurfaceEntry>`.

use crate::core::JSON_SCHEMA_SURFACE;
use crate::surface::entry::SurfaceEntry;
use crate::surface::options::OutputMode;
use colored::Colorize;
use serde::Serialize;
use std::collections::BTreeMap;

pub fn render(entries: &[SurfaceEntry], mode: OutputMode, include_chain: bool) -> String {
  match mode {
    OutputMode::Flat => render_flat(entries, include_chain),
    OutputMode::Tree => render_tree(entries),
    OutputMode::Json { compact } => render_json(entries, !compact),
  }
}

pub fn render_flat(entries: &[SurfaceEntry], include_chain: bool) -> String {
  if entries.is_empty() {
    return "# no public surface\n".to_string();
  }
  let max_name = entries
    .iter()
    .map(|e| e.qualified_path.chars().count())
    .max()
    .unwrap_or(0);
  let mut out = String::new();
  for e in entries {
    let location = format!("{}:{}", e.source_path.display(), e.source_line);
    let pad = " ".repeat(max_name.saturating_sub(e.qualified_path.chars().count()));
    out.push_str(&format!(
      "{}{}  {}",
      e.qualified_path.green().bold(),
      pad,
      location.dimmed()
    ));
    if e.via_glob {
      out.push_str(&format!("  {}", "[via *]".cyan()));
    }
    if include_chain && !e.re_export_chain.is_empty() {
      let chain_text: Vec<String> = e
        .re_export_chain
        .iter()
        .map(|h| format!("{}:{}", h.module_path, h.line))
        .collect();
      out.push_str(&format!("  [{}]", chain_text.join(" → ")));
    }
    out.push('\n');
  }
  out
}

pub fn render_tree(entries: &[SurfaceEntry]) -> String {
  if entries.is_empty() {
    return "# no public surface\n".to_string();
  }
  // Group by module prefix (everything before the last `::` or `.`)
  let mut groups: BTreeMap<String, Vec<&SurfaceEntry>> = BTreeMap::new();
  for e in entries {
    let prefix = _module_prefix(&e.qualified_path);
    groups.entry(prefix).or_default().push(e);
  }

  let mut out = String::new();
  for (module, items) in groups {
    out.push_str(&format!("{}\n", module.cyan().bold()));
    for it in items {
      let leaf = _leaf(&it.qualified_path);
      let kind = it.kind.to_string();
      let glob = if it.via_glob {
        " [via *]".cyan().to_string()
      } else {
        String::new()
      };
      out.push_str(&format!(
        "  ├─ {} {}  {}{}\n",
        kind.dimmed(),
        leaf.yellow(),
        format!("{}:{}", it.source_path.display(), it.source_line).dimmed(),
        glob,
      ));
    }
    out.push('\n');
  }
  out
}

#[derive(Serialize)]
struct Doc<'a> {
  schema: &'static str,
  entries: &'a [SurfaceEntry],
}

pub fn render_json(entries: &[SurfaceEntry], pretty: bool) -> String {
  let doc = Doc {
    schema: JSON_SCHEMA_SURFACE,
    entries,
  };
  if pretty {
    serde_json::to_string_pretty(&doc)
  } else {
    serde_json::to_string(&doc)
  }
  .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
}

fn _module_prefix(qpath: &str) -> String {
  if let Some(i) = qpath.rfind("::") {
    return qpath[..i].to_string();
  }
  if let Some(i) = qpath.rfind('.') {
    return qpath[..i].to_string();
  }
  String::new()
}

fn _leaf(qpath: &str) -> &str {
  if let Some(i) = qpath.rfind("::") {
    return &qpath[i + 2..];
  }
  if let Some(i) = qpath.rfind('.') {
    return &qpath[i + 1..];
  }
  qpath
}
