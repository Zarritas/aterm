//! The application shell: a session panel on the left, a tab bar of live
//! terminals on top, and the active terminal grid filling the centre. Owns
//! input routing (keys/scroll/selection), font zoom and the clipboard.

use eframe::egui;

use crate::sessions::{PanelAction, SessionPanel};
use crate::term::input::key_to_bytes;
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
    // Append as fallbacks to the *proportional* family only (the UI/buttons).
    // The terminal grid uses Monospace, where mixing in proportional fonts
    // would break cell alignment, so we leave that family untouched.
    let list = fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default();
    for name in &loaded {
        if !list.contains(name) {
            list.push(name.clone());
        }
    }
    ctx.set_fonts(fonts);
}

/// Apply a cohesive dark theme (Catppuccin Mocha) so the UI isn't the flat
/// default grey: warmer surfaces, a blue accent, rounded widgets, more spacing.
pub fn install_theme(ctx: &egui::Context) {
    use egui::{Color32, Rounding, Stroke};
    let rgb = Color32::from_rgb;
    // Catppuccin Mocha palette.
    let base = rgb(0x1e, 0x1e, 0x2e);
    let mantle = rgb(0x18, 0x18, 0x25);
    let crust = rgb(0x11, 0x11, 0x1b);
    let surface0 = rgb(0x31, 0x32, 0x44);
    let surface1 = rgb(0x45, 0x47, 0x5a);
    let surface2 = rgb(0x58, 0x5b, 0x70);
    let overlay = rgb(0x6c, 0x70, 0x86);
    let text = rgb(0xcd, 0xd6, 0xf4);
    let blue = rgb(0x89, 0xb4, 0xfa);
    let lavender = rgb(0xb4, 0xbe, 0xfe);
    let red = rgb(0xf3, 0x8b, 0xa8);
    let yellow = rgb(0xf9, 0xe2, 0xaf);

    let mut v = egui::Visuals::dark();
    let rounding = Rounding::same(6.0);
    v.panel_fill = base;
    v.window_fill = mantle;
    v.window_stroke = Stroke::new(1.0, surface1);
    v.window_rounding = rounding;
    v.menu_rounding = rounding;
    v.extreme_bg_color = crust; // text-edit background
    v.faint_bg_color = surface0; // striped rows
    v.code_bg_color = mantle;
    v.hyperlink_color = blue;
    v.warn_fg_color = yellow;
    v.error_fg_color = red;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0x89, 0xb4, 0xfa, 70);
    v.selection.stroke = Stroke::new(1.0, lavender);

    let set = |w: &mut egui::style::WidgetVisuals, fill, stroke_c, fg| {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, stroke_c);
        w.fg_stroke = Stroke::new(1.0, fg);
        w.rounding = rounding;
    };
    set(&mut v.widgets.noninteractive, base, surface0, text);
    set(&mut v.widgets.inactive, surface0, surface0, text);
    set(&mut v.widgets.hovered, surface1, overlay, text);
    set(&mut v.widgets.active, surface2, lavender, text);
    set(&mut v.widgets.open, surface0, surface1, text);

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    ctx.set_style(style);
}

/// Default monospace point size for new terminals.
const FONT_SIZE: f32 = 14.0;
const MIN_FONT: f32 = 7.0;
const MAX_FONT: f32 = 40.0;

/// One open terminal tab: its PTY-backed model plus per-tab view state.
struct Tab {
    term: TermInstance,
    font_size: f32,
    /// True while a mouse drag-selection is in progress.
    selecting: bool,
    /// `provider:id` for resumed sessions; `None` for plain shells. Used to
    /// avoid resuming the same session into two tabs.
    key: Option<String>,
}

pub struct AtermApp {
    panel: SessionPanel,
    tabs: Vec<Tab>,
    active: usize,
    clipboard: Option<arboard::Clipboard>,
    /// Set after opening a tab so it grabs keyboard focus next frame.
    focus_pending: bool,
}

