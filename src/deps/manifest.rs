//! Project-manifest reading for resolver hints:
//!
//! - `tsconfig.json` (or, without one, `jsconfig.json`): extract
//!   `compilerOptions.paths` + `baseUrl`, through any relative `extends`.
//! - `Cargo.toml`: extract `[package].name` (with hyphen→underscore).
//! - `composer.json`: extract `autoload.psr-4` map (PHP).
//! - `go.mod`: extract the `module <prefix>` directive.
//!
//! The first three are read at the graph root only. `go.mod` is the
//! exception: a repository may keep its module in a subdirectory or
//! declare several, so the resolver's index walk calls `parse_go_module`
//! for every `go.mod` it meets — see `resolver::build::build_suffix_index`.

use std::path::{Path, PathBuf};

/// The manifests `detect_aliases` reads at the graph root. They are read
/// directly, whatever the ignore files say, so the graph cache tracks them
/// by name rather than through its walk (#123). A config they extend is
/// read too: see `alias_sources`.
pub const ROOT_MANIFESTS: [&str; 4] = [CARGO_TOML, TSCONFIG_JSON, JSCONFIG_JSON, COMPOSER_JSON];
const CARGO_TOML: &str = "Cargo.toml";
const TSCONFIG_JSON: &str = "tsconfig.json";
const JSCONFIG_JSON: &str = "jsconfig.json";
const COMPOSER_JSON: &str = "composer.json";

#[derive(Debug, Clone, Default)]
pub struct ProjectAliases {
  /// Rust crate name (hyphens already converted to underscores). Used
  /// when resolving `crate::x::y` in inter-crate workspaces.
  #[allow(dead_code)]
  pub rust_crate_name: Option<String>,
  /// TS path aliases — `(prefix, replacement)` pairs.
  pub ts_path_aliases: Vec<(String, String)>,
  /// TS `compilerOptions.baseUrl`, relative to the root, without a leading
  /// `./`; `""` when it is the root itself. `None` when there is none or it
  /// leaves the root.
  pub ts_base_url: Option<String>,
  /// PHP PSR-4 namespace prefix → directory pairs from `composer.json`.
  /// Prefixes are normalised to slash form (e.g. `App/` from `App\\`).
  pub php_psr4: Vec<(String, String)>,
}

pub fn detect_aliases(root: &Path) -> ProjectAliases {
  // Read once: `paths` and `baseUrl` come from the same version of the files.
  let ts = read_ts_config(root);
  ProjectAliases {
    rust_crate_name: parse_cargo_name(&root.join(CARGO_TOML)),
    ts_path_aliases: ts.path_aliases(),
    ts_base_url: ts.base_url.clone().flatten(),
    php_psr4: parse_composer_psr4(&root.join(COMPOSER_JSON)),
  }
}

/// Every file `detect_aliases` reads: the root manifests that exist, and
/// each config the root `tsconfig.json` (or `jsconfig.json`) extends. The
/// graph cache tracks their contents, so an edit to a base config reaches
/// the graph like an edit to the root one.
pub fn alias_sources(root: &Path) -> Vec<PathBuf> {
  let mut out: Vec<PathBuf> = ROOT_MANIFESTS
    .iter()
    .map(|name| root.join(name))
    .filter(|p| p.is_file())
    .collect();
  for file in read_ts_config(root).files {
    if !out.contains(&file) {
      out.push(file);
    }
  }
  out
}

/// Parse `module github.com/aero/foo` from `go.mod`. Returns the value.
pub fn parse_go_module(path: &Path) -> Option<String> {
  let s = std::fs::read_to_string(path).ok()?;
  for line in s.lines() {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix("module ") {
      // Could be `module name` or `module "name"`.
      let n = rest.trim().trim_matches('"').trim();
      if !n.is_empty() {
        return Some(n.to_string());
      }
    }
  }
  None
}

/// Pull `[package].name` out of a Cargo.toml without depending on `toml`.
fn parse_cargo_name(path: &Path) -> Option<String> {
  let s = std::fs::read_to_string(path).ok()?;
  let mut in_package = false;
  for line in s.lines() {
    let t = line.trim();
    if t.starts_with('[') {
      in_package = t == "[package]";
      continue;
    }
    if in_package {
      if let Some(rest) = t.strip_prefix("name") {
        if let Some(eq) = rest.find('=') {
          let val = rest[eq + 1..].trim().trim_matches('"').trim_matches('\'');
          if !val.is_empty() {
            return Some(val.replace('-', "_"));
          }
        }
      }
    }
  }
  None
}

