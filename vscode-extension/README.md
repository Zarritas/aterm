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
- **Agrupado** por proveedor / proyecto (cwd) / cascada / **fecha** (Hoy, Ayer, …).
- **Modo compacto** (toggle de densidad) para ver más sesiones de un vistazo.
- **Plegar / desplegar** todas las secciones con un botón.
- **Paleta de acciones** (`Ctrl/Cmd+Alt+A`): quick-pick de todas las sesiones →
  menú de acciones, sin ratón.
- Modelo visible por sesión; indicador **"abierta"** (el clic enfoca el terminal
  existente en vez de duplicar el resume).

### Reanudar y lanzar
- **Reanudar** en el terminal integrado (clic o ▶) con el `resumeArgv` del proveedor.
- **Reanudar con prompt** y **Continuar en otro agente** (handoff) desde el menú.
- **Nueva sesión** eligiendo directorio (workspace / cwd conocido con alias / otra
  ruta), con acciones rápidas **«nueva sesión aquí»** y **«abrir terminal aquí»**
  en cada cabecera de proyecto.
- **Smart-launch**: agente recomendado según el cwd.
- **Compactar contexto** (»«, solo Claude): lanza `/compact` sin reanudar.
- **Terminal Profiles** por proveedor en el desplegable `+` del terminal.
- **Plantillas** de lanzamiento con etiquetas y cwd; **comparativa paralela** con
  un git worktree por agente.

### Búsqueda y filtros
- Filtro con **predicados**: `provider:`, `model:`, `cwd:`, `branch:`, `#tag`,
  `cost>`, `tokens>`, `ctx>`, `age<`, `has:notes|favorite|persisted`, `active:true`.
- **Filtros rápidos**: botón «solo activos» y popover **«por etiqueta»**.
- **Búsqueda en el contenido** (FTS) dentro del panel: el botón 🔍 usa el filtro
  como query y muestra los resultados con el fragmento resaltado.

### Metadata y organización
- Renombrar, **etiquetas** (+ **catálogo** de tags reutilizables), **color**,
  **notas** y **favoritos** por sesión. Compartido con la app nativa.
- **Proyectos**: alias y color por cwd; **drag & drop** de Claude entre proyectos.

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
- Estado de servicio (statuspage) por proveedor; **notificaciones** idle/finish.

### Integración
- **MCP server** (`agent-sessions-cli serve`) + comando **«Configurar servidor
  MCP…»** que lo registra con un clic. Ver "Uso como MCP".
- **Previsualizar** la conversación (Markdown).
- Auto-localización del sidecar (empaquetado en el `.vsix` o `target/`).

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
| `agentSessions.groupBy`     | `provider`              | Agrupado del árbol: `provider`, `project`, `cascade` o `date`.               |
| `agentSessions.scanProviders` | los 4 proveedores     | Proveedores visibles en el panel (filtro de visualización; el escaneo sigue). Vacío = todos. |
| `agentSessions.fetchStatus` | `true`                  | Interruptor de red: consultar statuspage y mostrar la cuota del proveedor. Desactívalo para trabajar sin tráfico de red. |
| `agentSessions.refreshSec`  | `120`                   | Cada cuántos segundos re-escanear el disco completo. `0` desactiva; valores 1–14 se ajustan a 15. |
| `agentSessions.tagCatalog`  | `[]`                    | Etiquetas predefinidas que aparecen como opciones marcables al asignar tags. |
| `agentSessions.pollIntervalSec` | `5`                 | Cadencia del sondeo de estado en vivo (activas / esperando input).           |
| `agentSessions.notifyOnIdle`| `true`                  | Notificar cuando una sesión activa pasa de «trabajando» a «esperando input». |
| `agentSessions.notifyOnFinish`| `true`                | Notificar cuando una sesión activa termina.                                  |
| `agentSessions.costAlertDaily`| `0`                   | Umbral diario de gasto en USD; `0` desactiva la alerta.                      |
