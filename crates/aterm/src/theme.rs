//! Switchable colour themes. A global `Palette` drives both egui's chrome
//! (`apply`) and the app's own accents (panel, terminal grid), so changing the
//! theme at runtime recolours everything. The choice is persisted to disk.

use std::sync::RwLock;

use eframe::egui::{self, Color32};

/// Every colour the UI pulls from the active theme.
#[derive(Clone, Copy)]
pub struct Palette {
    pub base: Color32,
    pub mantle: Color32,
    pub crust: Color32,
    pub surface0: Color32,
    pub surface1: Color32,
    pub surface2: Color32,
    pub overlay: Color32,
    pub text: Color32,
    pub blue: Color32,
    pub lavender: Color32,
    pub green: Color32,
    pub yellow: Color32,
    pub peach: Color32,
    pub red: Color32,
    pub teal: Color32,
    pub mauve: Color32,
    pub sapphire: Color32,
    /// Session-card background (between `base` and `surface0`).
    pub card: Color32,
    pub term_bg: Color32,
    pub term_fg: Color32,
    pub selection: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const MOCHA: Palette = Palette {
    base: rgb(0x1e, 0x1e, 0x2e),
    mantle: rgb(0x18, 0x18, 0x25),
    crust: rgb(0x11, 0x11, 0x1b),
    surface0: rgb(0x31, 0x32, 0x44),
    surface1: rgb(0x45, 0x47, 0x5a),
    surface2: rgb(0x58, 0x5b, 0x70),
    overlay: rgb(0x6c, 0x70, 0x86),
    text: rgb(0xcd, 0xd6, 0xf4),
    blue: rgb(0x89, 0xb4, 0xfa),
    lavender: rgb(0xb4, 0xbe, 0xfe),
    green: rgb(0xa6, 0xe3, 0xa1),
    yellow: rgb(0xf9, 0xe2, 0xaf),
    peach: rgb(0xfa, 0xb3, 0x87),
    red: rgb(0xf3, 0x8b, 0xa8),
    teal: rgb(0x94, 0xe2, 0xd5),
    mauve: rgb(0xcb, 0xa6, 0xf7),
    sapphire: rgb(0x74, 0xc7, 0xec),
    card: rgb(0x28, 0x28, 0x3a),
    term_bg: rgb(0x1e, 0x1e, 0x2e),
    term_fg: rgb(0xcd, 0xd6, 0xf4),
    selection: rgb(0x58, 0x5b, 0x70),
};

pub const TOKYO_NIGHT: Palette = Palette {
    base: rgb(0x1a, 0x1b, 0x26),
    mantle: rgb(0x16, 0x16, 0x1e),
    crust: rgb(0x13, 0x13, 0x1a),
    surface0: rgb(0x24, 0x28, 0x3b),
    surface1: rgb(0x34, 0x3a, 0x52),
    surface2: rgb(0x41, 0x48, 0x68),
    overlay: rgb(0x56, 0x5f, 0x89),
    text: rgb(0xc0, 0xca, 0xf5),
    blue: rgb(0x7a, 0xa2, 0xf7),
    lavender: rgb(0xbb, 0x9a, 0xf7),
    green: rgb(0x9e, 0xce, 0x6a),
    yellow: rgb(0xe0, 0xaf, 0x68),
    peach: rgb(0xff, 0x9e, 0x64),
    red: rgb(0xf7, 0x76, 0x8e),
    teal: rgb(0x2a, 0xc3, 0xde),
    mauve: rgb(0xbb, 0x9a, 0xf7),
    sapphire: rgb(0x7d, 0xcf, 0xff),
    card: rgb(0x22, 0x24, 0x31),
    term_bg: rgb(0x1a, 0x1b, 0x26),
    term_fg: rgb(0xc0, 0xca, 0xf5),
    selection: rgb(0x33, 0x46, 0x7c),
};

pub const DRACULA: Palette = Palette {
    base: rgb(0x28, 0x2a, 0x36),
    mantle: rgb(0x21, 0x22, 0x2c),
    crust: rgb(0x19, 0x1a, 0x21),
    surface0: rgb(0x34, 0x37, 0x46),
    surface1: rgb(0x42, 0x44, 0x50),
    surface2: rgb(0x56, 0x57, 0x61),
    overlay: rgb(0x62, 0x72, 0xa4),
    text: rgb(0xf8, 0xf8, 0xf2),
    blue: rgb(0xbd, 0x93, 0xf9),
    lavender: rgb(0xbd, 0x93, 0xf9),
    green: rgb(0x50, 0xfa, 0x7b),
    yellow: rgb(0xf1, 0xfa, 0x8c),
    peach: rgb(0xff, 0xb8, 0x6c),
    red: rgb(0xff, 0x55, 0x55),
    teal: rgb(0x8b, 0xe9, 0xfd),
    mauve: rgb(0xff, 0x79, 0xc6),
    sapphire: rgb(0x8b, 0xe9, 0xfd),
    card: rgb(0x34, 0x37, 0x46),
    term_bg: rgb(0x28, 0x2a, 0x36),
    term_fg: rgb(0xf8, 0xf8, 0xf2),
    selection: rgb(0x44, 0x47, 0x5a),
};

pub const NORD: Palette = Palette {
    base: rgb(0x2e, 0x34, 0x40),
    mantle: rgb(0x2b, 0x30, 0x3b),
    crust: rgb(0x27, 0x2c, 0x36),
    surface0: rgb(0x3b, 0x42, 0x52),
    surface1: rgb(0x43, 0x4c, 0x5e),
    surface2: rgb(0x4c, 0x56, 0x6a),
    overlay: rgb(0x61, 0x6e, 0x88),
    text: rgb(0xe5, 0xe9, 0xf0),
    blue: rgb(0x81, 0xa1, 0xc1),
    lavender: rgb(0xb4, 0x8e, 0xad),
    green: rgb(0xa3, 0xbe, 0x8c),
    yellow: rgb(0xeb, 0xcb, 0x8b),
    peach: rgb(0xd0, 0x87, 0x70),
    red: rgb(0xbf, 0x61, 0x6a),
    teal: rgb(0x8f, 0xbc, 0xbb),
    mauve: rgb(0xb4, 0x8e, 0xad),
    sapphire: rgb(0x88, 0xc0, 0xd0),
    card: rgb(0x3b, 0x42, 0x52),
    term_bg: rgb(0x2e, 0x34, 0x40),
    term_fg: rgb(0xd8, 0xde, 0xe9),
    selection: rgb(0x43, 0x4c, 0x5e),
};

/// Selectable themes, by display name.
pub const THEMES: [(&str, Palette); 4] = [
    ("Catppuccin Mocha", MOCHA),
    ("Tokyo Night", TOKYO_NIGHT),
    ("Dracula", DRACULA),
    ("Nord", NORD),
];

static CURRENT: RwLock<Palette> = RwLock::new(MOCHA);
static CURRENT_NAME: RwLock<String> = RwLock::new(String::new());

/// The active palette (cheap `Copy`).
pub fn pal() -> Palette {
    *CURRENT.read().unwrap()
}

/// Name of the active theme (for the selector's current selection).
pub fn current_name() -> String {
    let n = CURRENT_NAME.read().unwrap();
    if n.is_empty() {
        THEMES[0].0.to_string()
    } else {
        n.clone()
    }
}

/// Switch theme by name, persist the choice, and reapply egui's style.
pub fn select(ctx: &egui::Context, name: &str) {
    if let Some((_, p)) = THEMES.iter().find(|(n, _)| *n == name) {
        *CURRENT.write().unwrap() = *p;
        *CURRENT_NAME.write().unwrap() = name.to_string();
        apply(ctx);
        save(name);
    }
}

/// Load the persisted theme (call once at startup, before `apply`).
pub fn load_persisted() {
    if let Ok(name) = std::fs::read_to_string(theme_path()) {
        let name = name.trim();
        if let Some((_, p)) = THEMES.iter().find(|(n, _)| *n == name) {
            *CURRENT.write().unwrap() = *p;
            *CURRENT_NAME.write().unwrap() = name.to_string();
        }
    }
}

fn save(name: &str) {
    let path = theme_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, name);
}

