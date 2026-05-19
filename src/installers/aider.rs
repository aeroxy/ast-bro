use std::path::PathBuf;

use serde_yaml::{Mapping, Value as Yaml};

use super::io::{atomic_write, read_optional};
use super::paths;
use super::{common, Change, Detection, InstallOpts, Installer, Scope, Status};
use crate::prompt::AGENT_PROMPT;

pub struct Aider;

impl Aider {
    fn config_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join(".aider.conf.yml")),
            Scope::Global => paths::under_home(".aider.conf.yml"),
        }
    }
    fn conventions_path(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            Scope::Local(root) => Ok(root.join("CONVENTIONS.md")),
            Scope::Global => paths::under_home(".aider/CONVENTIONS.md"),
        }
    }
    fn relative_conventions(&self, scope: &Scope) -> &'static str {
        match scope {
            Scope::Local(_) => "CONVENTIONS.md",
            Scope::Global => "~/.aider/CONVENTIONS.md",
        }
    }
}

impl Installer for Aider {
    fn name(&self) -> &'static str {
        "aider"
    }

    fn detect(&self, scope: &Scope) -> Detection {
        let cfg_exists = self
            .config_path(scope)
            .ok()
            .map(|p| p.exists())
            .unwrap_or(false);
        Detection {
            present: cfg_exists || paths::binary_on_path("aider"),
        }
    }

    fn install_prompt(&self, scope: &Scope, opts: &InstallOpts) -> Result<Change, String> {
        let conv_path = self.conventions_path(scope)?;
        let change = common::install_prompt_in(&conv_path, AGENT_PROMPT, opts)?;

        let cfg_path = self.config_path(scope)?;
        let existing = read_optional(&cfg_path)?.unwrap_or_default();
        let mut root: Yaml = if existing.trim().is_empty() {
            Yaml::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(&existing)
                .map_err(|e| format!("parse {}: {}", cfg_path.display(), e))?
        };
        let map = root
            .as_mapping_mut()
            .ok_or_else(|| format!("{}: top level must be a mapping", cfg_path.display()))?;
        let key = Yaml::String("read".into());
        let entry = map.entry(key).or_insert(Yaml::Sequence(Vec::new()));
        if let Yaml::String(s) = entry.clone() {
            *entry = Yaml::Sequence(vec![Yaml::String(s)]);
        }
        let target = self.relative_conventions(scope).to_string();
        if let Yaml::Sequence(seq) = entry {
            let already = seq.iter().any(|v| v.as_str() == Some(&target));
            if !already {
                seq.push(Yaml::String(target));
                if !opts.dry_run {
                    let yaml = serde_yaml::to_string(&root)
                        .map_err(|e| format!("serialize yaml: {}", e))?;
                    atomic_write(&cfg_path, &yaml)?;
                }
            }
        }
        Ok(change)
    }

    fn install_hook(&self, _scope: &Scope, _opts: &InstallOpts) -> Result<Change, String> {
        Ok(Change::NotApplicable)
    }

    fn uninstall(&self, scope: &Scope, opts: &InstallOpts) -> Result<Vec<Change>, String> {
        let mut changes = Vec::new();
        if let Some(c) = common::uninstall_prompt_in(&self.conventions_path(scope)?, opts)? {
            changes.push(c);
        }

        let cfg_path = self.config_path(scope)?;
        if let Some(existing) = read_optional(&cfg_path)? {
            if let Ok(mut root) = serde_yaml::from_str::<Yaml>(&existing) {
                let target = self.relative_conventions(scope).to_string();
                let read_key = Yaml::String("read".into());
                let mut removed_any = false;
                if let Some(map) = root.as_mapping_mut() {
                    if let Some(Yaml::Sequence(seq)) = map.get_mut(&read_key) {
                        let before = seq.len();
                        seq.retain(|v| v.as_str() != Some(&target));
                        removed_any = seq.len() != before;
                        if seq.is_empty() {
                            map.remove(&read_key);
                        }
                    }
                }
                if removed_any {
                    if !opts.dry_run {
                        let yaml = serde_yaml::to_string(&root)
                            .map_err(|e| format!("serialize yaml: {}", e))?;
                        atomic_write(&cfg_path, &yaml)?;
                    }
                    changes.push(Change::Removed(cfg_path));
                }
            }
        }
        Ok(changes)
    }

    fn status(&self, scope: &Scope) -> Status {
        common::status_for_prompt_only(self.conventions_path(scope).ok().as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn install_writes_conventions_and_yaml_read_entry() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Aider
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();

        let conv = std::fs::read_to_string(dir.path().join("CONVENTIONS.md")).unwrap();
        assert!(conv.contains("ast-bro:begin"));

        let yaml = std::fs::read_to_string(dir.path().join(".aider.conf.yml")).unwrap();
        assert!(yaml.contains("CONVENTIONS.md"));
        assert!(yaml.contains("read:"));
    }

    #[test]
    fn install_idempotent_does_not_duplicate_yaml_entry() {
        let dir = TempDir::new().unwrap();
        let scope = Scope::Local(dir.path().to_path_buf());
        Aider
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        Aider
            .install_prompt(&scope, &InstallOpts::default())
            .unwrap();
        let yaml = std::fs::read_to_string(dir.path().join(".aider.conf.yml")).unwrap();
        let count = yaml.matches("CONVENTIONS.md").count();
        assert_eq!(count, 1);
    }
}
