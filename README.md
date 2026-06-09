# aterm

Native terminal (Rust) with a built-in coding-agent **session manager** for
Claude Code, Codex, OpenCode and Gemini.

A small, fully-owned alternative to forking a whole terminal: it embeds
[`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) as a library
for the VT core and adds a session panel on top, instead of carrying the
rebase debt of a Terax or Warp fork.

## Status

Early scaffold. **Working today:** a native [egui](https://github.com/emilk/egui)
window that scans and lists your real agent sessions by provider, with a Resume
action. **Next:** wiring the terminal grid itself (see `crates/aterm/src/term/`).

Full design, rationale and roadmap: [`CLAUDE.md`](./CLAUDE.md).

## Run

```bash
cargo run -p aterm
```

## Layout

- `crates/agent-sessions` — read-only session discovery (vendored, 59 tests).
- `crates/aterm` — the app: egui window + session panel + (WIP) terminal core.

## License

MIT.
