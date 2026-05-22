use std::path::PathBuf;

use serde_json::{json, Value};

use super::json_hook::matches_any_marker;
use super::paths;
use super::{common, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::AGENT_PROMPT;

pub struct Tabnine;

const HOOK_PATH: &[&str] = &["hooks", "BeforeTool"];
const HOOK_NAME: &str = "ast-bro-read-interceptor";
const OLD_HOOK_NAME: &str = "ast-outline-read-interceptor";

impl Tabnine {
    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".tabnine/guidelines/ast-bro.md")),
            Scope::Global => paths::under_home(".tabnine/guidelines/ast-bro.md"),
        }
    }

    fn old_prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".tabnine/guidelines/ast-outline.md")),
            Scope::Global => paths::under_home(".tabnine/guidelines/ast-outline.md"),
        }
    }
    fn settings_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".tabnine/agent/settings.json")),
            Scope::Global => paths::under_home(".tabnine/agent/settings.json"),
        }
    }
    fn hook_entry(&self, opts: &InstallOpts) -> Value {
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

fn matches_entry(v: &Value) -> bool {
    v.get("matcher").and_then(|m| m.as_str()) == Some("read_file")
        && v.get("hooks")
            .and_then(|h| h.as_array())
            .and_then(|h| h.first())
            .map(|h0| {
                h0.get("name").and_then(|n| n.as_str()) == Some(HOOK_NAME)
                    || h0.get("name").and_then(|n| n.as_str()) == Some(OLD_HOOK_NAME)
                    || h0
                        .get("command")
                        .and_then(|c| c.as_str())
                        .map(matches_any_marker)
                        .unwrap_or(false)
            })
            .unwrap_or(false)
}

impl Installer for Tabnine {
    fn name(&self) -> &'static str {
        "tabnine"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = self
            .prompt_path(scope)
            .ok()
            .and_then(|p| p.parent().map(|r| r.to_path_buf()))
            .map(|r| r.exists())
            .unwrap_or(false);
        Detection {
            present: dir_exists || paths::binary_on_path("tabnine"),
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

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(c) = common::uninstall_prompt_in(&self.prompt_path(scope)?, opts)? {
            changes.push(c);
        }
        // Also clean up legacy prompt path from pre-rename installs
        if let Some(c) = common::uninstall_prompt_in(&self.old_prompt_path(scope)?, opts)? {
            changes.push(c);
        }
        if let Some(c) =
            common::uninstall_json_hook_in(&self.settings_path(scope)?, HOOK_PATH, matches_entry, opts)?
        {
            changes.push(c);
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        common::status_for(
            self.prompt_path(scope).ok().as_deref(),
            self.settings_path(scope).ok().as_deref(),
            HOOK_PATH,
            matches_entry,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_creates_tabnine_files() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Tabnine
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        Tabnine
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        let prompt =
            std::fs::read_to_string(dir.path().join(".tabnine/guidelines/ast-bro.md")).unwrap();
        let settings =
            std::fs::read_to_string(dir.path().join(".tabnine/agent/settings.json")).unwrap();
        assert!(prompt.contains("ast-bro:begin"));
        assert!(settings.contains("--protocol gemini"));
    }
}
