# Agent Sessions — extensión de VS Code

Lista, previsualiza y **reanuda en el terminal integrado** tus sesiones de
agentes de código (Claude Code, Codex, OpenCode, Gemini).

Es la mitad "gestor de sesiones" del proyecto [`aterm`](../README.md) portada a
VS Code. El editor ya pone el terminal, así que la extensión solo aporta la UI
(un **WebviewView** en la barra lateral con cards a tamaño real, no un
TreeView) y delega todo el descubrimiento, metadata y transferencia en el
binario sidecar `agent-sessions-cli`, que comparte el núcleo Rust
(`agent-sessions`) con la app nativa. Una sola fuente de verdad.

## Arquitectura

```
VS Code (TypeScript)                  Rust (compartido con aterm)
┌─────────────────────────┐  spawn   ┌────────────────────────────┐
│ WebviewView (cards)      │ ───────▶ │ agent-sessions-cli (JSON)  │
│ comando "Reanudar"       │ ◀─────── │  └─ agent-sessions (core)  │
│  └─ window.createTerminal│  stdout  └────────────────────────────┘
└─────────────────────────┘
```

El sidecar es **read-only para las sesiones** (rutas derivadas del `$HOME`,
nunca recibe paths del editor). Sí escribe en `~/.config/aterm/session-metadata.json`
(rename/tags/color) y en `~/.claude/projects/**` al importar — los mismos sitios
que usa la app nativa, así ambas UIs ven la misma metadata.

