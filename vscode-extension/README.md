# Agent Sessions — extensión de VS Code

Lista, previsualiza y **reanuda en el terminal integrado** tus sesiones de
agentes de código (Claude Code, Codex, OpenCode, Gemini).

Es la mitad "gestor de sesiones" del proyecto [`aterm`](../README.md) portada a
VS Code. El editor ya pone el terminal, así que la extensión solo aporta la UI
(un árbol en la barra lateral) y delega todo el descubrimiento de sesiones en el
binario sidecar `agent-sessions-cli`, que comparte el núcleo Rust
(`agent-sessions`) con la app nativa. Una sola fuente de verdad.

## Arquitectura

```
VS Code (TypeScript)                  Rust (compartido con aterm)
┌─────────────────────────┐  spawn   ┌────────────────────────────┐
│ TreeDataProvider         │ ───────▶ │ agent-sessions-cli (JSON)  │
│ comando "Reanudar"       │ ◀─────── │  └─ agent-sessions (core)  │
│  └─ window.createTerminal│  stdout  └────────────────────────────┘
└─────────────────────────┘
```

El sidecar es **read-only** y deriva todas las rutas del `$HOME`; nunca recibe
paths del editor.

## Requisitos

El binario `agent-sessions-cli` debe estar disponible. Compílalo desde la raíz
del workspace:

```bash
cargo build --release -p agent-sessions-cli
# binario en target/release/agent-sessions-cli
```

Y apunta el ajuste `agentSessions.cliPath` a esa ruta (o ponlo en el `PATH`).

## Desarrollo

```bash
cd vscode-extension
npm install
npm run compile        # tsc → out/
# F5 en VS Code para lanzar una ventana de desarrollo con la extensión cargada
```

## Estado (MVP)

- ✅ Árbol de sesiones agrupado por proveedor (título, tiempo relativo, cwd, rama).
- ✅ Reanudar en terminal integrado (clic o ▶) con el `resumeArgv` del proveedor.
- ✅ Previsualizar la conversación (Markdown read-only).
- ✅ Nueva sesión (elige proveedor → terminal nuevo).
- ⏳ Pendiente: empaquetar el sidecar por plataforma en el `.vsix`, metadata
  (rename/tags/color), filtro/búsqueda, export/import.
```
