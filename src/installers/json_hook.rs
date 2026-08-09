//! Manages our hook entry inside a JSON settings file. Caller supplies
//! the JSON path-into-array and a predicate that identifies our entry —
//! sibling entries are preserved.

use serde_json::{json, Map, Value};

/// String prefix every adapter's hook command begins with. Adapter
/// predicates use this to find our entry without relying on a stable
/// `id` field (Claude Code and Gemini hook entries do not have one).
pub const MARKER: &str = "ast-bro hook";

/// Pre-rebrand prefix. Still recognized so legacy entries get upgraded /
/// removed on the next install or uninstall, rather than orphaned in place.
pub const LEGACY_MARKER: &str = "ast-outline hook";

/// True if `command` is one of ours — either current or legacy form.
pub fn matches_any_marker(command: &str) -> bool {
    command.starts_with(MARKER) || command.starts_with(LEGACY_MARKER)
}

pub fn upsert<F>(root: &mut Value, path: &[&str], entry: Value, matches: F) -> bool
where
    F: Fn(&Value) -> bool,
{
    let arr = ensure_array(root, path);
    if let Some(pos) = arr.iter().position(matches) {
        if arr[pos] == entry {
            return false;
        }
        arr[pos] = entry;
    } else {
        arr.push(entry);
    }
    true
}

pub fn remove<F>(root: &mut Value, path: &[&str], matches: F) -> bool
where
    F: Fn(&Value) -> bool,
{
    let arr = match navigate_array_mut(root, path) {
        Some(a) => a,
        None => return false,
    };
    let before = arr.len();
    arr.retain(|v| !matches(v));
    if arr.len() == before {
        return false;
    }
    if arr.is_empty() {
        prune_empty(root, path);
    }
    true
}

/// Deletes the key at `path`, and each ancestor the deletion left empty.
///
/// An event key we emptied is litter in a file the user owns: it reads as a hook
/// configured for that event when none is. It disables nothing — Claude Code
/// merges hook entries across settings levels rather than letting one level
/// replace another — so what it costs is the next reader's time.
fn prune_empty(root: &mut Value, path: &[&str]) {
    for depth in (1..=path.len()).rev() {
        let (parent_path, tail) = path[..depth].split_at(depth - 1);
        let key = tail[0];
        let Some(parent) = navigate_object_mut(root, parent_path) else {
            return;
        };
        let empty = match parent.get(key) {
            Some(Value::Array(a)) => a.is_empty(),
            Some(Value::Object(o)) => o.is_empty(),
            _ => return,
        };
        if !empty {
            return;
        }
        parent.remove(key);
    }
}

pub fn is_installed<F>(root: &Value, path: &[&str], matches: F) -> bool
where
    F: Fn(&Value) -> bool,
{
    let arr = match navigate_array(root, path) {
        Some(a) => a,
        None => return false,
    };
    arr.iter().any(matches)
}

fn ensure_array<'a>(root: &'a mut Value, path: &[&str]) -> &'a mut Vec<Value> {
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let mut current = root;
    for (i, key) in path.iter().enumerate() {
        let is_last = i + 1 == path.len();
        let obj = current.as_object_mut().unwrap();
        let entry = obj
            .entry((*key).to_string())
            .or_insert_with(|| if is_last { json!([]) } else { json!({}) });
        if is_last {
            if !entry.is_array() {
                *entry = json!([]);
            }
        } else if !entry.is_object() {
            *entry = json!({});
        }
        current = entry;
    }
    current.as_array_mut().expect("ensure_array invariant")
}

fn navigate_array<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    let mut current = root;
    for key in path {
        current = current.as_object()?.get(*key)?;
    }
    current.as_array()
}

fn navigate_object_mut<'a>(
    root: &'a mut Value,
    path: &[&str],
) -> Option<&'a mut Map<String, Value>> {
    let mut current = root;
    for key in path {
        current = current.as_object_mut()?.get_mut(*key)?;
    }
    current.as_object_mut()
}

