//! Local, user-editable metadata overlaid on the read-only session list:
//! a display-name override, free-form tags, and a color token. Keyed by
//! `provider:id` and persisted as a single JSON file so it survives restarts
//! and is trivial to inspect or sync.
//!
//! The on-disk session logs are never mutated; this is a sidecar the panel
//! merges in at render time.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One session's local metadata. All fields optional: an entry exists only
/// once the user sets something. `notes` and `favorite` are recent additions;
/// the `#[serde(default)]` guards make older on-disk files (without these
/// keys) load fine, and serializing with `skip_serializing_if` keeps the
/// store byte-stable so a freshly-installed older build round-trips it
/// without churning the file.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SessionMetadata {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub color: Option<String>,
    /// Free-form notes the user attaches to the session.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub notes: Option<String>,
    /// Pin-to-top flag.
    #[serde(skip_serializing_if = "is_false", default)]
    pub favorite: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl SessionMetadata {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.tags.is_empty()
            && self.color.is_none()
            && self.notes.is_none()
            && !self.favorite
    }
}

/// `provider:id` -> metadata. Wraps a flat map with load/save/mutate helpers.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MetadataStore {
    entries: HashMap<String, SessionMetadata>,
}

fn key(provider: &str, id: &str) -> String {
    format!("{provider}:{id}")
}

impl MetadataStore {
    /// Read the store from `path`; an absent or unparseable file yields an
    /// empty store (metadata is best-effort, never fatal).
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Persist to `path`, creating parent dirs. Errors are returned so callers
    /// can surface a toast, but the in-memory store stays authoritative.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn get(&self, provider: &str, id: &str) -> Option<&SessionMetadata> {
        self.entries.get(&key(provider, id))
    }

    /// Apply `mutate` to the entry (created if absent); drop it if it ends up
    /// empty so the file doesn't accrue dead keys.
    pub fn update(
        &mut self,
        provider: &str,
        id: &str,
        mutate: impl FnOnce(&mut SessionMetadata),
    ) {
        let k = key(provider, id);
        let entry = self.entries.entry(k.clone()).or_default();
        mutate(entry);
        if entry.is_empty() {
            self.entries.remove(&k);
        }
    }

    /// All distinct tags across every entry, sorted — for tag autocomplete.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .entries
            .values()
            .flat_map(|m| m.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }
}

/// Parse a comma/space separated tag string into a clean, deduped list.
pub fn parse_tags(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw.split([',', ' ']) {
        let t = tag.trim();
        if !t.is_empty() && !out.iter().any(|e| e == t) {
            out.push(t.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_creates_and_prunes_empty() {
        let mut s = MetadataStore::default();
        s.update("claude", "a", |m| m.name = Some("Mi tarea".into()));
        assert_eq!(s.get("claude", "a").unwrap().name.as_deref(), Some("Mi tarea"));
        // Clearing back to empty removes the entry.
        s.update("claude", "a", |m| m.name = None);
        assert!(s.get("claude", "a").is_none());
    }

    #[test]
    fn roundtrips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("md.json");
        let mut s = MetadataStore::default();
        s.update("codex", "x", |m| {
            m.tags = vec!["wip".into(), "review".into()];
            m.color = Some("blue".into());
        });
        s.save(&path).unwrap();
        let loaded = MetadataStore::load(&path);
        let e = loaded.get("codex", "x").unwrap();
        assert_eq!(e.tags, vec!["wip", "review"]);
        assert_eq!(e.color.as_deref(), Some("blue"));
        assert_eq!(loaded.all_tags(), vec!["review", "wip"]);
    }

    #[test]
    fn parse_tags_splits_and_dedupes() {
        assert_eq!(parse_tags("a, b a,  c "), vec!["a", "b", "c"]);
        assert!(parse_tags("   ").is_empty());
    }

    #[test]
    fn missing_file_is_empty() {
        assert!(MetadataStore::load(std::path::Path::new("/no/such/file")).all_tags().is_empty());
    }
}
