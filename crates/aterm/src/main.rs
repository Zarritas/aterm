//! aterm — native terminal with a built-in coding-agent session manager.
//!
//! The left panel scans and lists your real agent sessions
//! (Claude/Codex/OpenCode/Gemini) via the `agent-sessions` crate — with rich
//! rows, filter, preview, rename/tags/colour, export/import and cleanup. The
//! top bar holds tabs of live terminals backed by `alacritty_terminal`
//! (PTY + parser + read-loop); the active grid fills the centre.
//!
//! Architecture, rationale and the full roadmap live in CLAUDE.md.

mod app;
mod service_status;
mod settings;
mod sessions;
mod term;
mod theme;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("aterm"),
        ..Default::default()
    };
    eframe::run_native(
        "aterm",
        options,
        Box::new(|cc| {
            app::install_fonts(&cc.egui_ctx);
            theme::load_persisted();
            theme::apply(&cc.egui_ctx);
            Ok(Box::<app::AtermApp>::default())
        }),
    )
}