/// The TS resolver settings of a project: the root `tsconfig.json`, or
/// `jsconfig.json` when there is none, merged with the configs it extends.
#[derive(Debug, Default)]
struct TsConfig {
  /// `compilerOptions.baseUrl`, relative to the root: `Some(None)` when it
  /// is set but absolute or outside the root, where it names no project
  /// file and joining an absolute path onto the root would replace it.
  base_url: Option<Option<String>>,
  /// `compilerOptions.paths`, and the root-relative directory of the
  /// config that sets it: the targets resolve against that directory when
  /// no `baseUrl` is set.
  paths: Option<(serde_json::Map<String, serde_json::Value>, String)>,
  /// Every config file found, the root one first, once each. A file
  /// that does not parse is here too: an edit that makes it parse again
  /// must reach the cache.
  files: Vec<PathBuf>,
  /// How many configs were read; see `MAX_READS`.
  reads: usize,
}

/// How deep an `extends` chain is followed.
const MAX_EXTENDS: usize = 16;

/// How many configs one project reads. A config two parents extend is
/// read under each, so a deep diamond would otherwise read it
/// exponentially often.
const MAX_READS: usize = 256;

fn read_ts_config(root: &Path) -> TsConfig {
  let mut cfg = TsConfig::default();
  let file = [TSCONFIG_JSON, JSCONFIG_JSON]
    .iter()
    .map(|name| root.join(name))
    .find(|p| p.is_file());
  if let Some(file) = file {
    load_ts_config(root, &file, &mut cfg, &mut Vec::new());
  }
  cfg
}

/// Read `path` into `cfg`: the configs it extends first, in order, so a
/// later one, and then `path` itself, overrides what they set. `baseUrl`
/// and `paths` resolve against the directory of the file that sets them.
///
/// `chain` holds the configs that extend `path`, which stops a cycle. A
/// config two parents extend is read under each: `tsc` merges each
/// parent's effective settings, so the later parent's inherited value wins.
fn load_ts_config(root: &Path, path: &Path, cfg: &mut TsConfig, chain: &mut Vec<PathBuf>) {
  if chain.len() > MAX_EXTENDS || cfg.reads >= MAX_READS || chain.iter().any(|f| f == path) {
    return;
  }
  cfg.reads += 1;
  if !cfg.files.iter().any(|f| f == path) {
    cfg.files.push(path.to_path_buf());
  }
  let Some(v) = std::fs::read_to_string(path)
    .ok()
    .and_then(|s| serde_json::from_str::<serde_json::Value>(&strip_jsonc(&s)).ok())
  else {
    return;
  };
  let Some(dir) = path.parent().and_then(|d| d.strip_prefix(root).ok()) else {
    return;
  };
  let dir = dir
    .components()
    .map(|c| c.as_os_str().to_string_lossy().into_owned())
    .collect::<Vec<_>>()
    .join("/");
  let extends: Vec<&str> = match v.get("extends") {
    Some(serde_json::Value::String(s)) => vec![s.as_str()],
    Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).collect(),
    _ => Vec::new(),
  };
  for spec in extends {
    if let Some(base) = extended_config(root, &dir, spec) {
      chain.push(path.to_path_buf());
      load_ts_config(root, &base, cfg, chain);
      chain.pop();
    }
  }
  let Some(co) = v.get("compilerOptions") else {
    return;
  };
  if let Some(base) = co.get("baseUrl").and_then(|x| x.as_str()) {
    cfg.base_url = Some(join_in_root(&dir, base));
  }
  if let Some(paths) = co.get("paths").and_then(|x| x.as_object()) {
    cfg.paths = Some((paths.clone(), dir));
  }
}

/// The config file a relative `extends` names, `.json` optional, when it
/// is inside the root. A package name (`@tsconfig/node20/tsconfig.json`)
/// is not followed: it lives in `node_modules`.
fn extended_config(root: &Path, dir: &str, spec: &str) -> Option<PathBuf> {
  if !(spec.starts_with("./") || spec.starts_with("../")) {
    return None;
  }
  let path = root.join(join_in_root(dir, spec)?);
  if path.is_file() {
    return Some(path);
  }
  let mut with_json = path.into_os_string();
  with_json.push(".json");
  Some(PathBuf::from(with_json)).filter(|p| p.is_file())
}

/// `value` joined onto `dir` (both relative to the root, `/`-separated),
/// normalised: `./src/` and `src/../src` are `src`, `.` is `""`. `None`
/// when `value` is absolute or the result leaves the root.
pub(crate) fn join_in_root(dir: &str, value: &str) -> Option<String> {
  if value.starts_with(['/', '\\']) || Path::new(value).is_absolute() || value.contains(':') {
    return None;
  }
  let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
  for seg in value.split(['/', '\\']) {
    match seg {
      "" | "." => {}
      ".." => {
        parts.pop()?;
      }
      s => parts.push(s),
    }
  }
  Some(parts.join("/"))
}

