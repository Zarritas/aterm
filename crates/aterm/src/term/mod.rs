//! Terminal core: one PTY + VT model per tab, driven by alacritty_terminal.
//!
//! What alacritty_terminal gives us for free (we do NOT reimplement):
//!   - `Term`            the cell grid + cursor + scrollback model
//!   - `tty::new`        spawns the child process under a real PTY
//!   - `EventLoop`       a background thread that reads the PTY, feeds the VT
//!                       parser, and mutates `Term`. We only react to events.
//!
//! What we DO write: render.rs (cells → egui) and input.rs (keys → bytes).
//!
//! API pinned against alacritty_terminal 0.25.1 (see CLAUDE.md "Gotchas").

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{
    Event, EventListener, Notify, OnResize, WindowSize,
};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};

pub mod input;
pub mod render;

/// Visible terminal size in cells + the pixel cell metrics the PTY needs for
/// SIGWINCH (`WindowSize`). Recompute from the egui font + panel rect.
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub lines: usize,
    pub cell_width: f32,
    pub cell_height: f32,
}

impl TermSize {
    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_cols: self.columns as u16,
            num_lines: self.lines as u16,
            cell_width: self.cell_width.max(1.0) as u16,
            cell_height: self.cell_height.max(1.0) as u16,
        }
    }
}

/// `Term` is generic over a type implementing `Dimensions`; `TermSize` plays
/// that role both at construction and on every resize. A fresh grid carries no
/// scrollback, so `total_lines == screen_lines`; history grows internally as
/// the child scrolls past the top.
impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// VT mode flags the input/render layer reads each frame.
#[derive(Clone, Copy)]
pub struct Modes {
    pub app_cursor: bool,
    /// Any mouse-reporting mode is on (forward clicks/wheel to the child).
    pub mouse_report: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
}

/// A live terminal: the shared VT model plus the channel to write input.
///
/// `term` is an `Arc<FairMutex<Term<_>>>`: the EventLoop thread mutates it on
/// PTY output; the UI thread locks it briefly each frame to render.
pub struct TermInstance {
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    title: Arc<Mutex<String>>,
    exit_code: Arc<Mutex<Option<i32>>>,
    fallback_title: String,
    /// PTY master fd + the shell's pid, to detect a foreground command (a
    /// process group other than the shell's own owns the terminal).
    master_fd: i32,
    shell_pid: i32,
    pub size: TermSize,
}

