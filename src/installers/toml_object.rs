//! TOML counterpart to `json_object`. Manages a single child table inside
//! a parent table, preserving the user's existing formatting and comments
//! (we use `toml_edit::DocumentMut`, not the serde-based `toml` crate).
//!
//! Used by adapters whose config file is TOML (currently: Codex CLI's
//! `~/.codex/config.toml` for the `[mcp_servers.<name>]` namespace).

use toml_edit::{DocumentMut, Item, Table};

/// Insert or replace `parent.<key>` with `entry`. Returns true if the
/// document was modified, false if it was already byte-identical.
pub fn upsert(doc: &mut DocumentMut, parent: &str, key: &str, entry: Table) -> bool {
    if !doc.contains_key(parent) || !doc[parent].is_table() {
        let mut t = Table::new();
        t.set_implicit(false);
        doc[parent] = Item::Table(t);
    }
    let parent_tbl = doc[parent].as_table_mut().expect("ensured above");
    if let Some(existing) = parent_tbl.get(key) {
        if existing.to_string() == Item::Table(entry.clone()).to_string() {
            return false;
        }
    }
    parent_tbl.insert(key, Item::Table(entry));
    true
}

pub fn remove(doc: &mut DocumentMut, parent: &str, key: &str) -> bool {
    let Some(parent_tbl) = doc.get_mut(parent).and_then(|i| i.as_table_mut()) else {
        return false;
    };
    parent_tbl.remove(key).is_some()
}

pub fn is_installed(doc: &DocumentMut, parent: &str, key: &str) -> bool {
    doc.get(parent)
        .and_then(|i| i.as_table())
        .map(|t| t.contains_key(key))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Table {
        let mut t = Table::new();
        t["command"] = toml_edit::value("ast-bro");
        let mut args = toml_edit::Array::new();
        args.push("mcp");
        t["args"] = toml_edit::value(args);
        t
    }

    #[test]
    fn upsert_into_empty_doc() {
        let mut doc: DocumentMut = "".parse().unwrap();
        let modified = upsert(&mut doc, "mcp_servers", "ast-bro", entry());
        assert!(modified);
        assert!(is_installed(&doc, "mcp_servers", "ast-bro"));
        let s = doc.to_string();
        assert!(s.contains("[mcp_servers.ast-bro]"));
        assert!(s.contains("command = \"ast-bro\""));
    }

    #[test]
    fn upsert_preserves_unrelated_top_level_keys() {
        let src = r#"
# my codex config
model = "gpt-5"
approval_policy = "auto"

[shell]
default_shell = "zsh"
"#;
        let mut doc: DocumentMut = src.parse().unwrap();
        upsert(&mut doc, "mcp_servers", "ast-bro", entry());
        let out = doc.to_string();
        assert!(out.contains("# my codex config"), "comment lost: {}", out);
        assert!(out.contains("model = \"gpt-5\""));
        assert!(out.contains("default_shell = \"zsh\""));
        assert!(out.contains("[mcp_servers.ast-bro]"));
    }

    #[test]
    fn upsert_preserves_other_servers() {
        let src = r#"
[mcp_servers.docs]
command = "docs-server"
args = ["serve"]
"#;
        let mut doc: DocumentMut = src.parse().unwrap();
        upsert(&mut doc, "mcp_servers", "ast-bro", entry());
        let out = doc.to_string();
        assert!(out.contains("[mcp_servers.docs]"));
        assert!(out.contains("[mcp_servers.ast-bro]"));
    }

    #[test]
    fn upsert_idempotent_when_entry_unchanged() {
        let mut doc: DocumentMut = "".parse().unwrap();
        upsert(&mut doc, "mcp_servers", "ast-bro", entry());
        let modified = upsert(&mut doc, "mcp_servers", "ast-bro", entry());
        assert!(!modified);
    }

    #[test]
    fn remove_drops_entry_keeps_siblings() {
        let mut doc: DocumentMut = "".parse().unwrap();
        upsert(&mut doc, "mcp_servers", "ast-bro", entry());
        let mut other = Table::new();
        other["command"] = toml_edit::value("x");
        upsert(&mut doc, "mcp_servers", "other", other);
        assert!(remove(&mut doc, "mcp_servers", "ast-bro"));
        assert!(!is_installed(&doc, "mcp_servers", "ast-bro"));
        assert!(is_installed(&doc, "mcp_servers", "other"));
    }

    #[test]
    fn remove_noop_when_parent_missing() {
        let mut doc: DocumentMut = "model = \"gpt-5\"\n".parse().unwrap();
        assert!(!remove(&mut doc, "mcp_servers", "ast-bro"));
    }

    #[test]
    fn is_installed_false_when_path_missing() {
        let doc: DocumentMut = "".parse().unwrap();
        assert!(!is_installed(&doc, "mcp_servers", "ast-bro"));
    }
}
