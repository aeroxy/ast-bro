//! Shared file-walk filtering used by every ast-bro subcommand.
//!
//! Two layers on top of `ignore::WalkBuilder`'s default `.gitignore` handling:
//!
//! 1. **`.ast-bro-ignore`** — a custom gitignore-syntax file that lets a
//!    repo exclude paths from ast-bro specifically without polluting
//!    `.gitignore`. Useful for things like generated fixtures that you want
//!    git-tracked but not analysed.
//! 2. **Hardcoded denylist** — directories almost no one wants ast-bro to
//!    walk into (build outputs, dependency caches, vendored deps). A safety
//!    net for repos that forget to gitignore these.
//!
//! Both are applied uniformly across `outline`, `digest`, `show`,
//! `implements`, and the new `search` / `find-related` / `index` commands.

use ignore::WalkBuilder;
use std::fs;
use std::path::Path;

/// Directories we always skip — even if `.gitignore` doesn't list them.
///
/// Synced with the file-selection plan. New entries should be ones that:
///   - virtually never contain searchable user code
///   - are huge enough to slow indexing meaningfully
///   - have a stable, conventional name
pub const HARDCODED_IGNORE_DIRS: &[&str] = &[
    // VCS
    ".git", ".hg", ".svn", ".jj",
    // Python
    "__pycache__", ".venv", "venv", ".tox",
    ".mypy_cache", ".pytest_cache", ".ruff_cache",
    // JS/TS
    "node_modules", ".next", ".nuxt", ".turbo", ".parcel-cache",
    // Build outputs
    "dist", "build", "out", ".eggs", "target",
    // Other
    ".cache", ".gradle", ".idea", ".vscode",
    // Self
    ".ast-bro",
];

/// Wire `.ast-bro-ignore` into a `WalkBuilder`.
///
/// Call this on every walker that should observe ast-bro's per-repo
/// excludes. It's separate from `should_skip_path` because the `ignore` crate
/// can prune ignored directories before recursing into them — much faster
/// than visiting every entry and post-filtering.
///
/// Also accepts `.ast-outline-ignore` for backward compatibility. If only the
/// old file exists, it is auto-renamed to `.ast-bro-ignore` with a stderr
/// notice.
pub fn add_filters(builder: &mut WalkBuilder, repo_root: &Path) {
    let new_name = ".ast-bro-ignore";
    let old_name = ".ast-outline-ignore";

    let new_path = repo_root.join(new_name);
    let old_path = repo_root.join(old_name);

    if old_path.exists() && !new_path.exists() {
        if let Err(e) = fs::rename(&old_path, &new_path) {
            eprintln!("warning: could not rename {old_name} -> {new_name}: {e}");
        } else {
            eprintln!("info: auto-renamed {old_name} -> {new_name}");
        }
    }

    builder.add_custom_ignore_filename(new_name);
}

/// Return `true` if any component of `path` (relative to `repo_root`) matches
/// the hardcoded denylist. Used as a post-filter — the `ignore` crate handles
/// `.gitignore` and `.ast-bro-ignore` for us, but the denylist is our
/// belt-and-suspenders.
///
/// Components are compared case-sensitively; directory names like
/// `node_modules` are conventionally lower-case on every platform.
pub fn should_skip_path(path: &Path, repo_root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(repo_root) else {
        return false;
    };
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        HARDCODED_IGNORE_DIRS.iter().any(|d| *d == s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn skip_node_modules_anywhere() {
        let root = PathBuf::from("/r");
        assert!(should_skip_path(&root.join("node_modules/lodash/index.js"), &root));
        assert!(should_skip_path(
            &root.join("packages/foo/node_modules/lib.js"),
            &root,
        ));
    }

    #[test]
    fn skip_target_dir() {
        let root = PathBuf::from("/r");
        assert!(should_skip_path(&root.join("target/debug/build/x.rs"), &root));
    }

    #[test]
    fn skip_self_managed_index() {
        let root = PathBuf::from("/r");
        assert!(should_skip_path(&root.join(".ast-bro/index/meta.json"), &root));
    }

    #[test]
    fn allow_normal_paths() {
        let root = PathBuf::from("/r");
        assert!(!should_skip_path(&root.join("src/main.rs"), &root));
        assert!(!should_skip_path(&root.join("docs/README.md"), &root));
    }

    #[test]
    fn allow_paths_outside_root() {
        let root = PathBuf::from("/r");
        // strip_prefix fails → not skipped (let caller decide).
        assert!(!should_skip_path(&PathBuf::from("/elsewhere/node_modules/x"), &root));
    }
}
