//! aterm — native terminal with a built-in coding-agent session manager.
//!
//! Day-1 scaffold status:
//!   - WORKING: native egui window that scans and lists your real agent
//!     sessions (Claude/Codex/OpenCode/Gemini) via the `agent-sessions` crate,
//!     grouped by provider, with a "Resume" button that computes the resume
//!     command. Run with `cargo run -p aterm`.
//!   - NEXT (see `term/`): wire the real terminal grid. The `term` module holds
//!     the alacritty_terminal integration (PTY + parser + read-loop) ready to
//!     activate — uncomment `mod term;` below and iterate `cargo check` against
//!     the installed alacritty_terminal API, then render a `TermInstance` in the
//!     central panel instead of the placeholder.
//!
//! Architecture, rationale and the full roadmap live in CLAUDE.md.

// mod term; // ← activate in Phase 1 (terminal core). See term/mod.rs.

use agent_sessions::{all_providers, types::AgentSession};
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("aterm"),
        ..Default::default()
    };
    eframe::run_native("aterm", options, Box::new(|_cc| Ok(Box::<AtermApp>::default())))
}

/// One provider's sessions, as scanned. `error` holds the provider's failure
/// message when `list_sessions` failed (e.g. CLI never used on this machine).
struct ProviderGroup {
    /// Provider id ("claude"/…). Unused in the scaffold UI; kept for the
    /// per-provider collapsed-state and grouping work in later phases.
    #[allow(dead_code)]
    id: String,
    display_name: String,
    sessions: Vec<AgentSession>,
    error: Option<String>,
}

#[derive(Default)]
struct AtermApp {
    groups: Vec<ProviderGroup>,
    scanned: bool,
    /// Last resume command the user clicked — placeholder for "open a terminal
    /// tab and write this argv" once the terminal core is wired.
    last_resume: Option<Vec<String>>,
}

impl AtermApp {
    /// Scan every provider once. Synchronous for the scaffold; move to a
    /// background thread (the model pattern from the Warp port) when scans grow.
    fn scan(&mut self) {
        self.groups = all_providers()
            .into_iter()
            .map(|p| match p.list_sessions() {
                Ok(mut sessions) => {
                    // Stamp resume_argv centrally, exactly like the panel does upstream.
                    for s in &mut sessions {
                        s.resume_argv = p.resume_argv(&s.id);
                    }
                    sessions.sort_by(|a, b| b.last_activity.total_cmp(&a.last_activity));
                    ProviderGroup {
                        id: p.id().to_string(),
                        display_name: p.display_name().to_string(),
                        sessions,
                        error: None,
                    }
                }
                Err(e) => ProviderGroup {
                    id: p.id().to_string(),
                    display_name: p.display_name().to_string(),
                    sessions: Vec::new(),
                    error: Some(e),
                },
            })
            .collect();
        self.scanned = true;
    }
}

impl eframe::App for AtermApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.scanned {
            self.scan();
        }

        egui::SidePanel::left("sessions")
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Agent sessions");
                    if ui.button("⟳").on_hover_text("Re-scan").clicked() {
                        self.scanned = false;
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for group in &self.groups {
                        let header = if let Some(err) = &group.error {
                            format!("{} — {err}", group.display_name)
                        } else {
                            format!("{} ({})", group.display_name, group.sessions.len())
                        };
                        egui::CollapsingHeader::new(header)
                            .default_open(!group.sessions.is_empty())
                            .show(ui, |ui| {
                                for s in &group.sessions {
                                    let title = s.title.as_deref().unwrap_or("(sin título)");
                                    ui.horizontal(|ui| {
                                        let live = if s.is_active { "● " } else { "" };
                                        if ui
                                            .button("▶")
                                            .on_hover_text("Resume")
                                            .clicked()
                                        {
                                            self.last_resume = Some(s.resume_argv.clone());
                                        }
                                        ui.label(format!("{live}{title}"));
                                    });
                                }
                            });
                    }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Terminal");
            ui.separator();
            ui.label(
                "Placeholder. The terminal grid (alacritty_terminal) gets rendered here \
                 once the `term` module is wired — see term/mod.rs and CLAUDE.md.",
            );
            if let Some(argv) = &self.last_resume {
                ui.add_space(8.0);
                ui.label("Último Resume solicitado (se escribirá en un PTY nuevo):");
                ui.code(argv.join(" "));
            }
        });
    }
}
