//! The application shell: a session panel on the left, a tab bar of live
//! terminals on top, and the active terminal grid filling the centre. Owns
//! input routing (keys/scroll/selection), font zoom and the clipboard.

use eframe::egui;

use crate::sessions::{PanelAction, SessionPanel};
use crate::term::input::key_to_bytes;
use crate::term::render::{self, pixel_to_point, CellMetrics};
use crate::term::{TermInstance, TermSize};

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
    fn open_tab(&mut self, ctx: &egui::Context, argv: Vec<String>, cwd: Option<std::path::PathBuf>) {
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
        let mut pending_open: Option<(Vec<String>, Option<std::path::PathBuf>)> = None;

        egui::SidePanel::left("sessions")
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                if let Some(PanelAction::Open { argv, cwd }) = self.panel.ui(ui) {
                    pending_open = Some((argv, cwd));
                }
            });

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("＋ shell").clicked() {
                    pending_open = Some((vec![default_shell()], None));
                }
                ui.separator();
                let mut to_close = None;
                let mut to_activate = None;
                for (i, tab) in self.tabs.iter().enumerate() {
                    let label = truncate(&tab.term.title(), 24);
                    if ui.selectable_label(i == self.active, label).clicked() {
                        to_activate = Some(i);
                    }
                    if ui.small_button("✕").on_hover_text("Cerrar").clicked() {
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

        if let Some((argv, cwd)) = pending_open {
            self.open_tab(ctx, argv, cwd);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tabs.is_empty() {
                ui.heading("Terminal");
                ui.separator();
                ui.label(
                    "Pulsa «＋ shell» para abrir una shell, o «▶» en una sesión \
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
            if response.has_focus() {
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
