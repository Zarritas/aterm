//! Terminal core: one PTY + VT model per tab, driven by alacritty_terminal.
//!
//! STATUS: reference skeleton (not yet compiled — `mod term;` is commented out
//! in main.rs). Activating this is Phase 1. The alacritty_terminal API moves
//! between releases; treat the calls below as the shape, and fix exact names
//! against the installed version with `cargo check`.
//!
//! What alacritty_terminal gives you for free (do NOT reimplement):
//!   - `Term`            the cell grid + cursor + scrollback model
//!   - `tty::new`        spawns the child process under a real PTY
//!   - `EventLoop`       a background thread that reads the PTY, feeds the VT
//!                       parser, and mutates `Term`. You only react to events.
//!
//! What you DO write: render.rs (cells → egui) and input.rs (keys → bytes).

use std::sync::Arc;

// NOTE: exact import paths depend on the alacritty_terminal version.
// As of ~0.25 these live roughly at:
//   alacritty_terminal::{Term, tty, event::{Event, EventListener, WindowSize},
//                        event_loop::{EventLoop, Notifier, Msg}, sync::FairMutex,
//                        term::Config, grid::Dimensions}
// Pin them when you uncomment `mod term;`.

/// Visible terminal size in cells + the pixel cell metrics the PTY needs for
/// SIGWINCH (`WindowSize`). Recompute from the egui font + panel rect.
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub lines: usize,
    pub cell_width: f32,
    pub cell_height: f32,
}

/// A live terminal: the shared VT model plus the channel to write input.
///
/// `term` is an `Arc<FairMutex<Term<_>>>`: the EventLoop thread mutates it on
/// PTY output; the UI thread locks it briefly each frame to render.
pub struct TermInstance {
    // pub term: Arc<FairMutex<Term<EventProxy>>>,
    // pub notifier: Notifier,
    pub title: String,
    _marker: std::marker::PhantomData<Arc<()>>,
}

impl TermInstance {
    /// Spawn `argv` (e.g. `["claude", "--resume", id]`) under a PTY in `cwd`.
    ///
    /// ```ignore
    /// let pty_opts = tty::Options {
    ///     shell: Some(tty::Shell::new(argv[0].clone(), argv[1..].to_vec())),
    ///     working_directory: Some(cwd),
    ///     ..Default::default()
    /// };
    /// let win = WindowSize { num_cols: size.columns as u16, num_lines: size.lines as u16,
    ///                        cell_width: size.cell_width as u16, cell_height: size.cell_height as u16 };
    /// let pty = tty::new(&pty_opts, win, 0)?;
    /// let term = Arc::new(FairMutex::new(Term::new(Config::default(), &size, proxy.clone())));
    /// let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)?;
    /// let notifier = Notifier(event_loop.channel());
    /// event_loop.spawn(); // background read/parse thread
    /// ```
    pub fn spawn(_argv: Vec<String>, _cwd: std::path::PathBuf, _size: TermSize) -> Self {
        unimplemented!("Phase 1: wire alacritty_terminal::tty + EventLoop here")
    }

    /// Send raw bytes to the child (from input.rs key mapping).
    /// `self.notifier.0.send(Msg::Input(bytes.into()))`.
    pub fn write(&self, _bytes: &[u8]) {
        unimplemented!()
    }

    /// Tell the PTY the visible size changed (after a panel resize).
    /// `self.notifier.0.send(Msg::Resize(window_size))`.
    pub fn resize(&self, _size: TermSize) {
        unimplemented!()
    }
}

/// egui-side event sink for alacritty_terminal. On `Wakeup` it requests a
/// repaint; `PtyWrite` must be forwarded back to the notifier; `Title` updates
/// the tab. Implement `EventListener` for this once the types are imported.
///
/// ```ignore
/// #[derive(Clone)]
/// pub struct EventProxy(pub egui::Context);
/// impl EventListener for EventProxy {
///     fn send_event(&self, event: Event) {
///         match event {
///             Event::Wakeup => self.0.request_repaint(),
///             Event::PtyWrite(text) => { /* notifier.send(Msg::Input(text.into_bytes())) */ }
///             Event::Title(t) => { /* store on the tab */ }
///             _ => {}
///         }
///     }
/// }
/// ```
pub struct EventProxy;

pub mod input;
pub mod render;
