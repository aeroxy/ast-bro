use glob::glob;
use std::path::{Path, PathBuf};

pub fn expand_existing(path: &Path) -> Vec<PathBuf> {
    if path.exists() {
        return vec![path.to_path_buf()];
    }
    expand_pattern(path)
}

pub fn expand_pattern(pattern: &Path) -> Vec<PathBuf> {
    glob(&pattern.to_string_lossy().replace('\\', "/"))
        .map(|paths| paths.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Does `pattern` match anything? Stops at the first hit instead of building
/// the whole match list, which is what a caller asking a yes/no question wants:
/// `!expand_pattern(p).is_empty()` walks and allocates every match of a broad
/// pattern (`show '**/*'`) only to look at whether the vector is non-empty.
pub fn pattern_matches_any(pattern: &Path) -> bool {
    glob(&pattern.to_string_lossy().replace('\\', "/"))
        .map(|mut paths| paths.any(|p| p.is_ok()))
        .unwrap_or(false)
}
