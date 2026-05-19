//! Filesystem primitives shared by every adapter.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {}", path.display(), e)),
    }
}

pub fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create dir {}: {}", parent.display(), e))?;
    }
    if path.exists() && !backup_exists(path) {
        let backup = backup_path(path);
        fs::copy(path, &backup).map_err(|e| format!("backup {}: {}", path.display(), e))?;
    }
    let tmp = path.with_extension(tmp_extension(path));
    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("create temp {}: {}", tmp.display(), e))?;
        f.write_all(contents.as_bytes())
            .map_err(|e| format!("write temp {}: {}", tmp.display(), e))?;
        f.sync_all()
            .map_err(|e| format!("fsync temp {}: {}", tmp.display(), e))?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        format!(
            "rename temp {} -> {}: {}",
            tmp.display(),
            path.display(),
            e
        )
    })?;
    Ok(())
}

fn tmp_extension(path: &Path) -> String {
    let prev = path.extension().and_then(|o| o.to_str()).unwrap_or("");
    if prev.is_empty() {
        "ast-bro.tmp".to_string()
    } else {
        format!("{}.ast-bro.tmp", prev)
    }
}

fn backup_path(path: &Path) -> std::path::PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let prev = path.extension().and_then(|o| o.to_str()).unwrap_or("");
    let suffix = if prev.is_empty() {
        format!("ast-bro.bak.{}", ts)
    } else {
        format!("{}.ast-bro.bak.{}", prev, ts)
    };
    path.with_extension(suffix)
}

fn backup_exists(path: &Path) -> bool {
    let parent = match path.parent() {
        Some(p) => p,
        None => return false,
    };
    let stem = match path.file_name().and_then(|o| o.to_str()) {
        Some(s) => s,
        None => return false,
    };
    let new_prefix = format!("{}.ast-bro.bak.", stem);
    let old_prefix = format!("{}.ast-outline.bak.", stem);
    fs::read_dir(parent)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with(&new_prefix) || n.starts_with(&old_prefix))
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn read_optional_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("missing.md");
        assert_eq!(read_optional(&p).unwrap(), None);
    }

    #[test]
    fn atomic_write_creates_file_and_parent() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("nested/sub/file.md");
        atomic_write(&p, "hello").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
    }

    #[test]
    fn atomic_write_backs_up_existing_once() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("file.md");
        fs::write(&p, "original").unwrap();
        atomic_write(&p, "v1").unwrap();
        atomic_write(&p, "v2").unwrap();
        let backups: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.contains(".ast-bro.bak."))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "expected exactly one backup, got {}",
            backups.len()
        );
        let backup_contents = fs::read_to_string(backups[0].path()).unwrap();
        assert_eq!(backup_contents, "original");
        assert_eq!(fs::read_to_string(&p).unwrap(), "v2");
    }
}
