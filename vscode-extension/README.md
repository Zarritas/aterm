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

## Estado (MVP)

- ✅ Panel de sesiones tipo card (avatar de proveedor, dos líneas de meta,
  acento de color del proyecto, acciones al hover).
- ✅ Reanudar en terminal integrado (clic o ▶) con el `resumeArgv` del proveedor.
- ✅ Previsualizar la conversación (Markdown read-only).
- ✅ Nueva sesión (elige proveedor → terminal nuevo).
- ✅ Filtro: caja de búsqueda contra título/nombre/cwd/rama/etiquetas.
- ✅ Agrupado: proveedor / proyecto (cwd) / cascada (proveedor → proyecto → sesión).
- ✅ Metadata: renombrar, etiquetas, color (compartido con la app nativa).
- ✅ Proyectos: alias y color por proyecto (cwd), compartido con la app nativa
  (`~/.config/aterm/project-names.json`).
- ✅ Modelo visible en la descripción de cada sesión.
- ✅ Borrar sesión (con confirmación; force-retry si el proveedor la marca activa).
- ✅ Filtro por etiqueta: clic en `#tag` lo añade o quita del filtro (predicado exacto).
- ✅ Quota del proveedor: pill de % en el header con tooltip por ventana.
- ✅ Indicador "abierta" en sesiones ya lanzadas en este VS Code; el clic enfoca
  el terminal existente en vez de duplicar el resume.
- ✅ Drag & drop de Claude entre proyectos (mueve el jsonl y el subagents subdir).
- ✅ Notas + favoritos (estrella) por sesión; los favoritos suben arriba.
- ✅ Agrupado adicional por **fecha** (Hoy / Ayer / Esta semana / Este mes / Más antiguo).
- ✅ **Dashboard** con KPIs (sesiones, coste $, tokens), barras por proveedor /
  top proyectos y sparkline de 30 días.
- ✅ **MCP server** (`agent-sessions-cli serve`) que expone tools `list_sessions`,
  `get_session_turns`, `search_sessions` para que el propio agente consulte su
  historial. Ver "Uso como MCP" más abajo.
- ✅ **Comparativa paralela** (`Agent Sessions: Lanzar comparativa paralela…`):
  un git worktree por agente, terminal por worktree, mismo prompt enviado a
  todos. Limpieza con `…: Limpiar worktrees de comparativa…`.
- ✅ Export a `.zip` (byte-compatible con multi-claude) e import (sólo Claude).
- ✅ Auto-localización del sidecar (binario empaquetado en el `.vsix` o cargo target).
- ✅ **Filtros rápidos** en el header: botón «solo activos» (toggle `active:true`)
  y botón «por etiqueta» (popover con las tags en uso y su conteo, multi-selección).
- ✅ **Catálogo de etiquetas** (`agentSessions.tagCatalog` + comando «Gestionar
  catálogo de etiquetas»): al asignar tags eliges de una lista marcable en vez
  de escribirlas.
- ✅ **Compactar contexto** (»« en el menú contextual, solo Claude): lanza el
  `/compact` del proveedor en un terminal efímero sin reanudar la conversación.
- ✅ **Nueva sesión eligiendo directorio**: workspace, cualquier cwd conocido del
  proveedor (con su alias) u otra ruta vía diálogo de carpeta. Además, cada
  cabecera de bucket de proyecto tiene acciones rápidas **«nueva sesión aquí»** y
  **«abrir terminal aquí»** (terminal normal en ese cwd, sin agente).
- ✅ **Plegar / desplegar todas las secciones** con un botón de la barra.
- ✅ **Continuar en otro agente** (handoff) y **reanudar con prompt** desde el menú.
- ✅ **Plantillas** de lanzamiento, **backup/restore** del catálogo y **alerta de
  coste diario** (con indicador en la barra de estado).
- ✅ Búsqueda en el **contenido** de las conversaciones (FTS) vía `search-content`.
- ✅ Ajustes de **proveedores visibles** (`scanProviders`), **interruptor de red**
  (`fetchStatus`) y **auto-refresco completo** periódico (`refreshSec`).

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
