use std::path::PathBuf;

use serde_json::{json, Value};

use super::paths;
use super::{common, json_object, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::AGENT_PROMPT;

pub struct Copilot;

// VS Code's MCP file uses "servers" (NOT "mcpServers" like Cursor/Claude/Gemini).
const MCP_KEY_PATH: &[&str] = &["servers"];
const MCP_SERVER_NAME: &str = "ast-bro";

impl Copilot {
    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".github/copilot-instructions.md")),
            Scope::Global => paths::under_home(".copilot/copilot-instructions.md"),
        }
    }
    /// VS Code's MCP for Copilot. Project-scoped only — VS Code's user-global
    /// `mcp.json` lives at an OS-dependent path that VS Code does not commit
    /// to in docs (open via `MCP: Open User Configuration`). Global scope is
    /// returned as `None` so install_mcp / uninstall report `n/a`.
    fn mcp_path(&self, scope: &Scope) -> Option<Result<PathBuf, String>> {
        match scope {
            Scope::Local(root) => Some(Ok(root.join(".vscode/mcp.json"))),
            Scope::Global => None,
        }
    }
    fn mcp_entry(&self) -> Value {
        json!({ "command": "ast-bro", "args": ["mcp"] })
    }
}

impl Installer for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = self
            .prompt_path(scope)
            .ok()
            .and_then(|p| p.parent().map(|r| r.to_path_buf()))
            .map(|r| r.exists())
            .unwrap_or(false);
        Detection {
            present: dir_exists || paths::binary_on_path("copilot"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, AGENT_PROMPT, opts)
    }

    fn install_hook(&self, _scope: &Scope, _opts: &InstallOpts) -> Result<Change, String> {
        Ok(Change::NotApplicable)
    }

    fn install_mcp(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        let Some(path) = self.mcp_path(scope) else {
            return Ok(Change::NotApplicable);
        };
        common::install_mcp_in(
            &path?,
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
        if let Some(path) = self.mcp_path(scope) {
            let path = path?;
            // Remove current name
            if let Some(c) =
                common::uninstall_json_object_in(&path, MCP_KEY_PATH, MCP_SERVER_NAME, opts)?
            {
                changes.push(c);
            }
            // Also remove legacy name from pre-rename installs
            if let Some(c) =
                common::uninstall_json_object_in(&path, MCP_KEY_PATH, common::OLD_MCP_SERVER_NAME, opts)?
            {
                changes.push(c);
            }
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        let mut s = common::status_for_prompt_only(self.prompt_path(scope).ok().as_deref());
        if let Some(Ok(mcp_p)) = self.mcp_path(scope) {
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
    fn install_writes_copilot_instructions() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Copilot
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        let p = dir.path().join(".github/copilot-instructions.md");
        assert!(p.exists());
    }

    #[test]
    fn install_mcp_writes_vscode_mcp_json_with_servers_key() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Copilot
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".vscode/mcp.json")).unwrap(),
        )
        .unwrap();
        // VS Code uses "servers", not "mcpServers".
        assert_eq!(v["servers"]["ast-bro"]["command"], "ast-bro");
        assert!(v.get("mcpServers").is_none());
    }

    #[test]
    fn install_mcp_global_is_not_applicable() {
        let change = Copilot
            .install_mcp(&Scope::Global, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::NotApplicable));
    }

    #[test]
    fn install_mcp_preserves_other_servers() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".vscode/mcp.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"servers":{"docs":{"type":"http","url":"https://x"}}}"#).unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Copilot
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["servers"]["docs"]["type"], "http");
        assert_eq!(v["servers"]["ast-bro"]["command"], "ast-bro");
    }
}
