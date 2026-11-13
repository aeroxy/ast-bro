//! Per-user and per-repo hook policy, read from `config.toml`.
//!
//! One key so far, and it exists because the hook cannot infer intent from a
//! path. A map is a good answer for a file someone consults a section of, and a
//! bad one for a file that has to be loaded whole before acting — an agent that
//! reads the outline of an instruction document and judges itself informed has
//! failed silently, with nothing in the transcript to show it. Which documents
//! are which is knowledge its owner has and the hook does not, so the owner
//! states it:
//!
//! ```toml
//! [hook]
//! keep_source = ["**/SKILL.md", "AGENTS.md", "docs/runbooks/**"]
//! ```
//!
//! Two files carry it, because the documents that need it do not all live in one
//! place. Skills and agent instructions sit under the home directory and belong
//! to no repository at all, so a repo-root config would never be consulted for
//! the case this key was written for; project runbooks are the opposite, and
//! belong to the repository so a teammate receives the policy with a checkout.
//! So `~/.config/ast-bro/config.toml` states a policy for everything the user
//! reads — patterns relative to the home directory — and `.ast-bro.toml` at a
//! project root states one for that project, patterns relative to the root. The
//! spelling follows the installers, which prefer an explicit `~/.config/<tool>/`
//! to `dirs::config_dir()` — on macOS that would put a developer dotfile in
//! `~/Library/Application Support`.
//!
//! A path either file protects is protected. Union rather than precedence
//! because every entry is the same kind of claim — *do not stand a map in for
//! this* — and for a claim like that the safe composition is the one where
//! neither file can quietly cancel the other. `!` still un-protects within the
//! file that says it.
//!
//! Deliberately **not** in `.ast-bro/`, which the search subsystem fills with a
//! `.gitignore` of `*`: config placed there could not be committed, so a
//! teammate would never receive the policy, and `rm -rf .ast-bro/` — the
//! documented cure for a stale cache — would take it with them. `.ast-bro/`
//! holds derived state that is safe to delete; this is authored state that is
//! not.
//!
//! Also distinct from `.ast-bro-ignore`, whose entries are excluded from every
//! walk. The two would read as synonyms and mean opposites: an ignored file is
//! one the tool never looks at, and a `keep_source` file is one it hands over in
//! full.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Per-project config, at a project root.
const REPO_CONFIG: &str = ".ast-bro.toml";

/// Per-user config, relative to the home directory.
const USER_CONFIG: &str = ".config/ast-bro/config.toml";

/// Paths whose source a map may not stand in for.
///
/// Gitignore syntax, matched by the same crate that applies `.ast-bro-ignore`,
/// so `**/` globs, directory prefixes, and `!` re-inclusions behave the way they
/// do everywhere else in the tool and need no separate explanation.
///
/// One matcher per config, each paired with the root its patterns are relative
/// to — the home directory for the user config, the project root for the repo
/// one. The root is kept because `Gitignore` *panics* on a path outside the one
/// it was built against, and the path reaching the hook comes from the host
/// rather than from here: `/var/…` against a root canonicalized to
/// `/private/var/…` is the same file by a different name, and enough to take the
/// hook process down. [`covers`](Self::covers) resolves both ends to the same
/// form and skips a matcher whose root the file is outside of — which is also
/// what lets a project outside the home directory consult only its own config.
#[derive(Default)]
pub struct KeepSource {
    scopes: Vec<(PathBuf, Gitignore)>,
}

impl KeepSource {
    /// Read every policy that governs `file`: the user's, and the one belonging
    /// to its project when it has one.
    ///
    /// An absent config and one with no `keep_source` key are the same answer —
    /// nothing added — because the common case is a machine that has never
    /// heard of this file. The exception is a config that exists and still
    /// cannot take effect: unreadable, unparseable, or holding a `keep_source`
    /// of the wrong shape. Ignoring that silently would hide the policy while
    /// looking like it applied, so it warns.
    pub fn for_file(file: &Path) -> Self {
        let mut out = Self::default();
        if let Ok(home) = crate::installers::paths::home() {
            out.add(home.join(USER_CONFIG), home);
        }
        // A skill under the home directory belongs to no project, and
        // `find_root_for` then settles on its own directory — harmless, since a
        // config it does not contain adds nothing.
        if let Ok(root) = crate::project_root::find_root_for(file) {
            out.add(root.join(REPO_CONFIG), root);
        }
        out
    }