impl TermInstance {
    /// Spawn `argv` (e.g. `["claude", "--resume", id]`) under a PTY in `cwd`.
    ///
    /// `ctx` is the egui context: the event proxy calls `request_repaint` on it
    /// whenever the child produces output, so the UI redraws without polling.
    pub fn spawn(
        argv: Vec<String>,
        cwd: Option<std::path::PathBuf>,
        size: TermSize,
        ctx: egui::Context,
    ) -> std::io::Result<Self> {
        if argv.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty argv",
            ));
        }

        let shell = Shell::new(argv[0].clone(), argv[1..].to_vec());
        let options = PtyOptions {
            shell: Some(shell),
            working_directory: cwd,
            drain_on_exit: false,
            env: HashMap::new(),
        };

        let pty = tty::new(&options, size.window_size(), 0)?;
        // Grab the master fd + shell pid before the PTY moves into the loop.
        let master_fd = {
            use std::os::fd::AsRawFd;
            pty.file().as_raw_fd()
        };
        let shell_pid = pty.child().id() as i32;

        let proxy = EventProxy::new(ctx);
        let term = Term::new(Config::default(), &size, proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(term.clone(), proxy.clone(), pty, false, false)?;
        // Two independent handles onto the same channel: one feeds device
        // responses (PtyWrite) from the proxy, one drives input/resize here.
        proxy.attach_sender(event_loop.channel());
        let notifier = Notifier(event_loop.channel());

        // Background read/parse thread. We deliberately drop the JoinHandle:
        // the thread owns the PTY and lives until it sees EOF or `Msg::Shutdown`
        // (sent from `Drop`).
        let _handle = event_loop.spawn();

        Ok(Self {
            term,
            notifier,
            title: proxy.title,
            exit_code: proxy.exit_code,
            fallback_title: argv.join(" "),
            master_fd,
            shell_pid,
            size,
        })
    }

    /// True when a foreground command (other than the shell itself) currently
    /// owns the terminal — i.e. there's a running process worth confirming
    /// before closing. Best-effort via `tcgetpgrp`.
    pub fn has_foreground_process(&self) -> bool {
        if self.exit_code().is_some() {
            return false;
        }
        let pgrp = unsafe { libc::tcgetpgrp(self.master_fd) };
        pgrp > 0 && pgrp != self.shell_pid
    }

    /// Send raw bytes to the child (from input.rs key mapping).
    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.notifier.notify(bytes.to_vec());
    }

    /// Tell `Term` and the PTY that the visible size changed (panel resize).
    pub fn resize(&mut self, size: TermSize) {
        self.size = size;
        self.term.lock().resize(size);
        self.notifier.on_resize(size.window_size());
    }

    /// Snapshot of the VT modes the input layer needs this frame (one lock).
    pub fn modes(&self) -> Modes {
        let term = self.term.lock();
        let m = *term.mode();
        Modes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            mouse_report: m
                .intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
            mouse_drag: m.contains(TermMode::MOUSE_DRAG),
            mouse_motion: m.contains(TermMode::MOUSE_MOTION),
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
        }
    }

    /// Scroll the viewport by `lines` (positive = towards history / older).
    pub fn scroll(&self, lines: i32) {
        if lines != 0 {
            self.term.lock().scroll_display(Scroll::Delta(lines));
        }
    }

    /// How far the viewport is scrolled into scrollback (0 = at the bottom).
    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    /// Begin a simple (linear) selection anchored at `point`.
    pub fn start_selection(&self, point: Point, side: Side) {
        let mut term = self.term.lock();
        term.selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    /// Extend the in-progress selection to `point`.
    pub fn update_selection(&self, point: Point, side: Side) {
        let mut term = self.term.lock();
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Drop any active selection.
    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// The selected text, if any, in reading order.
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// Search the grid + scrollback upward for `query` (case-insensitive),
    /// starting above `before_line` (or from the bottom when `None`). On a hit,
    /// scrolls it into view, selects it, and returns the matched buffer line so
    /// the caller can continue searching further up.
    pub fn search_up(&self, query: &str, before_line: Option<i32>) -> Option<i32> {
        if query.is_empty() {
            return None;
        }
        let needle = query.to_lowercase();
        let mut term = self.term.lock();

        let (line, col, cols) = {
            let grid = term.grid();
            let top = grid.topmost_line().0;
            let bottom = grid.bottommost_line().0;
            let cols = grid.columns();
            let start = before_line.map_or(bottom, |l| l - 1).min(bottom);
            let mut hit = None;
            let mut l = start;
            while l >= top {
                let row = &grid[Line(l)];
                let text: String = (0..cols).map(|c| row[Column(c)].c).collect();
                if let Some(pos) = text.to_lowercase().find(&needle) {
                    // Byte offset → column (cell) index.
                    let col = text[..pos].chars().count();
                    hit = Some((l, col));
                    break;
                }
                l -= 1;
            }
            match hit {
                Some((l, c)) => (l, c, cols),
                None => return None,
            }
        };

        let point = Point::new(Line(line), Column(col));
        term.scroll_to_point(point);
        let end = (col + needle.chars().count()).saturating_sub(1).min(cols - 1);
        let mut sel = Selection::new(SelectionType::Simple, point, Side::Left);
        sel.update(Point::new(Line(line), Column(end)), Side::Right);
        term.selection = Some(sel);
        Some(line)
    }

    /// Tab label: the OSC-2 title set by the child, else the spawn command,
    /// with an `[exited N]` suffix once the child has terminated.
    pub fn title(&self) -> String {
        let live = self.title.lock().unwrap();
        let base = if live.is_empty() {
            self.fallback_title.clone()
        } else {
            live.clone()
        };
        match *self.exit_code.lock().unwrap() {
            Some(code) => format!("{base} [exited {code}]"),
            None => base,
        }
    }

    /// The child's exit code, once it has terminated.
    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock().unwrap()
    }
}

