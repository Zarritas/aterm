//! The application shell: a session panel on the left, a tab bar of live
//! terminals on top, and the active terminal grid filling the centre. Owns
//! input routing (keys/scroll/selection), font zoom and the clipboard.

use eframe::egui;

use crate::sessions::{PanelAction, SessionPanel};
use crate::term::input::{key_to_bytes, mouse_report};
use crate::term::render::{self, pixel_to_point, CellMetrics};
use crate::term::{TermInstance, TermSize};

/// Install system fonts as fallbacks so symbol glyphs (✕ ⤓ ⟳ ▶ …) render.
/// egui's built-in fonts cover little beyond Latin + a tiny emoji subset, so
/// without this the action-button icons show as missing/blank.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // DejaVu Sans → broad Latin/arrows/geometric; Noto Symbols2 → dingbats,
    // technical and misc-symbol blocks (✕ ✎ ⤓ ⟳ ⎇ …).
    let candidates = [
        (
            "sys-dejavu",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ),
        (
            "sys-noto-symbols2",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        ),
        (
            "sys-noto",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        ),
    ];
    let mut loaded = Vec::new();
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_owned(), egui::FontData::from_owned(bytes));
            loaded.push(name.to_owned());
        }
    }
    if loaded.is_empty() {
        return;
    }
    // The UI (proportional) family: make Noto Sans the primary when available
    // (more legible than egui's default Ubuntu-Light), with the rest as
    // fallbacks. Monospace is left untouched so the terminal grid stays aligned.
    let list = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    // Prepend Noto Sans (legible body face) ahead of the built-in primary.
    if loaded.iter().any(|n| n == "sys-noto") {
        list.insert(0, "sys-noto".to_string());
    }
    for name in &loaded {
        if !list.contains(name) {
            list.push(name.clone());
        }
    }
    ctx.set_fonts(fonts);
}

/// Tab accent swatches (Catppuccin), offered in the rename/colour dialog.
const TAB_SWATCHES: [(&str, egui::Color32); 6] = [
    ("Lavanda", egui::Color32::from_rgb(0xb4, 0xbe, 0xfe)),
    ("Verde", egui::Color32::from_rgb(0xa6, 0xe3, 0xa1)),
    ("Amarillo", egui::Color32::from_rgb(0xf9, 0xe2, 0xaf)),
    ("Melocotón", egui::Color32::from_rgb(0xfa, 0xb3, 0x87)),
    ("Rojo", egui::Color32::from_rgb(0xf3, 0x8b, 0xa8)),
    ("Malva", egui::Color32::from_rgb(0xcb, 0xa6, 0xf7)),
];

// Per-tab font zoom clamps (the default size comes from settings).
const MIN_FONT: f32 = 7.0;
const MAX_FONT: f32 = 40.0;

/// One open terminal tab: its PTY-backed model plus per-tab view state.
struct Tab {
    /// Stable id (tabs are addressed by id, not Vec position, so closing a tab
    /// never reshuffles the split layout / focus).
    id: u64,
    term: TermInstance,
    font_size: f32,
    /// True while a mouse drag-selection is in progress.
    selecting: bool,
    /// `provider:id` for resumed sessions; `None` for plain shells. Used to
    /// avoid resuming the same session into two tabs.
    key: Option<String>,
    /// User-set tab name (overrides the child's title) and accent colour.
    name: Option<String>,
    color: Option<egui::Color32>,
    /// When true, this terminal is shown in its own OS window, not the grid.
    detached: bool,
    /// How this tab was spawned, so a closed tab can be reopened (Ctrl+Shift+T).
    argv: Vec<String>,
    cwd: Option<std::path::PathBuf>,
}

/// In-flight tab rename/recolour dialog.
struct TabEdit {
    id: u64,
    name: String,
}

/// A Pro/licence action requested from the chrome, deferred until after the
/// panels so the handling code can borrow `self` (and the Pro module) freely.
enum ProAction {
    Parallel,
    Compare,
    Cleanup,
    License,
}

/// Settings dialog categories (left-nav).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SettingsCat {
    #[default]
    Appearance,
    Terminal,
    Panel,
}

pub struct AtermApp {
    panel: SessionPanel,
    tabs: Vec<Tab>,
    next_id: u64,
    /// Tab ids tiled in the central area (1 = single view, N = split grid).
    visible: Vec<u64>,
    /// Tab id receiving keyboard input.
    focused: u64,
    clipboard: Option<arboard::Clipboard>,
    /// Set after opening/focusing a tab so it grabs keyboard focus next frame.
    focus_pending: bool,
    /// Scrollback search bar state (toggled with Ctrl+Shift+F).
    search_open: bool,
    search_query: String,
    /// Buffer line of the last match, so "previous" continues further up.
    search_last: Option<i32>,
    /// Tab id currently being dragged to reorder, if any.
    dragging: Option<u64>,
    /// Open tab rename/colour dialog.
    tab_edit: Option<TabEdit>,
    /// Whether the left session panel is shown.
    panel_open: bool,
    /// Whether the settings window is open.
    settings_open: bool,
    /// Active category in the settings dialog.
    settings_cat: SettingsCat,
    /// Tab id pending a close confirmation (process still running).
    close_confirm: Option<u64>,
    /// Spawn specs of recently closed tabs, for reopen (Ctrl+Shift+T).
    closed_stack: Vec<(Vec<String>, Option<std::path::PathBuf>, Option<String>)>,
    /// Showing the "quit with running processes?" confirmation.
    quit_confirm: bool,
    /// Tab id whose right-click context menu is currently open (so it survives
    /// a Shift release while still showing inside a mouse-reporting TUI).
    ctx_menu_open: Option<u64>,
    /// URL under the cursor when the context menu was opened (link actions).
    menu_link: Option<String>,
    /// Relative column / row sizes of the split grid (draggable dividers).
    /// Reset to uniform when the pane count changes the grid dimensions.
    col_fracs: Vec<f32>,
    row_fracs: Vec<f32>,
    /// Tabs loaded from the last session, opened on the first frame (needs ctx).
    restore_pending: Vec<crate::persist::TabSpec>,
    /// Last session JSON written to disk, to skip redundant writes.
    last_saved: String,
    /// When the session was last persisted (writes are throttled).
    last_save_at: std::time::Instant,
    /// Pro features module (real with `--features pro`, Community stub else).
    pro: Box<dyn aterm_pro_api::ProModule>,
    /// Deferred PTY writes: `(tab id, bytes, fire-at)`. Used to inject a prompt
    /// into a freshly-spawned agent after it has had time to start.
    deferred_writes: Vec<(u64, Vec<u8>, std::time::Instant)>,
    /// Transient status toast: `(message, shown-at)`.
    toast: Option<(String, std::time::Instant)>,
    /// Open Markdown report window: `(title, body)` (e.g. worktree compare).
    report: Option<(String, String)>,
    /// Whether the licence/upsell window is open.
    license_open: bool,
    /// Draft licence key in the licence window.
    license_key_input: String,
    /// Result message of the last activation attempt.
    license_msg: Option<String>,
    /// The current frame's egui context, stashed so `ProHost::open_agent` (which
    /// has no `ctx` parameter) can spawn terminals. Set at the top of `update`.
    egui_ctx: egui::Context,
}

