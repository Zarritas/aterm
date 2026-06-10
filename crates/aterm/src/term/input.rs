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

/// Encode a mouse event for the child. `button`: 0=left, 1=middle, 2=right,
/// 64=wheel-up, 65=wheel-down. `col`/`line` are 0-based viewport cells. With
/// SGR mouse on, uses `ESC [ < b ; x ; y M|m`; otherwise the legacy X10 form
/// (which can't address cells past 223). `motion` flags a drag report.
pub fn mouse_report(
    sgr: bool,
    button: u8,
    col: usize,
    line: usize,
    pressed: bool,
    mods: egui::Modifiers,
    motion: bool,
) -> Option<Vec<u8>> {
    let mut cb = button;
    if motion {
        cb += 32;
    }
    if mods.shift {
        cb += 4;
    }
    if mods.alt {
        cb += 8;
    }
    if mods.ctrl {
        cb += 16;
    }
    let (x, y) = (col + 1, line + 1);
    if sgr {
        let trailer = if pressed { 'M' } else { 'm' };
        Some(format!("\x1b[<{cb};{x};{y}{trailer}").into_bytes())
    } else {
        // Legacy X10: ESC [ M  (Cb+32) (Cx+32) (Cy+32). Coordinates above 223
        // can't be encoded in one byte; bail rather than send garbage.
        if x > 223 || y > 223 {
            return None;
        }
        let mut out = vec![0x1b, b'[', b'M'];
        out.push(32 + cb);
        out.push(32 + x as u8);
        out.push(32 + y as u8);
        Some(out)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Modifiers;

    #[test]
    fn sgr_mouse_press_and_release() {
        // Left press at col 0,line 0 → 1-based 1;1.
        let press = mouse_report(true, 0, 0, 0, true, Modifiers::default(), false).unwrap();
        assert_eq!(press, b"\x1b[<0;1;1M");
        // Release uses lowercase 'm'.
        let rel = mouse_report(true, 0, 0, 0, false, Modifiers::default(), false).unwrap();
        assert_eq!(rel, b"\x1b[<0;1;1m");
    }

    #[test]
    fn sgr_wheel_and_modifiers() {
        // Wheel up (64) at col 4,line 2 → 5;3.
        let up = mouse_report(true, 64, 4, 2, true, Modifiers::default(), false).unwrap();
        assert_eq!(up, b"\x1b[<64;5;3M");
        // Ctrl adds 16 to the button code.
        let ctrl = Modifiers { ctrl: true, ..Default::default() };
        let c = mouse_report(true, 0, 0, 0, true, ctrl, false).unwrap();
        assert_eq!(c, b"\x1b[<16;1;1M");
    }

    #[test]
    fn legacy_x10_bails_past_addressable_range() {
        // Column 300 (1-based 301) can't be encoded in legacy X10.
        assert!(mouse_report(false, 0, 300, 0, true, Modifiers::default(), false).is_none());
        // Within range it emits the 6-byte form.
        let m = mouse_report(false, 0, 0, 0, true, Modifiers::default(), false).unwrap();
        assert_eq!(m, vec![0x1b, b'[', b'M', 32, 33, 33]);
    }
}
