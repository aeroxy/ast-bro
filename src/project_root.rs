//! Project-root resolution: walk up from a path arg looking for an existing
//! `.ast-bro/` directory, capped at CWD so we never escape the project
//! the user is working in.
//!
//! Used by both the search subsystem (looking for `.ast-bro/index/`) and
//! the deps subsystem (looking for `.ast-bro/deps/`). The two flavours
//! differ only in which subdirectory marker counts as "found".

use std::path::{Path, PathBuf};

/// Which `.ast-bro/<sub>` marker counts as "this is an existing project root".
#[derive(Clone, Copy)]
pub enum Marker {
    /// `.ast-bro/index/meta.json` — the search index.
    SearchIndex,
    /// `.ast-bro/deps/graph.bin` — the deps cache. Reserved for the
    /// deps-subsystem follow-up; not yet wired into deps CLI.
    #[allow(dead_code)]
    DepsCache,
    /// Any `.ast-bro/` directory. Reserved for shared resolver use.
    #[allow(dead_code)]
    Any,
}

impl Marker {
    fn matches(self, candidate: &Path) -> bool {
        let dir = candidate.join(".ast-bro");
        if !dir.is_dir() {
            return false;
        }
        match self {
            Marker::SearchIndex => dir.join("index").join("meta.json").is_file(),
            Marker::DepsCache => dir.join("deps").join("graph.bin").is_file(),
            Marker::Any => true,
        }
    }
}

/// Walk up from `path_arg` looking for an existing `.ast-bro/` (per
/// `marker`). Stop ascent at `cwd` (inclusive). Cap is enforced via
/// canonical-path prefix check so symlinks don't fool us.
///
/// Returns `(home, found_existing)`.
///
/// - `found_existing = true`: an existing `.ast-bro/<marker>` was located
///   at `home` (between `path_arg` and `cwd` inclusive).
/// - `found_existing = false`: no existing marker. `home` is `cwd` when
///   `path_arg` is under `cwd`, else `path_arg` itself.
pub fn resolve_home(path_arg: &Path, cwd: &Path, marker: Marker) -> (PathBuf, bool) {
    let abs_path = canonicalize_lenient(path_arg);
    let abs_cwd = canonicalize_lenient(cwd);

    // If path_arg is not under cwd (e.g. an absolute foreign path or an MCP
    // call with no meaningful cwd), don't walk up — treat the arg itself as
    // authoritative.
    if !abs_path.starts_with(&abs_cwd) {
        return (abs_path, false);
    }

    let start_dir: PathBuf = if abs_path.is_dir() {
        abs_path.clone()
    } else {
        abs_path.parent().map(Path::to_path_buf).unwrap_or(abs_path.clone())
    };

    let mut cur = start_dir.as_path();
    loop {
        if marker.matches(cur) {
            return (cur.to_path_buf(), true);
        }
        if cur == abs_cwd {
            break;
        }
        match cur.parent() {
            Some(p) if p.starts_with(&abs_cwd) || p == abs_cwd => cur = p,
            _ => break,
        }
    }

    // No marker found between path_arg and cwd → build at cwd.
    (abs_cwd, false)
}

/// Best-effort canonicalize. Handles paths whose tail doesn't exist yet
/// (common for `index <new-subdir>` and `find-related` against an
/// abstractly-named chunk file) by walking up to the nearest existing
/// ancestor, canonicalizing that, and rejoining the missing tail. This
/// keeps results consistent with `Path::canonicalize` on macOS where
/// `/var → /private/var` symlink resolution would otherwise produce
/// mismatched prefixes.
fn canonicalize_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    // Make the path absolute against cwd so we have a stable starting point.
    let abs: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(p)
    } else {
        return p.to_path_buf();
    };
    // Walk up looking for an existing ancestor we can canonicalize, then
    // rejoin the unresolved tail.
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cur = abs.as_path();
    loop {
        match cur.canonicalize() {
            Ok(c) => {
                let mut out = c;
                for seg in tail.into_iter().rev() {
                    out.push(seg);
                }
                return out;
            }
            Err(_) => match (cur.file_name(), cur.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    cur = parent;
                }
                _ => return abs,
            },
        }
    }
}

/// Express `path` relative to `home` as a POSIX-normalised string. Returns
/// `""` when `path == home`. Returns `None` when `path` is not under `home`.
pub fn relative_posix(path: &Path, home: &Path) -> Option<String> {
    let abs_path = canonicalize_lenient(path);
    let abs_home = canonicalize_lenient(home);
    let rel = abs_path.strip_prefix(&abs_home).ok()?;
    let s = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Some(s)
}

/// How a requested corpus relates to a recorded one. Both are POSIX paths
/// relative to home; `""` means whole home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusRel {
    /// Requested ⊆ recorded — already covered.
    Subset,
    /// Recorded ⊆ requested — widening.
    Superset,
    /// Neither contains the other — widen to common ancestor.
    /// `common` is the path components shared by both, joined with `/`.
    Sibling { common: String },
}