impl Default for AtermApp {
    fn default() -> Self {
        Self {
            panel: SessionPanel::default(),
            tabs: Vec::new(),
            next_id: 0,
            visible: Vec::new(),
            focused: 0,
            clipboard: arboard::Clipboard::new().ok(),
            focus_pending: false,
            search_open: false,
            search_query: String::new(),
            search_last: None,
            dragging: None,
            tab_edit: None,
            panel_open: true,
            settings_open: false,
            settings_cat: SettingsCat::default(),
            close_confirm: None,
            closed_stack: Vec::new(),
            quit_confirm: false,
            ctx_menu_open: None,
            menu_link: None,
            col_fracs: Vec::new(),
            row_fracs: Vec::new(),
            restore_pending: crate::persist::load(),
            last_saved: String::new(),
            last_save_at: std::time::Instant::now(),
            pro: crate::pro::module(),
            deferred_writes: Vec::new(),
            toast: None,
            report: None,
            license_open: false,
            license_key_input: String::new(),
            license_msg: None,
            egui_ctx: egui::Context::default(),
        }
    }
}

impl aterm_pro_api::ProHost for AtermApp {
    fn providers(&self) -> Vec<aterm_pro_api::ProviderLite> {
        agent_sessions::all_providers()
            .iter()
            .map(|p| aterm_pro_api::ProviderLite {
                id: p.id().to_string(),
                display_name: p.display_name().to_string(),
                available: p.detect(),
                new_session_argv: p.new_session_argv(),
            })
            .collect()
    }

    fn repo_root(&self) -> Option<std::path::PathBuf> {
        let cwd = self
            .tabs
            .iter()
            .find(|t| t.id == self.focused)
            .and_then(|t| t.term.cwd().or_else(|| t.cwd.clone()))?;
        let top = self
            .exec_git(&["rev-parse", "--show-toplevel"], &cwd)
            .ok()?;
        let top = top.trim();
        (!top.is_empty()).then(|| std::path::PathBuf::from(top))
    }

    fn exec_git(&self, args: &[&str], cwd: &std::path::Path) -> Result<String, String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn open_agent(&mut self, argv: Vec<String>, cwd: std::path::PathBuf) -> Option<u64> {
        let ctx = self.egui_ctx.clone();
        self.open_tab(&ctx, argv, Some(cwd), None)
    }

    fn inject_prompt(&mut self, tab_id: u64, text: String, delay_ms: u64) {
        let at = std::time::Instant::now() + std::time::Duration::from_millis(delay_ms);
        self.deferred_writes.push((tab_id, text.into_bytes(), at));
    }

    fn notify(&mut self, message: String) {
        self.toast = Some((message, std::time::Instant::now()));
    }

    fn show_report(&mut self, title: String, markdown: String) {
        self.report = Some((title, markdown));
    }

    fn is_pro(&self) -> bool {
        crate::license::is_pro()
    }

    fn open_buy(&self) {
        crate::license::open_buy();
    }
}

impl AtermApp {
    /// Deliver any deferred PTY writes (prompt injection) whose timer elapsed,
    /// and schedule a repaint for the soonest pending one.
    fn flush_deferred_writes(&mut self, ctx: &egui::Context) {
        if self.deferred_writes.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let mut soonest: Option<std::time::Duration> = None;
        let mut pending = std::mem::take(&mut self.deferred_writes);
        pending.retain(|(tab_id, bytes, at)| {
            if now >= *at {
                if let Some(t) = self.tabs.iter().find(|t| t.id == *tab_id) {
                    t.term.write(bytes);
                }
                false
            } else {
                let d = *at - now;
                soonest = Some(soonest.map_or(d, |s| s.min(d)));
                true
            }
        });
        self.deferred_writes = pending;
        if let Some(d) = soonest {
            ctx.request_repaint_after(d);
        }
    }

