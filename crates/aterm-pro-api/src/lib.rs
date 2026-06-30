//! The open-core contract between `aterm` (core) and `aterm-pro` (private).
//!
//! Mirrors the extension's `ProApi`/`ProModule` split (`src/pro-api.d.ts`):
//!
//! - [`ProHost`] is the surface the **core** exposes — providers, git exec,
//!   opening agent tabs, injecting prompts, notifications and the licence gate.
//!   The core (`AtermApp`) implements it.
//! - [`ProModule`] is what the **Pro** crate implements — the gated features
//!   (parallel worktree compare, …). The core holds it behind a feature flag;
//!   without `--features pro` it's a Community stub.
//!
//! egui is in the contract on purpose: unlike VS Code's async dialogs, egui is
//! immediate-mode, so a Pro feature's dialog must be redrawn every frame from
//! [`ProModule::ui`]. State for an in-flight dialog therefore lives inside the
//! `ProModule` implementation, not in the core.

use std::path::{Path, PathBuf};

/// A coding-agent provider, flattened for the Pro surface (no trait objects
/// cross the crate boundary). Built by the host from `agent-sessions`.
#[derive(Clone, Debug)]
pub struct ProviderLite {
    pub id: String,
    pub display_name: String,
    /// Whether the provider's binary was found in `PATH`.
    pub available: bool,
    /// argv that launches a fresh session for this provider.
    pub new_session_argv: Vec<String>,
}

/// A session, flattened for the Pro surface (dashboard, profiles, port).
#[derive(Clone, Debug)]
pub struct SessionLite {
    pub provider: String,
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    /// Unix epoch seconds of last activity.
    pub last_activity: f64,
    pub message_count: Option<u64>,
    /// argv to resume this session (empty if not resumable).
    pub resume_argv: Vec<String>,
}

/// A live terminal tab, flattened so a workspace profile can be snapshotted and
/// reopened.
#[derive(Clone, Debug)]
pub struct TabSnapshot {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub key: Option<String>,
    pub name: Option<String>,
}

/// One conversation turn (role + text), for export/port.
#[derive(Clone, Debug)]
pub struct Turn {
    pub role: String,
    pub text: String,
}

/// Services the core lends to Pro features. The core (`AtermApp`) implements
/// this; Pro code only ever sees this surface.
pub trait ProHost {
    /// Every known provider with availability + launch argv.
    fn providers(&self) -> Vec<ProviderLite>;

    /// The git repository root of the focused terminal's cwd, if it is inside
    /// a git repo. `None` when there's no focused tab or it isn't a repo.
    fn repo_root(&self) -> Option<PathBuf>;

    /// Run `git <args>` in `cwd`, returning stdout on success or a message on
    /// failure (non-zero exit → stderr text).
    fn exec_git(&self, args: &[&str], cwd: &Path) -> Result<String, String>;

    /// Spawn `argv` in a new terminal tab rooted at `cwd`. Returns the new
    /// tab id (for later prompt injection), or `None` if the spawn failed.
    fn open_agent(&mut self, argv: Vec<String>, cwd: PathBuf) -> Option<u64>;

    /// Inject `text` into the given tab's PTY after `delay_ms` (no trailing
    /// newline — the user reviews and presses Enter, matching the extension).
    fn inject_prompt(&mut self, tab_id: u64, text: String, delay_ms: u64);

    /// Surface a short transient message to the user (status toast).
    fn notify(&mut self, message: String);

    /// Show a longer Markdown report in a scrollable window (used by compare).
    fn show_report(&mut self, title: String, markdown: String);

    /// Is the Pro tier currently unlocked (valid licence or active trial)?
    fn is_pro(&self) -> bool;

    /// Open the purchase page in the browser (upsell).
    fn open_buy(&self);

    // ── Surface added for Fase 4 Pro features ────────────────────────────

    /// Every known session (across providers), newest first.
    fn sessions(&self) -> Vec<SessionLite>;

    /// Full conversation of a session as role/text turns, if available.
    fn transcript(&self, provider: &str, id: &str) -> Option<Vec<Turn>>;

    /// The currently open terminal tabs (for workspace-profile snapshots).
    fn current_tabs(&self) -> Vec<TabSnapshot>;

    /// `~/.config/aterm` — where Pro state files live.
    fn config_dir(&self) -> PathBuf;

    /// Write `content` to `path`, creating parent dirs. Returns an error string.
    fn write_file(&self, path: &Path, content: &str) -> Result<(), String>;

    /// Open a file or URL with the system handler (`xdg-open`/`open`).
    fn open_path(&self, path: &str);
}

/// The gated Pro features. Implemented by `aterm-pro` (private) for the
/// official build, or by the Community stub in the core.
pub trait ProModule {
    /// Open the "parallel compare with worktrees" dialog.
    fn open_parallel(&mut self, host: &mut dyn ProHost);

    /// Run "compare worktrees" immediately and show the report.
    fn run_compare(&mut self, host: &mut dyn ProHost);

    /// Open the "clean up worktrees" dialog.
    fn open_cleanup(&mut self, host: &mut dyn ProHost);

    /// Open the Pro features hub (workspace profiles, dashboard, export HTML,
    /// port, memory graph, MCP config).
    fn open_features(&mut self, host: &mut dyn ProHost);

    /// Draw any open Pro dialogs/windows for this frame and run confirmed
    /// actions. Called once per frame from the app's `update`.
    fn ui(&mut self, ctx: &egui::Context, host: &mut dyn ProHost);

    /// Human label for the edition (e.g. "Pro" / "Community"), for the chrome.
    fn edition(&self) -> &'static str;
}
