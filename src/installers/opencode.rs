use std::path::{Path, PathBuf};

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde_json::Value;

use super::io::{atomic_write, read_optional};
use super::paths;
use super::{common, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::{agent_prompt, agent_skill_md};

pub struct OpenCode;

const MCP_SERVER_NAME: &str = "ast-bro";
const SKILL_MARKER: &str = "name: ast-bro";
const CONFIG_SCHEMA: &str = "https://opencode.ai/config.json";

impl OpenCode {
    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            // Shared with Codex. The common marker block makes repeated
            // installs idempotent, but uninstalling either target removes it.
            Scope::Local(root) => Ok(root.join("AGENTS.md")),
            Scope::Global => paths::under_home(".config/opencode/AGENTS.md"),
        }
    }

    fn config_paths(&self, scope: &Scope) -> Result<Vec<PathBuf>, String> {
        let paths = match scope {
            // OpenCode merges direct project configs first, then configs in
            // .opencode. Prefer the highest-precedence existing file.
            Scope::Local(root) => vec![
                root.join(".opencode/opencode.jsonc"),
                root.join(".opencode/opencode.json"),
                root.join("opencode.jsonc"),
                root.join("opencode.json"),
            ],
            Scope::Global => {
                let base = paths::under_home(".config/opencode")?;
                vec![base.join("opencode.jsonc"), base.join("opencode.json")]
            }
        };
        Ok(paths)
    }

    fn default_config_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join("opencode.json")),
            Scope::Global => paths::under_home(".config/opencode/opencode.json"),
        }
    }

    fn config_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        let candidates = self.config_paths(scope)?;
        if let Some(path) = candidates.into_iter().find(|path| path.exists()) {
            return Ok(path);
        }
        self.default_config_path(scope)
    }

    fn skill_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".opencode/skills/ast-bro/SKILL.md")),
            Scope::Global => paths::under_home(".config/opencode/skills/ast-bro/SKILL.md"),
        }
    }

    fn mcp_entry_json(&self) -> Value {
        // Keep this in sync with mcp_entry_cst. The idempotency test compares
        // the CST-written value with this representation to guard drift.
        serde_json::json!({
            "type": "local",
            "command": ["ast-bro", "mcp"],
            "enabled": true
        })
    }

    fn mcp_entry_cst(&self) -> CstInputValue {
        CstInputValue::Object(vec![
            ("type".into(), "local".into()),
            (
                "command".into(),
                vec!["ast-bro".to_string(), "mcp".to_string()].into(),
            ),
            ("enabled".into(), true.into()),
        ])
    }

    fn install_mcp_in(&self, path: &Path, opts: &InstallOpts) -> Result<Change, String> {
        let existing = read_optional(path)?;
        let is_new = existing.as_deref().map(str::trim).unwrap_or("").is_empty();
        let source = if is_new {
            "{}"
        } else {
            existing.as_deref().unwrap()
        };
        let root = CstRootNode::parse(source, &Default::default())
            .map_err(|e| format!("parse {}: {}", path.display(), e))?;
        let root_object = root.object_value_or_create().ok_or_else(|| {
            format!(
                "{}: top-level OpenCode config must be an object",
                path.display()
            )
        })?;

        let mut modified = false;
        if is_new {
            root_object.append("$schema", CONFIG_SCHEMA.into());
            modified = true;
        }

        let mcp = root_object.object_value_or_create("mcp").ok_or_else(|| {
            format!(
                "{}: OpenCode `mcp` setting must be an object",
                path.display()
            )
        })?;

        if let Some(old) = mcp.get(common::OLD_MCP_SERVER_NAME) {
            old.remove();
            modified = true;
        }

        match mcp.get(MCP_SERVER_NAME) {
            Some(prop) if prop.to_serde_value().as_ref() == Some(&self.mcp_entry_json()) => {}
            Some(prop) => {
                prop.set_value(self.mcp_entry_cst());
                modified = true;
            }
            None => {
                mcp.append(MCP_SERVER_NAME, self.mcp_entry_cst());
                modified = true;
            }
        }

        if !modified {
            return Ok(Change::Skipped {
                path: path.to_path_buf(),
                reason: "already up to date".into(),
            });
        }

        if !opts.dry_run {
            atomic_write(path, &root.to_string())?;
        }
        Ok(if is_new {
            Change::Created(path.to_path_buf())
        } else {
            Change::Updated(path.to_path_buf())
        })
    }

    fn uninstall_mcp_in(&self, path: &Path, opts: &InstallOpts) -> Result<Option<Change>, String> {
        let Some(existing) = read_optional(path)? else {
            return Ok(None);
        };
        let root = CstRootNode::parse(&existing, &Default::default())
            .map_err(|e| format!("parse {}: {}", path.display(), e))?;
        let Some(root_object) = root.object_value() else {
            return Ok(None);
        };
        let Some(mcp) = root_object.object_value("mcp") else {
            return Ok(None);
        };

        let mut modified = false;
        for name in [MCP_SERVER_NAME, common::OLD_MCP_SERVER_NAME] {
            if let Some(prop) = mcp.get(name) {
                prop.remove();
                modified = true;
            }
        }
        if !modified {
            return Ok(None);
        }

        if !opts.dry_run {
            atomic_write(path, &root.to_string())?;
        }
        Ok(Some(Change::Removed(path.to_path_buf())))
    }

    fn mcp_is_installed(&self, path: &Path) -> bool {
        let Ok(Some(contents)) = read_optional(path) else {
            return false;
        };
        let Ok(root) = CstRootNode::parse(&contents, &Default::default()) else {
            return false;
        };
        root.object_value()
            .and_then(|object| object.object_value("mcp"))
            .and_then(|mcp| mcp.get(MCP_SERVER_NAME))
            .is_some()
    }
}