    /// The licence status / activation / upsell window.
    fn license_window(&mut self, ctx: &egui::Context) {
        if !self.license_open {
            return;
        }
        let mut open = true;
        let mut activate = false;
        egui::Window::new("Aterm Pro — licencia")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                let status_line = match crate::license::status() {
                    crate::license::Status::Licensed => "✔ Licencia Pro activa".to_string(),
                    crate::license::Status::Trial { days_left } => {
                        format!("⏳ Prueba: quedan {days_left} días")
                    }
                    crate::license::Status::Expired => {
                        "✖ Prueba terminada — sin licencia".to_string()
                    }
                };
                ui.label(status_line);
                ui.separator();
                ui.label("¿Tienes una clave de licencia? Actívala:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.license_key_input)
                        .hint_text("XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX")
                        .desired_width(320.0),
                );
                if ui.button("Activar").clicked() {
                    activate = true;
                }
                if let Some(msg) = &self.license_msg {
                    ui.label(msg);
                }
                ui.separator();
                ui.label("¿Aún no la tienes? Compra Aterm Pro:");
                ui.horizontal(|ui| {
                    if ui.button("Plan anual (mejor precio)").clicked() {
                        crate::license::open_url(crate::license::BUY_URL_ANNUAL);
                    }
                    if ui.button("Plan mensual").clicked() {
                        crate::license::open_url(crate::license::BUY_URL_MONTHLY);
                    }
                });
            });
        if activate {
            let key = self.license_key_input.clone();
            self.license_msg = Some(match crate::license::activate(&key) {
                Ok(()) => "✔ Licencia activada. ¡Gracias!".to_string(),
                Err(e) => format!("✖ {e}"),
            });
        }
        self.license_open = open;
    }

    /// A scrollable Markdown-ish report window (worktree compare output).
    fn report_window(&mut self, ctx: &egui::Context) {
        let Some((title, body)) = self.report.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new(title)
            .collapsible(true)
            .resizable(true)
            .default_size([560.0, 420.0])
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&body).monospace()).wrap());
                });
            });
        if !open {
            self.report = None;
        }
    }

    /// A transient status toast pinned to the bottom of the window (~6 s).
    fn toast_overlay(&mut self, ctx: &egui::Context) {
        let Some((msg, at)) = self.toast.clone() else {
            return;
        };
        if at.elapsed().as_secs_f32() > 6.0 {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::new("aterm-toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(msg);
                });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    /// Open (or focus, for a resumed session) a terminal tab. Returns the id of
    /// the tab now in front, or `None` if the spawn failed.
    fn open_tab(
        &mut self,
        ctx: &egui::Context,
        argv: Vec<String>,
        cwd: Option<std::path::PathBuf>,
        key: Option<String>,
    ) -> Option<u64> {
        // A resume whose session is already open just focuses that tab — never
        // resume the same transcript into two live agents.
        if let Some(k) = &key {
            // Only dedupe against a still-live tab; if the previous resume has
            // exited, a re-resume should spawn a fresh agent.
            if let Some(t) = self
                .tabs
                .iter()
                .find(|t| t.key.as_deref() == Some(k) && t.term.exit_code().is_none())
            {
                let id = t.id;
                self.focus_tab(id);
                return Some(id);
            }
        }

        let term_font = crate::settings::get().term_font;
        let metrics = CellMetrics::measure(ctx, term_font);
        let size = TermSize {
            columns: 80,
            lines: 24,
            cell_width: metrics.width,
            cell_height: metrics.height,
        };
        match TermInstance::spawn(argv.clone(), cwd.clone(), size, ctx.clone()) {
            Ok(term) => {
                let id = self.next_id;
                self.next_id += 1;
                self.tabs.push(Tab {
                    id,
                    term,
                    font_size: term_font,
                    selecting: false,
                    key,
                    name: None,
                    color: None,
                    detached: false,
                    argv,
                    cwd,
                });
                // A fresh terminal takes over the view as a single pane.
                self.visible = vec![id];
                self.focused = id;
                self.focus_pending = true;
                Some(id)
            }
            Err(e) => {
                eprintln!("aterm: failed to spawn terminal: {e}");
                None
            }
        }
    }

    /// Show `id` as the sole pane and give it focus.
    fn focus_tab(&mut self, id: u64) {
        self.visible = vec![id];
        self.focused = id;
        self.focus_pending = true;
    }

    /// Move focus to the next (`delta`=1) or previous (`delta`=-1) tab in bar
    /// order, wrapping around, and show it as the sole pane.
    fn cycle_tab(&mut self, delta: isize) {
        let n = self.tabs.len();
        if n == 0 {
            return;
        }
        let cur = self
            .tabs
            .iter()
            .position(|t| t.id == self.focused)
            .unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        let id = self.tabs[next].id;
        self.focus_tab(id);
    }

    /// Snapshot the open tabs as restartable specs (shells carry their live
    /// cwd so they reopen where the user left off; agent sessions keep theirs).
    fn current_specs(&self) -> Vec<crate::persist::TabSpec> {
        self.tabs
            .iter()
            .map(|t| {
                let cwd = if t.key.is_none() {
                    t.term.cwd().or_else(|| t.cwd.clone())
                } else {
                    t.cwd.clone()
                };
                crate::persist::TabSpec {
                    argv: t.argv.clone(),
                    cwd,
                    key: t.key.clone(),
                    name: t.name.clone(),
                }
            })
            .collect()
    }

    /// Persist the current tab set, throttled and only when it changed, so the
    /// next launch restores them. Reading each shell's live cwd is cheap but not
    /// free, hence the ~1.5s gate.
    fn persist_session(&mut self) {
        if self.last_save_at.elapsed().as_millis() < 1500 {
            return;
        }
        self.last_save_at = std::time::Instant::now();
        let specs = self.current_specs();
        let json = serde_json::to_string(&specs).unwrap_or_default();
        if json != self.last_saved {
            self.last_saved = json;
            crate::persist::save(&specs);
        }
    }

    /// Close `id`, but first pop a confirmation if a foreground command is
    /// running (an idle shell / exited child closes immediately).
    fn request_close(&mut self, id: u64) {
        let busy = self
            .tabs
            .iter()
            .any(|t| t.id == id && t.term.has_foreground_process());
        if busy {
            self.close_confirm = Some(id);
        } else {
            self.close_tab(id);
        }
    }

    /// Toggle whether `id` is tiled alongside the current panes (a split).
    fn toggle_split(&mut self, id: u64) {
        if let Some(pos) = self.visible.iter().position(|v| *v == id) {
            if self.visible.len() > 1 {
                self.visible.remove(pos);
                if self.focused == id {
                    self.focused = self.visible[0];
                }
            }
        } else {
            self.visible.push(id);
            self.focused = id;
            self.focus_pending = true;
        }
    }

    fn close_tab(&mut self, id: u64) {
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // Remember how to respawn it (Ctrl+Shift+T). For a plain shell, prefer
        // the live cwd (following any `cd`) so it reopens where the user left
        // off; an agent session (`key`) keeps its original launch dir.
        let t = &self.tabs[i];
        let cwd = if t.key.is_none() {
            t.term.cwd().or_else(|| t.cwd.clone())
        } else {
            t.cwd.clone()
        };
        self.closed_stack.push((t.argv.clone(), cwd, t.key.clone()));
        if self.closed_stack.len() > 20 {
            self.closed_stack.remove(0);
        }
        self.tabs.remove(i); // `Drop` shuts the PTY down.
        self.visible.retain(|v| *v != id);
        if self.visible.is_empty() {
            if let Some(last) = self.tabs.last() {
                self.visible = vec![last.id];
            }
        }
        if self.focused == id {
            self.focused = self.visible.first().copied().unwrap_or(0);
        }
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Toggle a tab between the in-window grid and its own OS window.
    fn toggle_detach(&mut self, id: u64) {
        let Some(i) = self.tab_index(id) else { return };
        let now = !self.tabs[i].detached;
        self.tabs[i].detached = now;
        if now {
            // Leaving the grid: drop from the visible set.
            self.visible.retain(|v| *v != id);
            if self.visible.is_empty() {
                if let Some(t) = self.tabs.iter().find(|t| !t.detached) {
                    self.visible = vec![t.id];
                    self.focused = t.id;
                }
            } else if self.focused == id {
                self.focused = self.visible[0];
            }
        } else {
            // Re-attaching: show it again and focus it.
            self.focus_tab(id);
        }
    }

    /// Move tab `src` to sit before tab `before` (or to the end when `None`).
    fn move_tab(&mut self, src: u64, before: Option<u64>) {
        if before == Some(src) {
            return; // dropped on itself
        }
        let Some(from) = self.tab_index(src) else {
            return;
        };
        let tab = self.tabs.remove(from);
        let to = before
            .and_then(|b| self.tab_index(b))
            .unwrap_or(self.tabs.len());
        self.tabs.insert(to, tab);
    }

    fn copy(&mut self, text: String) {
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
    }

    fn paste_text(&mut self) -> Option<String> {
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
    }
}

impl eframe::App for AtermApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut pending_open: Option<(Vec<String>, Option<std::path::PathBuf>, Option<String>)> =
            None;

        // Stash this frame's context so `ProHost::open_agent` (no ctx arg) can
        // spawn terminals, then fire any prompt injections whose timer elapsed.
        self.egui_ctx = ctx.clone();
        self.flush_deferred_writes(ctx);

        // Pro/licence actions requested from the chrome this frame, handled
        // after the panels (the panel closures borrow `self`).
        let mut pro_action: Option<ProAction> = None;

        // First frame: re-open the tabs from the previous session (needs `ctx`,
        // hence here and not in `default`). Names are reattached afterwards.
        if !self.restore_pending.is_empty() {
            let specs = std::mem::take(&mut self.restore_pending);
            for spec in specs {
                self.open_tab(ctx, spec.argv, spec.cwd, spec.key);
                if spec.name.is_some() {
                    if let Some(t) = self.tabs.last_mut() {
                        t.name = spec.name;
                    }
                }
            }
        }

        // Reabrir la última pestaña cerrada (Ctrl+Shift+T) a nivel global: el
        // handler por-pane solo corre con un terminal enfocado, así que tras
        // cerrar la última pestaña no habría a quién entregar el atajo.
        if ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::T)
        }) {
            if let Some((argv, cwd, key)) = self.closed_stack.pop() {
                self.open_tab(ctx, argv, cwd, key);
            }
        }

        // Tab navigation (global, so it works regardless of which pane is
        // focused): Ctrl+Tab / Ctrl+Shift+Tab cycle, Alt+1..9 jump to the Nth,
        // Ctrl+Shift+W closes the focused one.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Tab)) {
            self.cycle_tab(1);
        }
        if ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::Tab,
            )
        }) {
            self.cycle_tab(-1);
        }
        const NUM_KEYS: [egui::Key; 9] = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        for (n, key) in NUM_KEYS.into_iter().enumerate() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, key)) {
                if let Some(t) = self.tabs.get(n) {
                    let id = t.id;
                    self.focus_tab(id);
                }
            }
        }
        if ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::W)
        }) {
            self.request_close(self.focused);
        }

        // Auto-close tabs whose child has exited (`exit` / Ctrl+D) — unless the
        // user prefers to keep the `[exited N]` placeholder.
        if crate::settings::get().auto_close_on_exit {
            let exited: Vec<u64> = self
                .tabs
                .iter()
                .filter(|t| t.term.exit_code().is_some())
                .map(|t| t.id)
                .collect();
            for id in exited {
                self.close_tab(id);
            }
        }

        // Intercept window close: confirm if any terminal has a running command.
        if ctx.input(|i| i.viewport().close_requested())
            && self.tabs.iter().any(|t| t.term.has_foreground_process())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_confirm = true;
        }

        // Header first → it spans the full width on top; the session panel sits
        // *below* it on the left (so the sidebar never overlaps the header).
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.panel_open, "☰")
                    .on_hover_text("Mostrar/ocultar el panel de sesiones")
                    .clicked()
                {
                    self.panel_open = !self.panel_open;
                }
                ui.separator();
                if ui.button(">_").on_hover_text("Nueva shell").clicked() {
                    pending_open = Some((shell_argv(), shell_dir(), None));
                }
                ui.separator();
                // Pro: parallel worktree compare. The ⚡ opens the launch
                // dialog; the ▽ menu holds compare/cleanup.
                if ui
                    .button("⚡")
                    .on_hover_text("Comparativa paralela (Pro): lanza N agentes en worktrees")
                    .clicked()
                {
                    pro_action = Some(ProAction::Parallel);
                }
                ui.menu_button("▽", |ui| {
                    if ui.button("Comparar worktrees").clicked() {
                        pro_action = Some(ProAction::Compare);
                        ui.close_menu();
                    }
                    if ui.button("Limpiar worktrees…").clicked() {
                        pro_action = Some(ProAction::Cleanup);
                        ui.close_menu();
                    }
                });
                ui.separator();
                let mut to_close = None;
                let mut to_focus = None;
                let mut to_split = None;
                let mut to_edit_tab = None;
                let mut to_detach = None;
                // Each tab label's horizontal extent, to resolve a drop by x.
                let mut rects: Vec<(u64, egui::Rect)> = Vec::new();
                for tab in &self.tabs {
                    let id = tab.id;
                    let shown = self.visible.contains(&id);
                    // Bell on an unfocused tab → attention marker + colour.
                    let bell = id != self.focused && tab.term.bell_rung();
                    // User name overrides the child title.
                    let base = truncate(tab.name.as_deref().unwrap_or(&tab.term.title()), 22);
                    let label = if bell { format!("• {base}") } else { base };
                    let mut text = egui::RichText::new(label);
                    // Bell > custom colour > focused accent.
                    if bell {
                        text = text.color(crate::theme::pal().peach);
                    } else if let Some(c) = tab.color {
                        text = text.color(c);
                    } else if id == self.focused {
                        text = text.color(egui::Color32::from_rgb(0xb4, 0xbe, 0xfe));
                    }
                    if self.dragging == Some(id) {
                        text = text.italics();
                    }
                    // Make the label sense dragging as well as clicking.
                    let resp = ui
                        .selectable_label(shown, text)
                        .interact(egui::Sense::click_and_drag())
                        .on_hover_text(
                            "Click: enfocar · arrastra: reordenar · clic dcho: renombrar",
                        );
                    rects.push((id, resp.rect));
                    if resp.clicked() {
                        to_focus = Some(id);
                    }
                    if resp.secondary_clicked() {
                        to_edit_tab = Some(id);
                    }
                    if resp.drag_started() {
                        self.dragging = Some(id);
                    }
                    // Split toggle: highlighted while this terminal is on the
                    // grid. Disabled when it's the only visible one (nothing to
                    // split against) — that was the "does nothing" case.
                    let can_split = !shown || self.visible.len() > 1;
                    let split_resp = ui
                        .add_enabled(can_split, egui::SelectableLabel::new(shown, "⊞"))
                        .on_hover_text(if shown {
                            "Quitar del split"
                        } else {
                            "Ver en split junto a las demás"
                        });
                    if split_resp.clicked() {
                        to_split = Some(id);
                    }
                    let detached = self.tabs.iter().any(|t| t.id == id && t.detached);
                    if ui
                        .selectable_label(detached, "⇱")
                        .on_hover_text(if detached {
                            "Traer de vuelta a la ventana"
                        } else {
                            "Abrir en ventana nueva"
                        })
                        .clicked()
                    {
                        to_detach = Some(id);
                    }
                    if ui.small_button("×").on_hover_text("Cerrar").clicked() {
                        to_close = Some(id);
                    }
                    ui.separator();
                }
                if let Some(id) = to_detach {
                    self.toggle_detach(id);
                }
                if let Some(id) = to_edit_tab {
                    if let Some(t) = self.tabs.iter().find(|t| t.id == id) {
                        self.tab_edit = Some(TabEdit {
                            id,
                            name: t.name.clone().unwrap_or_default(),
                        });
                    }
                }

                // Resolve a drag once the button is no longer held (robust:
                // doesn't depend on catching the exact release frame, so the
                // drag state never gets stuck and blocks later clicks).
                if let Some(src) = self.dragging {
                    let held = ui.input(|i| i.pointer.any_down());
                    if held {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    } else {
                        let px = ui
                            .input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))
                            .map(|p| p.x)
                            .unwrap_or(f32::INFINITY);
                        let on_self = rects
                            .iter()
                            .find(|(id, _)| *id == src)
                            .is_some_and(|(_, r)| px >= r.left() && px <= r.right());
                        if on_self {
                            // Released over itself → it was really a click.
                            to_focus = Some(src);
                        } else {
                            let before = rects
                                .iter()
                                .find(|(_, r)| px < r.center().x)
                                .map(|(id, _)| *id);
                            self.move_tab(src, before);
                        }
                        self.dragging = None;
                    }
                }

                if let Some(id) = to_focus {
                    self.focus_tab(id);
                }
                if let Some(id) = to_split {
                    self.toggle_split(id);
                }
                if let Some(id) = to_close {
                    self.request_close(id);
                }

                // Settings cog + licence badge, pushed to the right edge.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙").on_hover_text("Ajustes").clicked() {
                        self.settings_open = true;
                    }
                    ui.separator();
                    if ui
                        .button(crate::license::badge())
                        .on_hover_text("Estado de la licencia / activar Pro")
                        .clicked()
                    {
                        pro_action = Some(ProAction::License);
                    }
                });
            });
        });

        egui::SidePanel::left("sessions")
            .resizable(true)
            .default_width(380.0)
            .show_animated(ctx, self.panel_open, |ui| {
                if let Some(PanelAction::Open { argv, cwd, key }) = self.panel.ui(ui) {
                    pending_open = Some((argv, cwd, key));
                }
            });

        if let Some((argv, cwd, key)) = pending_open {
            self.open_tab(ctx, argv, cwd, key);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Only in-grid (non-detached, existing) tabs are visible here.
            let grid_ok: std::collections::HashSet<u64> = self
                .tabs
                .iter()
                .filter(|t| !t.detached)
                .map(|t| t.id)
                .collect();
            self.visible.retain(|id| grid_ok.contains(id));
            if self.visible.is_empty() {
                if let Some(t) = self.tabs.iter().find(|t| !t.detached) {
                    self.visible = vec![t.id];
                    self.focused = t.id;
                }
            }
            if self.visible.is_empty() {
                ui.heading("Terminal");
                ui.separator();
                ui.label(
                    "Pulsa «>_» para abrir una shell, o «▶» en una sesión \
                     del panel para reanudarla.",
                );
                return;
            }
            if self.tab_index(self.focused).is_none() {
                self.focused = self.visible[0];
            }

            // Window title tracks the focused pane's child.
            if let Some(i) = self.tab_index(self.focused) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                    "aterm — {}",
                    self.tabs[i].term.title()
                )));
            }

            if self.search_open {
                self.search_bar(ui);
            }
            self.render_panes(ui);
        });

        self.tab_edit_window(ctx);
        self.settings_window(ctx);
        self.close_confirm_window(ctx);
        self.quit_confirm_window(ctx);
        self.render_detached(ctx);

        // Pro module: dispatch the chrome action, then let it draw its dialogs.
        // `self.pro` is moved out for the calls so the module can take `&mut
        // self` as its `ProHost` without aliasing.
        let mut pro = std::mem::replace(&mut self.pro, crate::pro::noop_module());
        match pro_action {
            Some(ProAction::Parallel) => pro.open_parallel(self),
            Some(ProAction::Compare) => pro.run_compare(self),
            Some(ProAction::Cleanup) => pro.open_cleanup(self),
            Some(ProAction::License) => self.license_open = true,
            None => {}
        }
        pro.ui(ctx, self);
        self.pro = pro;

        self.license_window(ctx);
        self.report_window(ctx);
        self.toast_overlay(ctx);

        self.persist_session();
    }
}

