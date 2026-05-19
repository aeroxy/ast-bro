use std::path::PathBuf;

use serde_json::{json, Value};

use super::json_hook::MARKER;
use super::paths;
use super::{common, json_object, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::AGENT_PROMPT;

pub struct Gemini;

const HOOK_PATH: &[&str] = &["hooks", "BeforeTool"];
const HOOK_NAME: &str = "ast-bro-read-interceptor";
const MCP_KEY_PATH: &[&str] = &["mcpServers"];
const MCP_SERVER_NAME: &str = "ast-bro";

impl Gemini {
    pub(crate) fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join("GEMINI.md")),
            Scope::Global => paths::under_home(".gemini/GEMINI.md"),
        }
    }
    pub(crate) fn settings_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".gemini/settings.json")),
            Scope::Global => paths::under_home(".gemini/settings.json"),
        }
    }
    fn mcp_entry(&self) -> Value {
        json!({ "command": "ast-bro", "args": ["mcp"] })
    }
    pub(crate) fn hook_entry(&self, opts: &InstallOpts) -> Value {
        let mut cmd = format!(
            "ast-bro hook --protocol gemini --min-lines {}",
            opts.min_lines
        );
        if opts.always {
            cmd.push_str(" --always");
        }
        json!({
            "matcher": "read_file",
            "hooks": [{ "name": HOOK_NAME, "type": "command", "command": cmd }]
        })
    }
}

pub(crate) fn matches_entry(v: &Value) -> bool {
    v.get("matcher").and_then(|m| m.as_str()) == Some("read_file")
        && v.get("hooks")
            .and_then(|h| h.as_array())
            .and_then(|h| h.first())
            .map(|h0| {
                h0.get("name").and_then(|n| n.as_str()) == Some(HOOK_NAME)
                    || h0
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.starts_with(MARKER))
                        .unwrap_or(false)
            })
            .unwrap_or(false)
}

impl Installer for Gemini {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = self
            .prompt_path(scope)
            .ok()
            .and_then(|p| p.parent().map(|r| r.to_path_buf()))
            .map(|r| r.exists())
            .unwrap_or(false);
        Detection {
            present: dir_exists || paths::binary_on_path("gemini"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, AGENT_PROMPT, opts)
    }

    fn install_hook(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_json_hook_in(
            &self.settings_path(scope)?,
            HOOK_PATH,
            self.hook_entry(opts),
            matches_entry,
            opts,
        )
    }

    fn install_mcp(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_json_object_in(
            &self.settings_path(scope)?,
            MCP_KEY_PATH,
            MCP_SERVER_NAME,
            self.mcp_entry(),
            opts,
        )
    }

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(c) = common::uninstall_prompt_in(&self.prompt_path(scope)?, opts)? {
            changes.push(c);
        }
        if let Some(c) =
            common::uninstall_json_hook_in(&self.settings_path(scope)?, HOOK_PATH, matches_entry, opts)?
        {
            changes.push(c);
        }
        // Remove current MCP server name
        if let Some(c) = common::uninstall_json_object_in(
            &self.settings_path(scope)?,
            MCP_KEY_PATH,
            MCP_SERVER_NAME,
            opts,
        )? {
            changes.push(c);
        }
        // Also remove legacy name from pre-rename installs
        if let Some(c) = common::uninstall_json_object_in(
            &self.settings_path(scope)?,
            MCP_KEY_PATH,
            common::OLD_MCP_SERVER_NAME,
            opts,
        )? {
            changes.push(c);
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        let mut s = common::status_for(
            self.prompt_path(scope).ok().as_deref(),
            self.settings_path(scope).ok().as_deref(),
            HOOK_PATH,
            matches_entry,
        );
        if let Ok(sp) = self.settings_path(scope) {
            if let Ok(Some(contents)) = super::io::read_optional(&sp) {
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
    fn install_creates_gemini_md_and_settings() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Gemini
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        Gemini
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        let prompt = std::fs::read_to_string(dir.path().join("GEMINI.md")).unwrap();
        let settings = std::fs::read_to_string(dir.path().join(".gemini/settings.json")).unwrap();
        assert!(prompt.contains("ast-bro:begin"));
        assert!(settings.contains("--protocol gemini"));
        assert!(settings.contains("\"matcher\": \"read_file\""));
        assert!(settings.contains("BeforeTool"));
    }

    #[test]
    fn install_mcp_writes_into_gemini_settings_alongside_hook() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let opts = InstallOpts::default();
        Gemini.install_hook(&scope, &opts).unwrap();
        Gemini.install_mcp(&scope, &opts).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        // Both the hook and the MCP entry must coexist in the same file.
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
        assert!(v["hooks"]["BeforeTool"].is_array());
    }

    #[test]
    fn uninstall_removes_mcp_entry_keeps_other_servers() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".gemini/settings.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"mcpServers":{"docs":{"command":"x","args":[]}}}"#).unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let opts = InstallOpts::default();
        Gemini.install_mcp(&scope, &opts).unwrap();
        Gemini.uninstall(&scope, &opts).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"].get("ast-bro").is_none());
        assert_eq!(v["mcpServers"]["docs"]["command"], "x");
    }
}
