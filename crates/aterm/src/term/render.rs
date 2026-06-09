//! Render the terminal grid into egui. egui handles font shaping/rasterising;
//! we map each cell to a painted background + glyph.
//!
//! MVP path: egui's painter with a monospaced font (good enough). Migrate to a
//! wgpu glyph atlas only if throughput on heavy TUIs (claude/codex repaint
//! storms) proves egui too slow.

use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::viewport_to_point;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};

use super::TermInstance;

/// Default foreground/background when the child hasn't overridden the palette.
/// Tuned to match the app's Catppuccin Mocha chrome (base / text).
const DEFAULT_FG: Rgb = Rgb { r: 0xcd, g: 0xd6, b: 0xf4 };
const DEFAULT_BG: Rgb = Rgb { r: 0x1e, g: 0x1e, b: 0x2e };
/// Highlight behind selected cells (surface2).
const SELECTION_BG: Rgb = Rgb { r: 0x58, g: 0x5b, b: 0x70 };

/// Pixel metrics for one monospaced cell, derived from the chosen egui font.
#[derive(Clone, Copy)]
pub struct CellMetrics {
    pub font_size: f32,
    pub width: f32,
    pub height: f32,
}

impl CellMetrics {
    /// Measure the active monospace font for `font_size` points. Done once per
    /// frame from the egui context's font atlas.
    pub fn measure(ctx: &egui::Context, font_size: f32) -> Self {
        let id = FontId::monospace(font_size);
        let (width, height) = ctx.fonts(|f| (f.glyph_width(&id, 'M'), f.row_height(&id)));
        Self {
            font_size,
            width: width.max(1.0),
            height: height.max(1.0),
        }
    }

    /// How many whole cells fit in `avail` pixels (always at least 1×1).
    pub fn grid_size(&self, avail: Vec2) -> (usize, usize) {
        let cols = (avail.x / self.width).floor().max(1.0) as usize;
        let lines = (avail.y / self.height).floor().max(1.0) as usize;
        (cols, lines)
    }
}

/// Map a pointer position relative to the grid `origin` to a buffer `Point`
/// (accounting for scrollback via `display_offset`) plus which half of the cell
/// it fell on — both needed to drive `Selection`.
pub fn pixel_to_point(
    local: Vec2,
    metrics: CellMetrics,
    display_offset: usize,
    columns: usize,
    lines: usize,
) -> (Point, Side) {
    let col_f = (local.x / metrics.width).floor();
    let line_f = (local.y / metrics.height).floor();
    let col = (col_f.max(0.0) as usize).min(columns.saturating_sub(1));
    let vline = (line_f.max(0.0) as usize).min(lines.saturating_sub(1));
    let side = if local.x - col_f * metrics.width < metrics.width / 2.0 {
        Side::Left
    } else {
        Side::Right
    };
    let point = viewport_to_point(display_offset, Point::new(vline, Column(col)));
    (point, side)
}

/// Paint `term`'s visible grid into the current `ui`, consuming the available
/// space. Returns the allocated response so the caller can drive focus/clicks.
pub fn draw(
    ui: &mut egui::Ui,
    term: &TermInstance,
    metrics: CellMetrics,
    focused: bool,
) -> egui::Response {
    let avail = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let origin = rect.min;

    // Backdrop: the terminal default background under the whole panel.
    painter.rect_filled(rect, 0.0, color32(DEFAULT_BG));

    let guard = term.term.lock();
    let content = guard.renderable_content();
    let colors = content.colors;
    let font = FontId::monospace(metrics.font_size);

    // Capture cursor + selection state before the loop consumes `display_iter`.
    let cursor_shape = content.cursor.shape;
    let cursor_point = content.cursor.point;
    let scrolled = content.display_offset != 0;
    let selection = content.selection;

    for indexed in content.display_iter {
        let point = indexed.point;
        let cell = indexed.cell;
        let line = point.line.0;
        if line < 0 {
            continue; // scrollback above the viewport; display_iter is clamped anyway
        }
        let col = point.column.0;

        // A wide char's trailing spacer carries no glyph of its own.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        let cell_w = if cell.flags.contains(Flags::WIDE_CHAR) {
            metrics.width * 2.0
        } else {
            metrics.width
        };
        let pos = Pos2::new(
            origin.x + col as f32 * metrics.width,
            origin.y + line as f32 * metrics.height,
        );
        let cell_rect = Rect::from_min_size(pos, Vec2::new(cell_w, metrics.height));

        let mut fg = resolve(cell.fg, colors, cell.flags, true);
        let mut bg = resolve(cell.bg, colors, cell.flags, false);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg);
        }

        let selected = selection.map_or(false, |r| r.contains(point));
        if selected {
            painter.rect_filled(cell_rect, 0.0, color32(SELECTION_BG));
        } else if bg != DEFAULT_BG {
            painter.rect_filled(cell_rect, 0.0, color32(bg));
        }

        let glyph = cell.c;
        if glyph != ' ' && glyph != '\0' && !cell.flags.contains(Flags::HIDDEN) {
            painter.text(pos, Align2::LEFT_TOP, glyph, font.clone(), color32(fg));
        }

        // Underline / strikeout as painted lines (egui's default monospace has
        // no styled variants; bold/italic are approximated by color only).
        if cell.flags.contains(Flags::UNDERLINE) {
            let y = cell_rect.bottom() - 1.0;
            painter.hline(cell_rect.x_range(), y, Stroke::new(1.0, color32(fg)));
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            let y = cell_rect.center().y;
            painter.hline(cell_rect.x_range(), y, Stroke::new(1.0, color32(fg)));
        }
    }

    // Cursor only when not scrolled into history and not explicitly hidden.
    if !scrolled && cursor_shape != CursorShape::Hidden {
        draw_cursor(&painter, origin, metrics, cursor_shape, cursor_point, focused);
    }

    response
}

