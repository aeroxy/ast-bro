use std::path::Path;

use serde_json::Value;

use super::io::{atomic_write, read_optional};
use super::json_hook;
use super::json_object;
use super::marker_block::{self, ApplyOutcome};
use super::toml_object;
use super::{Change, InstallOpts, Status};

use toml_edit::{DocumentMut, Table};

/// Legacy MCP server name used before the ast-outline → ast-bro rename.
/// Uninstall routines check for this in addition to the current name so
/// ghost entries from older installs are cleaned up.
pub const OLD_MCP_SERVER_NAME: &str = "ast-outline";

pub fn install_prompt_in(
    path: &Path,
    snippet: &str,
    opts: &InstallOpts,
) -> Result<Change, String> {
    let existing = read_optional(path)?.unwrap_or_default();

    let body = snippet.trim_end_matches('\n').to_string() + "\n";
    let (new_contents, outcome) = marker_block::apply(
        &existing,
        &body,
        &body,
        snippet,
        env!("CARGO_PKG_VERSION"),
        opts.force,
    );
    match outcome {
        ApplyOutcome::UserEditsBlocked(diff) => Err(format!(
            "{}: user edits inside marker block; pass --force to overwrite\n{}",
            path.display(),
            diff
        )),
        ApplyOutcome::Appended
            if !opts.force && marker_block::has_unmanaged_brand_content(&existing) =>
        {
            Err(format!(
                "{}: user-written ast-bro content outside marker block; pass --force to overwrite",
                path.display()
            ))
        }
        _ => {
            if existing == new_contents {
                return Ok(Change::Skipped {
                    path: path.to_path_buf(),
                    reason: "already up to date".into(),
                });
            }
            if !opts.dry_run {
                atomic_write(path, &new_contents)?;
            }
            Ok(if existing.is_empty() {
                Change::Created(path.to_path_buf())
            } else {
                Change::Updated(path.to_path_buf())
            })
        }
    }
}

/// Like `install_prompt_in` but seeds an empty file with `frontmatter` before
/// appending the marker block. This keeps valid YAML frontmatter at offset 0
/// for Claude Code sub-agent files, while still allowing users who already
/// have their own `.claude/agents/<Name>.md` to retain their customizations.
pub fn install_subagent_in(
    path: &Path,
    frontmatter: &str,
    snippet: &str,
    opts: &InstallOpts,
) -> Result<Change, String> {
    let on_disk = read_optional(path)?.unwrap_or_default();
    let is_new = on_disk.is_empty();
    // When the file doesn't exist yet, seed `existing` with the frontmatter so
    // marker_block::apply appends the block after it rather than at offset 0.
    let existing = if is_new { frontmatter.to_string() } else { on_disk.clone() };

    let body = snippet.trim_end_matches('\n').to_string() + "\n";
    let (new_contents, outcome) = marker_block::apply(
        &existing,
        &body,
        &body,
        snippet,
        env!("CARGO_PKG_VERSION"),
        opts.force,
    );
    match outcome {
        ApplyOutcome::UserEditsBlocked(diff) => Err(format!(
            "{}: user edits inside marker block; pass --force to overwrite\n{}",
            path.display(),
            diff
        )),
        // `!is_new` is defense-in-depth: when the file doesn't exist yet,
        // `on_disk` is empty and `has_unmanaged_brand_content` would return
        // false anyway, but skipping the call also makes the intent clear —
        // the snippet-shape guard only protects pre-existing user content.
        // The seeded `frontmatter` is content *we* control, so it would be
        // wrong to flag it as a "user-written conflict" even hypothetically.
        ApplyOutcome::Appended
            if !opts.force && !is_new && marker_block::has_unmanaged_brand_content(&on_disk) =>
        {
            Err(format!(
                "{}: user-written ast-bro content outside marker block; pass --force to overwrite",
                path.display()
            ))
        }
        _ => {
            if on_disk == new_contents {
                return Ok(Change::Skipped {
                    path: path.to_path_buf(),
                    reason: "already up to date".into(),
                });
            }
            if !opts.dry_run {
                atomic_write(path, &new_contents)?;
            }
            Ok(if is_new {
                Change::Created(path.to_path_buf())
            } else {
                Change::Updated(path.to_path_buf())
            })
        }
    }
}

pub fn install_json_hook_in<F>(
    path: &Path,
    hook_path: &[&str],
    entry: Value,
    matches: F,
    opts: &InstallOpts,
) -> Result<Change, String>
where
    F: Fn(&Value) -> bool,
{
    let existing = read_optional(path)?.unwrap_or_else(|| "{}".into());
    let mut root: Value = serde_json::from_str(&existing)
        .map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let modified = json_hook::upsert(&mut root, hook_path, entry, matches);
    if !modified {
        return Ok(Change::Skipped {
            path: path.to_path_buf(),
            reason: "already up to date".into(),
        });
    }
    let new_contents = serde_json::to_string_pretty(&root).unwrap() + "\n";
    if !opts.dry_run {
        atomic_write(path, &new_contents)?;
    }
    Ok(if existing.trim() == "{}" || existing.is_empty() {
        Change::Created(path.to_path_buf())
    } else {
        Change::Updated(path.to_path_buf())
    })
}

