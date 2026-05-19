//! Manages a single key inside a JSON object at a navigated path.
//! Sibling keys at every level are preserved (and so is insertion order,
//! courtesy of `serde_json`'s `preserve_order` feature).
//!
//! Parallel to `json_hook`, which manages an entry inside a JSON array.

use serde_json::{json, Map, Value};

pub fn upsert(root: &mut Value, path: &[&str], key: &str, entry: Value) -> bool {
    let obj = ensure_object(root, path);
    match obj.get(key) {
        Some(existing) if existing == &entry => false,
        _ => {
            obj.insert(key.to_string(), entry);
            true
        }
    }
}

pub fn remove(root: &mut Value, path: &[&str], key: &str) -> bool {
    match navigate_object_mut(root, path) {
        Some(obj) => obj.remove(key).is_some(),
        None => false,
    }
}

pub fn is_installed(root: &Value, path: &[&str], key: &str) -> bool {
    navigate_object(root, path)
        .map(|obj| obj.contains_key(key))
        .unwrap_or(false)
}

fn ensure_object<'a>(root: &'a mut Value, path: &[&str]) -> &'a mut Map<String, Value> {
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let mut current = root;
    for key in path {
        let obj = current.as_object_mut().unwrap();
        let entry = obj
            .entry((*key).to_string())
            .or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        current = entry;
    }
    current.as_object_mut().expect("ensure_object invariant")
}

fn navigate_object<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Map<String, Value>> {
    let mut current = root;
    for key in path {
        current = current.as_object()?.get(*key)?;
    }
    current.as_object()
}

fn navigate_object_mut<'a>(root: &'a mut Value, path: &[&str]) -> Option<&'a mut Map<String, Value>> {
    let mut current = root;
    for key in path {
        current = current.as_object_mut()?.get_mut(*key)?;
    }
    current.as_object_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Value {
        json!({ "command": "ast-bro", "args": ["mcp"] })
    }

    #[test]
    fn upsert_into_empty_root() {
        let mut root = json!({});
        let modified = upsert(&mut root, &["mcpServers"], "ast-bro", entry());
        assert!(modified);
        assert!(is_installed(&root, &["mcpServers"], "ast-bro"));
    }

    #[test]
    fn upsert_preserves_50_unrelated_keys() {
        // Mimic ~/.claude.json: a large flat object with many top-level keys.
        let mut root = Value::Object(Map::new());
        for i in 0..50 {
            root.as_object_mut()
                .unwrap()
                .insert(format!("key_{:02}", i), json!(i));
        }
        upsert(&mut root, &["mcpServers"], "ast-bro", entry());
        for i in 0..50 {
            assert_eq!(
                root[format!("key_{:02}", i)],
                json!(i),
                "key_{:02} was lost or mutated",
                i
            );
        }
    }

    #[test]
    fn upsert_replaces_in_place_by_key() {
        let mut root = json!({
            "mcpServers": {
                "ast-bro": { "command": "old", "args": ["mcp"] },
                "other": { "command": "x", "args": [] }
            }
        });
        upsert(&mut root, &["mcpServers"], "ast-bro", entry());
        assert_eq!(root["mcpServers"]["ast-bro"]["command"], "ast-bro");
        assert_eq!(root["mcpServers"]["other"]["command"], "x");
    }

    #[test]
    fn upsert_idempotent_when_entry_unchanged() {
        let mut root = json!({});
        upsert(&mut root, &["mcpServers"], "ast-bro", entry());
        let modified = upsert(&mut root, &["mcpServers"], "ast-bro", entry());
        assert!(!modified);
    }

    #[test]
    fn remove_drops_key_keeps_siblings() {
        let mut root = json!({
            "mcpServers": {
                "ast-bro": { "command": "ast-bro", "args": ["mcp"] },
                "other": { "command": "x", "args": [] }
            }
        });
        let removed = remove(&mut root, &["mcpServers"], "ast-bro");
        assert!(removed);
        assert!(!is_installed(&root, &["mcpServers"], "ast-bro"));
        assert!(is_installed(&root, &["mcpServers"], "other"));
    }

    #[test]
    fn remove_noop_when_path_absent() {
        let mut root = json!({});
        assert!(!remove(&mut root, &["mcpServers"], "ast-bro"));
    }

    #[test]
    fn remove_noop_when_key_absent() {
        let mut root = json!({ "mcpServers": { "other": {} } });
        assert!(!remove(&mut root, &["mcpServers"], "ast-bro"));
        assert!(is_installed(&root, &["mcpServers"], "other"));
    }

    #[test]
    fn is_installed_false_when_path_missing() {
        let root = json!({});
        assert!(!is_installed(&root, &["mcpServers"], "ast-bro"));
    }

    #[test]
    fn key_insertion_order_preserved() {
        let mut root = Value::Object(Map::new());
        for name in ["alpha", "beta", "gamma"] {
            root.as_object_mut()
                .unwrap()
                .insert(name.into(), json!({}));
        }
        upsert(&mut root, &["mcpServers"], "ast-bro", entry());
        let keys: Vec<&String> = root.as_object().unwrap().keys().collect();
        // First three preserve order; "mcpServers" appended last.
        assert_eq!(keys, vec!["alpha", "beta", "gamma", "mcpServers"]);
    }
}