impl TsConfig {
  /// `paths` as prefix → replacement pairs ready to feed into the
  /// resolver, under the effective `baseUrl`. Only handles the common
  /// single-target form (`"@app/*": ["src/app/*"]` style) — multiple
  /// targets pick the first.
  fn path_aliases(&self) -> Vec<(String, String)> {
    let Some((paths, dir)) = &self.paths else {
      return Vec::new();
    };
    let base = match &self.base_url {
      Some(Some(base)) => base.as_str(),
      // The targets sit under a `baseUrl` that names no project file.
      Some(None) => return Vec::new(),
      None => dir.as_str(),
    };
    let mut out = Vec::new();
    for (prefix, targets) in paths {
      let target = targets
        .as_array()
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .unwrap_or("");
      if target.is_empty() {
        continue;
      }
      let prefix_norm = prefix.replace("/*", "/");
      let target_norm = target.replace("/*", "/");
      // `configs/../src/` is the index key `src/`. A target that leaves
      // the root names no project file.
      let Some(mut combined) = join_in_root(base, &target_norm) else {
        continue;
      };
      if target_norm.ends_with('/') && !combined.is_empty() {
        combined.push('/');
      }
      out.push((prefix_norm, combined));
    }
    out
  }
}

/// JSON with comments, as TypeScript reads it, made plain JSON for
/// `serde_json`: `//` and `/* */` comments and trailing commas go, and
/// strings stay byte for byte, `//`, `/*` and `*/` in them included. Only
/// ASCII bytes outside strings are dropped, so the text stays UTF-8.
fn strip_jsonc(s: &str) -> String {
  let b = s.as_bytes();
  // Comments out, strings copied as they are.
  let mut plain: Vec<u8> = Vec::with_capacity(b.len());
  let mut i = 0;
  while i < b.len() {
    match (b[i], b.get(i + 1)) {
      (b'"', _) => {
        let end = string_end(b, i);
        plain.extend_from_slice(&b[i..end]);
        i = end;
      }
      (b'/', Some(b'/')) => {
        while i < b.len() && b[i] != b'\n' {
          i += 1;
        }
      }
      (b'/', Some(b'*')) => {
        i += 2;
        while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
          i += 1;
        }
        i = (i + 2).min(b.len());
        plain.push(b' ');
      }
      (c, _) => {
        plain.push(c);
        i += 1;
      }
    }
  }
  // Trailing commas out: a `,` followed, past whitespace, by `}` or `]`.
  let mut out: Vec<u8> = Vec::with_capacity(plain.len());
  let mut i = 0;
  while i < plain.len() {
    match plain[i] {
      b'"' => {
        let end = string_end(&plain, i);
        out.extend_from_slice(&plain[i..end]);
        i = end;
      }
      b',' => {
        let next = plain[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
        if !matches!(next, Some(b'}' | b']')) {
          out.push(b',');
        }
        i += 1;
      }
      c => {
        out.push(c);
        i += 1;
      }
    }
  }
  String::from_utf8(out).unwrap_or_default()
}

/// The index just past the string literal that opens at `start`.
fn string_end(b: &[u8], start: usize) -> usize {
  let mut i = start + 1;
  while i < b.len() && b[i] != b'"' {
    if b[i] == b'\\' {
      i += 1;
    }
    i += 1;
  }
  (i + 1).min(b.len())
}

/// Parse `autoload.psr-4` from `composer.json`. Returns prefix → directory
/// pairs ready for the resolver. Prefixes are converted from PHP's
/// backslash-separated form (`App\\`) to the slash form (`App/`) used
/// elsewhere in the deps subsystem.
///
/// PSR-4 allows the target to be a string or an array of strings (multiple
/// directories per prefix). For arrays, we emit one `(prefix, dir)` entry per
/// directory; the resolver tries them in order until one yields a hit.
pub fn parse_composer_psr4(path: &Path) -> Vec<(String, String)> {
  let Ok(s) = std::fs::read_to_string(path) else {
    return Vec::new();
  };
  let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for section in ["autoload", "autoload-dev"] {
    let Some(autoload) = v.get(section) else {
      continue;
    };
    let Some(psr4) = autoload.get("psr-4").and_then(|x| x.as_object()) else {
      continue;
    };
    for (prefix, target) in psr4 {
      // Normalise: `App\\` → `App/`.
      let prefix_norm = prefix.replace('\\', "/");
      let dirs: Vec<String> = if let Some(s) = target.as_str() {
        vec![s.to_string()]
      } else if let Some(arr) = target.as_array() {
        arr
          .iter()
          .filter_map(|x| x.as_str().map(str::to_string))
          .collect()
      } else {
        continue;
      };
      for dir in dirs {
        if dir.is_empty() {
          continue;
        }
        let dir_norm = dir.trim_end_matches('/').to_string();
        out.push((prefix_norm.clone(), dir_norm));
      }
    }
  }
  // Longest prefix wins on tie — sort descending so the resolver picks the
  // most specific match first. (Stable sort preserves array order within
  // a prefix, so multi-dir entries try directories in declaration order.)
  out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
  out
}

