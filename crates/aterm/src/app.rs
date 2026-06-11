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
        ("sys-dejavu", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        (
            "sys-noto-symbols2",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
        ),
        ("sys-noto", "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf"),
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
}

/// In-flight tab rename/recolour dialog.
struct TabEdit {
    id: u64,
    name: String,
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
        }
    }
}

impl AtermApp {
    fn open_tab(
        &mut self,
        ctx: &egui::Context,
        argv: Vec<String>,
        cwd: Option<std::path::PathBuf>,
        key: Option<String>,
    ) {
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
                return;
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
        match TermInstance::spawn(argv, cwd, size, ctx.clone()) {
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
                });
                // A fresh terminal takes over the view as a single pane.
                self.visible = vec![id];
                self.focused = id;
                self.focus_pending = true;
            }
            Err(e) => eprintln!("aterm: failed to spawn terminal: {e}"),
        }
    }

    /// Show `id` as the sole pane and give it focus.
    fn focus_tab(&mut self, id: u64) {
        self.visible = vec![id];
        self.focused = id;
        self.focus_pending = true;
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
                if ui
                    .button(">_")
                    .on_hover_text("Nueva shell")
                    .clicked()
                {
                    pending_open = Some((shell_argv(), shell_dir(), None));
                }
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
                    // User name overrides the child title.
                    let label = truncate(
                        tab.name.as_deref().unwrap_or(&tab.term.title()),
                        22,
                    );
                    let mut text = egui::RichText::new(label);
                    // Custom colour wins; else the focused pane shows in accent.
                    if let Some(c) = tab.color {
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
                        .on_hover_text("Click: enfocar · arrastra: reordenar · clic dcho: renombrar");
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
                            .map_or(false, |(_, r)| px >= r.left() && px <= r.right());
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
                    // A live child gets a confirmation; an exited one closes now.
                    let alive = self
                        .tabs
                        .iter()
                        .any(|t| t.id == id && t.term.exit_code().is_none());
                    if alive {
                        self.close_confirm = Some(id);
                    } else {
                        self.close_tab(id);
                    }
                }

                // Settings cog, pushed to the right edge of the header.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙").on_hover_text("Ajustes").clicked() {
                        self.settings_open = true;
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
            let grid_ok: std::collections::HashSet<u64> =
                self.tabs.iter().filter(|t| !t.detached).map(|t| t.id).collect();
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
        self.render_detached(ctx);
    }
}

impl AtermApp {
    /// Confirm before closing a tab whose child is still running.
    fn close_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.close_confirm else {
            return;
        };
        // If it exited meanwhile, just close it.
        let alive = self
            .tabs
            .iter()
            .any(|t| t.id == id && t.term.exit_code().is_none());
        if !alive {
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
                    "«{}» tiene un proceso en ejecución.\n¿Cerrar de todos modos?",
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
                            let r = ui.selectable_label(*c == this, egui::RichText::new(label).size(15.0));
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
                                ui.label(egui::RichText::new("APARIENCIA").color(accent).strong().size(13.0));
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    label_w(ui, "Tema");
                                    let current = crate::theme::current_name();
                                    egui::ComboBox::from_id_salt("settings-theme")
                                        .selected_text(&current)
                                        .show_ui(ui, |ui| {
                                            for (name, _) in crate::theme::THEMES {
                                                if ui.selectable_label(current == name, name).clicked() {
                                                    crate::theme::select(ui.ctx(), name);
                                                }
                                            }
                                        });
                                });
                                ui.separator();
                                ui.horizontal(|ui| {
                                    label_w(ui, "Fuente de la interfaz");
                                    if ui.add(egui::Slider::new(&mut s.ui_font, 11.0..=22.0)).changed() {
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
                                ui.label(egui::RichText::new("TERMINAL").color(accent).strong().size(13.0));
                                ui.add_space(8.0);
                                ui.checkbox(&mut s.auto_close_on_exit, "Cerrar la pestaña al salir (exit)");
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
                                ui.label(egui::RichText::new("PANEL DE SESIONES").color(accent).strong().size(13.0));
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
                                    ui.add(egui::Slider::new(&mut s.refresh_secs, 15..=600).suffix(" s"));
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
                            .add(egui::Button::new("  ").fill(c).min_size(egui::vec2(22.0, 18.0)))
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
            if enter || ui.button("▲").on_hover_text("Anterior coincidencia").clicked() {
                search = true;
            }
            if ui.button("×").on_hover_text("Cerrar (Ctrl+Shift+F)").clicked() {
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

    /// Tile the visible tabs into a near-square grid of panes.
    fn render_panes(&mut self, ui: &mut egui::Ui) {
        let ids: Vec<u64> = self.visible.clone();
        let n = ids.len();
        if n == 1 {
            self.render_pane(ui, ids[0]);
            return;
        }
        let cols = (n as f32).sqrt().ceil() as usize;
        let rows = n.div_ceil(cols);
        let area = ui.available_rect_before_wrap();
        let gap = 4.0;
        let cell_w = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let cell_h = (area.height() - gap * (rows as f32 - 1.0)) / rows as f32;
        for (idx, id) in ids.into_iter().enumerate() {
            let r = idx / cols;
            let c = idx % cols;
            let min = egui::pos2(
                area.min.x + c as f32 * (cell_w + gap),
                area.min.y + r as f32 * (cell_h + gap),
            );
            let rect = egui::Rect::from_min_size(min, egui::vec2(cell_w, cell_h));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.set_clip_rect(rect);
                self.render_pane(ui, id);
            });
        }
    }

    /// Render one terminal pane (resize, draw, focus, input).
    fn render_pane(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(i) = self.tab_index(id) else {
            return;
        };
        let focused = id == self.focused;
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

        let response = render::draw(ui, &self.tabs[i].term, metrics, focused);

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
        let alive = self.tabs[i].term.exit_code().is_none();
        if response.has_focus() && alive {
            self.handle_keyboard(ui, i);
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
            let col = ((local.x / metrics.width).floor().max(0.0) as usize).min(cols.saturating_sub(1));
            let line =
                ((local.y / metrics.height).floor().max(0.0) as usize).min(lines.saturating_sub(1));
            (col, line)
        };

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
            if response.drag_started() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (point, side) =
                        pixel_to_point(pos - origin, metrics, offset, cols, lines);
                    tab.term.start_selection(point, side);
                    tab.selecting = true;
                }
            } else if response.dragged() && tab.selecting {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (point, side) =
                        pixel_to_point(pos - origin, metrics, offset, cols, lines);
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
                    if let Some(bytes) = mouse_report(modes.sgr_mouse, 0, col, line, true, mods, true) {
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
                        if let Some(bytes) = mouse_report(modes.sgr_mouse, btn, col, line, true, mods, false) {
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
