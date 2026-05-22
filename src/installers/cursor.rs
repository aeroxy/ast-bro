use std::path::PathBuf;

use serde_json::{json, Value};

use super::paths;
use super::{common, json_object, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::AGENT_PROMPT;

pub struct Cursor;

const MCP_KEY_PATH: &[&str] = &["mcpServers"];
const MCP_SERVER_NAME: &str = "ast-bro";

impl Cursor {
    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".cursor/rules/ast-bro.mdc")),
            Scope::Global => paths::under_home(".cursor/User Rules.md"),
        }
    }
    fn mcp_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".cursor/mcp.json")),
            Scope::Global => paths::under_home(".cursor/mcp.json"),
        }
    }
    fn mcp_entry(&self) -> Value {
        json!({ "command": "ast-bro", "args": ["mcp"] })
    }
}

impl Installer for Cursor {
    fn name(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = self
            .prompt_path(scope)
            .ok()
            .and_then(|p| p.parent().map(|r| r.to_path_buf()))
            .map(|r| r.exists())
            .unwrap_or(false);
        Detection {
            present: dir_exists || paths::binary_on_path("cursor"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, AGENT_PROMPT, opts)
    }

    fn install_hook(&self, _scope: &Scope, _opts: &InstallOpts) -> Result<Change, String> {
        Ok(Change::NotApplicable)
    }

    fn install_mcp(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_mcp_in(
            &self.mcp_path(scope)?,
            MCP_KEY_PATH,
            MCP_SERVER_NAME,
            common::OLD_MCP_SERVER_NAME,
            self.mcp_entry(),
            opts,
        )
    }

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(c) = common::uninstall_prompt_in(&self.prompt_path(scope)?, opts)? {
            changes.push(c);
        }
        // Remove current name
        if let Some(c) = common::uninstall_json_object_in(
            &self.mcp_path(scope)?,
            MCP_KEY_PATH,
            MCP_SERVER_NAME,
            opts,
        )? {
            changes.push(c);
        }
        // Also remove legacy name from pre-rename installs
        if let Some(c) = common::uninstall_json_object_in(
            &self.mcp_path(scope)?,
            MCP_KEY_PATH,
            common::OLD_MCP_SERVER_NAME,
            opts,
        )? {
            changes.push(c);
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        let mut s = common::status_for_prompt_only(self.prompt_path(scope).ok().as_deref());
        if let Ok(mcp_p) = self.mcp_path(scope) {
            if let Ok(Some(contents)) = super::io::read_optional(&mcp_p) {
                if let Ok(root) = serde_json::from_str::<Value>(&contents) {
                    s.mcp_installed =
                        json_object::is_installed(&root, MCP_KEY_PATH, MCP_SERVER_NAME);
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_writes_to_cursor_rules() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Cursor
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        let p = dir.path().join(".cursor/rules/ast-bro.mdc");
        assert!(p.exists());
        let contents = std::fs::read_to_string(&p).unwrap();
        assert!(contents.contains("ast-bro:begin"));
    }

    #[test]
    fn install_hook_is_not_applicable() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let change = Cursor
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::NotApplicable));
    }

    #[test]
    fn install_mcp_writes_cursor_mcp_json() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let change = Cursor
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Created(_)));
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".cursor/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
    }

    #[test]
    fn install_mcp_preserves_other_servers() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".cursor/mcp.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"mcpServers":{"docs":{"command":"x","args":[]}}}"#).unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Cursor.install_mcp(&scope, &InstallOpts::default()).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["docs"]["command"], "x");
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
    }

    #[test]
    fn uninstall_removes_mcp_entry() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let opts = InstallOpts::default();
        Cursor.install_mcp(&scope, &opts).unwrap();
        Cursor.uninstall(&scope, &opts).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".cursor/mcp.json")).unwrap(),
        )
        .unwrap();
        assert!(v["mcpServers"].get("ast-bro").is_none());
    }
}
