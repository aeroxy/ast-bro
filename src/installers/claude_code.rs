use std::path::PathBuf;

use serde_json::{json, Value};

use super::json_hook::matches_any_marker;
use super::paths;
use super::{common, json_object, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::{agent_prompt, agent_skill_md, EXPLORE_FRONTMATTER};

pub struct ClaudeCode;

/// The read has to be intercepted before it runs. `PreToolUse` is the only
/// event that can do it, and it can only refuse the call, so the host reports a
/// failure that did not happen — the complaint in issue #34.
///
/// The alternative that would report a success, `PostToolUse` with
/// `updatedToolOutput`, is deliberately not registered: measured on Claude Code
/// 2.1.223, that field replaces the result of an MCP tool and is ignored for
/// the built-in `Read`, so registering it would deliver no map and send the
/// whole file to the model instead.
const HOOK_PATH: &[&str] = &["hooks", "PreToolUse"];

/// A read the host refuses outright never reaches `PreToolUse`'s substitution —
/// it fails on byte size, which no line threshold predicts — and would
/// otherwise reach the model as a bare error. This event covers it. It cannot
/// replace the result, only add context, which is all the map needs when the
/// result carries no file contents.
const FAILURE_HOOK_PATH: &[&str] = &["hooks", "PostToolUseFailure"];

/// Events an install registers the entry under.
const INSTALL_HOOK_PATHS: &[&[&str]] = &[HOOK_PATH, FAILURE_HOOK_PATH];

/// Every event our entry has lived under, for uninstall. `status` asks about
/// [`INSTALL_HOOK_PATHS`] instead, so a release that stops writing an event does
/// not keep reporting it as required.
const ALL_HOOK_PATHS: &[&[&str]] = &[HOOK_PATH, FAILURE_HOOK_PATH];

/// Built-in Claude Code subagents that run in their own context and never see
/// `CLAUDE.md`. Shadowing them with `.claude/agents/<Name>.md` is the official
/// way to push the ast-bro prompt into their system prompt.
const SHADOWED_SUBAGENTS: &[&str] = &["Explore"];

const MCP_KEY_PATH: &[&str] = &["mcpServers"];
const MCP_SERVER_NAME: &str = "ast-bro";
/// First-line marker used to confirm a SKILL.md file is one we wrote
/// before deleting it during uninstall.
const SKILL_MARKER: &str = "name: ast-bro";

impl ClaudeCode {
    fn prompt_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join("CLAUDE.md")),
            Scope::Global => paths::under_home(".claude/CLAUDE.md"),
        }
    }
    fn settings_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".claude/settings.json")),
            Scope::Global => paths::under_home(".claude/settings.json"),
        }
    }
    fn subagent_path(&self, scope: &Scope, name: &str) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".claude/agents").join(format!("{}.md", name))),
            Scope::Global => paths::under_home(format!(".claude/agents/{}.md", name)),
        }
    }
    fn mcp_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".mcp.json")),
            // Global MCP config is at the home root, NOT inside .claude/.
            Scope::Global => paths::under_home(".claude.json"),
        }
    }
    fn skill_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".claude/skills/ast-bro/SKILL.md")),
            Scope::Global => paths::under_home(".claude/skills/ast-bro/SKILL.md"),
        }
    }
    fn mcp_entry(&self) -> Value {
        json!({ "command": "ast-bro", "args": ["mcp"] })
    }
    fn hook_command(&self, opts: &InstallOpts) -> String {
        let mut cmd = format!(
            "ast-bro hook --protocol claude-code --min-lines {}",
            opts.min_lines
        );
        if opts.always {
            cmd.push_str(" --always");
        }
        cmd
    }
    fn hook_entry(&self, opts: &InstallOpts) -> Value {
        json!({
            "matcher": "Read",
            "hooks": [{ "type": "command", "command": self.hook_command(opts) }]
        })
    }
}

fn matches_entry(v: &Value) -> bool {
    v.get("matcher").and_then(|m| m.as_str()) == Some("Read")
        && v.get("hooks")
            .and_then(|h| h.as_array())
            .and_then(|h| h.first())
            .and_then(|h0| h0.get("command"))
            .and_then(|c| c.as_str())
            .map(matches_any_marker)
            .unwrap_or(false)
}

