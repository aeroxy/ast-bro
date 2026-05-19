//! Build a suffix index over all source files in the project.
//!
//! For each file `a/b/c.py` we index every path-suffix (`a/b/c`, `b/c`,
//! `c`) to the absolute file path. Python `__init__.py` also indexes
//! the parent directory name so `import a.b` resolves to `a/b/__init__.py`.
//! Java/Kotlin/Scala/C# also index `<package>.<TypeName>` after a
//! dot→slash conversion (the package is parsed from the file's
//! `package` / `namespace` declaration).

use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::file_filter::{add_filters, should_skip_path};

/// Per-file metadata collected during the walk.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IndexedFile {
    pub path: PathBuf,
    pub language: Lang,
    /// Package / namespace declared inside the file, if any. Used for
    /// Java/Kotlin/Scala/C# `<package>.<TypeName>` suffix indexing.
    pub package: Option<String>,
    /// Top-level type names declared in the file (for FQN suffix indexing).
    /// Empty for languages that don't need it (Rust, Python, TS/JS, Go).
    pub top_level_types: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Scala,
    Java,
    Kotlin,
    CSharp,
    Go,
    Cpp,
    Php,
    Ruby,
    Other,
}

impl Lang {
    pub fn from_path(p: &Path) -> Option<Self> {
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        let lang = match ext.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "ts" | "mts" | "cts" | "d.ts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "scala" | "sc" => Self::Scala,
            "java" => Self::Java,
            "kt" | "kts" => Self::Kotlin,
            "cs" => Self::CSharp,
            "go" => Self::Go,
            "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hxx" | "hh" | "h++" | "h" => Self::Cpp,
            "php" => Self::Php,
            "rb" => Self::Ruby,
            _ => return None,
        };
        // `Cargo.toml`/`go.mod` etc. don't go through here.
        let _ = name;
        Some(lang)
    }
}

/// The suffix index mapping a normalised slash-joined suffix → list of
/// candidate files. We keep multiple candidates because suffix collisions
/// happen (`utils.py` may exist in two packages); resolution picks the
/// one closest to the importer.
#[derive(Debug, Default)]
pub struct SuffixIndex {
    /// Suffix → candidate files. Suffixes use `/` as a separator regardless
    /// of platform; values are stored as absolute paths.
    pub by_suffix: HashMap<String, Vec<PathBuf>>,
    /// File → its own metadata (language, package, types). Populated for
    /// every walked file.
    pub by_file: HashMap<PathBuf, IndexedFile>,
    pub root: PathBuf,
}

impl SuffixIndex {
    pub fn lookup(&self, suffix: &str) -> Option<&[PathBuf]> {
        self.by_suffix.get(suffix).map(|v| v.as_slice())
    }
}

/// Walk `root` and build a suffix index of every indexable source file.
/// Honours `.gitignore`, `.ast-bro-ignore`, and the hardcoded denylist
/// (matches the rest of ast-bro).
pub fn build_suffix_index(root: &Path) -> SuffixIndex {
    let mut idx = SuffixIndex {
        root: root.to_path_buf(),
        ..Default::default()
    };

    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    add_filters(&mut builder, root);

    for entry in builder.build().flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if should_skip_path(path, root) {
            continue;
        }
        let Some(lang) = Lang::from_path(path) else {
            continue;
        };

        // Read package/types only when needed.
        let (package, top_level_types) = match lang {
            Lang::Java | Lang::Kotlin | Lang::Scala | Lang::CSharp => {
                extract_package_and_types(path, lang).unwrap_or((None, Vec::new()))
            }
            _ => (None, Vec::new()),
        };

        let info = IndexedFile {
            path: path.to_path_buf(),
            language: lang,
            package: package.clone(),
            top_level_types: top_level_types.clone(),
        };
        idx.by_file.insert(path.to_path_buf(), info);

        // Index relative path suffixes (a/b/c, b/c, c) for the file.
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        index_path_suffixes(&mut idx.by_suffix, path, &rel);

        // Python __init__.py also indexes the parent directory name so
        // `import a.b` finds `a/b/__init__.py`.
        if lang == Lang::Python && path.file_name().and_then(|s| s.to_str()) == Some("__init__.py")
        {
            if let Some(parent) = rel.parent() {
                if !parent.as_os_str().is_empty() {
                    let key = parent
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    idx.by_suffix.entry(key.clone()).or_default().push(path.to_path_buf());
                    // Also index just the package name (last segment).
                    if let Some(last) = key.rsplit('/').next() {
                        idx.by_suffix
                            .entry(last.to_string())
                            .or_default()
                            .push(path.to_path_buf());
                    }
                }
            }
        }

        // FQN entries for Java/Kotlin/Scala/C#.
        if let Some(pkg) = package {
            for ty in &top_level_types {
                let fqn = format!("{}.{}", pkg, ty).replace('.', "/");
                idx.by_suffix.entry(fqn).or_default().push(path.to_path_buf());
                // Also index just the type (commonly seen at top-level).
                idx.by_suffix
                    .entry(ty.clone())
                    .or_default()
                    .push(path.to_path_buf());
            }
        }
    }

    // Dedup + stable sort each candidate list.
    for v in idx.by_suffix.values_mut() {
        v.sort();
        v.dedup();
    }

    idx
}