pub fn uninstall_prompt_in(path: &Path, opts: &InstallOpts) -> Result<Option<Change>, String> {
    let Some(existing) = read_optional(path)? else {
        return Ok(None);
    };
    let (out, removed) = marker_block::remove(&existing);
    if !removed {
        return Ok(None);
    }
    if !opts.dry_run {
        atomic_write(path, &out)?;
    }
    Ok(Some(Change::Removed(path.to_path_buf())))
}

pub fn uninstall_json_hook_in<F>(
    path: &Path,
    hook_path: &[&str],
    matches: F,
    opts: &InstallOpts,
) -> Result<Option<Change>, String>
where
    F: Fn(&Value) -> bool,
{
    let Some(existing) = read_optional(path)? else {
        return Ok(None);
    };
    let mut root: Value = serde_json::from_str(&existing)
        .map_err(|e| format!("parse {}: {}", path.display(), e))?;
    if !json_hook::remove(&mut root, hook_path, matches) {
        return Ok(None);
    }
    let new_contents = serde_json::to_string_pretty(&root).unwrap() + "\n";
    if !opts.dry_run {
        atomic_write(path, &new_contents)?;
    }
    Ok(Some(Change::Removed(path.to_path_buf())))
}

pub fn install_mcp_in(
    path: &Path,
    key_path: &[&str],
    name: &str,
    old_name: &str,
    entry: Value,
    opts: &InstallOpts,
) -> Result<Change, String> {
    let existing = read_optional(path)?.unwrap_or_else(|| "{}".into());
    let mut root: Value = serde_json::from_str(&existing)
        .map_err(|e| format!("parse {}: {}", path.display(), e))?;

    let mut modified = false;
    // Remove old name if it exists (migration)
    if json_object::remove(&mut root, key_path, old_name) {
        modified = true;
    }
    // Upsert current name
    if json_object::upsert(&mut root, key_path, name, entry) {
        modified = true;
    }

    if !modified {
        return Ok(Change::Skipped {
            path: path.to_path_buf(),
            reason: "already up to date".into(),
        });
    }
    let new_contents = serde_json::to_string_pretty(&root).unwrap() + "\n";
    if !opts.dry_run {
        atomic_write(path, &new_contents)?;
    }
    Ok(if existing.trim() == "{}" || existing.is_empty() {
        Change::Created(path.to_path_buf())
    } else {
        Change::Updated(path.to_path_buf())
    })
}

pub fn uninstall_json_object_in(
    path: &Path,
    key_path: &[&str],
    key: &str,
    opts: &InstallOpts,
) -> Result<Option<Change>, String> {
    let Some(existing) = read_optional(path)? else {
        return Ok(None);
    };
    let mut root: Value = serde_json::from_str(&existing)
        .map_err(|e| format!("parse {}: {}", path.display(), e))?;
    if !json_object::remove(&mut root, key_path, key) {
        return Ok(None);
    }
    let new_contents = serde_json::to_string_pretty(&root).unwrap() + "\n";
    if !opts.dry_run {
        atomic_write(path, &new_contents)?;
    }
    Ok(Some(Change::Removed(path.to_path_buf())))
}

pub fn install_toml_mcp_in(
    path: &Path,
    parent: &str,
    name: &str,
    old_name: &str,
    entry: Table,
    opts: &InstallOpts,
) -> Result<Change, String> {
    let existing = read_optional(path)?.unwrap_or_default();
    let mut doc: DocumentMut = existing
        .parse()
        .map_err(|e| format!("parse {}: {}", path.display(), e))?;

    let mut modified = false;
    // Remove old name if it exists (migration)
    if toml_object::remove(&mut doc, parent, old_name) {
        modified = true;
    }
    // Upsert current name
    if toml_object::upsert(&mut doc, parent, name, entry) {
        modified = true;
    }

    if !modified {
        return Ok(Change::Skipped {
            path: path.to_path_buf(),
            reason: "already up to date".into(),
        });
    }
    let new_contents = doc.to_string();
    if !opts.dry_run {
        atomic_write(path, &new_contents)?;
    }
    Ok(if existing.is_empty() {
        Change::Created(path.to_path_buf())
    } else {
        Change::Updated(path.to_path_buf())
    })
}

pub fn uninstall_toml_object_in(
    path: &Path,
    parent: &str,
    key: &str,
    opts: &InstallOpts,
) -> Result<Option<Change>, String> {
    let Some(existing) = read_optional(path)? else {
        return Ok(None);
    };
    let mut doc: DocumentMut = existing
        .parse()
        .map_err(|e| format!("parse {}: {}", path.display(), e))?;
    if !toml_object::remove(&mut doc, parent, key) {
        return Ok(None);
    }
    if !opts.dry_run {
        atomic_write(path, &doc.to_string())?;
    }
    Ok(Some(Change::Removed(path.to_path_buf())))
}

