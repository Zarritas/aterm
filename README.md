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
- `crates/agent-sessions-cli` — JSON sidecar over the core (used by the VS Code extension).

## Related repos (org `Aterm-labs`)

- [`agent-sessions`](https://github.com/Aterm-labs/agent-sessions) — the VS Code
  extension (second UI). Consumes this repo as a git submodule for the sidecar.
- `aterm-pro` (private) — the Pro module for the extension (open-core).
- [`aterm-web`](https://github.com/Aterm-labs/aterm-web) — the product landing.

## License

MIT.
