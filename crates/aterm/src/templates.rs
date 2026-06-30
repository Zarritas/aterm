//! Launch templates: a saved `(provider, prompt, cwd, tags)` recipe to spin up
//! a fresh agent session quickly.
//!
//! Byte-compatible with the sidecar's `templates-{get,set,delete}` store so the
//! native app and the VS Code extension share one `~/.config/aterm/templates.
//! json` (same camelCase keys, same shape). The native app links this directly
//! instead of shelling out to the sidecar.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One launch recipe. `id` is a stable slug; `prompt`/`cwd` optional.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LaunchTemplate {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// The on-disk store: `{ "templates": [ … ] }` (matches the sidecar exactly).
#[derive(Default, Serialize, Deserialize)]
pub struct TemplateStore {
    #[serde(default)]
    pub templates: Vec<LaunchTemplate>,
}

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm/templates.json")
}

impl TemplateStore {
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        let p = path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(p, json).map_err(|e| e.to_string())
    }

    /// Insert or replace by `id`, then persist.
    pub fn upsert(&mut self, t: LaunchTemplate) -> Result<(), String> {
        if let Some(existing) = self.templates.iter_mut().find(|x| x.id == t.id) {
            *existing = t;
        } else {
            self.templates.push(t);
        }
        self.save()
    }

    /// Remove by `id`, then persist.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.templates.retain(|t| t.id != id);
        self.save()
    }
}

/// A stable-ish slug from a name + a disambiguating suffix (epoch secs in b36),
/// matching nothing in particular — just unique enough for a local store.
pub fn slug(name: &str, salt_secs: u64) -> String {
    let base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let base = base.trim_matches('-');
    let base = if base.is_empty() { "tpl" } else { base };
    format!("{base}-{:x}", salt_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_shape_is_sidecar_compatible() {
        let store = TemplateStore {
            templates: vec![LaunchTemplate {
                id: "x".into(),
                name: "Mi plantilla".into(),
                provider: "claude".into(),
                prompt: Some("hola".into()),
                cwd: None,
                tags: vec!["wip".into()],
            }],
        };
        let json = serde_json::to_string(&store).unwrap();
        // camelCase keys, no nulls for skipped optionals.
        assert!(json.contains("\"templates\""));
        assert!(json.contains("\"provider\":\"claude\""));
        assert!(!json.contains("\"cwd\""));
    }

    #[test]
    fn slug_is_clean() {
        assert_eq!(slug("Mi Plantilla!", 0), "mi-plantilla-0");
        assert_eq!(slug("  ", 255), "tpl-ff");
    }
}