/// Install a plain file (no marker block, no JSON merge). Idempotent by
/// byte-identical comparison. Used for files we own end-to-end (e.g. the
/// Claude Code `SKILL.md`, where YAML frontmatter forbids comment markers).
pub fn install_plain_file_in(
    path: &Path,
    contents: &str,
    opts: &InstallOpts,
) -> Result<Change, String> {
    let existing = read_optional(path)?;
    if existing.as_deref() == Some(contents) {
        return Ok(Change::Skipped {
            path: path.to_path_buf(),
            reason: "already up to date".into(),
        });
    }
    if !opts.dry_run {
        atomic_write(path, contents)?;
    }
    Ok(if existing.is_some() {
        Change::Updated(path.to_path_buf())
    } else {
        Change::Created(path.to_path_buf())
    })
}

/// Remove a plain file we wrote, but only if its first line still
/// contains `expected_marker` — guards against deleting a file the user
/// has fully replaced with their own content. Also tries to remove the
/// parent directory (succeeds only if empty), keeping `~/.claude/skills/`
/// itself intact when other skills are present.
pub fn uninstall_plain_file_in(
    path: &Path,
    expected_marker: &str,
    opts: &InstallOpts,
) -> Result<Option<Change>, String> {
    let Some(existing) = read_optional(path)? else {
        return Ok(None);
    };
    let first_line = existing.lines().next().unwrap_or("");
    let still_ours = existing.contains(expected_marker) || first_line.contains(expected_marker);
    if !still_ours {
        return Ok(None);
    }
    if !opts.dry_run {
        std::fs::remove_file(path)
            .map_err(|e| format!("remove {}: {}", path.display(), e))?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent); // succeeds only if empty
        }
    }
    Ok(Some(Change::Removed(path.to_path_buf())))
}

pub fn status_for<F>(
    prompt_path: Option<&Path>,
    settings_path: Option<&Path>,
    hook_path: &[&str],
    matches: F,
) -> Status
where
    F: Fn(&Value) -> bool,
{
    let mut s = Status::default();
    if let Some(pp) = prompt_path {
        if let Ok(Some(contents)) = read_optional(pp) {
            s.prompt_version = marker_block::installed_version(&contents);
            s.prompt_installed = s.prompt_version.is_some();
        }
    }
    if let Some(sp) = settings_path {
        if let Ok(Some(contents)) = read_optional(sp) {
            if let Ok(root) = serde_json::from_str::<Value>(&contents) {
                s.hook_installed = json_hook::is_installed(&root, hook_path, matches);
            }
        }
    }
    s
}

pub fn status_for_prompt_only(prompt_path: Option<&Path>) -> Status {
    let mut s = Status::default();
    if let Some(pp) = prompt_path {
        if let Ok(Some(contents)) = read_optional(pp) {
            s.prompt_version = marker_block::installed_version(&contents);
            s.prompt_installed = s.prompt_version.is_some();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_prompt_rejects_existing_snippet_like_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        // User pasted snippet-shaped content (backticked brand) without our markers.
        std::fs::write(&path, "Use `ast-bro` to explore the code.\n").unwrap();
        let err = install_prompt_in(&path, "## Snippet\n", &InstallOpts::default()).unwrap_err();
        assert!(err.contains("user-written ast-bro content outside marker block"));
        // File must not be touched on rejection.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "Use `ast-bro` to explore the code.\n");
    }

    #[test]
    fn install_prompt_force_overrides_snippet_check() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "Use `ast-bro` to explore.\n").unwrap();
        let opts = InstallOpts { force: true, ..Default::default() };
        let change = install_prompt_in(&path, "## Snippet\n", &opts).unwrap();
        assert!(matches!(change, Change::Updated(_)));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("<!-- ast-bro:begin"));
        // Original line is preserved above the new block.
        assert!(after.starts_with("Use `ast-bro` to explore.\n"));
    }

    #[test]
    fn install_prompt_allows_unrelated_existing_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "# My project notes\nNothing branded here.\n").unwrap();
        let change = install_prompt_in(&path, "## Snippet\n", &InstallOpts::default()).unwrap();
        assert!(matches!(change, Change::Updated(_)));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("<!-- ast-bro:begin"));
    }

    #[test]
    fn install_prompt_allows_casual_prose_mention() {
        // Plain prose mention isn't snippet-shaped — install should proceed.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "Our team uses ast-bro among other tools.\n").unwrap();
        let change = install_prompt_in(&path, "## Snippet\n", &InstallOpts::default()).unwrap();
        assert!(matches!(change, Change::Updated(_)));
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("<!-- ast-bro:begin"));
    }
}
