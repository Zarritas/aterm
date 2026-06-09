//! Keyboard → PTY bytes. Acotado pero traicionero: porta casi 1:1 del módulo
//! `input` de Alacritty. The child expects terminfo-correct escape sequences,
//! and several depend on terminal *modes* (application cursor) that `Term`
//! tracks — read `app_cursor` off the `TermInstance` before encoding.
//!
//! Printable text arrives separately as `egui::Event::Text` and is written
//! verbatim; this function handles the non-text keys and Ctrl combinations.

use eframe::egui::{Key, Modifiers};

/// Translate a key press to the bytes the child expects, or `None` when the
/// key has no terminal encoding (egui will have delivered it as `Text`).
pub fn key_to_bytes(key: Key, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    // Ctrl+letter → C0 control byte (Ctrl+A = 0x01 … Ctrl+Z = 0x1a). Takes
    // precedence: with Ctrl held egui won't emit a Text event for these.
    if mods.ctrl && !mods.alt {
        if let Some(byte) = ctrl_byte(key) {
            return Some(vec![byte]);
        }
    }

    let seq: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",

        Key::ArrowUp if app_cursor => b"\x1bOA",
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown if app_cursor => b"\x1bOB",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight if app_cursor => b"\x1bOC",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft if app_cursor => b"\x1bOD",
        Key::ArrowLeft => b"\x1b[D",

        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Insert => b"\x1b[2~",
        Key::Delete => b"\x1b[3~",

        Key::F1 => b"\x1bOP",
        Key::F2 => b"\x1bOQ",
        Key::F3 => b"\x1bOR",
        Key::F4 => b"\x1bOS",
        Key::F5 => b"\x1b[15~",
        Key::F6 => b"\x1b[17~",
        Key::F7 => b"\x1b[18~",
        Key::F8 => b"\x1b[19~",
        Key::F9 => b"\x1b[20~",
        Key::F10 => b"\x1b[21~",
        Key::F11 => b"\x1b[23~",
        Key::F12 => b"\x1b[24~",

        _ => return None,
    };
    Some(seq.to_vec())
}

/// C0 control byte for `Ctrl+<key>`, mirroring a VT100. Letters map to
/// `0x01..=0x1a`; a handful of symbol keys map to the classic controls.
fn ctrl_byte(key: Key) -> Option<u8> {
    let b = match key {
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0a,
        Key::K => 0x0b,
        Key::L => 0x0c,
        Key::M => 0x0d,
        Key::N => 0x0e,
        Key::O => 0x0f,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1a,
        Key::OpenBracket => 0x1b,  // Ctrl+[  → ESC
        Key::Backslash => 0x1c,    // Ctrl+\  → FS
        Key::CloseBracket => 0x1d, // Ctrl+]  → GS
        _ => return None,
    };
    Some(b)
}
