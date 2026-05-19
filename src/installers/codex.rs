use std::path::PathBuf;

use toml_edit::{Array, DocumentMut, Table};

use super::paths;
use super::{common, toml_object, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::{agent_skill_md, AGENT_PROMPT};

pub struct Codex;

const MCP_PARENT: &str = "mcp_servers";
const MCP_SERVER_NAME: &str = "ast-bro";
/// First-line marker used to confirm a SKILL.md file is one we wrote
/// before deleting it during uninstall.
const SKILL_MARKER: &str = "name: ast-bro";

impl Codex {
    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join("AGENTS.md")),
            Scope::Global => paths::under_home(".codex/AGENTS.md"),
        }
    }
    fn config_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            // Codex CLI does not document a per-project config override —
            // ~/.codex/config.toml is the only documented path. We still
            // honour the local scope by writing to a project-relative file
            // for predictability, but in practice users will use --global.
            Scope::Local(root) => Ok(root.join(".codex/config.toml")),
            Scope::Global => paths::under_home(".codex/config.toml"),
        }
    }
    /// Codex CLI auto-discovers skills from `.agents/skills/<name>/SKILL.md`
    /// in cwd / parent dirs / repo root, and `~/.agents/skills/<name>/SKILL.md`
    /// for user-global. Same SKILL.md shape as Claude Code.
    fn skill_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".agents/skills/ast-bro/SKILL.md")),
            Scope::Global => paths::under_home(".agents/skills/ast-bro/SKILL.md"),
        }
    }
    fn mcp_entry(&self) -> Table {
        let mut t = Table::new();
        t["command"] = toml_edit::value("ast-bro");
        let mut args = Array::new();
        args.push("mcp");
        t["args"] = toml_edit::value(args);
        t
    }
}

impl Installer for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = self
            .prompt_path(scope)
            .ok()
            .and_then(|p| p.parent().map(|r| r.to_path_buf()))
            .map(|r| r.exists())
            .unwrap_or(false);
        Detection {
            present: dir_exists || paths::binary_on_path("codex"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, AGENT_PROMPT, opts)
    }

    fn install_hook(&self, _scope: &Scope, _opts: &InstallOpts) -> Result<Change, String> {
        Ok(Change::NotApplicable)
    }

    fn install_mcp(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_toml_object_in(
            &self.config_path(scope)?,
            MCP_PARENT,
            MCP_SERVER_NAME,
            self.mcp_entry(),
            opts,
        )
    }

    fn install_skills(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_plain_file_in(&self.skill_path(scope)?, &agent_skill_md(), opts)
    }

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(c) = common::uninstall_prompt_in(&self.prompt_path(scope)?, opts)? {
            changes.push(c);
        }
        // Remove current MCP server name
        if let Some(c) = common::uninstall_toml_object_in(
            &self.config_path(scope)?,
            MCP_PARENT,
            MCP_SERVER_NAME,
            opts,
        )? {
            changes.push(c);
        }
        // Also remove legacy name from pre-rename installs
        if let Some(c) = common::uninstall_toml_object_in(
            &self.config_path(scope)?,
            MCP_PARENT,
            common::OLD_MCP_SERVER_NAME,
            opts,
        )? {
            changes.push(c);
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
        if let Ok(cp) = self.config_path(scope) {
            if let Ok(Some(contents)) = super::io::read_optional(&cp) {
                if let Ok(doc) = contents.parse::<DocumentMut>() {
                    s.mcp_installed =
                        toml_object::is_installed(&doc, MCP_PARENT, MCP_SERVER_NAME);
                }
            }
        }
        if let Ok(skill_p) = self.skill_path(scope) {
            if let Ok(Some(contents)) = super::io::read_optional(&skill_p) {
                s.skills_installed = contents.contains(SKILL_MARKER);
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
    fn install_writes_agents_md() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Codex
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        let p = dir.path().join("AGENTS.md");
        assert!(p.exists());
        let contents = std::fs::read_to_string(&p).unwrap();
        assert!(contents.contains("ast-bro:begin"));
    }

    #[test]
    fn install_mcp_writes_codex_config_toml() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Codex
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        let contents =
            std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        assert!(contents.contains("[mcp_servers.ast-bro]"));
        assert!(contents.contains("command = \"ast-bro\""));
        assert!(contents.contains("\"mcp\""));
    }

    #[test]
    fn install_mcp_preserves_user_toml_keys_and_comments() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".codex/config.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "# my codex config\nmodel = \"gpt-5\"\napproval_policy = \"auto\"\n",
        )
        .unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Codex
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("# my codex config"), "comment lost: {}", out);
        assert!(out.contains("model = \"gpt-5\""));
        assert!(out.contains("approval_policy = \"auto\""));
        assert!(out.contains("[mcp_servers.ast-bro]"));
    }

    #[test]
    fn install_skills_creates_agents_skills_skill_md() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let change = Codex
            .install_skills(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Created(_)));
        let contents = std::fs::read_to_string(
            dir.path().join(".agents/skills/ast-bro/SKILL.md"),
        )
        .unwrap();
        assert!(contents.starts_with("---\n"));
        assert!(contents.contains("name: ast-bro"));
        assert!(contents.contains("## Use `ast-bro` to explore the code"));
    }

    #[test]
    fn install_skills_idempotent() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let opts = InstallOpts::default();
        Codex.install_skills(&scope, &opts).unwrap();
        let change = Codex.install_skills(&scope, &opts).unwrap();
        assert!(matches!(change, Change::Skipped { .. }));
    }

    #[test]
    fn uninstall_removes_skills_file_and_empty_dir() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let opts = InstallOpts::default();
        Codex.install_skills(&scope, &opts).unwrap();
        let skill_dir = dir.path().join(".agents/skills/ast-bro");
        assert!(skill_dir.join("SKILL.md").exists());
        Codex.uninstall(&scope, &opts).unwrap();
        assert!(!skill_dir.exists());
        assert!(dir.path().join(".agents/skills").exists()); // parent preserved
    }

    #[test]
    fn uninstall_removes_mcp_entry_keeps_other_servers() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".codex/config.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "[mcp_servers.docs]\ncommand = \"docs-server\"\nargs = [\"serve\"]\n",
        )
        .unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        let opts = InstallOpts::default();
        Codex.install_mcp(&scope, &opts).unwrap();
        Codex.uninstall(&scope, &opts).unwrap();
        let out = std::fs::read_to_string(&p).unwrap();
        assert!(out.contains("[mcp_servers.docs]"));
        assert!(!out.contains("[mcp_servers.ast-bro]"));
    }
}
