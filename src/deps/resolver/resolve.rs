//! Resolve a single import string against the suffix index.
//!
//! Two pre-processing steps before the suffix lookup:
//!
//! - **Relative imports** (`.x`, `./x`, `../x`) — resolved against the
//!   importer's directory using path arithmetic, then handed off to the
//!   suffix lookup.
//! - **Manifest aliases** (Go `mymod/` prefix, TS `tsconfig.json`
//!   `compilerOptions.paths`, Rust `crate::` prefix) — stripped to a
//!   slash-joined module path.

use std::path::{Path, PathBuf};

use super::build::{Lang, SuffixIndex};

/// Per-call resolution context — what file is doing the import, and which
/// language we're resolving for.
#[derive(Debug, Clone)]
pub struct ResolveCtx<'a> {
    /// The importer file (used for relative resolution and pick-closest).
    pub from_file: &'a Path,
    pub lang: Lang,
    /// Manifest path-alias mappings (TS `tsconfig.json` `paths` field).
    /// Keys are bare prefixes (`@app/`); values are the substitution paths
    /// (`src/app/`) that the prefix expands to before suffix lookup.
    pub path_aliases: &'a [(String, String)],
    /// PHP PSR-4 prefix → directory pairs from `composer.json`. Prefixes are
    /// already in slash form (`App/`) — see `manifest::parse_composer_psr4`.
    /// Sorted longest-first so the resolver picks the most specific match.
    pub php_psr4: &'a [(String, String)],
}

impl<'a> ResolveCtx<'a> {
    #[allow(dead_code)]
    pub fn new(from_file: &'a Path, lang: Lang) -> Self {
        Self {
            from_file,
            lang,
            path_aliases: &[],
            php_psr4: &[],
        }
    }
}

