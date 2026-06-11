//! Persist the set of open tabs across restarts. A live PTY can't be
//! serialised, so we save each tab's *spawn recipe* (argv, cwd, optional
//! session key + name) and re-launch them on the next start. The cwd is the
//! shell's live directory at save time, so a reopened shell lands where you
//! left it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct TabSpec {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm/session.json")
}

/// Tabs to restore on startup (empty if none / unreadable).
pub fn load() -> Vec<TabSpec> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Overwrite the saved session with `specs` (best-effort).
pub fn save(specs: &[TabSpec]) {
    let p = path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(specs) {
        let _ = std::fs::write(p, json);
    }
}