pub fn compare_corpus(recorded: &str, requested: &str) -> CorpusRel {
    let r_parts: Vec<&str> = if recorded.is_empty() {
        Vec::new()
    } else {
        recorded.split('/').collect()
    };
    let q_parts: Vec<&str> = if requested.is_empty() {
        Vec::new()
    } else {
        requested.split('/').collect()
    };

    // Recorded is whole home → requested is always a subset.
    if r_parts.is_empty() {
        return CorpusRel::Subset;
    }
    // Requested is whole home → recorded is a subset of requested → superset.
    if q_parts.is_empty() {
        return CorpusRel::Superset;
    }

    // Subset: requested starts with recorded.
    if q_parts.len() >= r_parts.len() && q_parts[..r_parts.len()] == r_parts[..] {
        return CorpusRel::Subset;
    }
    // Superset: recorded starts with requested.
    if r_parts.len() >= q_parts.len() && r_parts[..q_parts.len()] == q_parts[..] {
        return CorpusRel::Superset;
    }

    // Sibling: take longest common prefix.
    let common_len = r_parts
        .iter()
        .zip(q_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let common = r_parts[..common_len].join("/");
    CorpusRel::Sibling { common }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn compare_corpus_subset_explicit() {
        assert_eq!(
            compare_corpus("packages", "packages/a"),
            CorpusRel::Subset
        );
        assert_eq!(compare_corpus("packages", "packages"), CorpusRel::Subset);
    }

    #[test]
    fn compare_corpus_subset_when_recorded_whole_home() {
        // recorded "" = whole home; anything is a subset.
        assert_eq!(compare_corpus("", "packages"), CorpusRel::Subset);
        assert_eq!(compare_corpus("", ""), CorpusRel::Subset);
    }

    #[test]
    fn compare_corpus_superset() {
        assert_eq!(
            compare_corpus("packages/a", "packages"),
            CorpusRel::Superset
        );
        assert_eq!(compare_corpus("packages", ""), CorpusRel::Superset);
    }

    #[test]
    fn compare_corpus_sibling_with_common_ancestor() {
        assert_eq!(
            compare_corpus("packages/a", "packages/b"),
            CorpusRel::Sibling {
                common: "packages".to_string()
            }
        );
    }

    #[test]
    fn compare_corpus_sibling_no_common_ancestor() {
        assert_eq!(
            compare_corpus("packages", "src"),
            CorpusRel::Sibling {
                common: String::new()
            }
        );
    }

    #[test]
    fn resolve_home_finds_existing_above() {
        let dir = tempdir().unwrap();
        let cwd = dir.path();
        fs::create_dir_all(cwd.join(".ast-bro").join("index")).unwrap();
        fs::write(
            cwd.join(".ast-bro").join("index").join("meta.json"),
            "{}",
        )
        .unwrap();
        fs::create_dir_all(cwd.join("packages").join("xyz")).unwrap();

        let (home, found) = resolve_home(
            &cwd.join("packages").join("xyz"),
            cwd,
            Marker::SearchIndex,
        );
        assert!(found);
        assert_eq!(
            home.canonicalize().unwrap(),
            cwd.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_home_returns_cwd_when_no_existing() {
        let dir = tempdir().unwrap();
        let cwd = dir.path();
        fs::create_dir_all(cwd.join("packages").join("xyz")).unwrap();

        let (home, found) =
            resolve_home(&cwd.join("packages").join("xyz"), cwd, Marker::SearchIndex);
        assert!(!found);
        assert_eq!(
            home.canonicalize().unwrap(),
            cwd.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_home_caps_at_cwd_does_not_escape() {
        // Outer dir has the marker, but cwd is the inner dir — must not
        // escape past cwd to find the outer marker.
        let dir = tempdir().unwrap();
        let outer = dir.path();
        fs::create_dir_all(outer.join(".ast-bro").join("index")).unwrap();
        fs::write(
            outer.join(".ast-bro").join("index").join("meta.json"),
            "{}",
        )
        .unwrap();
        let inner = outer.join("inner");
        fs::create_dir_all(inner.join("sub")).unwrap();

        let (home, found) =
            resolve_home(&inner.join("sub"), &inner, Marker::SearchIndex);
        assert!(!found);
        assert_eq!(
            home.canonicalize().unwrap(),
            inner.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_home_path_outside_cwd_uses_path() {
        let outer = tempdir().unwrap();
        let cwd_dir = tempdir().unwrap();
        let cwd = cwd_dir.path();
        fs::create_dir_all(outer.path().join("a")).unwrap();

        let (home, found) =
            resolve_home(&outer.path().join("a"), cwd, Marker::SearchIndex);
        assert!(!found);
        // Resolves to the path arg itself (canonicalized), not cwd.
        assert_eq!(
            home.canonicalize().unwrap(),
            outer.path().join("a").canonicalize().unwrap()
        );
    }

    #[test]
    fn relative_posix_basic() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join("a").join("b")).unwrap();
        assert_eq!(
            relative_posix(&home.join("a").join("b"), home),
            Some("a/b".to_string())
        );
        assert_eq!(relative_posix(home, home), Some(String::new()));
    }
}
