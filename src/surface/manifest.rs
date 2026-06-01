//! Tiny line-oriented parsers for `Cargo.toml` and `pyproject.toml`.
//!
//! We deliberately avoid the `toml` crate — these manifests vary a lot in
//! the wild, and we only need a handful of fields:
//! - `[package].name` for the crate name
//! - `[lib].path` for an explicit lib root
//! - `[[bin]].path` (and the matching `name`) for binary roots
//! - `[workspace].members` for workspace fan-out
//! - `[project].name` for Python pkg name
//!
//! The parser is strict-enough-for-our-needs: it understands sections,
//! quoted strings, and inline arrays of strings. Anything fancier (nested
//! tables, dotted keys, multi-line strings, comments mid-value) we tolerate
//! gracefully — unrecognized lines just get ignored.

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct CargoManifest {
    pub package_name: Option<String>,
    pub lib_path: Option<PathBuf>,
    pub bins: Vec<BinTarget>,
    pub workspace_members: Vec<String>,
    pub manifest_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BinTarget {
    pub name: Option<String>,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub struct PyProject {
    pub project_name: Option<String>,
}

/// Subset of `package.json` we care about for resolving the package's
/// public entry. `exports` is resolved through the full Node.js
/// algorithm — see `resolve_package_entry`.
#[derive(Debug, Default, Clone)]
pub struct PackageJson {
    pub name: Option<String>,
    pub main: Option<String>,
    pub module: Option<String>,
    pub types: Option<String>,
    /// Raw `exports` value, for conditional resolution.
    pub exports: Option<Value>,
    pub manifest_dir: PathBuf,
}

/// Conditions we'll consider when resolving the `exports` field, in
/// preference order. We prefer `types` first because the `.d.ts` it
/// points to carries the same identifier set as the implementation
/// and is what a TypeScript user actually consumes.
pub const EXPORT_CONDITIONS: &[&str] = &[
    "types",
    "typescript",
    "import",
    "module",
    "default",
    "node",
    "require",
    "browser",
];

pub fn parse_cargo_toml(path: &Path) -> Option<CargoManifest> {
    let raw = std::fs::read_to_string(path).ok()?;
    let manifest_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut m = CargoManifest {
        manifest_dir,
        ..Default::default()
    };

    let mut section = String::new();
    let mut current_bin: Option<BinTarget> = None;

    for raw_line in raw.lines() {
        let line = _strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        // Section header: `[package]` or `[[bin]]`.
        if let Some(hdr) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            // Flush any open `[[bin]]` before switching.
            if section == "[[bin]]" {
                if let Some(b) = current_bin.take() {
                    m.bins.push(b);
                }
            }
            section = format!("[{}]", hdr);
            if hdr == "[bin]" {
                section = "[[bin]]".to_string();
                current_bin = Some(BinTarget { name: None, path: None });
            }
            continue;
        }

        // Key = value.
        let (key, value) = match line.split_once('=') {
            Some(kv) => (kv.0.trim(), kv.1.trim()),
            None => continue,
        };

        match section.as_str() {
            "[package]" => {
                if key == "name" {
                    m.package_name = _unquote(value);
                }
            }
            "[lib]" => {
                if key == "path" {
                    m.lib_path = _unquote(value).map(PathBuf::from);
                }
            }
            "[[bin]]" => {
                if let Some(b) = current_bin.as_mut() {
                    if key == "name" {
                        b.name = _unquote(value);
                    } else if key == "path" {
                        b.path = _unquote(value).map(PathBuf::from);
                    }
                }
            }
            "[workspace]" => {
                if key == "members" {
                    m.workspace_members = _parse_string_array(value);
                }
            }
            _ => {}
        }
    }
    if let Some(b) = current_bin.take() {
        m.bins.push(b);
    }

    Some(m)
}

pub fn parse_pyproject_toml(path: &Path) -> Option<PyProject> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut p = PyProject::default();
    let mut section = String::new();
    for raw_line in raw.lines() {
        let line = _strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(hdr) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = format!("[{}]", hdr);
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(kv) => (kv.0.trim(), kv.1.trim()),
            None => continue,
        };
        if section == "[project]" && key == "name" {
            p.project_name = _unquote(value);
        }
    }
    Some(p)
}

