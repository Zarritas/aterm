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
│   ├── agent-sessions-cli/    # sidecar: envuelve el core y emite JSON por stdout
│   │   └── src/main.rs        #   scan/preview/resume-argv/new-argv/compact-argv/providers
│   └── aterm/                 # la app
│       └── src/
│           ├── main.rs        # entrada: instala fuentes/tema y lanza AtermApp
│           ├── app.rs         # AtermApp: chrome, tabs, splits, ratón/teclado, menús
│           ├── sessions.rs    # SessionPanel: panel izquierdo (scan/filtro/preview/…)
│           ├── theme.rs       # 10 paletas conmutables + apply() de egui Visuals
│           ├── settings.rs    # Settings persistidos (~/.config/aterm/settings.json)
│           ├── persist.rs     # recetas de pestañas → session.json (restaurar al abrir)
│           ├── service_status.rs # salud de proveedores vía curl a statuspage v2
│           └── term/          # núcleo del terminal
│               ├── mod.rs     #   TermInstance: PTY + Term + EventLoop + selección/búsqueda
│               ├── render.rs  #   grid de celdas → egui (EL GRUESO)
│               └── input.rs   #   tecla → bytes de escape · mouse_report SGR/X10
```

> **La extensión de VS Code vive ahora en un repo aparte:**
> [`Aterm-labs/agent-sessions`](https://github.com/Aterm-labs/agent-sessions)
> (la 2ª UI). Consume este repo como **git submodule** (`./aterm`) para compilar
> el sidecar `agent-sessions-cli`. Su edición **Pro** (open-core) está en el repo
> privado `aterm-pro`, y la landing en `aterm-web`. Lo que sigue describiendo la
> extensión en este documento es historia/contexto; el código y su doc viva
> están en esos repos.

**Segunda vía de UI (VS Code).** El mismo core `agent-sessions` alimenta dos
frontends: la app nativa `aterm` y una **extensión de VS Code**. En el editor el
terminal ya lo pone VS Code (`window.createTerminal`), así que la extensión solo
porta la *mitad gestor de sesiones*: un `TreeDataProvider` que llama al sidecar
`agent-sessions-cli` (JSON por stdout) y reanuda con el `resumeArgv` del proveedor.
El sidecar es read-only **para sesiones** (rutas derivadas del `$HOME`); sí
escribe en `~/.config/aterm/session-metadata.json` y en `~/.claude/projects/**`
al importar — los mismos sitios que la app nativa, así ambas UIs comparten
metadata y export/import. Distribución: `scripts/build-vsix.sh [<vsce-target>]`
construye el sidecar para esa plataforma y lo empaqueta en el `.vsix` bajo
`bin/<rust-triple>/`. La extensión auto-resuelve esa ruta en runtime.

**Reparto de superficie** (lo importante de entender):
- `alacritty_terminal` (dep) → modelo VT + PTY + parser + **read-loop en thread**. Gratis.
- `agent-sessions` (vendor, tuyo, ya escrito y testeado) → scan/resume/preview/tags/transfer.
- egui/eframe (dep) → ventana + shaping/rasterizado de fuente.
- **Tú escribes**: `render.rs` (~300-500 LoC), `input.rs` (~200-400), chrome/tabs
  (~600-1000), wiring (~200). Total nuevo MVP: **~1.5k-2.5k LoC**.

## Estado actual (2026-06-14)

- ✅ **Operativo (Fases 1-4)**: `cargo run -p aterm` abre la ventana nativa con el
  panel de sesiones a la izquierda, una barra de pestañas de terminales arriba y
  el grid del terminal activo en el centro.
- ✅ **Núcleo del terminal real**: `TermInstance` sobre `alacritty_terminal` 0.25.1
  (PTY + `Term` + `EventLoop`), render del grid a egui (paleta ANSI 16/256/truecolor,
  estilos, cursor, selección), input teclado→bytes, scrollback, zoom y copy/paste.
- ✅ **Panel con paridad funcional**: filas ricas, filtro, badges de quota, preview,
  rename/tags/color (metadata persistida), export/import y cleanup.
- ✅ **Tests verdes**: 60 en `agent-sessions` + 15 del crate `aterm` (e2e del núcleo:
  salida del hijo, input echo, exit code; + URL/www/mailto/file, ratón SGR/X10,
  helpers de sesión/status). Release con `lto thin` compila.
- ✅ **Salida del hijo**: al terminar (`exit`/Ctrl+D) la pestaña muestra `[exited N]`
  en el título vía `Event::ChildExit`; se cierra con ✕ (no autodestruye para poder
  leer el último output).
- ✅ **Fidelidad VT**: reporte de ratón SGR (clicks/drag/rueda → al hijo), bracketed
  paste, y alt-scroll (rueda→flechas en alt-screen). Ver `term::Modes` +
  `input::mouse_report`.
- ✅ **Splits**: varios terminales en rejilla (tabs con id estable; ⊞ alterna split;
  el pane enfocado recibe teclado). Divisores **arrastrables** para redimensionar
  (`split_dividers`/`shift_frac`, fracciones por columna/fila). Drag-and-drop para
  reordenar pestañas.
- ✅ **Atajos de pestañas (globales en `update`)**: `Ctrl+Tab`/`Ctrl+Shift+Tab`
  ciclan, `Alt+1..9` saltan a la N, `Ctrl+Shift+W` cierra (confirma si hay proceso),
  `Ctrl+Shift+T` reabre la última cerrada en su **cwd real** (`/proc/<pid>/cwd`).
- ✅ **Persistencia de pestañas entre arranques**: `persist.rs` guarda las recetas
  (argv, cwd vivo, key, name) en `~/.config/aterm/session.json` (throttled ~1.5s) y
  las reabre al iniciar. Un PTY vivo no se serializa; se relanza la receta.
- ✅ **Selección + ratón**: selección local sin "cuadrícula" (el rect solapa 1px),
  cursor I-beam sobre texto / mano sobre enlaces, clic central pega (X11), y **menú
  contextual con clic derecho** (`term_context_menu`: copiar/pegar/seleccionar todo/
  limpiar/enlaces/buscar; Shift fuerza local dentro de TUIs con mouse-reporting).
- ✅ **Búsqueda en scrollback (Ctrl+Shift+F)**: salta a la coincidencia y **resalta
  todas** las visibles (`TermInstance::viewport_matches` → `render::draw`).
- ✅ **Tema**: 10 paletas conmutables (Mocha, Tokyo Night, Dracula, Nord, Gruvbox,
  Solarized, One Dark, Rosé Pine, Monokai + **Catppuccin Latte claro**; `apply` elige
  base light/dark por luminancia). Color de marca por proveedor, badges de estado
  (statuspage) y quota/contexto coloreados por umbral (<40/40-60/≥60).
- ✅ **Extras de panel**: agrupación proveedor/proyecto/cascada, nombres de proyecto,
  buscar en el contenido de conversaciones (FTS), `move_session` cableado (mover sesión
  Claude a otro proyecto), auto-refresco cada 120s.
- ✅ **Ajustes** (`settings.rs`, ⚙): fuentes UI/terminal, proveedores a escanear,
  auto-cierre al exit, comando/dir de shell, cadencia de refresco, consulta de estado.
- ✅ **Extensión de VS Code (WebviewView)**: panel HTML/CSS/JS con **cards** a
  altura real (avatar de proveedor, dos líneas, acento lateral del color de
  proyecto, acciones al hover), no un TreeView. Filtro (con predicado `#tag`
  por click en badge) + **botones rápidos** en el header: «solo activos»
  (toggle `active:true`) y «por etiqueta» (popover con las tags en uso y su
  conteo, multi-selección que compone `#tag`). **Catálogo de tags** (setting
  `agentSessions.tagCatalog` + comando «Gestionar catálogo de etiquetas»): al
  asignar tags a una sesión se ofrece un QuickPick marcable de las predefinidas
  + usadas, en vez de escribirlas (con fallback a texto libre y «nueva
  etiqueta…»). Agrupado proveedor/proyecto/cascada/**fecha** (setting
  `agentSessions.groupBy`), metadata de sesión (rename/tags/color/**notas/
  favorito**), proyectos con alias y color, modelo visible, **quota del
  proveedor** como pill en el header, **borrar sesión** con confirmación y
  force-retry, **drag & drop** de Claude entre proyectos, **indicador
  "abierta"** que enfoca el terminal en vez de duplicar, **dashboard** de
  estadísticas (KPIs, barras por proveedor / top proyectos, sparkline 30d),
  export/import `.zip`. **Paridad con la app nativa** (2026-06-15): **compactar
  contexto** (acción »« en el menú contextual, solo Claude, vía `compact-argv`),
  **nueva sesión eligiendo cwd** (workspace / cwd conocido del proveedor con
  alias / otra ruta vía showOpenDialog; + acciones rápidas «nueva sesión aquí» y
  «abrir terminal aquí» en cada cabecera de bucket de proyecto), **plegar/desplegar
  todo** (botón en la barra), **terminal profiles por proveedor** (en el `+` del
  terminal), **paleta de acciones** (`Ctrl/Cmd+Alt+A` → quick-pick de sesiones),
  **copiar/guardar conversación** como Markdown, **modo compacto** (toggle de
  densidad), **plantillas con tags + cwd elegible**, **búsqueda en contenido
  (FTS) dentro del panel** (botón 🔍 → cards con snippet resaltado) y
  **configurar MCP con un clic** (escribe `.vscode/mcp.json` o copia el snippet), y tres settings: `scanProviders` (proveedores visibles, filtrado en
  display; el escaneo sigue), `fetchStatus` (interruptor de red para
  statuspage+quota, default on, paridad con `fetch_status` nativo) y `refreshSec`
  (auto-rescan completo periódico, 0 = off, <15 → 15). Estado UI (colapsado,
  filtro, scroll, dashboard on/off) persistido vía `vscode.setState`. Toda la
  persistencia comparte ficheros con la app nativa
  (`~/.config/aterm/{session-metadata.json, project-names.json}`). Sidecar
  empaquetado dentro del `.vsix` por plataforma vía `scripts/build-vsix.sh`.
  Comandos del sidecar: `scan`, `providers`, `preview`, `resume-argv`,
  `new-argv`, `compact-argv`, `metadata-{get,set,clear}`,
  `projects-{get,set,clear}`, `export`, `import`, `delete`, `move`, `backup`,
  `restore`, `service-status`, `live`, `search-content`, `templates-{get,set,delete}`,
  `serve` (MCP).
- ✅ **MCP server** (`agent-sessions-cli serve`, JSON-RPC sobre stdio,
  protocolo 2024-11-05): expone tools `list_sessions`, `get_session_turns`,
  `search_sessions` al propio agente — Claude/Codex/etc. pueden buscar en su
  historial sin que el usuario tenga que pegárselo.
- ✅ **Orquestación paralela con worktrees** (extensión, comandos
  `agentSessions.launchParallel` y `…cleanupWorktrees`): selección de N
  agentes + prompt → un `git worktree` por agente + terminal por worktree con
  el mismo prompt pegado. Es la feature killer de JetBrains Air, en OSS y en
  cualquier carpeta git.
- ✅ **Extensión — mejoras de issues (2026-06-16)**: implementadas **solo en la
  extensión de VS Code** (sin tocar el core Rust, para no arriesgar la interop
  con la app nativa). (#1) **compactar con instrucciones** (prompt opcional que
  se inyecta como `/compact <texto>` en el argv, lado extensión). (#2) **control
  de notificaciones**: setting `notificationLevel` (all/important/errors/none) +
  helpers `notifyInfo/notifyWarn/notifyError` (los diálogos modales interactivos
  nunca se suprimen, se detectan por sus items) + dedupe anti-flapping en los
  toasts de estado en vivo. (#3) **preview estilada**: panel webview reutilizable
  con cabecera de metadatos + turnos en burbujas y mini-render Markdown, en vez
  del documento `.md` plano. (#4) **borrado por fecha / multiselección**: modo
  selección con checkboxes y barra de acciones (abrir/eliminar N) + comando
  «Eliminar sesiones por fecha…». (#5) **abrir varias** (multiselección) y
  «Nueva sesión en varios proyectos…». (#6) **subproyectos + grupos
  manuales**: la vista por proyecto anida los cwd descendientes bajo su
  ancestro (jerarquía derivada del path); **además**, grupos/colecciones
  definidos por el usuario (nombre+color+icono) con su propia vista
  `groupBy=group`, asignación desde el menú de sesión, la barra de
  multiselección y **drag&drop** a un bucket de grupo, y «Gestionar grupos…».
  Persistidos en `globalState` (sin tocar la metadata compartida). (#7) **iconos/emojis** por sesión y por
  proyecto, guardados en `globalState` de la extensión (no en la metadata
  compartida). (#8) botón/comando «Añadir carpeta al workspace»
  (`updateWorkspaceFolders`). **Comandos del proyecto** (botón en la cabecera del
  bucket de proyecto + comando «Comandos del proyecto…»): un QuickPick que reúne
  los **slash-commands del agente** (`.claude/commands/**`, namespaced por
  subdir, descripción del frontmatter; lanza Claude en el cwd y envía `/cmd`),
  los **scripts del repo** (package.json con PM autodetectado, Makefile,
  justfile, Cargo) ejecutados en terminal, y las **acciones de la extensión**
  por proyecto. **Menú de acciones «⋯»** en la toolbar del panel (`actionsMenu`):
  QuickPick con *todas* las acciones de la extensión agrupadas (lanzar,
  buscar/filtrar, proyectos, grupos/etiquetas, plantillas, mantenimiento), para
  usarlas sin la paleta de comandos. Bug de paso: `currentGroupMode()` ya
  conserva el modo `date`.
  **% de contexto**: el core infiere la ventana de Claude (200k, sube a 1M solo
  si el uso supera 200k), lo que **infla el %** para cuentas con ventana de 1M
  cuando el uso está por debajo de 200k (los logs no registran la ventana ni el
  flag `[1m]`). Mitigado con el setting `claudeContextWindow` (auto/200k/1m),
  aplicado en card, filtro `ctx` y preview — sin tocar el core.
- ✅ **Modelo open-core / Pro (extensión, v1.2.0)**: la extensión de VS Code es
  open-core. Gate en `vscode-extension/src/license.ts` (`LicenseService`): prueba
  de 14 días + verificación de licencia **Ed25519 offline** (clave pública
  embebida; sin servidor). Helper `requirePro(feature)` con upsell. Features
  **Pro**: comparativa paralela (`launchParallel`/`compareWorktrees`/
  `cleanupWorktrees`) y plantillas (`saveTemplate`/`runTemplate`/`manageTemplates`).
  Comandos `activateLicense`/`proStatus` (+ `debugPro` para QA, quitar antes del
  release público). Validación de licencias vía **Lemon Squeezy License API**
  (online, sin servidor propio ni secretos en el cliente: `activate`/`validate`
  con la propia key) cacheada en `globalState` y tolerante a estar offline.
  Pendiente: `BUY_URL`/`PRODUCT_ID` reales (constantes en `license.ts`).
- ✅ **Split open-core (público Community ↔ privado Pro)**: el repo público es la
  **Community Edition**. El *source* de las features Pro (comparativa paralela y
  plantillas) ya **no vive en el público**: se movió al repo privado
  `../aterm-pro`. Contrato en `src/pro-api.d.ts` (interfaces `ProApi` que el core
  expone + `ProModule` que el privado implementa); `extension.ts` carga
  dinámicamente `require("./pro")` (presente solo en la build oficial) y, si
  falta, las acciones Pro muestran «edición Community». `aterm-pro/` tiene el
  módulo `pro/index.ts`, su `tsconfig` (emite plano a `out/pro/` del público) y
  `build.sh` (compila público + Pro y empaqueta el `.vsix` oficial). Topología
  hermana `../aterm` por defecto (convertible a git submodule). El history MIT
  previo conserva esas features; el split protege el desarrollo futuro.
  **Features Pro** (en `aterm-pro/pro/index.ts`): comparativa paralela,
  plantillas, **perfiles de espacio de trabajo** (guardar/abrir conjuntos de
  sesiones), **dashboard Pro** (informe webview con gráficas + export CSV;
  presupuestos reservados para tier Team), **exportar conversación a HTML** y
  **automatizaciones** (watcher de
  idle vía `ProModule.activate` + resumen diario). El contrato `ProApi`/
  `ProModule` (`src/pro-api.d.ts`) creció con `sessions`/`resume`/`getState`/
  `setState`/`addDisposable`. Refinamientos del gate: indicador en barra de
  estado, revalidación periódica (12h), onboarding y comando `(debug)` oculto
  salvo en `ExtensionMode.Development` (context key `agentSessions.dev`).
- ⏳ **Fase 5 (render GPU)**: no hecha por diseño — opcional, solo si el throughput
  lo justifica (ver roadmap).
- ⏳ **Pendientes menores**: import solo a Claude (el `.zip` es formato Claude);
  borrar sesión y filtro por tag en la extensión.

## Roadmap (fases)

> Patrón validado en el fork de Warp: cada fase compila (`cargo check`) + commit +
> prueba visual. Aquí igual.

- ✅ **Fase 1 — núcleo del terminal**: `TermInstance::spawn` con `tty::new` +
  `EventLoop`, render del grid en `render.rs`, input en `input.rs`. Un PTY con un
  shell pintándose en el panel central. (API fijada contra `alacritty_terminal` 0.25.1.)
- ✅ **Fase 2 — resume real**: el botón ▶ abre un PTY con el `resume_argv` bajo el
  `cwd` de la sesión; «＋ Nueva sesión» usa `new_session_argv` del proveedor.
- ✅ **Fase 3 — chrome**: tabs (nueva/cerrar/activar/reordenar), foco con focus-lock,
  copy/paste (`arboard`, Ctrl+Shift+C/V + copia al soltar selección + menú contextual),
  scrollback con rueda, resize → `WindowSize` del PTY, zoom de fuente (Ctrl +/-/0),
  splits redimensionables, atajos de pestañas y persistencia de sesión.
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
- **Atajos globales en `update()` con `consume_key`** (Ctrl+Tab, Alt+1..9, Ctrl+Shift+T/W):
  el handler por-pane solo corre con un terminal enfocado, así que los atajos que deben
  funcionar sin foco (p.ej. reabrir tras cerrar la última pestaña) van a nivel app.
- **cwd vivo de la shell vía `/proc/<pid>/cwd`** (solo Linux): `TermInstance::cwd()` lo
  usa la persistencia y el reopen para volver donde estabas tras los `cd`. `None` fuera
  de Linux → cae al cwd de lanzamiento.
- **Menú contextual y selección local**: solo disponibles cuando el hijo NO captura el
  ratón (sin mouse-reporting) o con Shift; el flag `ctx_menu_open` mantiene el menú
  abierto aunque se suelte Shift dentro de una TUI.
- Formatos de sesión por proveedor (ya implementados en `providers/`): Claude
  `~/.claude/projects/**.jsonl` + registro vivo `~/.claude/sessions/*.json`; Codex
  `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`; OpenCode `opencode session list
  --format json`; Gemini `~/.gemini/tmp/<projectId>/chats/session-*.jsonl`.

## Comandos

```bash
cargo run -p aterm            # arrancar la app
cargo check                   # validación rápida del workspace
cargo test --workspace        # 60 (agent-sessions) + 15 del crate aterm (núcleo + input + helpers)
cargo build --release         # binario optimizado (lto thin)
```

## Sincronizar el vendor con upstream

`agent-sessions` es copia verbatim de `../warp/crates/warp_agent_history/src/`.
Si mejoras la lógica de sesiones allí (o al revés), re-copia y re-aplica las tres
divergencias: (1) quitar `pub mod service_status;` de `lib.rs`; (2) declarar
`windows-sys` como dep `[target.'cfg(windows)'.dependencies]` en `Cargo.toml`
(la usa `live.rs::pid_alive`; upstream la hereda del workspace de Warp y al
vendorizar se pierde → rompe el build de Windows); (3) el campo
`SessionMetadata::persisted` en `metadata.rs` (flag de archivado durable; va en
`is_empty()`) — replicarlo upstream para no divergir. La interop de export/import
es **byte-compatible** con multi-claude y el panel de Terax — no romper el manifest.
