//! Render the terminal grid into egui. THIS is the bulk of the native work
//! (~300-500 LoC when complete). egui handles font shaping/rasterising; you
//! map each cell to a painted background + glyph.
//!
//! STATUS: reference skeleton. The MVP path uses egui's painter (monospaced,
//! good enough). Migrate to a wgpu glyph atlas only if throughput on heavy
//! TUIs (claude/codex repaint storms) proves egui too slow.
//!
//! ```ignore
//! pub fn draw(ui: &mut egui::Ui, term: &Term<EventProxy>, metrics: CellMetrics) {
//!     let content = term.renderable_content(); // cells + cursor + colors
//!     let painter = ui.painter();
//!     for cell in content.display_iter {
//!         let rect = cell_rect(cell.point, metrics);
//!         let (fg, bg) = resolve_colors(&cell, &content.colors); // 256 + truecolor + theme
//!         painter.rect_filled(rect, 0.0, bg);
//!         if cell.c != ' ' {
//!             painter.text(rect.min, Align2::LEFT_TOP, cell.c,
//!                          FontId::monospace(metrics.font_size), fg);
//!         }
//!         // TODO: bold/italic/underline/strike flags, selection highlight.
//!     }
//!     draw_cursor(painter, content.cursor, metrics);
//! }
//! ```
//!
//! Remaining surface after the basic loop above:
//!   - color resolution: ANSI 16/256, truecolor, dim/bright, theme palette
//!   - text styles: bold (font weight), italic, underline, strikethrough
//!   - cursor shapes (block/bar/underline) + blink + focused/unfocused
//!   - selection rendering + click-drag selection → clipboard (arboard)
//!   - scrollback viewport (mouse wheel → Term::scroll_display)
//!   - reflow on resize is handled by Term itself once you call resize().

/// Pixel metrics for one monospaced cell, derived from the chosen egui font.
#[derive(Clone, Copy)]
pub struct CellMetrics {
    pub font_size: f32,
    pub width: f32,
    pub height: f32,
}