> Funciona en VS Code y en sus forks (Cursor, VSCodium, Windsurf, …): se publica
> en el Marketplace de VS Code y en [Open VSX](https://open-vsx.org/extension/Zarritas/agent-sessions).

## Funcionalidades

### Panel y navegación
- Panel de sesiones tipo **card** (avatar de proveedor, dos líneas de meta, acento
  de color del proyecto, acciones al hover) — no un TreeView.
- **Agrupado** por proveedor / proyecto (cwd) / cascada / **fecha** (Hoy, Ayer, …)
  / **grupo** (colecciones propias). En la vista por proyecto, los **subproyectos**
  (cwd descendientes) se anidan bajo su proyecto ancestro.
- **Modo selección** (multiselección con checkboxes): **abrir varias** sesiones,
  **eliminar varias** o **agruparlas** de una vez desde la barra de acciones.
- **Modo compacto** (toggle de densidad) para ver más sesiones de un vistazo.
- **Plegar / desplegar** todas las secciones con un botón.
- **Paleta de acciones** (`Ctrl/Cmd+Alt+A`): quick-pick de todas las sesiones →
  menú de acciones, sin ratón.
- **Menú de acciones «⋯»** en la barra del panel: todas las acciones de la
  extensión agrupadas, sin pasar por la paleta de comandos.
- Modelo visible por sesión; indicador **"abierta"** (el clic enfoca el terminal
  existente en vez de duplicar el resume).

### Reanudar y lanzar
- **Reanudar** en el terminal integrado (clic o ▶) con el `resumeArgv` del proveedor.
- **Reanudar con prompt** y **Continuar en otro agente** (handoff) desde el menú.
- **Nueva sesión** eligiendo directorio (workspace / cwd conocido con alias / otra
  ruta), con acciones rápidas **«nueva sesión aquí»** y **«abrir terminal aquí»**
  en cada cabecera de proyecto.
- **Nueva sesión en varios proyectos** a la vez (un terminal por carpeta elegida).
- **Smart-launch**: agente recomendado según el cwd.
- **Compactar contexto** (»«, solo Claude): lanza `/compact` sin reanudar, o
  **«compactar con instrucciones…»** (un prompt que enfoca el resumen,
  `/compact <texto>`).
- **Comandos del proyecto**: explorador (botón en la cabecera de cada proyecto)
  que reúne, en secciones separadas, los **slash-commands del proyecto**
  (`.claude/commands/**`, versionados con el repo), los **globales del usuario**
  (`~/.claude/commands/**`), los **scripts del repo**
  (`package.json`/`Makefile`/`justfile`/`Cargo`) y las acciones de la extensión.
- **Comandos globales de usuario**: acceso propio (menú «⋯» / comando) a los
  slash-commands de `~/.claude/commands`, independiente de un proyecto: eliges
  dónde lanzarlos.
- **Terminal Profiles** por proveedor en el desplegable `+` del terminal.
- **Plantillas** de lanzamiento con etiquetas y cwd · **comparativa paralela** con
  un git worktree por agente. _(funciones **Pro** — ver más abajo)_

### Búsqueda y filtros
- Filtro con **predicados**: `provider:`, `model:`, `cwd:`, `branch:`, `#tag`,
  `cost>`, `tokens>`, `ctx>`, `age<`, `has:notes|favorite|persisted`, `active:true`.
- **Filtros rápidos**: botón «solo activos» y popover **«por etiqueta»**.
- **Búsqueda en el contenido** (FTS) dentro del panel: el botón 🔍 usa el filtro
  como query y muestra los resultados con el fragmento resaltado.

### Metadata y organización
- Renombrar, **etiquetas** (+ **catálogo** de tags reutilizables), **color**,
  **notas**, **favoritos** e **icono/emoji** por sesión. Compartido con la app
  nativa (el icono se guarda en el estado de la extensión).
- **Grupos manuales**: colecciones propias (nombre + color + icono) para agrupar
  sesiones; asignación por menú, por la barra de multiselección o **arrastrando**
  la card a un bucket de grupo, con su propia vista «por grupo».
- **Proyectos**: alias, color e **icono** por cwd; **drag & drop** de Claude entre
  proyectos; **añadir la carpeta del proyecto al workspace** de VS Code con un clic.
- **Eliminar sesiones**: una a una, en **multiselección**, o **por fecha**
  («más antiguas que N días»).

### Persistencia y transferencia
- ⭐ **Persistir sesiones** (`Persistir` en el menú): guarda una **copia durable**
  bajo `~/.config/aterm/archive` que **sobrevive aunque el proveedor borre el
  original** por inactividad. La sesión sigue en el panel (badge naranja) y, al
  reanudarla, se **restaura al vuelo** a la ruta que Claude espera. (Restaurar/
  reanudar: Claude por ahora.)
- **Export** a `.zip` (byte-compatible con multi-claude) e **import** (Claude).
- **Backup/restore** del catálogo (metadata + proyectos + plantillas).
- **Copiar/guardar** la conversación como Markdown.

### Costes y monitorización
- **Quota** del proveedor: pill de % con cuenta atrás hasta el reset.
- **Dashboard** con KPIs (sesiones, coste $, tokens), barras por proveedor/proyecto
  y sparkline de 30 días.
- **Alerta de coste diario** + indicador en la barra de estado.
- Estado de servicio (statuspage) por proveedor; **notificaciones** idle/finish
  con **nivel configurable** (`notificationLevel`: todas / importantes / errores /
  ninguna; los diálogos de confirmación nunca se silencian).
- **% de contexto** por sesión; en Claude la ventana es **fijable**
  (`claudeContextWindow`: auto / 200k / 1M) para que el porcentaje no se infle en
  cuentas con ventana de 1M.

### Integración
- **MCP server** (`agent-sessions-cli serve`) + comando **«Configurar servidor
  MCP…»** que lo registra con un clic. Ver "Uso como MCP".
- **Previsualizar** la conversación en un panel estilado (turnos en burbujas con
  cabecera de metadatos y render Markdown), reutilizable entre sesiones.
- Auto-localización del sidecar (empaquetado en el `.vsix` o `target/`).

## Edición Pro

La extensión es **open-core**: casi todo es gratis. Unas pocas funciones avanzadas
son **Pro**:

- **Comparativa paralela** (un agente por git worktree) + comparar/limpiar worktrees.
- **Plantillas de lanzamiento** (guardar / lanzar / gestionar).

Hay una **prueba de 14 días**; después, esas acciones piden activar una licencia
(las demás siguen gratis). Activa con **«Agent Sessions: Activar licencia Pro…»**
y consulta el estado con **«Estado de la licencia Pro»**.

### Emitir licencias (para el mantenedor)

Las licencias se firman **offline** con Ed25519 (sin servidor). Una vez por
proyecto:

```bash
cd vscode-extension
node scripts/sign-license.mjs keygen          # crea license-private.pem (¡backup!) y embebe la pública
node scripts/sign-license.mjs sign cliente@correo.com        # licencia perpetua
node scripts/sign-license.mjs sign cliente@correo.com 365    # caduca en 365 días
node scripts/sign-license.mjs verify ATERM-PRO.…             # validar una clave
```

`license-private.pem` es el secreto de firma (gitignored): guárdalo a buen recaudo;
si lo pierdes no podrás emitir más claves válidas contra la pública publicada.
Cualquier checkout (Lemon Squeezy / Polar / Gumroad) puede emitir claves llamando
a `sign` desde su webhook.

## Instalar

### Desde un `.vsix`

```bash
code --install-extension agent-sessions-<plataforma>-<versión>.vsix
```

El `.vsix` lleva el sidecar dentro, así que **no necesitas compilar nada más**.

### En desarrollo (F5)

Abre `vscode-extension/` en VS Code y pulsa **F5**. La extensión auto-localiza
el sidecar en `target/release/agent-sessions-cli` o `target/debug/…` del
workspace Cargo; si no existe, compila primero:

```bash
cargo build --release -p agent-sessions-cli
```

O fija `agentSessions.cliPath` a una ruta concreta en los ajustes.

## Empaquetar el `.vsix`

```bash
cd vscode-extension
npm install
./scripts/build-vsix.sh                 # auto-detecta plataforma host
./scripts/build-vsix.sh darwin-arm64    # explícito (requiere cross-compile)
```

El script construye `agent-sessions-cli` para el target indicado, lo deposita
en `bin/<rust-triple>/` y llama a `vsce package --target <vsce-target>`. El
resultado es un `.vsix` específico de plataforma, listo para subir al
Marketplace o compartir.

Targets soportados: `linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`,
`win32-x64`, `win32-arm64`. Para release multiplataforma corre el script una
vez por target en el host correspondiente (o vía CI con cross-compile).

## Uso como MCP server

El sidecar incluye un modo `serve` que habla JSON-RPC 2.0 sobre stdio
(protocolo MCP 2024-11-05) y expone tres tools al propio agente:

- `list_sessions(provider?, cwd?, limit?)` → resumen de sesiones recientes.
- `get_session_turns(provider, id, limit?)` → turnos user/assistant.
- `search_sessions(query, limit?)` → match contra título/cwd/branch/tags.

Para registrarlo con **Claude Code** (`~/.claude/mcp.json`):

```json
{
  "mcpServers": {
    "agent-sessions": {
      "command": "/ruta/a/agent-sessions-cli",
      "args": ["serve"]
    }
  }
}
```

Tras reiniciar Claude Code, el agente puede pedirse a sí mismo "busca en mis
sesiones donde toqué `transfer.rs`" y obtener un listado real con título, cwd
y modelo. Codex/OpenCode/Gemini se configuran de forma análoga si soportan MCP.

## Ajustes

| Clave                       | Por defecto             | Descripción                                                                  |
| --------------------------- | ----------------------- | ---------------------------------------------------------------------------- |
| `agentSessions.cliPath`     | `agent-sessions-cli`    | Ruta al sidecar. Si lo dejas por defecto, la extensión busca en el `.vsix`, en `target/{release,debug}/` y por último en el `PATH`. |
| `agentSessions.openInEditor`| `true`                  | Abrir las sesiones en el área del editor (pestaña a tamaño completo) en vez del panel inferior. |
| `agentSessions.closeOnExit` | `true`                  | Cerrar el terminal entero cuando el agente termina (ejecuta `exit` al acabar). |
| `agentSessions.groupBy`     | `provider`              | Agrupado del árbol: `provider`, `project`, `cascade`, `date` o `group`.      |
| `agentSessions.scanProviders` | los 4 proveedores     | Proveedores visibles en el panel (filtro de visualización; el escaneo sigue). Vacío = todos. |
| `agentSessions.fetchStatus` | `true`                  | Interruptor de red: consultar statuspage y mostrar la cuota del proveedor. Desactívalo para trabajar sin tráfico de red. |
| `agentSessions.refreshSec`  | `120`                   | Cada cuántos segundos re-escanear el disco completo. `0` desactiva; valores 1–14 se ajustan a 15. |
| `agentSessions.tagCatalog`  | `[]`                    | Etiquetas predefinidas que aparecen como opciones marcables al asignar tags. |
| `agentSessions.pollIntervalSec` | `5`                 | Cadencia del sondeo de estado en vivo (activas / esperando input).           |
| `agentSessions.notificationLevel`| `all`              | Qué notificaciones mostrar: `all`, `important`, `errors` o `none`. Los diálogos que requieren tu respuesta nunca se ven afectados. |
| `agentSessions.notifyOnIdle`| `true`                  | Notificar cuando una sesión activa pasa de «trabajando» a «esperando input». |
| `agentSessions.notifyOnFinish`| `true`                | Notificar cuando una sesión activa termina.                                  |
| `agentSessions.claudeContextWindow`| `auto`           | Ventana de contexto de Claude para el cálculo del %: `auto`, `200k` o `1m`.  |
| `agentSessions.costAlertDaily`| `0`                   | Umbral diario de gasto en USD; `0` desactiva la alerta.                      |
