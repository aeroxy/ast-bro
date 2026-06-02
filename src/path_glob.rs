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