fn draw_cursor(
    painter: &egui::Painter,
    origin: Pos2,
    metrics: CellMetrics,
    shape: CursorShape,
    p: alacritty_terminal::index::Point,
    focused: bool,
) {
    if p.line.0 < 0 {
        return;
    }
    let pos = Pos2::new(
        origin.x + p.column.0 as f32 * metrics.width,
        origin.y + p.line.0 as f32 * metrics.height,
    );
    let cell_rect = Rect::from_min_size(pos, Vec2::new(metrics.width, metrics.height));
    let cur = color32(DEFAULT_FG);

    if !focused {
        // Unfocused window: hollow box regardless of shape.
        painter.rect_stroke(cell_rect, 0.0, Stroke::new(1.0, cur));
        return;
    }

    match shape {
        CursorShape::Block | CursorShape::HollowBlock => {
            painter.rect_filled(cell_rect, 0.0, cur);
        }
        CursorShape::Underline => {
            let r = Rect::from_min_size(
                Pos2::new(cell_rect.left(), cell_rect.bottom() - 2.0),
                Vec2::new(metrics.width, 2.0),
            );
            painter.rect_filled(r, 0.0, cur);
        }
        CursorShape::Beam => {
            let r = Rect::from_min_size(cell_rect.min, Vec2::new(2.0, metrics.height));
            painter.rect_filled(r, 0.0, cur);
        }
        CursorShape::Hidden => {}
    }
}

/// Map an alacritty `Color` to a concrete RGB, consulting the live palette
/// overrides first and falling back to a standard xterm palette.
fn resolve(
    color: Color,
    colors: &alacritty_terminal::term::color::Colors,
    flags: Flags,
    is_fg: bool,
) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => {
            // Bold promotes the dim base colors (0..7) to their bright twins.
            let idx = if is_fg && flags.contains(Flags::BOLD) && i < 8 {
                i + 8
            } else {
                i
            };
            colors[idx as usize].unwrap_or_else(|| palette(idx))
        }
        Color::Named(named) => {
            let named = if is_fg && flags.contains(Flags::BOLD) {
                bright(named)
            } else {
                named
            };
            colors[named].unwrap_or_else(|| named_default(named))
        }
    }
}

/// Bold-promotion for the 8 base named colors; everything else is unchanged.
fn bright(c: NamedColor) -> NamedColor {
    use NamedColor::*;
    match c {
        Black => BrightBlack,
        Red => BrightRed,
        Green => BrightGreen,
        Yellow => BrightYellow,
        Blue => BrightBlue,
        Magenta => BrightMagenta,
        Cyan => BrightCyan,
        White => BrightWhite,
        other => other,
    }
}

fn named_default(c: NamedColor) -> Rgb {
    use NamedColor::*;
    match c {
        Foreground | BrightForeground => DEFAULT_FG,
        Background => DEFAULT_BG,
        Cursor => DEFAULT_FG,
        DimForeground => dim(DEFAULT_FG),
        Black => palette(0),
        Red => palette(1),
        Green => palette(2),
        Yellow => palette(3),
        Blue => palette(4),
        Magenta => palette(5),
        Cyan => palette(6),
        White => palette(7),
        BrightBlack => palette(8),
        BrightRed => palette(9),
        BrightGreen => palette(10),
        BrightYellow => palette(11),
        BrightBlue => palette(12),
        BrightMagenta => palette(13),
        BrightCyan => palette(14),
        BrightWhite => palette(15),
        DimBlack => dim(palette(0)),
        DimRed => dim(palette(1)),
        DimGreen => dim(palette(2)),
        DimYellow => dim(palette(3)),
        DimBlue => dim(palette(4)),
        DimMagenta => dim(palette(5)),
        DimCyan => dim(palette(6)),
        DimWhite => dim(palette(7)),
    }
}

/// Standard xterm 256-color palette: 16 base + 6×6×6 cube + 24-step grayscale.
fn palette(i: u8) -> Rgb {
    const BASE: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    if i < 16 {
        let (r, g, b) = BASE[i as usize];
        return Rgb { r, g, b };
    }
    if i < 232 {
        let i = i - 16;
        let levels = [0u8, 95, 135, 175, 215, 255];
        Rgb {
            r: levels[(i / 36) as usize],
            g: levels[((i / 6) % 6) as usize],
            b: levels[(i % 6) as usize],
        }
    } else {
        let v = 8 + (i - 232) * 10;
        Rgb { r: v, g: v, b: v }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb {
        r: (c.r as u16 * 2 / 3) as u8,
        g: (c.g as u16 * 2 / 3) as u8,
        b: (c.b as u16 * 2 / 3) as u8,
    }
}

#[inline]
fn color32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}
