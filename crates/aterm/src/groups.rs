//! Manual session groups/collections: user-defined buckets a session can be
//! assigned to, independent of provider/project.
//!
//! Native-only (the extension keeps these in `globalState`, not shared), so
//! they live in `~/.config/aterm/groups.json` and never touch the metadata
//! shared with the extension. Members are session keys (`provider:id`).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One user-defined group.
#[derive(Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub members: HashSet<String>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct GroupStore {
    #[serde(default)]
    pub groups: Vec<Group>,
}

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm/groups.json")
}

impl GroupStore {
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let p = path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }

    /// Create a group from a display name (id derived from name + ordinal).
    pub fn create(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let id = format!("g{}", self.groups.len() + 1);
        self.groups.push(Group {
            id,
            name: name.to_string(),
            color: None,
            members: HashSet::new(),
        });
        self.save();
    }

    pub fn delete(&mut self, id: &str) {
        self.groups.retain(|g| g.id != id);
        self.save();
    }

    /// Is `key` a member of group `id`?
    pub fn contains(&self, id: &str, key: &str) -> bool {
        self.groups
            .iter()
            .find(|g| g.id == id)
            .is_some_and(|g| g.members.contains(key))
    }

    /// Add or remove `key` from group `id`.
    pub fn toggle(&mut self, id: &str, key: &str) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == id) {
            if !g.members.remove(key) {
                g.members.insert(key.to_string());
            }
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_toggle_delete() {
        let mut s = GroupStore::default();
        s.groups.push(Group {
            id: "g1".into(),
            name: "Sprint".into(),
            color: None,
            members: HashSet::new(),
        });
        assert!(!s.contains("g1", "claude:a"));
        s.groups[0].members.insert("claude:a".into());
        assert!(s.contains("g1", "claude:a"));
        s.delete("g1");
        assert!(s.groups.is_empty());
    }

    #[test]
    fn color_skipped_when_none() {
        let g = Group {
            id: "g1".into(),
            name: "X".into(),
            color: None,
            members: HashSet::new(),
        };
        let json = serde_json::to_string(&g).unwrap();
        assert!(!json.contains("color"));
    }
}