impl Installer for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let config_exists = self
            .config_paths(scope)
            .ok()
            .map(|paths| paths.iter().any(|path| path.exists()))
            .unwrap_or(false);
        Detection {
            present: config_exists || paths::binary_on_path("opencode"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, agent_prompt(), opts)
    }

    fn install_hook(&self, _scope: &Scope, _opts: &InstallOpts) -> Result<Change, String> {
        Ok(Change::NotApplicable)
    }

    fn install_mcp(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        self.install_mcp_in(&self.config_path(scope)?, opts)
    }

    fn install_skills(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_plain_file_in(&self.skill_path(scope)?, agent_skill_md(), opts)
    }

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(change) = common::uninstall_prompt_in(&self.prompt_path(scope)?, opts)? {
            changes.push(change);
        }
        for path in self.config_paths(scope)? {
            if let Some(change) = self.uninstall_mcp_in(&path, opts)? {
                changes.push(change);
            }
        }
        if let Some(change) =
            common::uninstall_plain_file_in(&self.skill_path(scope)?, SKILL_MARKER, opts)?
        {
            changes.push(change);
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        let mut status = common::status_for_prompt_only(self.prompt_path(scope).ok().as_deref());
        if let Ok(paths) = self.config_paths(scope) {
            status.mcp_installed = paths.iter().any(|path| self.mcp_is_installed(path));
        }
        if let Ok(path) = self.skill_path(scope) {
            if let Ok(Some(contents)) = read_optional(&path) {
                status.skills_installed = contents.contains(SKILL_MARKER);
            }
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn local_scope(dir: &TempDir) -> Scope {
        Scope::Local(dir.path().to_path_buf())
    }

    #[test]
    fn install_writes_agents_md() {
        let dir = TempDir::new().unwrap();
        OpenCode
            .install_prompt(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let contents = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(contents.contains("ast-bro:begin"));
    }

    #[test]
    fn global_config_fallback_is_explicit_json_path() {
        let path = OpenCode.default_config_path(&Scope::Global).unwrap();
        assert!(path.ends_with(".config/opencode/opencode.json"));
    }

    #[test]
    fn install_mcp_creates_native_opencode_config() {
        let dir = TempDir::new().unwrap();
        let change = OpenCode
            .install_mcp(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        assert!(matches!(change, Change::Created(_)));
        let contents = std::fs::read_to_string(dir.path().join("opencode.json")).unwrap();
        let root = CstRootNode::parse(&contents, &Default::default()).unwrap();
        let value = root.to_serde_value().unwrap();
        assert_eq!(value["$schema"], CONFIG_SCHEMA);
        assert_eq!(value["mcp"]["ast-bro"]["type"], "local");
        assert_eq!(
            value["mcp"]["ast-bro"]["command"],
            serde_json::json!(["ast-bro", "mcp"])
        );
        assert_eq!(value["mcp"]["ast-bro"]["enabled"], true);
    }

    #[test]
    fn install_mcp_preserves_jsonc_comments_and_other_servers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(
            &path,
            r#"{
  // Keep this provider configuration.
  "provider": { "name": "openai" },
  "mcp": {
    "docs": {
      "type": "remote",
      "url": "https://example.com/mcp",
    },
  },
}
"#,
        )
        .unwrap();

        OpenCode
            .install_mcp(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("// Keep this provider configuration."));
        assert!(contents.contains("\"provider\": { \"name\": \"openai\" }"));
        assert!(contents.contains("\"docs\""));
        assert!(contents.contains("\"ast-bro\""));
        assert!(contents.contains("\"url\": \"https://example.com/mcp\","));
    }

    #[test]
    fn install_mcp_prefers_existing_dot_opencode_jsonc() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".opencode/opencode.jsonc");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n  // Keep this config.\n}\n").unwrap();

        OpenCode
            .install_mcp(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains("// Keep this config."));
        assert!(contents.contains("\"ast-bro\""));
        assert!(!dir.path().join("opencode.json").exists());
    }

    #[test]
    fn install_mcp_treats_zero_byte_config_as_new() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("opencode.json");
        std::fs::write(&path, "").unwrap();

        let change = OpenCode
            .install_mcp(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        assert!(matches!(change, Change::Created(_)));
        let root = CstRootNode::parse(&std::fs::read_to_string(path).unwrap(), &Default::default())
            .unwrap();
        let value = root.to_serde_value().unwrap();
        assert_eq!(value["$schema"], CONFIG_SCHEMA);
        assert_eq!(value["mcp"]["ast-bro"]["type"], "local");
    }

    #[test]
    fn install_mcp_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        OpenCode.install_mcp(&scope, &opts).unwrap();
        let before = std::fs::read_to_string(dir.path().join("opencode.json")).unwrap();

        let change = OpenCode.install_mcp(&scope, &opts).unwrap();
        let after = std::fs::read_to_string(dir.path().join("opencode.json")).unwrap();

        assert!(matches!(change, Change::Skipped { .. }));
        assert_eq!(before, after);
    }

    #[test]
    fn install_mcp_migrates_legacy_server_name() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{"mcp":{"ast-outline":{"type":"local","command":["ast-outline","mcp"]}}}"#,
        )
        .unwrap();

        OpenCode
            .install_mcp(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let root = CstRootNode::parse(&std::fs::read_to_string(path).unwrap(), &Default::default())
            .unwrap();
        let value = root.to_serde_value().unwrap();
        assert!(value["mcp"].get("ast-outline").is_none());
        assert_eq!(value["mcp"]["ast-bro"]["type"], "local");
    }

    #[test]
    fn install_skills_creates_native_opencode_skill() {
        let dir = TempDir::new().unwrap();
        OpenCode
            .install_skills(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let contents =
            std::fs::read_to_string(dir.path().join(".opencode/skills/ast-bro/SKILL.md")).unwrap();
        assert!(contents.starts_with("---\n"));
        assert!(contents.contains(SKILL_MARKER));
    }

    #[test]
    fn uninstall_removes_only_ast_bro_entries_and_preserves_comments() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(
            &path,
            r#"{
  // Keep this server.
  "mcp": {
    "docs": { "type": "remote", "url": "https://example.com/mcp" },
    "ast-bro": { "type": "local", "command": ["ast-bro", "mcp"] },
    "ast-outline": { "type": "local", "command": ["ast-outline", "mcp"] },
  },
}
"#,
        )
        .unwrap();

        let changes = OpenCode
            .uninstall(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        assert_eq!(changes.len(), 1);
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains("// Keep this server."));
        assert!(contents.contains("\"docs\""));
        assert!(!contents.contains("\"ast-bro\""));
        assert!(!contents.contains("\"ast-outline\""));
    }

    #[test]
    fn uninstall_cleans_dot_opencode_config_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".opencode/opencode.jsonc");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
  "mcp": {
    "ast-bro": { "type": "local", "command": ["ast-bro", "mcp"] },
  },
}
"#,
        )
        .unwrap();

        let changes = OpenCode
            .uninstall(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        assert_eq!(changes.len(), 1);
        assert!(!std::fs::read_to_string(path)
            .unwrap()
            .contains("\"ast-bro\""));
    }

    #[test]
    fn status_reports_mcp_in_dot_opencode_config_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            r#"{"mcp":{"ast-bro":{"type":"local","command":["ast-bro","mcp"]}}}"#,
        )
        .unwrap();

        let status = OpenCode.status(&local_scope(&dir));

        assert!(status.mcp_installed);
    }

    #[test]
    fn status_reports_prompt_mcp_and_skill() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        OpenCode.install_prompt(&scope, &opts).unwrap();
        OpenCode.install_mcp(&scope, &opts).unwrap();
        OpenCode.install_skills(&scope, &opts).unwrap();

        let status = OpenCode.status(&scope);
        assert!(status.prompt_installed);
        assert!(status.mcp_installed);
        assert!(status.skills_installed);
        assert!(!status.hook_installed);
    }

    #[test]
    fn dry_run_does_not_write_files() {
        let dir = TempDir::new().unwrap();
        let opts = InstallOpts {
            dry_run: true,
            ..Default::default()
        };

        OpenCode.install_mcp(&local_scope(&dir), &opts).unwrap();

        assert!(!dir.path().join("opencode.json").exists());
    }
}