/// Resolve `spec` to a file in the project, or `None` if external/unresolvable.
pub fn resolve(spec: &str, ctx: &ResolveCtx<'_>, idx: &SuffixIndex) -> Option<PathBuf> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    // Language-specific normalisation: `crate::x::y` → `x/y`,
    // `self::x` → relative-to-current-dir, `super::x` → ascend one.
    if ctx.lang == Lang::Rust {
        let resolve_with_fallback =
            |key: String| -> Option<PathBuf> {
                if let Some(p) = pick_closest(idx.lookup(&key), ctx.from_file) {
                    return Some(p);
                }
                let mut parts: Vec<&str> = key.split('/').collect();
                while parts.len() > 1 {
                    parts.pop();
                    let trimmed = parts.join("/");
                    if let Some(p) = pick_closest(idx.lookup(&trimmed), ctx.from_file) {
                        return Some(p);
                    }
                }
                None
            };

        if let Some(rest) = spec.strip_prefix("crate::") {
            return resolve_with_fallback(rest.replace("::", "/"));
        }
        if let Some(rest) = spec.strip_prefix("self::") {
            let key = rest.replace("::", "/");
            return resolve_relative(&key, ctx, idx, 0);
        }
        let mut s = spec;
        let mut up = 0usize;
        while let Some(rest) = s.strip_prefix("super::") {
            s = rest;
            up += 1;
        }
        if up > 0 {
            let key = s.replace("::", "/");
            return resolve_relative(&key, ctx, idx, up);
        }
        return resolve_with_fallback(spec.replace("::", "/"));
    }

    // Python relative imports come in as a key like `./x/y` or
    // `../pkg/util` — `extract` already accounts for the leading dots.
    if spec.starts_with("./") || spec.starts_with("../") {
        return resolve_relative_path(spec, ctx, idx);
    }

    // TS/JS relative: `./foo` / `../foo`.
    if (matches!(
        ctx.lang,
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript
    )) && (spec.starts_with('.'))
    {
        return resolve_relative_path(spec, ctx, idx);
    }

    // tsconfig path aliases.
    if matches!(
        ctx.lang,
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript
    ) {
        for (prefix, replacement) in ctx.path_aliases {
            if let Some(rest) = spec.strip_prefix(prefix.as_str()) {
                let combined = format!("{}{}", replacement, rest);
                let key = combined.trim_start_matches("./").to_string();
                if let Some(p) = pick_closest(idx.lookup(&key), ctx.from_file) {
                    return Some(p);
                }
            }
        }
    }

    // Go `import "mymod/pkg/foo"` — strip the module prefix. The module may
    // live in a subdirectory of the root, and a root may hold several, so
    // every `go.mod` under it is a candidate. Three orderings decide which
    // one the import belongs to, and a lexical tie-break keeps the result
    // independent of walk order:
    //
    //  - the deepest module directory holding the importer first. That is
    //    the module the importer is part of, and Go resolves an import
    //    within it before consulting anything else; a module elsewhere in
    //    the tree is a separate build even when it declares a longer path
    //    that also matches (`testdata/`, a nested checkout, a vendored copy);
    //  - then longest module path, which is what decides among candidates
    //    that do not hold the importer at all — there a nested module beats
    //    the one containing it;
    //  - then, among equal prefixes, the copy nearest the importer, the way
    //    `pick_closest` picks among suffix hits. A root can hold the same
    //    module twice, and an import must not jump into the other copy.
    //
    // A candidate whose directory holds no such package is skipped rather
    // than ending the search: the module path a nested module declares need
    // not mirror its directory.
    if ctx.lang == Lang::Go {
        let trimmed = spec.trim_matches('"');
        let mut candidates: Vec<(Option<usize>, usize, usize, String)> = Vec::new();
        for (prefix, dir) in &idx.go_modules {
            // `trimmed == prefix` names the module's own root package, which
            // the directory-as-package lookup below cannot express.
            let Some(rest) = trimmed
                .strip_prefix(prefix.as_str())
                .and_then(|r| r.strip_prefix('/'))
            else {
                continue;
            };
            // Look for any file in the `<module dir>/<rest>/` directory.
            let key = if dir.is_empty() {
                rest.to_string()
            } else {
                format!("{}/{}", dir, rest)
            };
            let mod_dir = idx.root.join(dir);
            // Depth of the module directory when it holds the importer, so
            // the innermost enclosing module wins; `None` — which sorts
            // below every `Some` — when it does not hold the importer.
            let enclosing = ctx
                .from_file
                .starts_with(&mod_dir)
                .then(|| mod_dir.components().count());
            let closeness = shared_leading_segments(ctx.from_file, &mod_dir);
            candidates.push((enclosing, prefix.len(), closeness, key));
        }
        candidates.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.3.cmp(&b.3))
        });
        for (_, _, _, key) in candidates {
            if let Some(found) = find_dir_file(idx, &key) {
                return Some(found);
            }
        }
        // Stdlib, a third-party dependency, or no go.mod at all → external.
        return None;
    }

    // Java/Kotlin/C#/Scala: `import com.foo.Bar;` — slash-joined suffix.
    if matches!(
        ctx.lang,
        Lang::Java | Lang::Kotlin | Lang::CSharp | Lang::Scala
    ) {
        let key = spec.replace('.', "/");
        // Try as-is first (matches `<package>/<TypeName>` index entries).
        if let Some(p) = pick_closest(idx.lookup(&key), ctx.from_file) {
            return Some(p);
        }
        // Try with last segment stripped — handles `import foo.Bar.Inner`
        // where Inner is a nested class inside `foo/Bar.java`.
        if let Some((parent, _)) = key.rsplit_once('/') {
            if let Some(p) = pick_closest(idx.lookup(parent), ctx.from_file) {
                return Some(p);
            }
        }
        return None;
    }

    // PHP: `use App\Core\Foo;` arrives normalised to `App/Core/Foo`.
    // Resolution order: PSR-4 prefix → suffix lookup → last-segment fallback.
    if ctx.lang == Lang::Php {
        // 1. PSR-4 prefix replacement (longest-first; `php_psr4` is pre-sorted).
        for (prefix, dir) in ctx.php_psr4 {
            if let Some(rest) = spec.strip_prefix(prefix.as_str()) {
                let key = if dir.is_empty() {
                    rest.to_string()
                } else {
                    format!("{}/{}", dir.trim_end_matches('/'), rest)
                };
                if let Some(p) = pick_closest(idx.lookup(&key), ctx.from_file) {
                    return Some(p);
                }
                // Direct file existence check — handles cases where the
                // suffix index doesn't have a matching key (e.g., file at
                // root is indexed by stem only).
                let abs = idx.root.join(format!("{}.php", key));
                if abs.is_file() {
                    return Some(abs);
                }
            }
        }
        // 2. Direct suffix lookup — covers projects without composer.json
        // where the file layout happens to mirror the namespace.
        if let Some(p) = pick_closest(idx.lookup(spec), ctx.from_file) {
            return Some(p);
        }
        // 3. Last-segment fallback — class name only (e.g. `Foo` from
        // `App\Core\Foo`). Mirrors the Java nested-class fallback above.
        if let Some(last) = spec.rsplit('/').next() {
            if last != spec {
                if let Some(p) = pick_closest(idx.lookup(last), ctx.from_file) {
                    return Some(p);
                }
            }
        }
        return None;
    }

    // C++: only relative includes resolve. System headers (`<vector>` etc.)
    // arrive here without a `./` prefix and have no project-local target.
    if ctx.lang == Lang::Cpp {
        return None;
    }

    // Ruby: only `require_relative` resolves. Bare `require 'gem'` and
    // `load`/`autoload` paths target $LOAD_PATH or installed gems.
    if ctx.lang == Lang::Ruby {
        return None;
    }

    // Python `from a.b import c` arrives normalised to `a/b/c` already.
    let key = spec.replace('.', "/");
    pick_closest(idx.lookup(&key), ctx.from_file)
}

