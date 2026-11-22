use std::path::PathBuf;

use serde_json::{json, Value};

use super::io::read_optional;
use super::paths;
use super::{common, json_object, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::{agent_prompt, agent_skill_md};

/// Pi (`pi-coding-agent`). It has no hook config, so the Read substitution
/// ships as a pi extension that calls `ast-bro hook --protocol claude-code`:
/// pi's `tool_call` block with a reason has the same shape as a Claude Code
/// PreToolUse block.
pub struct Pi;

const MCP_KEY_PATH: &[&str] = &["mcpServers"];
const MCP_SERVER_NAME: &str = "ast-bro";
const SKILL_MARKER: &str = "name: ast-bro";
const EXTENSION_MARKER: &str = "ast-bro: pi extension";
const EXTENSION_TEMPLATE: &str = include_str!("pi_extension.ts");

impl Pi {
    /// `<agent-dir>`: `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
    fn agent_dir(&self) -> Result<PathBuf, String> {
        match std::env::var_os("PI_CODING_AGENT_DIR") {
            // pi expands a leading `~` in it, even one the shell left quoted.
            Some(dir) if !dir.is_empty() => {
                let dir = PathBuf::from(dir);
                if dir.starts_with("~") {
                    Ok(paths::expand_tilde(&dir, &paths::home()?))
                } else {
                    Ok(dir)
                }
            }
            _ => paths::under_home(".pi/agent"),
        }
    }

    /// Project files live in `.pi`, user files in the agent directory.
    fn config_path(&self, scope: &Scope, relative: &str) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".pi").join(relative)),
            Scope::Global => Ok(self.agent_dir()?.join(relative)),
        }
    }

    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            // Pi reads context files from the working directory, not from
            // `.pi`. Shared with Codex and OpenCode, like theirs.
            Scope::Local(root) => Ok(root.join("AGENTS.md")),
            Scope::Global => Ok(self.agent_dir()?.join("AGENTS.md")),
        }
    }

    fn extension_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        self.config_path(scope, "extensions/ast-bro.ts")
    }

    fn mcp_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        self.config_path(scope, "mcp.json")
    }

    fn skill_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        self.config_path(scope, "skills/ast-bro/SKILL.md")
    }

    fn hook_args(&self, opts: &InstallOpts) -> Vec<String> {
        let mut args: Vec<String> = ["hook", "--protocol", "claude-code", "--min-lines"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.push(opts.min_lines.to_string());
        if opts.always {
            args.push("--always".into());
        }
        args
    }

    fn extension_source(&self, opts: &InstallOpts) -> String {
        let args = serde_json::to_string(&self.hook_args(opts)).unwrap();
        EXTENSION_TEMPLATE.replace("__AST_BRO_HOOK_ARGS__", &args)
    }

    fn mcp_entry(&self) -> Value {
        json!({ "command": "ast-bro", "args": ["mcp"] })
    }

    fn file_has(&self, path: Result<PathBuf, String>, marker: &str) -> bool {
        path.ok()
            .and_then(|p| read_optional(&p).ok().flatten())
            .map(|contents| contents.contains(marker))
            .unwrap_or(false)
    }
}

impl Installer for Pi {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = match scope {
            Scope::Local(root) => root.join(".pi").exists(),
            Scope::Global => self.agent_dir().map(|d| d.exists()).unwrap_or(false),
        };
        Detection {
            present: dir_exists || paths::binary_on_path("pi"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, agent_prompt(), opts)
    }