fn theme_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".config/aterm/theme")
}

/// Build egui's `Style`/`Visuals` from the active palette.
pub fn apply(ctx: &egui::Context) {
    use egui::{Rounding, Stroke};
    let p = pal();
    let mut v = egui::Visuals::dark();
    let rounding = Rounding::same(6.0);
    v.panel_fill = p.base;
    v.window_fill = p.mantle;
    v.window_stroke = Stroke::new(1.0, p.surface1);
    v.window_rounding = rounding;
    v.menu_rounding = rounding;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    v.extreme_bg_color = p.crust;
    v.faint_bg_color = p.surface0;
    v.code_bg_color = p.mantle;
    v.hyperlink_color = p.blue;
    v.warn_fg_color = p.yellow;
    v.error_fg_color = p.red;
    v.selection.bg_fill = p.surface2.gamma_multiply(0.9);
    v.selection.stroke = Stroke::new(1.0, p.lavender);

    let set = |w: &mut egui::style::WidgetVisuals, fill, stroke_c, fg| {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, stroke_c);
        w.fg_stroke = Stroke::new(1.0, fg);
        w.rounding = rounding;
    };
    set(&mut v.widgets.noninteractive, p.base, p.surface0, p.text);
    set(&mut v.widgets.inactive, p.surface0, p.surface0, p.text);
    set(&mut v.widgets.hovered, p.surface1, p.overlay, p.text);
    set(&mut v.widgets.active, p.surface2, p.lavender, p.text);
    set(&mut v.widgets.open, p.surface0, p.surface1, p.text);

    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(8.0, 5.0);
    style.spacing.indent = 14.0;
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    use egui::{FontFamily::Proportional, FontId, TextStyle};
    let ui_font = crate::settings::get().ui_font;
    style.text_styles = [
        (TextStyle::Heading, FontId::new(ui_font + 4.5, Proportional)),
        (TextStyle::Body, FontId::new(ui_font, Proportional)),
        (TextStyle::Button, FontId::new(ui_font, Proportional)),
        (TextStyle::Small, FontId::new((ui_font - 2.5).max(9.0), Proportional)),
        (TextStyle::Monospace, FontId::new(ui_font - 1.5, egui::FontFamily::Monospace)),
    ]
    .into();
    ctx.set_style(style);
}