/// Walk a relative `./x/y` style path against `from_file`'s directory.
fn resolve_relative_path(
    spec: &str,
    ctx: &ResolveCtx<'_>,
    idx: &SuffixIndex,
) -> Option<PathBuf> {
    let parent = ctx.from_file.parent()?;
    let mut cur = parent.to_path_buf();
    let mut remaining = spec;
    while let Some(rest) = remaining.strip_prefix("../") {
        cur = cur.parent()?.to_path_buf();
        remaining = rest;
    }
    while let Some(rest) = remaining.strip_prefix("./") {
        remaining = rest;
    }
    // Strip a known extension if present.
    let target = cur.join(remaining);
    if target.is_file() {
        return Some(target);
    }
    // Try common extensions for TS/JS.
    if matches!(
        ctx.lang,
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript
    ) {
        let ext_order: &[&str] = &[
            ".ts", ".tsx", ".mts", ".cts", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".json",
        ];
        for e in ext_order {
            let p = with_ext(&target, e);
            if p.is_file() {
                return Some(p);
            }
        }
        // Index file fallback.
        for e in ext_order {
            let p = target.join(format!("index{}", e));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if ctx.lang == Lang::Python {
        for e in [".py", ".pyi"] {
            let p = with_ext(&target, e);
            if p.is_file() {
                return Some(p);
            }
        }
        let init = target.join("__init__.py");
        if init.is_file() {
            return Some(init);
        }
        // The imported name may not be its own file — drop the trailing
        // segment and try again. `from .helpers import greet` → first
        // tries `helpers/greet.py`, then falls back to `helpers.py`.
        if let Some(parent) = target.parent() {
            for e in [".py", ".pyi"] {
                let p = with_ext(parent, e);
                if p.is_file() {
                    return Some(p);
                }
            }
            let init_parent = parent.join("__init__.py");
            if init_parent.is_file() {
                return Some(init_parent);
            }
        }
    }
    // Last-ditch: ask the suffix index using the relative key.
    let rel = match target.strip_prefix(&idx.root) {
        Ok(r) => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => target.display().to_string(),
    };
    pick_closest(idx.lookup(&rel), ctx.from_file)
}

fn with_ext(p: &Path, ext: &str) -> PathBuf {
    let mut s = p.as_os_str().to_string_lossy().into_owned();
    s.push_str(ext);
    PathBuf::from(s)
}

/// Variant for Rust `super::` / `self::` chains that *don't* arrive as
/// `./x` strings — `key` is already slash-joined.
fn resolve_relative(
    key: &str,
    ctx: &ResolveCtx<'_>,
    idx: &SuffixIndex,
    ascend: usize,
) -> Option<PathBuf> {
    let mut parent = ctx.from_file.parent()?.to_path_buf();
    for _ in 0..ascend {
        parent = parent.parent()?.to_path_buf();
    }
    let target = parent.join(key);
    let candidates = [
        with_ext(&target, ".rs"),
        target.join("mod.rs"),
        target.clone(),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    // Fall back to suffix index (caller may have given us a path that
    // matches a deeper file).
    let rel = match target.strip_prefix(&idx.root) {
        Ok(r) => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => return None,
    };
    pick_closest(idx.lookup(&rel), ctx.from_file)
}

/// For Go: given a relative directory like `pkg/foo`, return any source
/// file inside it that we've indexed.
fn find_dir_file(idx: &SuffixIndex, rel_dir: &str) -> Option<PathBuf> {
    let dir_abs = idx.root.join(rel_dir);
    let mut best: Option<PathBuf> = None;
    for f in idx.by_file.keys() {
        if f.starts_with(&dir_abs) && f.parent() == Some(dir_abs.as_path()) {
            // Prefer non-test-file by name; otherwise lexicographic.
            match &best {
                None => best = Some(f.clone()),
                Some(prev) => {
                    if f < prev {
                        best = Some(f.clone());
                    }
                }
            }
        }
    }
    best
}

/// How many leading components two paths share. The measure of "nearest
/// the importer" used whenever several candidates are equally valid.
fn shared_leading_segments(a: &Path, b: &Path) -> usize {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .count()
}

/// When multiple files match a suffix, prefer the one whose path shares
/// the most leading components with the importer.
fn pick_closest(candidates: Option<&[PathBuf]>, from_file: &Path) -> Option<PathBuf> {
    let cands = candidates?;
    if cands.is_empty() {
        return None;
    }
    if cands.len() == 1 {
        return Some(cands[0].clone());
    }
    let mut best: Option<(usize, &PathBuf)> = None;
    for c in cands {
        let common = shared_leading_segments(from_file, c);
        match best {
            None => best = Some((common, c)),
            Some((prev_common, prev_c)) => {
                if common > prev_common || (common == prev_common && c < prev_c) {
                    best = Some((common, c));
                }
            }
        }
    }
    best.map(|(_, p)| p.clone())
}