impl Installer for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let dir_exists = self
            .prompt_path(scope)
            .ok()
            .and_then(|p| p.parent().map(|r| r.to_path_buf()))
            .map(|r| r.exists())
            .unwrap_or(false);
        Detection {
            present: dir_exists || paths::binary_on_path("claude"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_prompt_in(&self.prompt_path(scope)?, agent_prompt(), opts)
    }

    fn install_hook(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        common::install_json_hook_in(
            &self.settings_path(scope)?,
            INSTALL_HOOK_PATHS,
            self.hook_entry(opts),
            matches_entry,
            opts,
        )
    }

    fn install_subagents(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::with_capacity(SHADOWED_SUBAGENTS.len());
        for name in SHADOWED_SUBAGENTS {
            let path = self.subagent_path(scope, name)?;
            let frontmatter = match *name {
                "Explore" => EXPLORE_FRONTMATTER,
                _ => "",
            };
            changes.push(common::install_subagent_in(&path, frontmatter, agent_prompt(), opts)?);
        }
        Ok(changes)
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
        if let Some(c) = common::uninstall_json_hook_in(
            &self.settings_path(scope)?,
            ALL_HOOK_PATHS,
            matches_entry,
            opts,
        )? {
            changes.push(c);
        }
        for name in SHADOWED_SUBAGENTS {
            if let Some(c) = common::uninstall_prompt_in(&self.subagent_path(scope, name)?, opts)? {
                changes.push(c);
            }
        }
        // Remove current MCP server name
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
        if let Some(c) =
            common::uninstall_plain_file_in(&self.skill_path(scope)?, SKILL_MARKER, opts)?
        {
            changes.push(c);
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        let mut s = common::status_for(
            self.prompt_path(scope).ok().as_deref(),
            self.settings_path(scope).ok().as_deref(),
            INSTALL_HOOK_PATHS,
            matches_entry,
        );
        if let Ok(mcp_p) = self.mcp_path(scope) {
            if let Ok(Some(contents)) = super::io::read_optional(&mcp_p) {
                if let Ok(root) = serde_json::from_str::<Value>(&contents) {
                    s.mcp_installed =
                        json_object::is_installed(&root, MCP_KEY_PATH, MCP_SERVER_NAME);
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
    use crate::installers::json_hook;
    use tempfile::TempDir;

    fn local_scope(dir: &TempDir) -> Scope {
        Scope::Local(dir.path().to_path_buf())
    }

    #[test]
    fn install_prompt_creates_file_with_marker_block() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let change = ClaudeCode
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Created(_)));
        let contents = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(contents.contains("<!-- ast-bro:begin"));
        assert!(contents.contains("ast-bro"));
    }

    #[test]
    fn install_prompt_idempotent() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        let after_first = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        ClaudeCode
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        let after_second = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn install_hook_creates_settings_with_entry() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let change = ClaudeCode
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Created(_)));
        let contents = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(contents.contains("--protocol claude-code"));
        assert!(contents.contains("\"matcher\": \"Read\""));
        let root: Value = serde_json::from_str(&contents).unwrap();
        assert!(json_hook::is_installed(&root, HOOK_PATH, matches_entry));
    }


    /// The entry is registered under `PreToolUse`, which is the only event that
    /// can intercept the read before it happens, and under
    /// `PostToolUseFailure`, which covers the read the host refuses outright.
    /// `PostToolUse` is deliberately absent: on Claude Code 2.1.223
    /// `updatedToolOutput` is ignored for the built-in `Read`, so registering it
    /// would deliver no map at all.
    #[test]
    fn install_hook_registers_pre_tool_use_and_the_failure_event() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        let contents = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        let root: Value = serde_json::from_str(&contents).unwrap();
        for path in INSTALL_HOOK_PATHS {
            assert!(
                json_hook::is_installed(&root, path, matches_entry),
                "missing under {path:?}: {contents}"
            );
        }
        assert!(
            !contents.contains("\"PostToolUse\""),
            "PostToolUse must not be registered: {contents}"
        );
    }

    /// A settings file carrying only the pre-read entry — what every install
    /// from before the failure event looks like — reports partial, not current.
    /// Otherwise the new capability never reaches an existing install and the one
    /// diagnostic that could say so asserts the opposite.
    #[test]
    fn status_reports_partial_for_an_install_missing_the_failure_event() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"ast-bro hook --protocol claude-code --min-lines 200"}]}]}}"#,
        )
        .unwrap();
        let scope = local_scope(&dir);
        let s = ClaudeCode.status(&scope);
        assert!(s.hook_installed, "the entry is there, just not everywhere");
        assert!(s.hook_partial);
    }

    /// A complete install is not partial, or the warning means nothing — and a
    /// reinstall over a partial one clears it.
    #[test]
    fn status_is_not_partial_after_install_completes_it() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"ast-bro hook --protocol claude-code --min-lines 200"}]}]}}"#,
        )
        .unwrap();
        let scope = local_scope(&dir);
        assert!(ClaudeCode.status(&scope).hook_partial);
        ClaudeCode
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        let s = ClaudeCode.status(&scope);
        assert!(s.hook_installed);
        assert!(!s.hook_partial, "install must complete the registration");
    }

    #[test]
    fn install_hook_preserves_other_hooks() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        ).unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        let contents = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(contents.contains("echo hi"));
        assert!(contents.contains("--protocol claude-code"));
    }

    #[test]
    fn uninstall_removes_block_and_hook_keeps_siblings() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        ).unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_prompt(&scope, &opts).unwrap();
        ClaudeCode.install_hook(&scope, &opts).unwrap();
        let removed = ClaudeCode.uninstall(&scope, &opts).unwrap();
        assert_eq!(removed.len(), 2);
        let prompt = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(!prompt.contains("ast-bro:begin"));
        let settings = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(settings.contains("echo hi"));
        assert!(!settings.contains("ast-bro hook"));
    }

    #[test]
    fn status_reports_versions_and_flags() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let s0 = ClaudeCode.status(&scope);
        assert!(!s0.prompt_installed);
        assert!(!s0.hook_installed);
        ClaudeCode
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        ClaudeCode
            .install_hook(&scope, &InstallOpts::default())
            .unwrap();
        let s1 = ClaudeCode.status(&scope);
        assert!(s1.prompt_installed);
        assert!(s1.hook_installed);
        assert_eq!(s1.prompt_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts { dry_run: true, ..Default::default() };
        ClaudeCode.install_prompt(&scope, &opts).unwrap();
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn install_subagents_creates_explore_md_with_marker_block() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let changes = ClaudeCode
            .install_subagents(&scope, &InstallOpts::default())
            .unwrap();
        assert_eq!(changes.len(), SHADOWED_SUBAGENTS.len());
        assert!(matches!(changes[0], Change::Created(_)));
        let path = dir.path().join(".claude/agents/Explore.md");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with("---\nname: Explore\n"), "frontmatter must be at offset 0");
        assert!(contents.contains("<!-- ast-bro:begin"));
        assert!(contents.contains("ast-bro"));
    }

    #[test]
    fn install_subagents_idempotent() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_subagents(&scope, &opts).unwrap();
        let path = dir.path().join(".claude/agents/Explore.md");
        let after_first = std::fs::read_to_string(&path).unwrap();
        let changes = ClaudeCode.install_subagents(&scope, &opts).unwrap();
        assert!(matches!(changes[0], Change::Skipped { .. }));
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn install_subagents_wraps_legacy_explore_md_in_place() {
        // Simulates a user who manually created ~/.claude/agents/Explore.md by
        // pasting `ast-bro prompt` output before this installer existed.
        let dir = TempDir::new().unwrap();
        let agent_path = dir.path().join(".claude/agents/Explore.md");
        std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        std::fs::write(&agent_path, agent_prompt()).unwrap();
        let scope = local_scope(&dir);
        let changes = ClaudeCode
            .install_subagents(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(changes[0], Change::Updated(_)));
        let contents = std::fs::read_to_string(&agent_path).unwrap();
        assert!(contents.contains("<!-- ast-bro:begin"));
        // Body is wrapped exactly once — the legacy bare snippet is gone.
        assert_eq!(
            contents
                .matches("## Use `sb` (the `ast-bro` toolkit) to explore the code")
                .count(),
            1
        );
    }

    #[test]
    fn install_subagents_appends_to_user_customized_file() {
        let dir = TempDir::new().unwrap();
        let agent_path = dir.path().join(".claude/agents/Explore.md");
        std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        let custom = "---\nname: Explore\ntools: Read, Grep\n---\nUser prompt body.\n";
        std::fs::write(&agent_path, custom).unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .install_subagents(&scope, &InstallOpts::default())
            .unwrap();
        let contents = std::fs::read_to_string(&agent_path).unwrap();
        assert!(contents.starts_with(custom));
        assert!(contents.contains("<!-- ast-bro:begin"));
    }

    #[test]
    fn uninstall_removes_subagent_block_and_keeps_user_content() {
        let dir = TempDir::new().unwrap();
        let agent_path = dir.path().join(".claude/agents/Explore.md");
        std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        let custom = "---\nname: Explore\n---\nKeep me.\n";
        std::fs::write(&agent_path, custom).unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_subagents(&scope, &opts).unwrap();
        let removed = ClaudeCode.uninstall(&scope, &opts).unwrap();
        assert!(removed.iter().any(|c| matches!(c, Change::Removed(p) if p.ends_with("Explore.md"))));
        let contents = std::fs::read_to_string(&agent_path).unwrap();
        assert!(!contents.contains("ast-bro:begin"));
        assert!(contents.contains("Keep me."));
    }

    #[test]
    fn uninstall_subagent_noop_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        let removed = ClaudeCode.uninstall(&scope, &opts).unwrap();
        assert!(removed.iter().all(|c| !matches!(c, Change::Removed(p) if p.ends_with("Explore.md"))));
    }

    #[test]
    fn install_subagents_dry_run_does_not_write() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts { dry_run: true, ..Default::default() };
        ClaudeCode.install_subagents(&scope, &opts).unwrap();
        assert!(!dir.path().join(".claude/agents/Explore.md").exists());
    }

    #[test]
    fn install_mcp_creates_mcp_json_with_entry() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let change = ClaudeCode
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Created(_)));
        let contents = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let v: Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
        assert_eq!(v["mcpServers"]["ast-bro"]["args"][0], "mcp");
    }

    #[test]
    fn install_mcp_idempotent() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_mcp(&scope, &opts).unwrap();
        let after_first = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let change = ClaudeCode.install_mcp(&scope, &opts).unwrap();
        assert!(matches!(change, Change::Skipped { .. }));
        let after_second = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn install_mcp_preserves_other_servers() {
        let dir = TempDir::new().unwrap();
        let mcp_path = dir.path().join(".mcp.json");
        std::fs::write(
            &mcp_path,
            r#"{"mcpServers":{"other":{"command":"x","args":[]}}}"#,
        )
        .unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        let contents = std::fs::read_to_string(&mcp_path).unwrap();
        let v: Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
    }

    #[test]
    fn install_mcp_preserves_unrelated_top_level_keys() {
        // Mimics ~/.claude.json: many flat top-level keys that must survive.
        let dir = TempDir::new().unwrap();
        let mcp_path = dir.path().join(".mcp.json");
        let mut seed = serde_json::Map::new();
        for i in 0..50 {
            seed.insert(format!("key_{:02}", i), json!(i));
        }
        seed.insert("mcpServers".into(), json!({}));
        std::fs::write(&mcp_path, serde_json::to_string(&seed).unwrap()).unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        for i in 0..50 {
            assert_eq!(v[format!("key_{:02}", i)], json!(i), "key_{:02} lost", i);
        }
        assert_eq!(v["mcpServers"]["ast-bro"]["command"], "ast-bro");
    }

    #[test]
    fn install_mcp_dry_run_does_not_write() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts { dry_run: true, ..Default::default() };
        ClaudeCode.install_mcp(&scope, &opts).unwrap();
        assert!(!dir.path().join(".mcp.json").exists());
    }

    #[test]
    fn install_skills_creates_skill_md_with_frontmatter() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let change = ClaudeCode
            .install_skills(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Created(_)));
        let contents =
            std::fs::read_to_string(dir.path().join(".claude/skills/ast-bro/SKILL.md"))
                .unwrap();
        assert!(contents.starts_with("---\n"));
        assert!(contents.contains("name: ast-bro"));
        assert!(contents.contains("user-invocable: true"));
        assert!(contents.contains("## Use `sb` (the `ast-bro` toolkit) to explore the code"));
    }

    #[test]
    fn install_skills_idempotent() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_skills(&scope, &opts).unwrap();
        let change = ClaudeCode.install_skills(&scope, &opts).unwrap();
        assert!(matches!(change, Change::Skipped { .. }));
    }

    #[test]
    fn install_skills_overwrites_when_content_differs() {
        let dir = TempDir::new().unwrap();
        let skill_path = dir.path().join(".claude/skills/ast-bro/SKILL.md");
        std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
        std::fs::write(&skill_path, "---\nname: ast-bro\n---\nold body\n").unwrap();
        let scope = local_scope(&dir);
        let change = ClaudeCode
            .install_skills(&scope, &InstallOpts::default())
            .unwrap();
        assert!(matches!(change, Change::Updated(_)));
        let contents = std::fs::read_to_string(&skill_path).unwrap();
        assert!(contents.contains("Use `sb` (the `ast-bro` toolkit)"));
        assert!(!contents.contains("old body"));
    }

    #[test]
    fn install_skills_dry_run_does_not_write() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts { dry_run: true, ..Default::default() };
        ClaudeCode.install_skills(&scope, &opts).unwrap();
        assert!(!dir.path().join(".claude/skills/ast-bro/SKILL.md").exists());
    }

    #[test]
    fn uninstall_removes_mcp_entry_keeps_other_servers() {
        let dir = TempDir::new().unwrap();
        let mcp_path = dir.path().join(".mcp.json");
        std::fs::write(
            &mcp_path,
            r#"{"mcpServers":{"other":{"command":"x","args":[]}}}"#,
        )
        .unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_mcp(&scope, &opts).unwrap();
        ClaudeCode.uninstall(&scope, &opts).unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("ast-bro").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
    }

    #[test]
    fn uninstall_removes_skills_file_and_empty_dir() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_skills(&scope, &opts).unwrap();
        let skill_dir = dir.path().join(".claude/skills/ast-bro");
        assert!(skill_dir.join("SKILL.md").exists());
        ClaudeCode.uninstall(&scope, &opts).unwrap();
        assert!(!skill_dir.join("SKILL.md").exists());
        assert!(!skill_dir.exists()); // empty parent removed
        // .claude/skills/ stays intact (might be host to other skills).
        assert!(dir.path().join(".claude/skills").exists());
    }

    #[test]
    fn uninstall_keeps_skills_dir_when_other_skills_present() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let opts = InstallOpts::default();
        ClaudeCode.install_skills(&scope, &opts).unwrap();
        // Drop a sibling skill in the same parent — but ast-bro is in its
        // own subdir, so this verifies the parent .claude/skills/ stays.
        std::fs::create_dir_all(dir.path().join(".claude/skills/other")).unwrap();
        std::fs::write(
            dir.path().join(".claude/skills/other/SKILL.md"),
            "---\nname: other\n---\nbody\n",
        )
        .unwrap();
        ClaudeCode.uninstall(&scope, &opts).unwrap();
        assert!(dir.path().join(".claude/skills/other/SKILL.md").exists());
    }

    #[test]
    fn uninstall_skips_user_replaced_skill_file() {
        let dir = TempDir::new().unwrap();
        let skill_path = dir.path().join(".claude/skills/ast-bro/SKILL.md");
        std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
        // User completely replaced our file with their own content (no
        // ast-bro marker). Uninstall must NOT delete it.
        std::fs::write(&skill_path, "---\nname: my-skill\n---\nmine\n").unwrap();
        let scope = local_scope(&dir);
        ClaudeCode
            .uninstall(&scope, &InstallOpts::default())
            .unwrap();
        assert!(skill_path.exists());
    }

    #[test]
    fn status_reports_mcp_and_skills_flags() {
        let dir = TempDir::new().unwrap();
        let scope = local_scope(&dir);
        let s0 = ClaudeCode.status(&scope);
        assert!(!s0.mcp_installed);
        assert!(!s0.skills_installed);
        ClaudeCode
            .install_mcp(&scope, &InstallOpts::default())
            .unwrap();
        ClaudeCode
            .install_skills(&scope, &InstallOpts::default())
            .unwrap();
        let s1 = ClaudeCode.status(&scope);
        assert!(s1.mcp_installed);
        assert!(s1.skills_installed);
    }
}