impl Drop for TermInstance {
    fn drop(&mut self) {
        // Best-effort: ask the read thread to stop and reap the child.
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// egui-side event sink for alacritty_terminal. On `Wakeup`/`Title` it requests
/// a repaint; `PtyWrite` (device-status responses, clipboard pulls, …) is
/// forwarded straight back to the PTY through the shared channel.
///
/// The sender slot is filled *after* construction (`attach_sender`) because the
/// `EventLoop` — which owns the channel — is itself built from this proxy.
#[derive(Clone)]
pub struct EventProxy {
    ctx: egui::Context,
    sender: Arc<Mutex<Option<EventLoopSender>>>,
    title: Arc<Mutex<String>>,
    exit_code: Arc<Mutex<Option<i32>>>,
}

impl EventProxy {
    fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            sender: Arc::new(Mutex::new(None)),
            title: Arc::new(Mutex::new(String::new())),
            exit_code: Arc::new(Mutex::new(None)),
        }
    }

    fn attach_sender(&self, sender: EventLoopSender) {
        *self.sender.lock().unwrap() = Some(sender);
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => self.ctx.request_repaint(),
            Event::PtyWrite(text) => {
                if let Some(sender) = self.sender.lock().unwrap().as_ref() {
                    let _ = sender.send(Msg::Input(text.into_bytes().into()));
                }
            }
            Event::Title(title) => {
                *self.title.lock().unwrap() = title;
                self.ctx.request_repaint();
            }
            Event::ResetTitle => {
                self.title.lock().unwrap().clear();
                self.ctx.request_repaint();
            }
            Event::Bell => self.ctx.request_repaint(),
            Event::ChildExit(code) => {
                *self.exit_code.lock().unwrap() = Some(code);
                self.ctx.request_repaint();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end smoke test of the core wiring: spawn a real child under a
    /// PTY, let the EventLoop thread parse its output into `Term`, and confirm
    /// the bytes landed in the grid. Exercises tty::new + EventLoop + Term
    /// without an actual window.
    #[test]
    fn child_output_reaches_the_grid() {
        let size = TermSize {
            columns: 80,
            lines: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let term = TermInstance::spawn(
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf MARKER; sleep 1".to_string(),
            ],
            None,
            size,
            egui::Context::default(),
        )
        .expect("spawn");

        // Poll the grid for up to ~2s for the child's output to arrive.
        let mut found = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let guard = term.term.lock();
            let text: String = guard
                .renderable_content()
                .display_iter
                .map(|c| c.cell.c)
                .collect();
            drop(guard);
            if text.contains("MARKER") {
                found = true;
                break;
            }
        }
        assert!(found, "child output never appeared in the terminal grid");
    }

    /// Round-trip the input path: type a command into the PTY and confirm the
    /// shell echoes it back into the grid. Exercises `write` → EventLoop → Term.
    #[test]
    fn typed_input_is_echoed_into_the_grid() {
        let size = TermSize {
            columns: 80,
            lines: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let term = TermInstance::spawn(
            vec!["/bin/sh".to_string()],
            None,
            size,
            egui::Context::default(),
        )
        .expect("spawn");

        // Let the shell come up, then type a command that prints a marker.
        std::thread::sleep(std::time::Duration::from_millis(200));
        term.write(b"echo XYZZY_OK\n");

        let mut found = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let guard = term.term.lock();
            let text: String = guard
                .renderable_content()
                .display_iter
                .map(|c| c.cell.c)
                .collect();
            drop(guard);
            if text.contains("XYZZY_OK") {
                found = true;
                break;
            }
        }
        assert!(found, "typed command never echoed into the grid");
    }

    /// The child's exit code propagates to `exit_code()` (drives the
    /// `[exited N]` tab suffix).
    #[test]
    fn child_exit_code_is_observed() {
        let size = TermSize {
            columns: 80,
            lines: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let term = TermInstance::spawn(
            vec!["/bin/sh".to_string(), "-c".to_string(), "exit 7".to_string()],
            None,
            size,
            egui::Context::default(),
        )
        .expect("spawn");

        let mut code = None;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if let Some(c) = term.exit_code() {
                code = Some(c);
                break;
            }
        }
        assert_eq!(code, Some(7), "child exit code never observed");
    }
}
