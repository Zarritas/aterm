# aterm — guía para Claude Code

> Terminal nativo (Rust) con un gestor de sesiones de agentes integrado.
> Repo nuevo creado el 2026-06-09. Idioma de trabajo: **español**.

## Qué es y por qué existe

`aterm` es la **tercera vía** de un linaje de trabajo previo:

1. **multi-claude** (`../multi-claude`, Python/Textual) — TUI que navega y reanuda
   sesiones de Claude Code. El origen de toda la lógica.
2. **Fork de Terax** (`../terax-ai`, Tauri 2 + Rust/React) — el mismo gestor como
   panel "Agent Sessions" nativo, **multi-proveedor** (Claude/Codex/OpenCode/Gemini).
3. **Fork de Warp** (`../warp`, Rust nativo + GPU) — el panel portado a Rust; de ahí
   sale el crate `warp_agent_history`, vendorizado aquí como `agent-sessions`.

Las vías 2 y 3 son **forks de repos enormes y activos** → deuda de rebase perpetua.
`aterm` nace para tener **algo propio, mínimo y 100% editable, sin esa deuda**:
en vez de forkear un terminal, **embebe un emulador de terminal como librería**
(`alacritty_terminal`) y le añade el panel de sesiones (`agent-sessions`).

### Por qué nativo (no web)

Decisión tomada: **nativo es más liviano**. Referencia: Alacritty corre en ~40-80 MB
RAM; una app Tauri+xterm.js andaría en ~100-180 MB; Electron en 300-500 MB.
`alacritty_terminal` vuelca el grid a un render propio; xterm.js pinta vía DOM/canvas
dentro de un motor web, y sufre con TUIs que repintan agresivo (claude/codex).
Coste: la UI en Rust es más trabajo que reusar el React del fork de Terax — ese es
el trade-off aceptado.

## Arquitectura

```
aterm/                         # workspace Cargo
├── crates/
│   ├── agent-sessions/        # VENDOR verbatim de warp_agent_history (read-only)
│   │   └── src/{providers/*, extract, live, metadata, transfer, provider, types}
│   └── aterm/                 # la app
│       └── src/
│           ├── main.rs        # app egui (ventana + panel de sesiones)  ← FUNCIONA
│           └── term/          # núcleo del terminal                     ← REFERENCIA
│               ├── mod.rs     #   TermInstance: PTY + Term + EventLoop
│               ├── render.rs  #   grid de celdas → egui (EL GRUESO)
│               └── input.rs   #   tecla → bytes de escape
```

**Reparto de superficie** (lo importante de entender):
- `alacritty_terminal` (dep) → modelo VT + PTY + parser + **read-loop en thread**. Gratis.
- `agent-sessions` (vendor, tuyo, ya escrito y testeado) → scan/resume/preview/tags/transfer.
- egui/eframe (dep) → ventana + shaping/rasterizado de fuente.
- **Tú escribes**: `render.rs` (~300-500 LoC), `input.rs` (~200-400), chrome/tabs
  (~600-1000), wiring (~200). Total nuevo MVP: **~1.5k-2.5k LoC**.

## Estado actual (2026-06-09)

- ✅ **Operativo (Fases 1-4)**: `cargo run -p aterm` abre la ventana nativa con el
  panel de sesiones a la izquierda, una barra de pestañas de terminales arriba y
  el grid del terminal activo en el centro.
- ✅ **Núcleo del terminal real**: `TermInstance` sobre `alacritty_terminal` 0.25.1
  (PTY + `Term` + `EventLoop`), render del grid a egui (paleta ANSI 16/256/truecolor,
  estilos, cursor, selección), input teclado→bytes, scrollback, zoom y copy/paste.
- ✅ **Panel con paridad funcional**: filas ricas, filtro, badges de quota, preview,
  rename/tags/color (metadata persistida), export/import y cleanup.
- ✅ **Tests verdes**: 59 en `agent-sessions` + 3 e2e del núcleo del terminal (salida
  del hijo en el grid, input echo, código de salida). Release con `lto thin` compila.