/// Best-effort discovery of additional crate roots in a Cargo workspace.
/// Returns paths to each member crate's directory.
#[allow(dead_code)]
pub fn cargo_workspace_members(root: &Path) -> Vec<PathBuf> {
  let s = match std::fs::read_to_string(root.join("Cargo.toml")) {
    Ok(s) => s,
    Err(_) => return Vec::new(),
  };
  let mut in_ws = false;
  let mut members: Vec<String> = Vec::new();
  for raw in s.lines() {
    let line = raw.trim();
    if line.starts_with('[') {
      in_ws = line == "[workspace]";
      continue;
    }
    if in_ws {
      if let Some(rest) = line.strip_prefix("members") {
        if let Some(eq) = rest.find('=') {
          let val = rest[eq + 1..].trim();
          if let Some(inner) = val.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            for tok in inner.split(',') {
              let t = tok.trim().trim_matches('"').trim_matches('\'');
              if !t.is_empty() {
                members.push(t.to_string());
              }
            }
          }
        }
      }
    }
  }
  members
    .into_iter()
    .flat_map(|m| crate::path_glob::expand_pattern(&root.join(m)))
    .filter(|p| p.join("Cargo.toml").is_file())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn base_url(json: &str) -> Option<String> {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(TSCONFIG_JSON), json).unwrap();
    read_ts_config(tmp.path()).base_url.flatten()
  }

  #[test]
  fn base_url_is_read_relative_to_the_root() {
    let with = |b: &str| {
      base_url(&format!(
        r#"{{ "compilerOptions": {{ "baseUrl": "{b}" }} }}"#
      ))
    };
    assert_eq!(with("src").as_deref(), Some("src"));
    assert_eq!(with("./src/").as_deref(), Some("src"));
    assert_eq!(with(".").as_deref(), Some(""));
    assert_eq!(with("./").as_deref(), Some(""));
    assert_eq!(with("src/../lib").as_deref(), Some("lib"));
    // Outside the root there is no project file to name, and an absolute
    // path would replace the root in a join.
    assert_eq!(with("../shared"), None);
    assert_eq!(with("src/../../shared"), None);
    assert_eq!(with("/opt/shared"), None);
    assert_eq!(base_url(r#"{ "compilerOptions": {} }"#), None);
  }

  #[test]
  fn jsonc_keeps_strings_and_drops_comments_and_trailing_commas() {
    let src = "{\n  // line\n  \"a\": \"http://x/*y*/\", /* block\n spans */ \"b\": \"q\\\"//\",\n  \"c\": [1, 2,],\n  \"d\": \"源码\",\n}\n";
    let v: serde_json::Value = serde_json::from_str(&strip_jsonc(src)).unwrap();
    assert_eq!(v["a"], "http://x/*y*/");
    assert_eq!(v["b"], "q\"//");
    assert_eq!(v["c"], serde_json::json!([1, 2]));
    assert_eq!(v["d"], "源码");
  }

  #[test]
  fn an_extends_cycle_is_read_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(
      root.join(TSCONFIG_JSON),
      r#"{ "extends": "./a.json", "compilerOptions": { "baseUrl": "src" } }"#,
    )
    .unwrap();
    std::fs::write(root.join("a.json"), r#"{ "extends": "./tsconfig.json" }"#).unwrap();
    let cfg = read_ts_config(root);
    assert_eq!(cfg.base_url.flatten().as_deref(), Some("src"));
    assert_eq!(cfg.files.len(), 2);
  }

  #[test]
  fn jsconfig_is_read_only_without_a_tsconfig() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(
      root.join(JSCONFIG_JSON),
      r#"{ "compilerOptions": { "baseUrl": "js" } }"#,
    )
    .unwrap();
    assert_eq!(
      read_ts_config(root).base_url.flatten().as_deref(),
      Some("js")
    );
    std::fs::write(
      root.join(TSCONFIG_JSON),
      r#"{ "compilerOptions": { "baseUrl": "ts" } }"#,
    )
    .unwrap();
    assert_eq!(
      read_ts_config(root).base_url.flatten().as_deref(),
      Some("ts")
    );
  }
}