fn navigate_array_mut<'a>(root: &'a mut Value, path: &[&str]) -> Option<&'a mut Vec<Value>> {
    let mut current = root;
    for key in path {
        current = current.as_object_mut()?.get_mut(*key)?;
    }
    current.as_array_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Value {
        json!({
            "matcher": "Read",
            "hooks": [{"type": "command", "command": "ast-bro hook --protocol claude-code"}]
        })
    }

    fn predicate(v: &Value) -> bool {
        v.get("matcher").and_then(|m| m.as_str()) == Some("Read")
            && v.get("hooks")
                .and_then(|h| h.as_array())
                .and_then(|h| h.first())
                .and_then(|h0| h0.get("command"))
                .and_then(|c| c.as_str())
                .map(matches_any_marker)
                .unwrap_or(false)
    }

    #[test]
    fn upsert_into_empty_root() {
        let mut root = json!({});
        let modified = upsert(&mut root, &["hooks", "PreToolUse"], entry(), predicate);
        assert!(modified);
        assert!(is_installed(&root, &["hooks", "PreToolUse"], predicate));
    }

    #[test]
    fn upsert_preserves_sibling_entries() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Edit", "hooks": [{"type": "command", "command": "echo hi"}] }
                ]
            }
        });
        upsert(&mut root, &["hooks", "PreToolUse"], entry(), predicate);
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["matcher"].as_str().unwrap(), "Edit");
        assert_eq!(arr[1]["matcher"].as_str().unwrap(), "Read");
    }

    #[test]
    fn upsert_replaces_in_place_by_predicate() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read", "hooks": [{"type": "command", "command": "ast-bro hook OLD"}] },
                    { "matcher": "Edit", "hooks": [{"type": "command", "command": "echo hi"}] }
                ]
            }
        });
        upsert(&mut root, &["hooks", "PreToolUse"], entry(), predicate);
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0]["hooks"][0]["command"].as_str().unwrap(),
            "ast-bro hook --protocol claude-code"
        );
        assert_eq!(arr[1]["matcher"].as_str().unwrap(), "Edit");
    }

    #[test]
    fn upsert_idempotent_when_entry_unchanged() {
        let mut root = json!({});
        upsert(&mut root, &["hooks", "PreToolUse"], entry(), predicate);
        let modified = upsert(&mut root, &["hooks", "PreToolUse"], entry(), predicate);
        assert!(!modified);
    }

    #[test]
    fn remove_drops_entry_keeps_siblings() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read", "hooks": [{"type": "command", "command": "ast-bro hook X"}] },
                    { "matcher": "Edit", "hooks": [{"type": "command", "command": "echo Y"}] }
                ]
            }
        });
        let removed = remove(&mut root, &["hooks", "PreToolUse"], predicate);
        assert!(removed);
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["matcher"].as_str().unwrap(), "Edit");
    }

    #[test]
    fn remove_noop_when_path_absent() {
        let mut root = json!({});
        assert!(!remove(&mut root, &["hooks", "PreToolUse"], predicate));
    }

    #[test]
    fn is_installed_false_when_path_missing() {
        let root = json!({});
        assert!(!is_installed(&root, &["hooks", "PreToolUse"], predicate));
    }

    #[test]
    fn matches_any_marker_accepts_legacy_and_current() {
        assert!(matches_any_marker("ast-bro hook --protocol claude-code"));
        assert!(matches_any_marker("ast-outline hook --protocol claude-code"));
        assert!(!matches_any_marker("echo hi"));
    }

    #[test]
    fn legacy_entry_is_upgraded_in_place() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read", "hooks": [{"type": "command", "command": "ast-outline hook --protocol claude-code"}] }
                ]
            }
        });
        let modified = upsert(&mut root, &["hooks", "PreToolUse"], entry(), predicate);
        assert!(modified);
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["hooks"][0]["command"].as_str().unwrap(),
            "ast-bro hook --protocol claude-code"
        );
    }

    /// Removing the last entry takes the event key with it, and the `hooks`
    /// object too when that was the last event. An emptied key is litter in a
    /// file the user owns: it reads as a hook configured for that event.
    #[test]
    fn legacy_entry_is_removed_and_leaves_no_empty_key() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read", "hooks": [{"type": "command", "command": "ast-outline hook --protocol claude-code"}] }
                ]
            }
        });
        let removed = remove(&mut root, &["hooks", "PreToolUse"], predicate);
        assert!(removed);
        assert_eq!(root, json!({}), "left something behind: {root}");
    }

    /// Pruning stops at the first key that still holds something, so an event
    /// the user configured for their own hooks survives ours being removed.
    #[test]
    fn remove_keeps_a_sibling_event_and_its_parent() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read", "hooks": [{"type": "command", "command": "ast-bro hook --protocol claude-code"}] }
                ],
                "PostToolUse": [
                    { "matcher": "Bash", "hooks": [{"type": "command", "command": "echo mine"}] }
                ]
            }
        });
        assert!(remove(&mut root, &["hooks", "PreToolUse"], predicate));
        assert!(root["hooks"].get("PreToolUse").is_none(), "{root}");
        assert!(root["hooks"]["PostToolUse"].is_array(), "{root}");
    }
}