pub fn parse_package_json(path: &Path) -> Option<PackageJson> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let manifest_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let name = v.get("name").and_then(|x| x.as_str()).map(|s| s.to_string());
    let main = v.get("main").and_then(|x| x.as_str()).map(|s| s.to_string());
    let module = v.get("module").and_then(|x| x.as_str()).map(|s| s.to_string());
    let types = v
        .get("types")
        .or_else(|| v.get("typings"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let exports = v.get("exports").cloned();

    Some(PackageJson {
        name,
        main,
        module,
        types,
        exports,
        manifest_dir,
    })
}

/// Resolve a `package.json` to its primary public entry file. Implements
/// the relevant subset of Node's `exports` algorithm:
///   1. If `exports` is set, walk it for the `"."` subpath, then pick
///      the best condition (preferred order in `EXPORT_CONDITIONS`).
///   2. Subpath patterns like `"./feature/*"` are recognized but only
///      `"."` is followed.
///   3. Fall back to `module` (ESM hint), then `main` (CommonJS), then
///      `index.{ts,tsx,mts,cts,js,jsx,mjs,cjs}` in the package dir.
///   4. If the resolved file is `.js`/`.cjs`/`.mjs`, also probe the
///      sibling `.ts`/`.tsx` source — that's what most monorepos check
///      into git, and what the user actually wants to inspect.
pub fn resolve_package_entry(pkg: &PackageJson) -> Option<PathBuf> {
    if let Some(ex) = &pkg.exports {
        if let Some(rel) = _resolve_exports_root(ex) {
            if let Some(p) = _exists_with_source_pref(&pkg.manifest_dir.join(rel)) {
                return Some(p);
            }
        }
    }
    if let Some(rel) = pkg.types.as_deref().or(pkg.module.as_deref()).or(pkg.main.as_deref()) {
        if let Some(p) = _exists_with_source_pref(&pkg.manifest_dir.join(rel)) {
            return Some(p);
        }
    }
    for stem in ["index", "main", "src/index"] {
        for ext in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "d.ts"] {
            let cand = pkg.manifest_dir.join(format!("{}.{}", stem, ext));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn _resolve_exports_root(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            // Pure subpath map: keys begin with "./". Look up "."
            let is_subpath_map = map.keys().any(|k| k.starts_with("./") || k == ".");
            if is_subpath_map {
                if let Some(dot) = map.get(".") {
                    return _resolve_conditional(dot);
                }
                return None;
            }
            // Otherwise the whole object is a conditional map at "."
            _resolve_conditional(v)
        }
        _ => None,
    }
}

fn _resolve_conditional(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) => {
            // Array: try each fallback in order.
            for item in arr {
                if let Some(s) = _resolve_conditional(item) {
                    return Some(s);
                }
            }
            None
        }
        Value::Object(map) => {
            // Honour our preferred conditions first, then anything
            // remaining.
            for cond in EXPORT_CONDITIONS {
                if let Some(inner) = map.get(*cond) {
                    if let Some(s) = _resolve_conditional(inner) {
                        return Some(s);
                    }
                }
            }
            for (k, inner) in map {
                if EXPORT_CONDITIONS.iter().any(|c| c == k) {
                    continue;
                }
                if k.starts_with('.') {
                    continue;
                }
                if let Some(s) = _resolve_conditional(inner) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

/// If `p` resolves to a `.js` (or `.mjs`/`.cjs`) but a `.ts`/`.tsx` lives
/// next to it, prefer the source. Same for `.d.ts` ↔ `.ts`. This makes
/// `surface` work on monorepos that publish compiled output but check in
/// the source.
fn _exists_with_source_pref(p: &Path) -> Option<PathBuf> {
    if !p.is_file() {
        // Try sibling source if compiled-only path is given.
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            let parent = p.parent().unwrap_or(Path::new("."));
            for ext in ["ts", "tsx", "mts", "cts"] {
                let alt = parent.join(format!("{}.{}", stem, ext));
                if alt.is_file() {
                    return Some(alt);
                }
            }
        }
        return None;
    }
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
    if matches!(ext, "ts" | "tsx" | "mts" | "cts") {
        return Some(p.to_path_buf());
    }
    if matches!(ext, "js" | "jsx" | "mjs" | "cjs") {
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            let parent = p.parent().unwrap_or(Path::new("."));
            for ext in ["ts", "tsx", "mts", "cts"] {
                let alt = parent.join(format!("{}.{}", stem, ext));
                if alt.is_file() {
                    return Some(alt);
                }
            }
        }
    }
    Some(p.to_path_buf())
}

fn _strip_comment(s: &str) -> &str {
    // Naive — does not handle `#` inside quoted strings, but our keys
    // don't contain them so we get away with it.
    if let Some(i) = s.find('#') {
        &s[..i]
    } else {
        s
    }
}

fn _unquote(v: &str) -> Option<String> {
    let v = v.trim();
    if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(s.to_string())
    } else { v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).map(|s| s.to_string()) }
}

fn _parse_string_array(v: &str) -> Vec<String> {
    let v = v.trim();
    let inner = v.strip_prefix('[').and_then(|s| s.strip_suffix(']'));
    let inner = match inner {
        Some(s) => s,
        None => return Vec::new(),
    };
    inner
        .split(',')
        .filter_map(|item| _unquote(item.trim()))
        .collect()
}
