//! User settings: a global, persisted struct read across the app (theme sizes,
//! which providers to scan, shell defaults, refresh cadence, …).

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// UI (proportional) font size in points.
    pub ui_font: f32,
    /// Default terminal grid font size for new tabs.
    pub term_font: f32,
    pub scan_claude: bool,
    pub scan_codex: bool,
    pub scan_opencode: bool,
    pub scan_gemini: bool,
    /// Close a tab automatically once its child exits.
    pub auto_close_on_exit: bool,
    /// Shell command for the `>_` button (whitespace-split argv; empty = $SHELL).
    pub shell_command: String,
    /// Start directory for new shells (empty = $HOME).
    pub shell_dir: String,
    /// Panel auto-refresh cadence in seconds.
    pub refresh_secs: u64,
    /// Whether to query provider service status + account quota (network).
    pub fetch_status: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_font: 15.5,
            term_font: 14.0,
            scan_claude: true,
            scan_codex: true,
            scan_opencode: true,
            scan_gemini: true,
            auto_close_on_exit: true,
            shell_command: String::new(),
            shell_dir: String::new(),
            refresh_secs: 120,
            fetch_status: true,
        }
    }
}

impl Settings {
    /// Is provider `id` enabled for scanning?
    pub fn scans(&self, id: &str) -> bool {
        match id {
            "claude" => self.scan_claude,
            "codex" => self.scan_codex,
            "opencode" => self.scan_opencode,
            "gemini" => self.scan_gemini,
            _ => true,
        }
    }
}

static SETTINGS: LazyLock<RwLock<Settings>> = LazyLock::new(|| RwLock::new(load_from_disk()));

/// A snapshot of the current settings.
pub fn get() -> Settings {
    SETTINGS.read().unwrap().clone()
}

/// Mutate and persist the settings.
pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut s = SETTINGS.write().unwrap();
    f(&mut s);
    let _ = save_to_disk(&s);
}

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm/settings.json")
}

fn load_from_disk() -> Settings {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_to_disk(s: &Settings) -> std::io::Result<()> {
    let p = path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(s).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}
