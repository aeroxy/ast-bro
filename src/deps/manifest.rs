//! Project-manifest reading for resolver hints:
//!
//! - `go.mod`: extract `module <prefix>` directive.
//! - `tsconfig.json`: extract `compilerOptions.paths` + `baseUrl`.
//! - `Cargo.toml`: extract `[package].name` (with hyphen→underscore).
//! - `composer.json`: extract `autoload.psr-4` map (PHP).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct ProjectAliases {
    /// Rust crate name (hyphens already converted to underscores). Used
    /// when resolving `crate::x::y` in inter-crate workspaces.
    #[allow(dead_code)]
    pub rust_crate_name: Option<String>,
    /// Go module name from `go.mod`.
    pub go_module: Option<String>,
    /// TS path aliases — `(prefix, replacement)` pairs.
    pub ts_path_aliases: Vec<(String, String)>,
    /// PHP PSR-4 namespace prefix → directory pairs from `composer.json`.
    /// Prefixes are normalised to slash form (e.g. `App/` from `App\\`).
    pub php_psr4: Vec<(String, String)>,
}

pub fn detect_aliases(root: &Path) -> ProjectAliases {
    ProjectAliases {
        rust_crate_name: parse_cargo_name(&root.join("Cargo.toml")),
        go_module: parse_go_module(&root.join("go.mod")),
        ts_path_aliases: parse_tsconfig_paths(&root.join("tsconfig.json")),
        php_psr4: parse_composer_psr4(&root.join("composer.json")),
    }
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

/// Read `compilerOptions.paths` and `compilerOptions.baseUrl` out of
/// tsconfig.json. Returns prefix → replacement pairs ready to feed
/// into the resolver. Only handles the common single-target form
/// (`"@app/*": ["src/app/*"]` style) — multiple targets pick the first.
pub fn parse_tsconfig_paths(path: &Path) -> Vec<(String, String)> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(strip_jsonc(&s).as_str()) else {
        return Vec::new();
    };
    let Some(co) = v.get("compilerOptions") else {
        return Vec::new();
    };
    let base_url = co
        .get("baseUrl")
        .and_then(|x| x.as_str())
        .map(|s| s.trim_start_matches("./").to_string())
        .unwrap_or_default();
    let mut out = Vec::new();
    let Some(paths) = co.get("paths").and_then(|x| x.as_object()) else {
        return out;
    };
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
        let mut target_norm = target.replace("/*", "/");
        target_norm = target_norm.trim_start_matches("./").to_string();
        let combined = if base_url.is_empty() {
            target_norm
        } else {
            format!("{}/{}", base_url.trim_end_matches('/'), target_norm)
        };
        out.push((prefix_norm, combined));
    }
    out
}

/// Strip JSON-with-comments artefacts so `serde_json` can parse the file.
/// Cheap, line-based — not perfect but works for typical tsconfig.json.
fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        if let Some(idx) = line.find("//") {
            // Don't strip if the `//` is inside a string. Cheap heuristic:
            // count quotes before the `//`. Even count → outside string.
            let before = &line[..idx];
            let quotes = before.chars().filter(|c| *c == '"').count();
            if quotes % 2 == 0 {
                out.push_str(before);
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    // Strip trailing commas before `}`/`]` — common in tsconfig.
    let mut cleaned = String::with_capacity(out.len());
    let bytes = out.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b',' {
            // Look ahead past whitespace.
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        cleaned.push(bytes[i] as char);
        i += 1;
    }
    cleaned
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
                arr.iter()
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
    out.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
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