fn index_path_suffixes(
    by_suffix: &mut HashMap<String, Vec<PathBuf>>,
    abs_path: &Path,
    rel_path: &Path,
) {
    // Index every suffix of the path *without* the extension. So
    // `src/foo/bar.rs` indexes `src/foo/bar`, `foo/bar`, `bar`.
    let stem_path = match (rel_path.parent(), rel_path.file_stem()) {
        (Some(parent), Some(stem)) => parent.join(stem),
        (None, Some(stem)) => PathBuf::from(stem),
        _ => return,
    };
    let segs: Vec<String> = stem_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if segs.is_empty() {
        return;
    }
    for start in 0..segs.len() {
        let key = segs[start..].join("/");
        by_suffix
            .entry(key)
            .or_default()
            .push(abs_path.to_path_buf());
    }
}

/// Best-effort package + top-level-type extraction for FQN indexing.
/// Uses tiny regex-y parsing rather than a full AST pass — keeps the
/// suffix index build fast enough that a 10k-file repo finishes in
/// well under a second.
fn extract_package_and_types(path: &Path, lang: Lang) -> Option<(Option<String>, Vec<String>)> {
    let src = std::fs::read_to_string(path).ok()?;
    let mut package: Option<String> = None;
    let mut types: Vec<String> = Vec::new();

    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }

        // Package / namespace declaration.
        if package.is_none() {
            match lang {
                Lang::Java | Lang::Kotlin | Lang::Scala => {
                    if let Some(rest) = line.strip_prefix("package ") {
                        let p = rest.trim_end_matches(';').trim();
                        if !p.is_empty() {
                            package = Some(p.to_string());
                        }
                    }
                }
                Lang::CSharp => {
                    if let Some(rest) = line
                        .strip_prefix("namespace ")
                        .or_else(|| line.strip_prefix("internal namespace "))
                    {
                        let p = rest
                            .trim_end_matches(';')
                            .trim_end_matches('{')
                            .trim();
                        if !p.is_empty() {
                            package = Some(p.to_string());
                        }
                    }
                }
                _ => {}
            }
        }

        // Top-level types.
        match lang {
            Lang::Java => {
                if let Some(name) = pick_after(line, &["class ", "interface ", "enum ", "record "])
                {
                    types.push(name);
                }
            }
            Lang::Kotlin => {
                if let Some(name) = pick_after(
                    line,
                    &[
                        "class ",
                        "interface ",
                        "object ",
                        "enum class ",
                        "data class ",
                        "sealed class ",
                    ],
                ) {
                    types.push(name);
                }
                if let Some(name) = pick_after(line, &["fun "]) {
                    types.push(name);
                }
            }
            Lang::Scala => {
                if let Some(name) = pick_after(
                    line,
                    &[
                        "class ", "object ", "trait ", "enum ", "case class ", "case object ",
                    ],
                ) {
                    types.push(name);
                }
            }
            Lang::CSharp => {
                if let Some(name) = pick_after(
                    line,
                    &[
                        "class ", "struct ", "interface ", "record ", "enum ", "delegate ",
                    ],
                ) {
                    types.push(name);
                }
            }
            _ => {}
        }
    }

    types.sort();
    types.dedup();

    Some((package, types))
}

fn pick_after(line: &str, keywords: &[&str]) -> Option<String> {
    for kw in keywords {
        if let Some(idx) = line.find(kw) {
            // Avoid matching inside strings: require keyword at start or after whitespace/access modifier.
            let before = &line[..idx];
            let prev = before.chars().last();
            let ok = matches!(prev, None | Some(' ') | Some('\t') | Some('('));
            if !ok {
                continue;
            }
            let rest = &line[idx + kw.len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            // Skip language-keyword false-positives ("class abstract", etc.).
            if matches!(
                name.as_str(),
                "abstract"
                    | "static"
                    | "public"
                    | "private"
                    | "protected"
                    | "internal"
                    | "open"
                    | "sealed"
                    | "final"
            ) {
                continue;
            }
            return Some(name);
        }
    }
    None
}