impl AtermApp {
    /// Confirm quitting the whole app while terminals have running commands.
    fn quit_confirm_window(&mut self, ctx: &egui::Context) {
        if !self.quit_confirm {
            return;
        }
        let running = self
            .tabs
            .iter()
            .filter(|t| t.term.has_foreground_process())
            .count();
        let mut open = true;
        let mut decision: Option<bool> = None;
        egui::Window::new("Salir de aterm")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Hay {running} terminal(es) con un proceso en ejecución.\n¿Salir de todos modos?"
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(egui::RichText::new("Salir").color(crate::theme::pal().red))
                        .clicked()
                    {
                        decision = Some(true);
                    }
                    if ui.button("Cancelar").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.quit_confirm = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(false) => self.quit_confirm = false,
            None if !open => self.quit_confirm = false,
            None => {}
        }
    }

    /// Confirm before closing a tab whose child is still running.
    fn close_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.close_confirm else {
            return;
        };
        // If the foreground command finished meanwhile, just close it.
        let busy = self
            .tabs
            .iter()
            .any(|t| t.id == id && t.term.has_foreground_process());
        if !busy {
            self.close_tab(id);
            self.close_confirm = None;
            return;
        }
        let name = self
            .tabs
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.name.clone().unwrap_or_else(|| t.term.title()))
            .unwrap_or_default();
        let mut open = true;
        let mut decision: Option<bool> = None; // Some(true)=close, Some(false)=cancel
        egui::Window::new("Cerrar terminal")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!(
                    "«{}» tiene un proceso en primer plano.\n¿Cerrar de todos modos?",
                    truncate(&name, 40)
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(egui::RichText::new("Cerrar").color(crate::theme::pal().red))
                        .clicked()
                    {
                        decision = Some(true);
                    }
                    if ui.button("Cancelar").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.close_tab(id);
                self.close_confirm = None;
            }
            Some(false) => self.close_confirm = None,
            None if !open => self.close_confirm = None, // window's × = cancel
            None => {}
        }
    }

    /// Render each detached terminal in its own OS window (immediate viewport,
    /// so it can borrow the live `TermInstance`). Closing the window re-attaches.
    fn render_detached(&mut self, ctx: &egui::Context) {
        let detached: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| t.detached)
            .map(|t| t.id)
            .collect();
        for id in detached {
            let title = self
                .tabs
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.name.clone().unwrap_or_else(|| t.term.title()))
                .unwrap_or_default();
            let mut close = false;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("detached-term", id)),
                egui::ViewportBuilder::default()
                    .with_title(format!("aterm — {title}"))
                    .with_inner_size([820.0, 520.0]),
                |vctx, _class| {
                    if vctx.input(|i| i.viewport().close_requested()) {
                        close = true;
                    }
                    egui::CentralPanel::default().show(vctx, |ui| {
                        self.render_pane(ui, id);
                    });
                },
            );
            if close {
                self.toggle_detach(id); // re-attach into the grid
            }
        }
    }

    /// Settings popup, opened from the header cog.
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let initial = crate::settings::get();
        let mut s = initial.clone();
        let mut reapply_theme = false;

        let accent = crate::theme::pal().lavender;
        let cat = &mut self.settings_cat;
        egui::Window::new("Ajustes")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size([560.0, 360.0])
            .show(ctx, |ui| {
                ui.spacing_mut().slider_width = 170.0;
                ui.horizontal_top(|ui| {
                    // Left nav: categories.
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        ui.add_space(4.0);
                        let item = |ui: &mut egui::Ui, c: &mut SettingsCat, this, label: &str| {
                            let r = ui.selectable_label(
                                *c == this,
                                egui::RichText::new(label).size(15.0),
                            );
                            if r.clicked() {
                                *c = this;
                            }
                        };
                        item(ui, cat, SettingsCat::Appearance, "  Apariencia");
                        ui.separator();
                        item(ui, cat, SettingsCat::Terminal, "  Terminal");
                        ui.separator();
                        item(ui, cat, SettingsCat::Panel, "  Panel de sesiones");
                    });
                    ui.separator();
                    // Right content for the active category.
                    ui.vertical(|ui| {
                        ui.set_width(370.0);
                        match *cat {
                            SettingsCat::Appearance => {
                                ui.label(
                                    egui::RichText::new("APARIENCIA")
                                        .color(accent)
                                        .strong()
                                        .size(13.0),
                                );
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    label_w(ui, "Tema");
                                    let current = crate::theme::current_name();
                                    egui::ComboBox::from_id_salt("settings-theme")
                                        .selected_text(&current)
                                        .show_ui(ui, |ui| {
                                            for (name, _) in crate::theme::THEMES {
                                                if ui
                                                    .selectable_label(current == name, name)
                                                    .clicked()
                                                {
                                                    crate::theme::select(ui.ctx(), name);
                                                }
                                            }
                                        });
                                });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    label_w(ui, "Fuente de la interfaz");
                                    if ui
                                        .add(egui::Slider::new(&mut s.ui_font, 11.0..=22.0))
                                        .changed()
                                    {
                                        reapply_theme = true;
                                    }
                                });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    label_w(ui, "Fuente del terminal");
                                    ui.add(egui::Slider::new(&mut s.term_font, 8.0..=28.0));
                                });
                                ui.label(
                                    egui::RichText::new(
                                        "La fuente del terminal aplica a pestañas nuevas.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            SettingsCat::Terminal => {
                                ui.label(
                                    egui::RichText::new("TERMINAL")
                                        .color(accent)
                                        .strong()
                                        .size(13.0),
                                );
                                ui.add_space(8.0);
                                ui.checkbox(
                                    &mut s.auto_close_on_exit,
                                    "Cerrar la pestaña al salir (exit)",
                                );
                                ui.separator();
                                ui.horizontal(|ui| {
                                    label_w(ui, "Shell");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut s.shell_command)
                                            .hint_text("$SHELL")
                                            .desired_width(210.0),
                                    );
                                });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    label_w(ui, "Directorio inicial");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut s.shell_dir)
                                            .hint_text("~ (home)")
                                            .desired_width(210.0),
                                    );
                                });
                            }
                            SettingsCat::Panel => {
                                ui.label(
                                    egui::RichText::new("PANEL DE SESIONES")
                                        .color(accent)
                                        .strong()
                                        .size(13.0),
                                );
                                ui.add_space(8.0);
                                ui.label("Proveedores a escanear");
                                ui.horizontal_wrapped(|ui| {
                                    ui.checkbox(&mut s.scan_claude, "Claude");
                                    ui.checkbox(&mut s.scan_codex, "Codex");
                                    ui.checkbox(&mut s.scan_opencode, "OpenCode");
                                    ui.checkbox(&mut s.scan_gemini, "Gemini");
                                });
                                ui.separator();
                                ui.checkbox(&mut s.fetch_status, "Consultar estado y quota (red)");
                                ui.separator();
                                ui.horizontal(|ui| {
                                    label_w(ui, "Auto-refresco");
                                    ui.add(
                                        egui::Slider::new(&mut s.refresh_secs, 15..=600)
                                            .suffix(" s"),
                                    );
                                });
                            }
                        }
                    });
                });
            });

        // Persist + react only when something actually changed.
        if s != initial {
            let providers_changed = s.scan_claude != initial.scan_claude
                || s.scan_codex != initial.scan_codex
                || s.scan_opencode != initial.scan_opencode
                || s.scan_gemini != initial.scan_gemini
                || s.fetch_status != initial.fetch_status;
            crate::settings::update(|cur| *cur = s);
            if reapply_theme {
                crate::theme::apply(ctx);
            }
            if providers_changed {
                self.panel.request_rescan();
            }
        }
        self.settings_open = open;
    }

    /// Rename / recolour the active tab (opened by right-clicking its label).
    fn tab_edit_window(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.tab_edit.as_mut() else {
            return;
        };
        let id = edit.id;
        let mut open = true;
        let mut apply_name = false;
        let mut set_color: Option<Option<egui::Color32>> = None;
        egui::Window::new("Pestaña")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Nombre:");
                let resp = ui.text_edit_singleline(&mut edit.name);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    apply_name = true;
                }
                if ui.button("Aplicar nombre").clicked() {
                    apply_name = true;
                }
                ui.separator();
                ui.label("Color:");
                ui.horizontal_wrapped(|ui| {
                    for (name, c) in TAB_SWATCHES {
                        if ui
                            .add(
                                egui::Button::new("  ")
                                    .fill(c)
                                    .min_size(egui::vec2(22.0, 18.0)),
                            )
                            .on_hover_text(name)
                            .clicked()
                        {
                            set_color = Some(Some(c));
                        }
                    }
                    if ui.button("Sin color").clicked() {
                        set_color = Some(None);
                    }
                });
            });

        // Apply outside the borrow of `edit`.
        if apply_name {
            let name = self.tab_edit.as_ref().map(|e| e.name.trim().to_string());
            if let (Some(name), Some(t)) = (name, self.tabs.iter_mut().find(|t| t.id == id)) {
                t.name = (!name.is_empty()).then_some(name);
            }
        }
        if let Some(c) = set_color {
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.color = c;
            }
        }
        if !open {
            self.tab_edit = None;
        }
    }

    /// Scrollback search bar for the focused pane (Ctrl+Shift+F toggles it).
    fn search_bar(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.tab_index(self.focused) else {
            return;
        };
        let mut search = false;
        ui.horizontal(|ui| {
            ui.label("Buscar:");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("texto en el scrollback…")
                    .desired_width(240.0),
            );
            if resp.changed() {
                self.search_last = None; // new query → search from the bottom
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter
                || ui
                    .button("▲")
                    .on_hover_text("Anterior coincidencia")
                    .clicked()
            {
                search = true;
            }
            if ui
                .button("×")
                .on_hover_text("Cerrar (Ctrl+Shift+F)")
                .clicked()
            {
                self.search_open = false;
            }
        });
        if search {
            let query = self.search_query.clone();
            match self.tabs[idx].term.search_up(&query, self.search_last) {
                Some(line) => self.search_last = Some(line),
                None => self.search_last = None, // wrap: next search starts over
            }
        }
    }

    /// Tile the visible tabs into a near-square grid of panes, with draggable
    /// dividers between columns and rows to resize them.
    fn render_panes(&mut self, ui: &mut egui::Ui) {
        let ids: Vec<u64> = self.visible.clone();
        let n = ids.len();
        if n == 1 {
            self.render_pane(ui, ids[0]);
            return;
        }
        let cols = (n as f32).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        // Reset to uniform whenever the grid dimensions change (panes added /
        // removed); otherwise keep the user's dragged ratios.
        if self.col_fracs.len() != cols {
            self.col_fracs = vec![1.0 / cols as f32; cols];
        }
        if self.row_fracs.len() != rows {
            self.row_fracs = vec![1.0 / rows as f32; rows];
        }

        let area = ui.available_rect_before_wrap();
        let gap = 6.0;
        let total_w = area.width() - gap * (cols as f32 - 1.0);
        let total_h = area.height() - gap * (rows as f32 - 1.0);

        // Per-track pixel sizes and top-left starts.
        let widths: Vec<f32> = self.col_fracs.iter().map(|f| f * total_w).collect();
        let heights: Vec<f32> = self.row_fracs.iter().map(|f| f * total_h).collect();
        let mut xs = Vec::with_capacity(cols);
        let mut acc = area.min.x;
        for w in &widths {
            xs.push(acc);
            acc += w + gap;
        }
        let mut ys = Vec::with_capacity(rows);
        let mut acc = area.min.y;
        for h in &heights {
            ys.push(acc);
            acc += h + gap;
        }

        for (idx, id) in ids.into_iter().enumerate() {
            let r = idx / cols;
            let c = idx % cols;
            let min = egui::pos2(xs[c], ys[r]);
            let rect = egui::Rect::from_min_size(min, egui::vec2(widths[c], heights[r]));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.set_clip_rect(rect);
                self.render_pane(ui, id);
            });
        }

        self.split_dividers(ui, area, gap, &xs, &widths, &ys, &heights, total_w, total_h);
    }

    /// Draggable column/row dividers for the split grid. Dragging shifts size
    /// between the two adjacent tracks (kept above a small minimum).
    #[allow(clippy::too_many_arguments)]
    fn split_dividers(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        gap: f32,
        xs: &[f32],
        widths: &[f32],
        ys: &[f32],
        heights: &[f32],
        total_w: f32,
        total_h: f32,
    ) {
        const MIN_FRAC: f32 = 0.08;
        let line = crate::theme::pal().surface2;

        // Vertical dividers between columns.
        for c in 0..xs.len().saturating_sub(1) {
            let x = xs[c] + widths[c] + gap / 2.0;
            let rect = egui::Rect::from_min_max(
                egui::pos2(x - gap / 2.0, area.min.y),
                egui::pos2(x + gap / 2.0, area.max.y),
            );
            let resp = ui
                .interact(rect, ui.id().with(("vsplit", c)), egui::Sense::drag())
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if resp.hovered() || resp.dragged() {
                ui.painter()
                    .vline(x, rect.y_range(), egui::Stroke::new(2.0, line));
            }
            if resp.dragged() {
                let df = resp.drag_delta().x / total_w;
                self.shift_frac(true, c, df, MIN_FRAC);
            }
        }

        // Horizontal dividers between rows.
        for r in 0..ys.len().saturating_sub(1) {
            let y = ys[r] + heights[r] + gap / 2.0;
            let rect = egui::Rect::from_min_max(
                egui::pos2(area.min.x, y - gap / 2.0),
                egui::pos2(area.max.x, y + gap / 2.0),
            );
            let resp = ui
                .interact(rect, ui.id().with(("hsplit", r)), egui::Sense::drag())
                .on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if resp.hovered() || resp.dragged() {
                ui.painter()
                    .hline(rect.x_range(), y, egui::Stroke::new(2.0, line));
            }
            if resp.dragged() {
                let df = resp.drag_delta().y / total_h;
                self.shift_frac(false, r, df, MIN_FRAC);
            }
        }
    }

    /// Move `df` of the total from track `i+1` into track `i` (or back), within
    /// the grid's column (`cols=true`) or row fractions, respecting `min`.
    fn shift_frac(&mut self, cols: bool, i: usize, df: f32, min: f32) {
        let fracs = if cols {
            &mut self.col_fracs
        } else {
            &mut self.row_fracs
        };
        if i + 1 >= fracs.len() {
            return;
        }
        // Clamp so neither adjacent track drops below `min`.
        let df = df.clamp(-(fracs[i] - min), fracs[i + 1] - min);
        fracs[i] += df;
        fracs[i + 1] -= df;
    }

    /// Render one terminal pane (resize, draw, focus, input).
    fn render_pane(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(i) = self.tab_index(id) else {
            return;
        };
        let focused = id == self.focused;
        if focused {
            self.tabs[i].term.clear_bell(); // viewing the tab dismisses the bell
        }
        let metrics = CellMetrics::measure(ui.ctx(), self.tabs[i].font_size);
        let (cols, lines) = metrics.grid_size(ui.available_size());
        {
            let term = &mut self.tabs[i].term;
            if term.size.columns != cols || term.size.lines != lines {
                term.resize(TermSize {
                    columns: cols,
                    lines,
                    cell_width: metrics.width,
                    cell_height: metrics.height,
                });
            }
        }

        // Cursor feedback over the grid: hand over a Ctrl-hovered link, text
        // I-beam where local selection is available, default while the child
        // owns the mouse. Also underline the hovered link.
        let rect = ui.available_rect_before_wrap();
        let mut link_span: Option<(usize, usize, usize)> = None;
        if let Some(p) = ui.input(|inp| inp.pointer.hover_pos()) {
            if rect.contains(p) {
                let local = p - rect.min;
                let col = (local.x / metrics.width).floor().max(0.0) as usize;
                let vline = (local.y / metrics.height).floor().max(0.0) as usize;
                let link = self.tabs[i].term.url_span_at(col, vline);
                let ctrl = ui.input(|inp| inp.modifiers.ctrl);
                let shift = ui.input(|inp| inp.modifiers.shift);
                let mouse_report = self.tabs[i].term.modes().mouse_report;
                if link.is_some() && ctrl {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                } else if !mouse_report || shift {
                    // Selection is available here → text I-beam.
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                }
                if let Some((s, e)) = link {
                    link_span = Some((vline, s, e));
                }
            }
        }

        // Highlight every visible occurrence of the search query (the focused
        // pane only, while the search bar is open).
        let matches: Vec<(usize, usize, usize)> =
            if focused && self.search_open && !self.search_query.is_empty() {
                self.tabs[i].term.viewport_matches(&self.search_query)
            } else {
                Vec::new()
            };

        let response = render::draw(
            ui,
            &self.tabs[i].term,
            metrics,
            focused,
            link_span,
            &matches,
        );

        if focused && self.focus_pending {
            response.request_focus();
            self.focus_pending = false;
        }
        if response.clicked() {
            response.request_focus();
            self.focused = id;
        }
        if response.has_focus() {
            self.focused = id;
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        self.handle_mouse(ui, &response, metrics, i);

        // Right-click context menu (Copiar / Pegar / …). Allowed when local
        // selection is available (no mouse-reporting, or Shift held) — or while
        // it's already open, so releasing Shift to pick an item inside a TUI
        // doesn't dismiss it.
        let mouse_report = self.tabs[i].term.modes().mouse_report;
        let shift = ui.input(|inp| inp.modifiers.shift);
        if !mouse_report || shift || self.ctx_menu_open == Some(id) {
            if response.secondary_clicked() {
                let link = response.interact_pointer_pos().and_then(|pos| {
                    let local = pos - rect.min;
                    let col = (local.x / metrics.width).floor().max(0.0) as usize;
                    let vline = (local.y / metrics.height).floor().max(0.0) as usize;
                    self.tabs[i].term.url_at(col, vline)
                });
                self.menu_link = link;
            }
            let menu = response.context_menu(|ui| self.term_context_menu(ui, i));
            if menu.is_some() {
                self.ctx_menu_open = Some(id);
            } else if self.ctx_menu_open == Some(id) {
                self.ctx_menu_open = None;
            }
        }

        let alive = self.tabs[i].term.exit_code().is_none();
        if response.has_focus() && alive {
            self.handle_keyboard(ui, i);
        }
    }

    /// Contents of a terminal pane's right-click menu.
    fn term_context_menu(&mut self, ui: &mut egui::Ui, idx: usize) {
        let has_sel = self.tabs[idx]
            .term
            .selection_text()
            .is_some_and(|t| !t.is_empty());
        let bracketed = self.tabs[idx].term.modes().bracketed_paste;

        if ui
            .add_enabled(has_sel, egui::Button::new("Copiar"))
            .clicked()
        {
            if let Some(t) = self.tabs[idx].term.selection_text() {
                self.copy(t);
            }
            ui.close_menu();
        }
        if ui.button("Pegar").clicked() {
            if let Some(t) = self.paste_text() {
                self.paste_into(idx, &t, bracketed);
            }
            ui.close_menu();
        }
        if ui.button("Seleccionar todo").clicked() {
            self.tabs[idx].term.select_all();
            ui.close_menu();
        }
        if ui
            .add_enabled(has_sel, egui::Button::new("Limpiar selección"))
            .clicked()
        {
            self.tabs[idx].term.clear_selection();
            ui.close_menu();
        }

        if let Some(url) = self.menu_link.clone() {
            ui.separator();
            if ui.button("Abrir enlace").clicked() {
                open_url(&url);
                ui.close_menu();
            }
            if ui.button("Copiar enlace").clicked() {
                self.copy(url);
                ui.close_menu();
            }
        }

        ui.separator();
        if ui.button("Buscar…  (Ctrl+Shift+F)").clicked() {
            self.search_open = true;
            self.search_last = None;
            ui.close_menu();
        }
    }

    /// When the child requests mouse reporting, forward clicks/drag/wheel to it.
    /// Otherwise: drag → local selection (copied on release), click clears it,
    /// wheel scrolls the scrollback (or sends arrows on the alternate screen).
    fn handle_mouse(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        metrics: CellMetrics,
        idx: usize,
    ) {
        let origin = response.rect.min;
        let modes = self.tabs[idx].term.modes();
        let (cols, lines, offset) = {
            let term = &self.tabs[idx].term;
            (term.size.columns, term.size.lines, term.display_offset())
        };
        let cell_at = |pos: egui::Pos2| -> (usize, usize) {
            let local = pos - origin;
            let col =
                ((local.x / metrics.width).floor().max(0.0) as usize).min(cols.saturating_sub(1));
            let line =
                ((local.y / metrics.height).floor().max(0.0) as usize).min(lines.saturating_sub(1));
            (col, line)
        };

        // Ctrl+click opens a URL under the cursor (works even inside TUIs).
        if response.clicked() && ui.input(|i| i.modifiers.ctrl) {
            if let Some(pos) = response.interact_pointer_pos() {
                let (col, line) = cell_at(pos);
                if let Some(url) = self.tabs[idx].term.url_at(col, line) {
                    open_url(&url);
                    return;
                }
            }
        }

        // When the child captures the mouse (TUIs like Claude), forward events
        // to it — unless Shift is held, the standard override to select/copy
        // locally instead.
        let shift = ui.input(|i| i.modifiers.shift);
        if modes.mouse_report && !shift {
            self.report_mouse(ui, response, modes, &cell_at, idx);
            return;
        }

        let mut copy_text: Option<String> = None;
        {
            let tab = &mut self.tabs[idx];
            if response.triple_clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (point, _) = pixel_to_point(pos - origin, metrics, offset, cols, lines);
                    tab.term.select_line(point);
                    copy_text = tab.term.selection_text();
                }
            } else if response.double_clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (point, _) = pixel_to_point(pos - origin, metrics, offset, cols, lines);
                    tab.term.select_word(point);
                    copy_text = tab.term.selection_text();
                }
            } else if response.drag_started() {
                // Anchor at the press origin, not the post-threshold position,
                // so the selection starts exactly where the click began.
                let anchor = ui
                    .input(|i| i.pointer.press_origin())
                    .or_else(|| response.interact_pointer_pos());
                if let Some(pos) = anchor {
                    let (point, side) = pixel_to_point(pos - origin, metrics, offset, cols, lines);
                    tab.term.start_selection(point, side);
                    tab.selecting = true;
                }
            } else if response.dragged() && tab.selecting {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (point, side) = pixel_to_point(pos - origin, metrics, offset, cols, lines);
                    tab.term.update_selection(point, side);
                }
            } else if response.drag_stopped() && tab.selecting {
                tab.selecting = false;
                copy_text = tab.term.selection_text();
            } else if response.clicked() {
                tab.term.clear_selection();
            }
        }
        if let Some(text) = copy_text.filter(|t| !t.is_empty()) {
            self.copy(text);
        }

        // Middle-click pastes (X11-style): the clipboard, which a drag-select
        // has just populated.
        if response.middle_clicked() {
            if let Some(text) = self.paste_text() {
                self.paste_into(idx, &text, modes.bracketed_paste);
            }
        }

        // Wheel: scrollback normally; on the alternate screen send arrow keys
        // (alternate-scroll) so pagers/TUIs move instead of scrolling our buffer.
        if response.hovered() {
            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if scroll_y != 0.0 {
                let lines = (scroll_y / metrics.height).round() as i32;
                if modes.alt_screen {
                    let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
                    for _ in 0..lines.unsigned_abs() {
                        self.tabs[idx].term.write(seq);
                    }
                } else {
                    self.tabs[idx].term.scroll(lines);
                }
            }
        }
    }

    /// Forward pointer buttons and wheel to the child as mouse-reporting bytes.
    fn report_mouse(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        modes: crate::term::Modes,
        cell_at: &dyn Fn(egui::Pos2) -> (usize, usize),
        idx: usize,
    ) {
        let term = &self.tabs[idx].term;
        let events = ui.input(|i| i.events.clone());
        for event in events {
            if let egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } = event
            {
                if !response.rect.contains(pos) {
                    continue;
                }
                let b = match button {
                    egui::PointerButton::Primary => 0,
                    egui::PointerButton::Middle => 1,
                    egui::PointerButton::Secondary => 2,
                    _ => continue,
                };
                let (col, line) = cell_at(pos);
                if let Some(bytes) =
                    mouse_report(modes.sgr_mouse, b, col, line, pressed, modifiers, false)
                {
                    term.write(&bytes);
                }
            }
        }
        // Drag motion (only if the child asked for it).
        if (modes.mouse_drag || modes.mouse_motion) && response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                if response.rect.contains(pos) {
                    let (col, line) = cell_at(pos);
                    let mods = ui.input(|i| i.modifiers);
                    if let Some(bytes) =
                        mouse_report(modes.sgr_mouse, 0, col, line, true, mods, true)
                    {
                        term.write(&bytes);
                    }
                }
            }
        }
        // Wheel → buttons 64 (up) / 65 (down).
        if response.hovered() {
            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if scroll_y != 0.0 {
                let pos = ui
                    .input(|i| i.pointer.hover_pos())
                    .unwrap_or_else(|| response.rect.center());
                if response.rect.contains(pos) {
                    let (col, line) = cell_at(pos);
                    let btn = if scroll_y > 0.0 { 64 } else { 65 };
                    let mods = ui.input(|i| i.modifiers);
                    let steps = (scroll_y.abs() / 40.0).max(1.0) as usize;
                    for _ in 0..steps.min(5) {
                        if let Some(bytes) =
                            mouse_report(modes.sgr_mouse, btn, col, line, true, mods, false)
                        {
                            term.write(&bytes);
                        }
                    }
                }
            }
        }
    }

    /// Route this frame's key/text events to the focused terminal, intercepting
    /// font-zoom and copy/paste chords first.
    fn handle_keyboard(&mut self, ui: &egui::Ui, idx: usize) {
        let modes = self.tabs[idx].term.modes();
        let app_cursor = modes.app_cursor;
        let events = ui.input(|i| i.events.clone());

        // egui-winit turns Ctrl+C/X/V into Copy/Cut/Paste events (and drops the
        // Key event), so we read modifiers separately to tell the terminal
        // chords (Ctrl+C = SIGINT) from the app ones (Ctrl+Shift+C = copy).
        let shift = ui.input(|i| i.modifiers.shift);
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    self.tabs[idx].term.write(text.as_bytes());
                }
                // Ctrl+C (no Shift) → SIGINT; Ctrl+Shift+C → copy selection.
                egui::Event::Copy => {
                    if shift {
                        if let Some(t) = self.tabs[idx].term.selection_text() {
                            self.copy(t);
                        }
                    } else {
                        self.tabs[idx].term.write(&[0x03]);
                    }
                }
                // Ctrl+X → 0x18 (terminals don't have a "cut").
                egui::Event::Cut => {
                    self.tabs[idx].term.write(&[0x18]);
                }
                // Ctrl+Shift+V → paste; Ctrl+V → 0x16 (literal-next).
                egui::Event::Paste(text) => {
                    if shift {
                        self.paste_into(idx, &text, modes.bracketed_paste);
                    } else {
                        self.tabs[idx].term.write(&[0x16]);
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    // App chords that aren't intercepted as Copy/Cut/Paste.
                    if modifiers.ctrl {
                        match key {
                            egui::Key::Plus | egui::Key::Equals => {
                                self.zoom(idx, 1.0);
                                continue;
                            }
                            egui::Key::Minus => {
                                self.zoom(idx, -1.0);
                                continue;
                            }
                            egui::Key::Num0 => {
                                self.tabs[idx].font_size = crate::settings::get().term_font;
                                continue;
                            }
                            egui::Key::F if modifiers.shift => {
                                self.search_open = !self.search_open;
                                self.search_last = None;
                                continue;
                            }
                            // Ctrl+Shift+T (reabrir pestaña) se maneja a nivel
                            // global en update(), no aquí.
                            _ => {}
                        }
                    }
                    if let Some(bytes) = key_to_bytes(key, modifiers, app_cursor) {
                        self.tabs[idx].term.write(&bytes);
                    }
                }
                _ => {}
            }
        }
    }

    fn zoom(&mut self, idx: usize, delta: f32) {
        let tab = &mut self.tabs[idx];
        tab.font_size = (tab.font_size + delta).clamp(MIN_FONT, MAX_FONT);
    }

    /// Write pasted text, wrapping it in bracketed-paste markers when the child
    /// has enabled that mode (so editors/REPLs don't auto-indent or auto-run it).
    fn paste_into(&self, idx: usize, text: &str, bracketed: bool) {
        let term = &self.tabs[idx].term;
        if bracketed {
            term.write(b"\x1b[200~");
            term.write(text.as_bytes());
            term.write(b"\x1b[201~");
        } else {
            term.write(text.as_bytes());
        }
    }
}

/// A fixed-width, left-aligned label for a settings row (keeps controls aligned).
fn label_w(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(150.0, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(text);
        },
    );
}

/// Open a URL in the system browser (best-effort).
fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

/// The user's home directory.
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// argv for the `>_` button: the configured command (whitespace-split), or
/// `$SHELL` when unset.
fn shell_argv() -> Vec<String> {
    let cmd = crate::settings::get().shell_command;
    let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    if parts.is_empty() {
        vec![default_shell()]
    } else {
        parts
    }
}

/// Start directory for the `>_` button: the configured path, or `$HOME`.
fn shell_dir() -> Option<std::path::PathBuf> {
    let dir = crate::settings::get().shell_dir;
    if dir.trim().is_empty() {
        home_dir()
    } else {
        Some(std::path::PathBuf::from(dir.trim()))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