    fn install_hook(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_plain_file_in(
            &self.extension_path(scope)?,
            &self.extension_source(opts),
            opts,
        )
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

    fn install_skills(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_plain_file_in(&self.skill_path(scope)?, agent_skill_md(), opts)
    }

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(c) = common::uninstall_prompt_in(&self.prompt_path(scope)?, opts)? {
            changes.push(c);
        }
        if let Some(c) =
            common::uninstall_plain_file_in(&self.extension_path(scope)?, EXTENSION_MARKER, opts)?
        {
            changes.push(c);
        }
        for name in [MCP_SERVER_NAME, common::OLD_MCP_SERVER_NAME] {
            if let Some(c) =
                common::uninstall_json_object_in(&self.mcp_path(scope)?, MCP_KEY_PATH, name, opts)?
            {
                changes.push(c);
            }
        }
        if let Some(c) =
            common::uninstall_plain_file_in(&self.skill_path(scope)?, SKILL_MARKER, opts)?
        {
            changes.push(c);
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        let mut s = common::status_for_prompt_only(self.prompt_path(scope).ok().as_deref());
        s.hook_installed = self.file_has(self.extension_path(scope), EXTENSION_MARKER);
        s.skills_installed = self.file_has(self.skill_path(scope), SKILL_MARKER);
        if let Ok(Some(contents)) = self.mcp_path(scope).and_then(|p| read_optional(&p)) {
            if let Ok(root) = serde_json::from_str::<Value>(&contents) {
                s.mcp_installed = json_object::is_installed(&root, MCP_KEY_PATH, MCP_SERVER_NAME);
            }
        }
        s
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
    fn install_prompt_writes_agents_md() {
        let dir = TempDir::new().unwrap();
        Pi.install_prompt(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let contents = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(contents.contains("ast-bro:begin"));
    }

    #[test]
    fn install_hook_writes_extension_with_hook_options() {
        let dir = TempDir::new().unwrap();
        let opts = InstallOpts {
            min_lines: 120,
            always: true,
            ..Default::default()
        };
        let change = Pi.install_hook(&local_scope(&dir), &opts).unwrap();

        assert!(matches!(change, Change::Created(_)));
        let source = std::fs::read_to_string(dir.path().join(".pi/extensions/ast-bro.ts")).unwrap();
        assert!(source.starts_with(&format!("// {EXTENSION_MARKER}")));
        assert!(source.contains(
            r#"const HOOK_ARGS: string[] = ["hook","--protocol","claude-code","--min-lines","120","--always"];"#
        ));
        assert!(!source.contains("__AST_BRO_HOOK_ARGS__"));
    }

    #[test]
    fn install_hook_is_idempotent_and_follows_option_changes() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        Pi.install_hook(&scope, &InstallOpts::default()).unwrap();

        let again = Pi.install_hook(&scope, &InstallOpts::default()).unwrap();
        assert!(matches!(again, Change::Skipped { .. }));

        let opts = InstallOpts {
            always: true,
            ..Default::default()
        };
        let changed = Pi.install_hook(&scope, &opts).unwrap();
        assert!(matches!(changed, Change::Updated(_)));
    }

    #[test]
    fn install_mcp_writes_pi_mcp_json_and_keeps_other_servers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".pi/mcp.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers":{"docs":{"url":"https://example.com/mcp"}}}"#,
        )
        .unwrap();

        Pi.install_mcp(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
        assert_eq!(v["mcpServers"]["ast-bro"]["args"], json!(["mcp"]));
        assert_eq!(v["mcpServers"]["docs"]["url"], "https://example.com/mcp");
    }

    #[test]
    fn install_skills_writes_pi_skill() {
        let dir = TempDir::new().unwrap();
        Pi.install_skills(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        let contents =
            std::fs::read_to_string(dir.path().join(".pi/skills/ast-bro/SKILL.md")).unwrap();
        assert!(contents.starts_with("---\n"));
        assert!(contents.contains(SKILL_MARKER));
    }

    #[test]
    fn status_and_uninstall_cover_every_mode() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        Pi.install_prompt(&scope, &opts).unwrap();
        Pi.install_hook(&scope, &opts).unwrap();
        Pi.install_mcp(&scope, &opts).unwrap();
        Pi.install_skills(&scope, &opts).unwrap();

        let status = Pi.status(&scope);
        assert!(status.prompt_installed);
        assert!(status.hook_installed);
        assert!(status.mcp_installed);
        assert!(status.skills_installed);

        let changes = Pi.uninstall(&scope, &opts).unwrap();
        assert_eq!(changes.len(), 4);
        let status = Pi.status(&scope);
        assert!(!status.prompt_installed);
        assert!(!status.hook_installed);
        assert!(!status.mcp_installed);
        assert!(!status.skills_installed);
        assert!(!dir.path().join(".pi/extensions/ast-bro.ts").exists());
    }

    #[test]
    fn uninstall_keeps_an_extension_the_user_replaced() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".pi/extensions/ast-bro.ts");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "// my own extension\n").unwrap();

        Pi.uninstall(&local_scope(&dir), &InstallOpts::default())
            .unwrap();

        assert!(path.exists());
    }

    #[test]
    fn dry_run_does_not_write_files() {
        let dir = TempDir::new().unwrap();
        let opts = InstallOpts {
            dry_run: true,
            ..Default::default()
        };
        let scope = local_scope(&dir);

        Pi.install_hook(&scope, &opts).unwrap();
        Pi.install_mcp(&scope, &opts).unwrap();

        assert!(!dir.path().join(".pi").exists());
    }
}
