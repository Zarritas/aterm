//! Keyboard → PTY bytes. Acotado pero traicionero: porta casi 1:1 del módulo
//! `input` de Alacritty. The child expects terminfo-correct escape sequences,
//! and several depend on terminal *modes* (application cursor/keypad) that
//! `Term` tracks — read them off `term.mode()` before encoding.
//!
//! STATUS: reference skeleton (~200-400 LoC when complete).
//!
//! ```ignore
//! pub fn key_to_bytes(key: egui::Key, mods: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
//!     use egui::Key::*;
//!     let b: &[u8] = match key {
//!         Enter                    => b"\r",
//!         Backspace                => b"\x7f",
//!         Escape                   => b"\x1b",
//!         Tab                      => b"\t",
//!         ArrowUp    if app_cursor => b"\x1bOA",
//!         ArrowUp                  => b"\x1b[A",
//!         ArrowDown  if app_cursor => b"\x1bOB",
//!         ArrowDown                => b"\x1b[B",
//!         ArrowRight if app_cursor => b"\x1bOC",
//!         ArrowRight               => b"\x1b[C",
//!         ArrowLeft  if app_cursor => b"\x1bOD",
//!         ArrowLeft                => b"\x1b[D",
//!         Home                     => b"\x1b[H",
//!         End                      => b"\x1b[F",
//!         // F-keys, PageUp/Down, Insert/Delete: CSI sequences.
//!         _ => return None,
//!     };
//!     // Ctrl+letter → control byte (Ctrl+A = 0x01, …); handled separately from
//!     // egui's `Event::Text` which already delivers typed characters.
//!     Some(b.to_vec())
//! }
//! ```
//!
//! Wiring in the app's `update`:
//!   - `egui::Event::Text(s)` for printable input → `term_instance.write(s.as_bytes())`
//!   - `egui::Event::Key { key, pressed: true, modifiers, .. }` → `key_to_bytes(...)`
//!   - apply `term.mode().contains(TermMode::APP_CURSOR)` for the app_cursor flag
//!   - mouse reporting (SGR) is a later refinement, gated on `TermMode` mouse bits.