- ✅ **Salida del hijo**: al terminar (`exit`/Ctrl+D) la pestaña muestra `[exited N]`
  en el título vía `Event::ChildExit`; se cierra con ✕ (no autodestruye para poder
  leer el último output).
- ⏳ **Fase 5 (render GPU)**: no hecha por diseño — opcional, solo si el throughput
  lo justifica (ver roadmap).
- ⏳ **Pendiente menor conocido**: `transfer::move_session` (re-ruteo de proyecto
  Claude) no está cableado en la UI (nicho).

## Roadmap (fases)

> Patrón validado en el fork de Warp: cada fase compila (`cargo check`) + commit +
> prueba visual. Aquí igual.

- ✅ **Fase 1 — núcleo del terminal**: `TermInstance::spawn` con `tty::new` +
  `EventLoop`, render del grid en `render.rs`, input en `input.rs`. Un PTY con un
  shell pintándose en el panel central. (API fijada contra `alacritty_terminal` 0.25.1.)
- ✅ **Fase 2 — resume real**: el botón ▶ abre un PTY con el `resume_argv` bajo el
  `cwd` de la sesión; «＋ Nueva sesión» usa `new_session_argv` del proveedor.
- ✅ **Fase 3 — chrome**: tabs (nueva/cerrar/activar), foco con focus-lock,
  copy/paste (`arboard`, Ctrl+Shift+C/V + copia al soltar selección), scrollback con
  rueda, resize → `WindowSize` del PTY, zoom de fuente (Ctrl +/-/0). Splits: pendiente.
- ✅ **Fase 4 — paridad con el panel de Warp/Terax**: filas ricas (modelo, branch,
  % contexto, msgs, tiempo relativo), filtro, preview de conversación, rename/tags/color
  (`metadata.rs`), export/import (`transfer.rs`), quota badges, cleanup. `move_session`
  queda sin cablear en UI (re-ruteo de proyecto Claude, nicho).
- ⏳ **Fase 5 — render GPU** (opcional): migrar `render.rs` de egui-painter a un atlas
  de glifos wgpu solo si el throughput en TUIs pesadas lo justifica. No antes.

## Gotchas / decisiones

- **API de `alacritty_terminal` se mueve entre versiones.** Ya fijada contra 0.25.1
  en `term/`; si subes la versión, re-valida con `cargo check` (cambian nombres/firmas).
- **Salud de servicios**: implementada en `aterm/src/service_status.rs` (NO el
  vendor). Para evitar la dep de red pesada (reqwest), hace `curl` a las statuspage
  v2 de Claude/OpenAI; best-effort (None si no hay curl/red). Badge por proveedor
  en el panel. Otros proveedores (opencode/gemini) no publican statuspage.
- **`agent-sessions` es read-only por diseño**: los providers derivan rutas del HOME,
  nunca aceptan paths del caller. Mantener esa propiedad.
- **No re-implementar el VT loop**: usar `EventLoop` de alacritty_terminal (te da el
  thread de lectura/parseo). Tu listener solo reacciona a `Wakeup`/`PtyWrite`/`Title`.
- **Render primero con egui-painter** (monoespaciado, suficiente). wgpu solo si pesa.
- Formatos de sesión por proveedor (ya implementados en `providers/`): Claude
  `~/.claude/projects/**.jsonl` + registro vivo `~/.claude/sessions/*.json`; Codex
  `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`; OpenCode `opencode session list
  --format json`; Gemini `~/.gemini/tmp/<projectId>/chats/session-*.jsonl`.

## Comandos

```bash
cargo run -p aterm            # arrancar la app
cargo check                   # validación rápida del workspace
cargo test --workspace        # 59 (agent-sessions) + 2 e2e del núcleo del terminal
cargo build --release         # binario optimizado (lto thin)
```

## Sincronizar el vendor con upstream

`agent-sessions` es copia verbatim de `../warp/crates/warp_agent_history/src/`.
Si mejoras la lógica de sesiones allí (o al revés), re-copia y re-aplica la única
divergencia: quitar `pub mod service_status;` de `lib.rs`. La interop de export/import
es **byte-compatible** con multi-claude y el panel de Terax — no romper el manifest.