impl Default for AtermApp {
    fn default() -> Self {
        Self {
            panel: SessionPanel::default(),
            tabs: Vec::new(),
            active: 0,
            clipboard: arboard::Clipboard::new().ok(),
            focus_pending: false,
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
            if let Some(i) = self.tabs.iter().position(|t| {
                t.key.as_deref() == Some(k) && t.term.exit_code().is_none()
            }) {
                self.active = i;
                self.focus_pending = true;
                return;
            }
        }

        let metrics = CellMetrics::measure(ctx, FONT_SIZE);
        let size = TermSize {
            columns: 80,
            lines: 24,
            cell_width: metrics.width,
            cell_height: metrics.height,
        };
        match TermInstance::spawn(argv, cwd, size, ctx.clone()) {
            Ok(term) => {
                self.tabs.push(Tab {
                    term,
                    font_size: FONT_SIZE,
                    selecting: false,
                    key,
                });
                self.active = self.tabs.len() - 1;
                self.focus_pending = true;
            }
            Err(e) => eprintln!("aterm: failed to spawn terminal: {e}"),
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i); // `Drop` shuts the PTY down.
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
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

        egui::SidePanel::left("sessions")
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                if let Some(PanelAction::Open { argv, cwd, key }) = self.panel.ui(ui) {
                    pending_open = Some((argv, cwd, key));
                }
            });

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("+ shell").clicked() {
                    pending_open = Some((vec![default_shell()], None, None));
                }
                ui.separator();
                let mut to_close = None;
                let mut to_activate = None;
                for (i, tab) in self.tabs.iter().enumerate() {
                    let label = truncate(&tab.term.title(), 24);
                    if ui.selectable_label(i == self.active, label).clicked() {
                        to_activate = Some(i);
                    }
                    if ui.small_button("×").on_hover_text("Cerrar").clicked() {
                        to_close = Some(i);
                    }
                    ui.separator();
                }
                if let Some(i) = to_activate {
                    self.active = i;
                    self.focus_pending = true;
                }
                if let Some(i) = to_close {
                    self.close_tab(i);
                }
            });
        });

        if let Some((argv, cwd, key)) = pending_open {
            self.open_tab(ctx, argv, cwd, key);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tabs.is_empty() {
                ui.heading("Terminal");
                ui.separator();
                ui.label(
                    "Pulsa «+ shell» para abrir una shell, o «▶» en una sesión \
                     del panel para reanudarla.",
                );
                return;
            }
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len() - 1;
            }

            let font_size = self.tabs[self.active].font_size;
            let metrics = CellMetrics::measure(ctx, font_size);

            // Match the PTY grid to the panel's current cell capacity.
            let (cols, lines) = metrics.grid_size(ui.available_size());
            {
                let term = &mut self.tabs[self.active].term;
                if term.size.columns != cols || term.size.lines != lines {
                    term.resize(TermSize {
                        columns: cols,
                        lines,
                        cell_width: metrics.width,
                        cell_height: metrics.height,
                    });
                }
            }

            // Keep the OS window title in sync with the active child.
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "aterm — {}",
                self.tabs[self.active].term.title()
            )));

            let response = render::draw(ui, &self.tabs[self.active].term, metrics, true);

            if self.focus_pending {
                response.request_focus();
                self.focus_pending = false;
            }
            if response.clicked() {
                response.request_focus();
            }
            // Keep Tab / arrows / Escape flowing to the child instead of egui's
            // focus traversal while the grid is focused.
            if response.has_focus() {
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

            self.handle_mouse(ui, &response, metrics);
            // No point typing into a shell that has already exited.
            let alive = self.tabs[self.active].term.exit_code().is_none();
            if response.has_focus() && alive {
                self.handle_keyboard(ui);
            }
        });
    }
}

impl AtermApp {
    /// Drag → selection; release copies it; plain click clears it; wheel scrolls
    /// the scrollback.
    fn handle_mouse(&mut self, ui: &egui::Ui, response: &egui::Response, metrics: CellMetrics) {
        let origin = response.rect.min;
        let (cols, lines, offset) = {
            let term = &self.tabs[self.active].term;
            (term.size.columns, term.size.lines, term.display_offset())
        };

        let mut copy_text: Option<String> = None;
        {
            let tab = &mut self.tabs[self.active];
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

        // Scrollback via the mouse wheel while hovering the grid.
        if response.hovered() {
            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if scroll_y != 0.0 {
                let delta = (scroll_y / metrics.height).round() as i32;
                self.tabs[self.active].term.scroll(delta);
            }
        }
    }

    /// Route this frame's key/text events to the focused terminal, intercepting
    /// font-zoom and copy/paste chords first.
    fn handle_keyboard(&mut self, ui: &egui::Ui) {
        let app_cursor = self.tabs[self.active].term.app_cursor();
        let events = ui.input(|i| i.events.clone());

        for event in events {
            match event {
                egui::Event::Text(text) => {
                    self.tabs[self.active].term.write(text.as_bytes());
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    // Ctrl(+Shift) chords handled by the app, not the child.
                    if modifiers.ctrl {
                        match key {
                            egui::Key::Plus | egui::Key::Equals => {
                                self.zoom(1.0);
                                continue;
                            }
                            egui::Key::Minus => {
                                self.zoom(-1.0);
                                continue;
                            }
                            egui::Key::Num0 => {
                                self.tabs[self.active].font_size = FONT_SIZE;
                                continue;
                            }
                            egui::Key::C if modifiers.shift => {
                                if let Some(t) = self.tabs[self.active].term.selection_text() {
                                    self.copy(t);
                                }
                                continue;
                            }
                            egui::Key::V if modifiers.shift => {
                                if let Some(t) = self.paste_text() {
                                    self.tabs[self.active].term.write(t.as_bytes());
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                    if let Some(bytes) = key_to_bytes(key, modifiers, app_cursor) {
                        self.tabs[self.active].term.write(&bytes);
                    }
                }
                _ => {}
            }
        }
    }

    fn zoom(&mut self, delta: f32) {
        let tab = &mut self.tabs[self.active];
        tab.font_size = (tab.font_size + delta).clamp(MIN_FONT, MAX_FONT);
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
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
