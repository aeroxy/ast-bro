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
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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
    // Self (keep legacy name during transition)
    ".ast-bro", ".ast-outline",
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
    let rename_failed = migrate_legacy_ignore_file(repo_root, new_name, old_name);
    if rename_failed {
        builder.add_custom_ignore_filename(old_name);
    }
    builder.add_custom_ignore_filename(new_name);
}

/// Per-process guard so the `.ast-outline-ignore` -> `.ast-bro-ignore`
/// rename is attempted at most once per repo root. Returns `true` when a
/// previous (or current) attempt left the legacy file in place, so the
/// caller should keep the legacy filename registered as a fallback.
fn migrate_legacy_ignore_file(repo_root: &Path, new_name: &str, old_name: &str) -> bool {
    // The walker accepts file paths too (e.g. `ast-bro run -p X a.rs b.rs`);
    // those flow in here as `repo_root`. `repo_root.join(".ast-bro-ignore")`
    // on a file path is nonsensical, so skip the migration attempt entirely
    // — and don't cache the file-path key, since it can't represent a repo.
    if !repo_root.is_dir() {
        return false;
    }
    static STATE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let map = STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    if let Some(&needs_fallback) = guard.get(repo_root) {
        return needs_fallback;
    }
    let new_path = repo_root.join(new_name);
    let old_path = repo_root.join(old_name);
    let needs_fallback = if old_path.exists() && !new_path.exists() {
        match fs::rename(&old_path, &new_path) {
            Err(e) => {
                eprintln!("warning: could not rename {old_name} -> {new_name}: {e}");
                true
            }
            Ok(()) => {
                eprintln!("info: auto-renamed {old_name} -> {new_name}");
                false
            }
        }
    } else {
        false
    };
    guard.insert(repo_root.to_path_buf(), needs_fallback);
    needs_fallback
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

/// Path components that denote a test directory (case-insensitive).
const TEST_DIR_TOKENS: &[&str] = &[
    "test", "tests", "__tests__", "e2e", "cypress", "playwright",
    "integration", "integration-tests", "test-fixtures", "fixtures",
    "spec", "specs", "mocha", "jest",
];

/// File-stem suffixes (before the final extension) that mark test files.
/// Checked against the lowered stem.
const TEST_FILE_SUFFIXES: &[&str] = &[
    "_test", ".test", "_spec", ".spec",
    "_tests", ".tests", "_specs", ".specs",
];

/// Return `true` when `path` (relative to `repo_root`) looks like a test
/// file by path heuristics — covers `tests/`, `__tests__`,
/// `*.test.ts`, `*_test.go`, `*.spec.js`, `e2e/`, `cypress/`, etc.
///
/// Both directory components and the file stem are consulted. Returns
/// `false` for production-looking paths outside those directories.
pub fn is_test_file(path: &Path, repo_root: &Path) -> bool {
    let rel = path.strip_prefix(repo_root).unwrap_or(path);
    for component in rel.components() {
        let s = component.as_os_str().to_string_lossy();
        let lower = s.to_lowercase();
        if TEST_DIR_TOKENS.iter().any(|t| *t == lower) {
            return true;
        }
    }
    let stem = rel
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if TEST_FILE_SUFFIXES.iter().any(|suf| stem.ends_with(suf)) {
        return true;
    }
    false
}

/// Detect the programming language of `path` by inspecting its shebang line.
///
/// This is used as a fallback for extensionless files (e.g. scripts
/// like `./bin/deploy` with `#!/usr/bin/env python3`).
pub fn detect_language(path: &Path) -> Option<ast_grep_language::SupportLang> {
    use ast_grep_language::SupportLang;
    use std::io::Read;

    // Read first 256 bytes to check for shebang
    let mut file = fs::File::open(path).ok()?;
    let mut buffer = [0u8; 256];
    let bytes_read = file.read(&mut buffer).ok()?;
    
    if bytes_read < 2 {
        return None;
    }
    
    // Must start with #!
    if buffer[0] != b'#' || buffer[1] != b'!' {
        return None;
    }
    
    // Find the first newline within the read bytes
    let newline_pos = buffer[..bytes_read]
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes_read);
    
    // If the shebang line runs past the scan window, it's too long to be valid.
    // When the file fit entirely inside the window (`bytes_read < buffer.len()`),
    // EOF terminates the line just as well as a newline.
    if newline_pos == bytes_read && bytes_read == buffer.len() {
        return None;
    }
    
    // Extract the first line as UTF-8
    let first_line = std::str::from_utf8(&buffer[..newline_pos]).ok()?;

    // Parse the shebang line
    let shebang = first_line[2..].trim();

    let mut tokens = shebang.split_whitespace();
    let command = tokens.next()?;

    // Check if the command is `env` (basename), e.g. /usr/bin/env or /usr/local/bin/env.
    // This avoids false positives on paths like /home/envuser/bin/python3.
    let command_basename = command.rsplit('/').next().unwrap_or(command);

    let program = if command_basename == "env" {
        // Skip env flags (-S, -i, …) and VAR=value assignments to find the interpreter.
        let mut program = tokens.next()?;
        while program.starts_with('-') || program.contains('=') {
            program = tokens.next()?;
        }
        program.rsplit('/').next().unwrap_or(program)
    } else {
        // Direct shebang like #!/usr/bin/python3
        command_basename
    };

    // Normalize program name (strip version suffixes like python3.11 -> python)
    let program = program
        .trim_end_matches(|c: char| c.is_ascii_digit() || c == '.')
        .to_lowercase();

    // Map program to language
    match program.as_str() {
        "python" | "python3" | "pypy" | "pypy3" => Some(SupportLang::Python),
        "ruby" | "rb" => Some(SupportLang::Ruby),
        "node" | "nodejs" | "bun" | "deno" => Some(SupportLang::TypeScript),
        "php" => Some(SupportLang::Php),
        "bash" | "sh" | "zsh" | "ksh" => Some(SupportLang::Bash),
        "lua" | "luajit" => Some(SupportLang::Lua),
        _ => None,
    }
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

    #[test]
    fn test_detection_directory_components() {
        let root = PathBuf::from("/r");
        assert!(is_test_file(&root.join("tests/foo.rs"), &root));
        assert!(is_test_file(&root.join("src/features/__tests__/a.ts"), &root));
        assert!(is_test_file(&root.join("e2e/signup.spec.ts"), &root));
        assert!(is_test_file(&root.join("cypress/integration/login.js"), &root));
    }

    #[test]
    fn test_detection_file_suffixes() {
        let root = PathBuf::from("/r");
        assert!(is_test_file(&root.join("src/foo_test.go"), &root));
        assert!(is_test_file(&root.join("src/utils.test.ts"), &root));
        assert!(is_test_file(&root.join("src/auth.spec.js"), &root));
        assert!(is_test_file(&root.join("src/bar_tests.py"), &root));
    }

    #[test]
    fn test_detection_production_paths() {
        let root = PathBuf::from("/r");
        assert!(!is_test_file(&root.join("src/foo.rs"), &root));
        assert!(!is_test_file(&root.join("lib/auth.py"), &root));
        assert!(!is_test_file(&root.join("src/test_utils.rs"), &root));
    }

    #[test]
    fn shebang_python() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("myscript");
        std::fs::write(&path, "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Python)
        );
    }

    #[test]
    fn shebang_with_env_flags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deploy");
        std::fs::write(&path, "#!/usr/bin/env -S python3\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Python)
        );
    }

    #[test]
    fn shebang_direct_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        std::fs::write(&path, "#!/usr/bin/python3\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Python)
        );
    }

    #[test]
    fn shebang_ruby() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script");
        std::fs::write(&path, "#!/usr/bin/ruby\nputs 'hi'\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Ruby)
        );
    }

    #[test]
    fn shebang_node_resolves_to_typescript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server");
        std::fs::write(&path, "#!/usr/bin/env node\nconsole.log(1);\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::TypeScript)
        );
    }

    #[test]
    fn no_shebang_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain");
        std::fs::write(&path, "just a plain file\n").unwrap();
        assert_eq!(detect_language(&path), None);
    }

    #[test]
    fn empty_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, "").unwrap();
        assert_eq!(detect_language(&path), None);
    }

    #[test]
    fn shebang_unrecognized_interpreter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool");
        std::fs::write(&path, "#!/usr/bin/env foointerpreter\n").unwrap();
        assert_eq!(detect_language(&path), None);
    }

    #[test]
    fn shebang_env_substring_in_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script");
        // "envuser" contains "env" but the command basename is "python3", not "env"
        std::fs::write(&path, "#!/home/envuser/bin/python3\nprint('hi')\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Python)
        );
    }

    #[test]
    fn shebang_env_with_tab_separator() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script");
        std::fs::write(&path, "#!/usr/bin/env\tpython3\nprint('hi')\n").unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Python)
        );
    }

    #[test]
    fn binary_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin");
        // Write some random binary bytes, no newline
        std::fs::write(&path, &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        assert_eq!(detect_language(&path), None);
    }

    #[test]
    fn long_line_without_newline_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long");
        // 300 bytes with no newline — longer than our 256-byte limit
        let content = "!".repeat(300);
        std::fs::write(&path, content).unwrap();
        // Should return None without reading the whole file
        assert_eq!(detect_language(&path), None);
    }

    #[test]
    fn shebang_at_boundary_256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge");
        // Valid shebang with lots of spaces, newline just inside 256 bytes
        let padding = " ".repeat(230);
        let content = format!("#!/usr/bin/env python3{}\nprint('hi')\n", padding);
        assert!(content.as_bytes()[..256].iter().position(|&b| b == b'\n').is_some());
        std::fs::write(&path, content).unwrap();
        assert_eq!(
            detect_language(&path),
            Some(ast_grep_language::SupportLang::Python)
        );
    }

    #[test]
    fn shebang_newline_beyond_256_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toolong");
        // Shebang with no newline within first 256 bytes
        let padding = " ".repeat(300);
        let content = format!("#!/usr/bin/env python3{}\nprint('hi')\n", padding);
        // Confirm newline is at position > 256
        assert!(content.as_bytes()[..256].iter().position(|&b| b == b'\n').is_none());
        std::fs::write(&path, content).unwrap();
        // Should return None since the line is too long to be trusted
        assert_eq!(detect_language(&path), None);
    }
}