    /// Add one config's patterns, rooted at `root`. A missing file is silent.
    ///
    /// The root is resolved here rather than trusted, because [`covers`] compares
    /// it against a canonicalized path and `dirs::home_dir()` hands back `$HOME`
    /// as spelled. A home behind a symlink would then match nothing and say
    /// nothing — a policy that looks applied and is not, which is the failure
    /// this key exists to prevent. `find_root_for` already returns a canonical
    /// path; resolving again is a no-op there.
    ///
    /// [`covers`]: Self::covers
    fn add(&mut self, config: PathBuf, root: PathBuf) {
        let text = match std::fs::read_to_string(&config) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!("# note: {} could not be read: {e}", config.display());
                return;
            }
        };
        let root = root.canonicalize().unwrap_or(root);
        let doc = match text.parse::<toml_edit::DocumentMut>() {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("# note: {} could not be parsed: {e}", config.display());
                return;
            }
        };
        let Some(item) = doc
            .get("hook")
            .and_then(|i| i.as_table_like())
            .and_then(|t| t.get("keep_source"))
        else {
            return;
        };
        // `keep_source = "SKILL.md"` parses, and a lookup that only asks for an
        // array would read it as absent — the policy inert, and nothing said.
        let Some(patterns) = item.as_array() else {
            eprintln!(
                "# note: {}: keep_source is not an array of strings, so none of it applies",
                config.display()
            );
            return;
        };
        let mut builder = GitignoreBuilder::new(&root);
        for value in patterns.iter() {
            let Some(line) = value.as_str() else {
                eprintln!(
                    "# note: {}: keep_source entry {} is not a string; skipped",
                    config.display(),
                    value.to_string().trim()
                );
                continue;
            };
            if let Err(e) = builder.add_line(None, line) {
                eprintln!(
                    "# note: {}: bad keep_source pattern {line:?}: {e}",
                    config.display()
                );
            }
        }
        if let Ok(matcher) = builder.build() {
            self.scopes.push((root, matcher));
        }
    }

    /// True when some config asked for this file to be delivered in full.
    pub fn covers(&self, file: &Path) -> bool {
        if self.scopes.is_empty() {
            return false;
        }
        // Resolved once, and the same way the roots were, because the matcher
        // panics rather than answering when handed a path outside its root and
        // the host's spelling of a path need not be the canonical one.
        let Ok(file) = file.canonicalize() else {
            return false;
        };
        self.scopes.iter().any(|(root, matcher)| {
            // `matched_path_or_any_parents` is what lets a directory pattern
            // (`docs/runbooks/**`) cover the files under it, matching how the
            // same patterns read in `.ast-bro-ignore`.
            file.starts_with(root)
                && matcher
                    .matched_path_or_any_parents(&file, false)
                    .is_ignore()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A repo root `find_root_for` will settle on, with a config in it.
    fn repo(config: Option<&str>) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        if let Some(text) = config {
            std::fs::write(dir.path().join(REPO_CONFIG), text).unwrap();
        }
        dir
    }

    fn touch(dir: &TempDir, rel: &str) -> std::path::PathBuf {
        let p = dir.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, "# doc\n").unwrap();
        p
    }

    #[test]
    fn a_repo_without_config_protects_nothing() {
        let dir = repo(None);
        let f = touch(&dir, "SKILL.md");
        assert!(!KeepSource::for_file(&f).covers(&f));
    }

    #[test]
    fn a_listed_file_is_covered_and_its_neighbours_are_not() {
        let dir = repo(Some(
            "[hook]\nkeep_source = [\"**/SKILL.md\", \"docs/runbooks/**\"]\n",
        ));
        let skill = touch(&dir, "skills/sweep/SKILL.md");
        let runbook = touch(&dir, "docs/runbooks/deploy.md");
        let readme = touch(&dir, "docs/README.md");

        let policy = KeepSource::for_file(&skill);
        assert!(policy.covers(&skill), "a **/ glob must reach a nested file");
        assert!(
            policy.covers(&runbook),
            "a directory pattern must cover its files"
        );
        assert!(!policy.covers(&readme), "an unlisted doc keeps the default");
    }

    /// `!` re-inclusion works, because the matcher is the one `.ast-bro-ignore`
    /// uses and nothing here re-implements glob semantics.
    #[test]
    fn a_negated_pattern_hands_one_file_back_to_the_outline() {
        let dir = repo(Some(
            "[hook]\nkeep_source = [\"docs/**\", \"!docs/api-reference.md\"]\n",
        ));
        let runbook = touch(&dir, "docs/deploy.md");
        let reference = touch(&dir, "docs/api-reference.md");

        let policy = KeepSource::for_file(&runbook);
        assert!(policy.covers(&runbook));
        assert!(
            !policy.covers(&reference),
            "! must un-protect the reference doc"
        );
    }

    /// A config that exists but does not parse protects nothing — and says so,
    /// because a policy that looks applied and is not is the failure this whole
    /// key exists to prevent.
    #[test]
    fn a_malformed_config_protects_nothing() {
        let dir = repo(Some("[hook\nkeep_source = ["));
        let f = touch(&dir, "SKILL.md");
        assert!(!KeepSource::for_file(&f).covers(&f));
    }

    /// The case the key was written for: a skill under the home directory,
    /// belonging to no repository.
    ///
    /// Exercised through `add` with a stand-in home, because a test may not
    /// write into the real one. What it pins is that a config's patterns are
    /// relative to *its own* root, so a user-level `**/SKILL.md` reaches a skill
    /// nested arbitrarily deep with no project root anywhere above it.
    #[test]
    fn a_user_config_reaches_a_skill_that_no_project_contains() {
        let home = TempDir::new().unwrap();
        let config = home.path().join("config.toml");
        std::fs::write(&config, "[hook]\nkeep_source = [\"**/SKILL.md\"]\n").unwrap();
        let skill = touch(&home, ".claude/skills/pr-description-sweep/SKILL.md");
        let other = touch(&home, ".claude/skills/pr-description-sweep/README.md");

        let mut policy = KeepSource::default();
        policy.add(config, home.path().canonicalize().unwrap());
        assert!(
            policy.covers(&skill),
            "a user pattern must reach a skill outside any repo"
        );
        assert!(
            !policy.covers(&other),
            "and must not spill onto its neighbours"
        );
    }

    /// Either config protecting a path is enough, and neither can cancel the
    /// other — the union the module doc argues for.
    #[test]
    fn the_two_scopes_union_rather_than_override() {
        let home = TempDir::new().unwrap();
        let user = home.path().join("config.toml");
        std::fs::write(&user, "[hook]\nkeep_source = [\"**/SKILL.md\"]\n").unwrap();

        let project = repo(Some("[hook]\nkeep_source = [\"docs/runbooks/**\"]\n"));
        let skill = touch(&home, "skills/a/SKILL.md");
        let runbook = touch(&project, "docs/runbooks/deploy.md");
        let plain = touch(&project, "docs/README.md");

        let mut policy = KeepSource::default();
        policy.add(user, home.path().canonicalize().unwrap());
        policy.add(
            project.path().join(REPO_CONFIG),
            project.path().canonicalize().unwrap(),
        );
        assert!(
            policy.covers(&skill),
            "the user scope still protects its own"
        );
        assert!(policy.covers(&runbook), "so does the project scope");
        assert!(
            !policy.covers(&plain),
            "and neither protects what neither listed"
        );
    }

    /// A file outside a scope's root is skipped, not asked about.
    ///
    /// `Gitignore::matched_path_or_any_parents` panics on a path it was not
    /// built for, so a project checked out beyond the home directory — or any
    /// path from a host that spells it differently — would take the hook process
    /// down rather than return an answer.
    #[test]
    fn a_path_outside_a_scope_is_skipped_instead_of_panicking() {
        let home = TempDir::new().unwrap();
        let user = home.path().join("config.toml");
        std::fs::write(&user, "[hook]\nkeep_source = [\"**/SKILL.md\"]\n").unwrap();

        let elsewhere = TempDir::new().unwrap();
        let outside = touch(&elsewhere, "SKILL.md");

        let mut policy = KeepSource::default();
        policy.add(user, home.path().canonicalize().unwrap());
        assert!(!policy.covers(&outside));
    }

    /// A root reached through a symlink still matches.
    ///
    /// `dirs::home_dir()` reports `$HOME` as spelled, and `covers` compares
    /// against a canonicalized path — so a home behind a symlink used to match
    /// nothing and report nothing, leaving the policy silently inert. Caught by
    /// running the real hook with `HOME` pointed at a temp dir under `/tmp`,
    /// which macOS canonicalizes to `/private/tmp`; the unit tests above could
    /// not see it because they pass a root already resolved.
    #[cfg(unix)]
    #[test]
    fn a_root_reached_through_a_symlink_still_matches() {
        let real = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("home-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let config = real.path().join("config.toml");
        std::fs::write(&config, "[hook]\nkeep_source = [\"**/SKILL.md\"]\n").unwrap();
        let skill = touch(&real, ".claude/skills/sweep/SKILL.md");

        let mut policy = KeepSource::default();
        // The un-resolved spelling, as `dirs::home_dir()` would hand it over.
        policy.add(link.join("config.toml"), link.clone());
        assert!(
            policy.covers(&skill),
            "a policy must not go inert because its root was spelled through a link"
        );
    }

    /// A config with other sections but no `keep_source` is not an error.
    #[test]
    fn a_config_without_the_key_protects_nothing() {
        let dir = repo(Some("[hook]\nmin_lines = 200\n"));
        let f = touch(&dir, "SKILL.md");
        assert!(!KeepSource::for_file(&f).covers(&f));
    }
}
