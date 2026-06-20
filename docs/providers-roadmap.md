# Proveedores: estado y roadmap

Agent Sessions escanea el **historial local** de agentes de código por CLI. Un
proveedor solo es integrable si guarda sus sesiones **en disco** en un formato
parseable. Este documento lista lo soportado y los candidatos, con su formato y
dificultad, para decidir el siguiente.

> Aviso: los formatos de estos CLIs **cambian a menudo**. Antes de implementar uno,
> hay que confirmar la ruta y el esquema contra una **sesión real** (instalar el
> agente y hacer un prompt). Las rutas marcadas «(verificar)» vienen de catálogos
> de terceros ([cass](https://github.com/Dicklesworthstone/coding_agent_session_search))
> y no se han confirmado contra una sesión propia.

## Soportados hoy

| Proveedor | Ruta | Formato | Notas |
|---|---|---|---|
| Claude Code | `~/.claude/projects/**.jsonl` (+ `~/.claude/sessions/*.json` vivo) | JSONL | provider completo |
| Codex | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | JSONL | provider completo |
| Gemini CLI | `~/.gemini/tmp/<projectId>/chats/session-*.jsonl` (+ `projects.json`) | JSONL | 1ª línea metadatos |
| OpenCode | SQLite vía `opencode session list --format json` | SQLite/CLI | read-only por CLI |
| Qwen Code | `~/.qwen/projects/<encoded-cwd>/chats/<id>.jsonl` | JSONL | estilo Claude; `message.parts[].text` |
| Goose | SQLite vía `goose session list/export --format json` | SQLite/CLI | preview por `export`; ISO timestamps |
| Factory Droid | `~/.factory/sessions/<encoded-cwd>/<id>.jsonl` | JSONL | `session_start` + `message`; uso pleno **requiere suscripción** |

Patrón: cada proveedor implementa el trait `AgentProvider` en
`crates/agent-sessions/src/providers/*.rs` y se registra en `providers/mod.rs`.
El parseo de turnos usa un `TurnExtractor` + los helpers de `extract.rs`.

> **Implementados (2026-06-20)**: Qwen Code, Goose y Factory Droid (ver tabla de
> soportados). Lecciones al hacerlo: los catálogos de terceros se equivocaban en
> el formato — **Goose es SQLite (no JSONL)** y se integró por su CLI; **Qwen no
> usa el layout de Gemini** sino uno estilo Claude (`projects/<cwd>/chats/`).
> Confirmar siempre contra una sesión real.

## Candidatos — 🟢 sencillos (JSON/JSONL, mismo patrón que los actuales)

Estos encajan como un `providers/<id>.rs` nuevo (scan/locate/preview/transcript/
resume/delete) + su extractor de turnos. **Requieren una sesión real para fijar
el esquema** (estructura de cada línea, rol, cómo recuperar el cwd).

| Proveedor | Vendor | Ruta (verificar) | Formato | Comentario |
|---|---|---|---|---|
| **Kimi Code** | Moonshot | `~/.kimi/sessions/*/*/wire.jsonl` | JSONL | esquema «wire» propio: confirmar roles/contenido. |
| **Pi / OpenClaw / Vibe (Mistral) / Clawdbot** | varios | `~/.pi/agent/sessions`, `~/.openclaw/agents/*/sessions`, `~/.vibe/logs/session/*/messages.jsonl`, `~/.clawdbot/sessions` | JSONL | nicho; mismo patrón si interesa. |

## Candidatos — 🟡 medios (SQLite o mixto; patrón OpenCode)

Requieren leer SQLite o un store mixto. Modelo a seguir: el provider de OpenCode
(consulta indirecta) o una dependencia SQLite ligera (`rusqlite`) si se lee el
`.db` directamente.

| Proveedor | Vendor | Ruta | Formato | Por qué es medio |
|---|---|---|---|---|
| **Cursor CLI** (`cursor-agent`) | Cursor | `~/.cursor/chats/` | JSON | Userbase enorme. Confirmar esquema de chats; el IDE usa SQLite aparte. |
| **GitHub Copilot CLI** | GitHub/MS | `~/.copilot/session-state` + store SQLite | JSONL + SQLite | Gran distribución. El store sincroniza con la cuenta GitHub; leer solo lo local. |
| **Crush** | Charmbracelet | `~/.crush/crush.db` + `.crush/crush.db` por proyecto | SQLite | Necesita esquema de la tabla de mensajes. |
| **Hermes** | — | `~/.hermes/state.db` + por proyecto | SQLite | idem. |
| **Amp** | Sourcegraph | `~/.local/share/amp` + storage de VS Code | Mixto | Parte en disco, parte en VS Code. |

## Candidatos — 🔴 difíciles / desaconsejados (de momento)

| Proveedor | Por qué |
|---|---|
| **Cline** | Las sesiones viven en el **globalStorage de VS Code** (directorios de tarea propietarios), no en una ruta limpia del HOME. Requiere lógica específica de VS Code y es frágil. |
| **Aider** | `~/.aider.chat.history.md` es un **log Markdown** de chat, sin ids de sesión ni metadatos estructurados: difícil mapear a «sesiones» reanudables. |
| **ChatGPT / Copilot Chat (IDE)** | Apps/almacenes propietarios, no CLIs con sesiones de proyecto en disco. |

## Cómo añadir un proveedor 🟢 (resumen)

1. Conseguir una **sesión real** (instalar el agente + un prompt) e inspeccionar
   el fichero en disco.
2. Crear `crates/agent-sessions/src/providers/<id>.rs` (mirror del más parecido:
   `gemini.rs` para los basados en JSONL con metadatos en la 1ª línea).
3. Implementar el `TurnExtractor` (`<id>_turn`) según los roles/contenido reales.
4. Registrar en `providers/mod.rs` (`all_providers`).
5. Si la extensión debe mostrarlo, añadir su marca/branding y, si aplica,
   `resume_argv`/`new_session_argv`.
6. Tests con un fixture del formato real (como en `gemini.rs::tests`).
