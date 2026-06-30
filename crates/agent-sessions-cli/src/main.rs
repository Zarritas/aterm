//! JSON sidecar over `agent-sessions`.
//!
//! A stateless, read-only-for-sessions binary the VS Code extension spawns.
//! Every subcommand prints a single JSON document to stdout and exits; errors
//! go to stderr with a non-zero status. Session discovery/parsing lives in the
//! shared `agent-sessions` core — this layer is just argv parsing and
//! serialisation. The metadata store and transfer (export/import) write to
//! disk via the same paths the native `aterm` app uses, so both UIs share one
//! source of truth.
//!
//! Subcommands:
//!   scan                              → { providers: [...], sessions: [...] }
//!   providers                         → [ { id, displayName, ... }, ... ]
//!   preview      <provider> <id>      → [ { role, text }, ... ]
//!   resume-argv  <provider> <id>      → [ "claude", "--resume", "<id>" ]
//!   new-argv     <provider>           → [ "claude" ]
//!   metadata-get                      → { "<provider>:<id>": { name, tags, color }, ... }
//!   metadata-set <provider> <id>      ← JSON {name?,tags?,color?} on stdin    → updated entry
//!   metadata-clear <provider> <id>    → null
//!   projects-get                      → { names: {<path>: alias}, colors: {<path>: "#rrggbb"} }
//!   projects-set  <path>              ← JSON {name?,color?} on stdin    → updated entry or null
//!   projects-clear <path>             → null
//!   export <provider> <id> <dest.zip> → { written }
//!   import <zip>                      → ImportOutcome (claude-only)
//!   delete <provider> <id> [--force]  → { ok } | error "ACTIVE" / other
//!   move   <id> <source-cwd> <dest-cwd>  → { ok } | error "ACTIVE"/"COLLISION"/other (claude-only)
//!   serve                              MCP server over stdio (JSON-RPC 2.0)
//!   backup  <dest.zip>                 catalog snapshot (metadata + projects + templates)
//!   restore <source.zip>               restore a snapshot in-place
//!   service-status                     [{provider, indicator, description}] (statuspage v2)
//!   live                               [{provider, sessionId, pid, status}] (cheap poll)
//!   search-content <query>             [{provider, id, title, snippet, lastActivity}]

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use agent_sessions::metadata::{MetadataStore, SessionMetadata};
use agent_sessions::provider::{binary_in_path, AgentProvider};
use agent_sessions::transfer::{
    export_sessions, import_archive_routed, move_session, read_manifest, ExportItem,
};
use agent_sessions::types::{
    AgentProviderInfo, DeleteError, ProviderQuota, QuotaWindow, ServiceStatus,
};
use agent_sessions::{all_providers, encode_cwd};
use serde::{Deserialize, Serialize};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("scan");
    match cmd {
        "scan" => scan(),
        "providers" => providers(),
        "preview" => preview(args.get(1), args.get(2)),
        "transcript" => transcript(args.get(1), args.get(2)),
        "resume-argv" => argv_cmd(args.get(1), args.get(2), false),
        "new-argv" => argv_cmd(args.get(1), None, true),
        "compact-argv" => compact_argv_cmd(args.get(1), args.get(2)),
        "metadata-get" => metadata_get(),
        "metadata-set" => metadata_set(args.get(1), args.get(2)),
        "metadata-clear" => metadata_clear(args.get(1), args.get(2)),
        "projects-get" => projects_get(),
        "projects-set" => projects_set(args.get(1)),
        "projects-clear" => projects_clear(args.get(1)),
        "export" => export(args.get(1), args.get(2), args.get(3)),
        "import" => import(args.get(1)),
        "archive" => archive_cmd(args.get(1), args.get(2)),
        "unarchive" => unarchive_cmd(args.get(1), args.get(2)),
        "archive-restore" => archive_restore_cmd(args.get(1), args.get(2)),
        "delete" => delete(args.get(1), args.get(2), args.get(3)),
        "move" => move_cmd(args.get(1), args.get(2), args.get(3)),
        "serve" => serve(),
        "backup" => backup(args.get(1)),
        "restore" => restore(args.get(1)),
        "service-status" => service_status_cmd(),
        "live" => live_cmd(),
        "search-content" => search_content_cmd(args.get(1)),
        "templates-get" => templates_get(),
        "templates-set" => templates_set(args.get(1)),
        "templates-delete" => templates_delete(args.get(1)),
        other => fail(&format!(
            "comando desconocido: {other:?}\nuso: agent-sessions-cli \
             <scan|providers|preview|transcript|resume-argv|new-argv|compact-argv|metadata-get|\
             metadata-set|metadata-clear|projects-get|projects-set|projects-clear|\
             export|import|archive|unarchive|archive-restore|delete|move|serve|\
             backup|restore|service-status|live|search-content|templates-get|\
             templates-set|templates-delete> [args]"
        )),
    }
}

/// Full discovery: every provider's metadata plus all sessions, newest first,
/// with `resumeArgv` stamped in (the trait leaves it empty on purpose). A
/// sidecar `quotas` map carries each provider's rate-limit snapshot when
/// available — paralleling `providers` so we don't have to extend the vendor's
/// `AgentProviderInfo` struct.
fn scan() {
    let providers = all_providers();
    let infos: Vec<AgentProviderInfo> = providers
        .iter()
        .map(|p| provider_info(p.as_ref()))
        .collect();
    let mut sessions = Vec::new();
    let mut quotas: HashMap<String, ProviderQuota> = HashMap::new();
    for p in &providers {
        if let Ok(mut ss) = p.list_sessions() {
            for s in &mut ss {
                s.resume_argv = p.resume_argv(&s.id);
            }
            sessions.append(&mut ss);
        }
        // Quota: primero la fuente NATIVA del proveedor (Claude escribe
        // `~/.claude/rate-limits-cache.json`); si falta, la calculamos NOSOTROS
        // con el endpoint oficial de uso de Anthropic + nuestro propio cache (sin
        // depender de plugins de terceros como claude-hud).
        let q = p.quota().or_else(|| {
            if p.id() == "claude" {
                claude_oauth_usage_quota()
            } else {
                None
            }
        });
        if let Some(q) = q {
            quotas.insert(p.id().to_string(), q);
        }
    }
    sessions.sort_by(|a, b| b.last_activity.total_cmp(&a.last_activity));
    emit(&serde_json::json!({
        "providers": infos,
        "sessions": sessions,
        "quotas": quotas,
        // Durable snapshots under ~/.config/aterm/archive. The frontend merges
        // these in so a persisted session still shows even if its provider
        // deleted the original.
        "archived": archive_entries(),
    }));
}

/// Provider metadata only (cheap: no session parsing).
fn providers() {
    let infos: Vec<AgentProviderInfo> = all_providers()
        .iter()
        .map(|p| provider_info(p.as_ref()))
        .collect();
    emit(&serde_json::json!(infos));
}

fn preview(provider: Option<&String>, id: Option<&String>) {
    let (p, id) = match (find(provider), id) {
        (Some(p), Some(id)) => (p, id),
        _ => fail("uso: preview <provider> <session-id>"),
    };
    match p.preview(id) {
        Ok(turns) => emit(&serde_json::json!(turns)),
        Err(e) => fail(&format!("preview no disponible: {e}")),
    }
}

/// Full conversation (no preview caps) as `[{role, text}]` — for export and
/// cross-provider hand-off. Unsupported for opencode (content not on disk).
fn transcript(provider: Option<&String>, id: Option<&String>) {
    let (p, id) = match (find(provider), id) {
        (Some(p), Some(id)) => (p, id),
        _ => fail("uso: transcript <provider> <session-id>"),
    };
    match p.transcript(id) {
        Ok(turns) => emit(&serde_json::json!(turns)),
        Err(e) => fail(&format!("transcript no disponible: {e}")),
    }
}

/// `resume-argv <provider> <id>` or `new-argv <provider>`.
fn argv_cmd(provider: Option<&String>, id: Option<&String>, new: bool) {
    let Some(p) = find(provider) else {
        fail("proveedor requerido");
    };
    let argv = if new {
        p.new_session_argv()
    } else {
        match id {
            Some(id) => p.resume_argv(id),
            None => fail("uso: resume-argv <provider> <session-id>"),
        }
    };
    emit(&serde_json::json!(argv));
}

/// `compact-argv <provider> <id>` → argv that compacts the session's context
/// without a full resume (e.g. `["claude","--resume",id,"-p","/compact"]`), or
/// JSON `null` when the provider doesn't support out-of-band compaction.
fn compact_argv_cmd(provider: Option<&String>, id: Option<&String>) {
    let Some(p) = find(provider) else {
        fail("proveedor requerido");
    };
    let Some(id) = id else {
        fail("uso: compact-argv <provider> <session-id>");
    };
    match p.compact_argv(id) {
        Some(argv) => emit(&serde_json::json!(argv)),
        None => emit(&serde_json::Value::Null),
    }
}

/// Whole store as a flat `{ "<provider>:<id>": { ... } }` object. Empty entries
/// are absent (the store auto-prunes), so what comes back is what's set.
fn metadata_get() {
    let store = MetadataStore::load(&metadata_path());
    // Re-serialize through the canonical schema rather than reaching into
    // private fields. The store's own Serialize wraps an `entries` key; we
    // unwrap that here for a flat object the frontend can index directly.
    let raw = serde_json::to_value(&store).unwrap_or(serde_json::Value::Null);
    let entries = raw.get("entries").cloned().unwrap_or(serde_json::json!({}));
    emit(&entries);
}

/// Apply a JSON patch from stdin to one session's metadata, persist, and echo
/// back the resulting entry (or null if it's now empty/absent). The patch is
/// `{name?, tags?, color?}`; any field omitted is left untouched, any field
/// present (including `null`) overwrites.
fn metadata_set(provider: Option<&String>, id: Option<&String>) {
    let (provider, id) = match (provider, id) {
        (Some(p), Some(i)) => (p.clone(), i.clone()),
        _ => fail("uso: metadata-set <provider> <session-id>  (JSON por stdin)"),
    };
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        fail("no se pudo leer stdin");
    }
    let body = if raw.trim().is_empty() { "{}" } else { &raw };
    let patch: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|e| fail(&format!("JSON inválido en stdin: {e}")));
    let path = metadata_path();
    let mut store = MetadataStore::load(&path);
    store.update(&provider, &id, |m| apply_patch(m, &patch));
    if let Err(e) = store.save(&path) {
        fail(&format!("no se pudo guardar metadata: {e}"));
    }
    let after = store
        .get(&provider, &id)
        .map(|m| serde_json::to_value(m).unwrap_or(serde_json::Value::Null))
        .unwrap_or(serde_json::Value::Null);
    emit(&after);
}

fn apply_patch(m: &mut SessionMetadata, patch: &serde_json::Value) {
    if let Some(v) = patch.get("name") {
        m.name = v.as_str().map(str::to_string).filter(|s| !s.is_empty());
        if v.is_null() {
            m.name = None;
        }
    }
    if let Some(v) = patch.get("tags") {
        if v.is_null() {
            m.tags.clear();
        } else if let Some(arr) = v.as_array() {
            m.tags = arr
                .iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    if let Some(v) = patch.get("color") {
        m.color = v.as_str().map(str::to_string).filter(|s| !s.is_empty());
        if v.is_null() {
            m.color = None;
        }
    }
    if let Some(v) = patch.get("notes") {
        if v.is_null() {
            m.notes = None;
        } else if let Some(s) = v.as_str() {
            m.notes = if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            };
        }
    }
    if let Some(v) = patch.get("favorite") {
        if let Some(b) = v.as_bool() {
            m.favorite = b;
        } else if v.is_null() {
            m.favorite = false;
        }
    }
    if let Some(v) = patch.get("persisted") {
        if let Some(b) = v.as_bool() {
            m.persisted = b;
        } else if v.is_null() {
            m.persisted = false;
        }
    }
}

/// Drop the entry entirely (`metadata-clear claude abc-123` → forget).
fn metadata_clear(provider: Option<&String>, id: Option<&String>) {
    let (provider, id) = match (provider, id) {
        (Some(p), Some(i)) => (p.clone(), i.clone()),
        _ => fail("uso: metadata-clear <provider> <session-id>"),
    };
    let path = metadata_path();
    let mut store = MetadataStore::load(&path);
    store.update(&provider, &id, |m| *m = SessionMetadata::default());
    if let Err(e) = store.save(&path) {
        fail(&format!("no se pudo guardar metadata: {e}"));
    }
    emit(&serde_json::Value::Null);
}

/// Per-project alias + accent colour, keyed by absolute cwd. Mirrors the
/// `ProjectNames` struct defined inside the native panel; both apps read and
/// write the same JSON file (`~/.config/aterm/project-names.json`) so a rename
/// in the extension shows up in the native app and vice versa.
///
/// The struct is duplicated here rather than imported because the native one
/// lives in `aterm`'s private `sessions.rs` (the shared core stays untouched);
/// drift is prevented by the on-disk format being the contract.
#[derive(Default, Serialize, Deserialize)]
struct ProjectNames {
    #[serde(default)]
    names: HashMap<String, String>,
    /// Hex colour `#rrggbb`. Optional; absence == no accent.
    #[serde(default)]
    colors: HashMap<String, String>,
}

impl ProjectNames {
    fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }
}

/// Whole project store as `{ names: {...}, colors: {...} }`. The schema is
/// stable; missing keys default to empty maps.
fn projects_get() {
    let store = ProjectNames::load(&projects_path());
    emit(&serde_json::to_value(&store).unwrap_or(serde_json::Value::Null));
}

/// Patch one project's alias/colour. JSON on stdin: `{name?, color?}`. Either
/// field set to `null` (or an empty string) clears that side; omitting a field
/// leaves it untouched. Returns the resulting `{name?, color?}` (or `null` if
/// the project no longer has either).
fn projects_set(path: Option<&String>) {
    let Some(cwd) = path else {
        fail("uso: projects-set <ruta>  (JSON por stdin)");
    };
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        fail("no se pudo leer stdin");
    }
    let body = if raw.trim().is_empty() { "{}" } else { &raw };
    let patch: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|e| fail(&format!("JSON inválido en stdin: {e}")));
    let path_file = projects_path();
    let mut store = ProjectNames::load(&path_file);
    if let Some(v) = patch.get("name") {
        let next = v.as_str().map(str::trim).filter(|s| !s.is_empty());
        match next {
            Some(s) => {
                store.names.insert(cwd.clone(), s.to_string());
            }
            None => {
                store.names.remove(cwd);
            }
        }
    }
    if let Some(v) = patch.get("color") {
        let next = v.as_str().map(str::trim).filter(|s| !s.is_empty());
        match next {
            Some(s) => {
                store.colors.insert(cwd.clone(), s.to_string());
            }
            None => {
                store.colors.remove(cwd);
            }
        }
    }
    if let Err(e) = store.save(&path_file) {
        fail(&format!("no se pudo guardar proyectos: {e}"));
    }
    let name = store.names.get(cwd).cloned();
    let color = store.colors.get(cwd).cloned();
    let value = if name.is_none() && color.is_none() {
        serde_json::Value::Null
    } else {
        serde_json::json!({ "name": name, "color": color })
    };
    emit(&value);
}

/// Drop the entry entirely.
fn projects_clear(path: Option<&String>) {
    let Some(cwd) = path else {
        fail("uso: projects-clear <ruta>");
    };
    let path_file = projects_path();
    let mut store = ProjectNames::load(&path_file);
    store.names.remove(cwd);
    store.colors.remove(cwd);
    if let Err(e) = store.save(&path_file) {
        fail(&format!("no se pudo guardar proyectos: {e}"));
    }
    emit(&serde_json::Value::Null);
}

/// Bundle one session into a `.zip` byte-compatible with multi-claude.
/// Pulls the per-session display_name/tags from the local metadata store so the
/// receiving side gets the user's rename/tags too.
fn export(provider: Option<&String>, id: Option<&String>, dest: Option<&String>) {
    let (p, sid, dest) = match (find(provider), id, dest) {
        (Some(p), Some(id), Some(dest)) => (p, id.clone(), PathBuf::from(dest)),
        _ => fail("uso: export <provider> <session-id> <dest.zip>"),
    };
    let sessions = p
        .list_sessions()
        .unwrap_or_else(|e| fail(&format!("scan falló: {e}")));
    let session = sessions
        .into_iter()
        .find(|s| s.id == sid)
        .unwrap_or_else(|| fail("sesión no encontrada"));
    let store = MetadataStore::load(&metadata_path());
    let meta = store.get(p.id(), &sid);
    let item = ExportItem {
        session_id: sid.clone(),
        display_name: meta.and_then(|m| m.name.clone()),
        tags: meta.map(|m| m.tags.clone()).unwrap_or_default(),
    };
    match export_sessions(&[(session, item)], |id| p.locate(id), &dest) {
        Ok(n) => emit(&serde_json::json!({ "written": n, "dest": dest })),
        Err(e) => fail(&format!("export falló: {e}")),
    }
}

/// Relocate a Claude session's jsonl (plus the optional subagents subdir)
/// from one project directory into another, computing the encoded project
/// dir from the absolute cwd. Mirrors the native panel's "move to project"
/// action. Claude-only: other providers' on-disk layouts aren't wired in
/// `transfer.rs`. Errors propagate the stable markers `ACTIVE` (session is
/// in the live registry) and `COLLISION` (the same id already exists at the
/// destination) so the frontend can show targeted messages.
fn move_cmd(id: Option<&String>, source: Option<&String>, dest: Option<&String>) {
    let (id, source, dest) = match (id, source, dest) {
        (Some(a), Some(b), Some(c)) => (a.clone(), b.clone(), c.clone()),
        _ => fail("uso: move <session-id> <source-cwd> <dest-cwd>  (sólo Claude)"),
    };
    let providers = all_providers();
    let claude = providers
        .iter()
        .find(|p| p.id() == "claude")
        .unwrap_or_else(|| fail("Claude no detectado en este sistema"));
    let live: std::collections::HashSet<String> = claude
        .live_sessions()
        .into_iter()
        .map(|l| l.session_id)
        .collect();
    let projects = home_dir().join(".claude/projects");
    let src_enc = encode_cwd(&source);
    let dst_enc = encode_cwd(&dest);
    match move_session(&projects, &id, &src_enc, &dst_enc, |sid| live.contains(sid)) {
        Ok(()) => emit(&serde_json::json!({ "ok": true })),
        Err(e) => fail(&e),
    }
}

/// Delete every on-disk artefact for `session_id`. `--force` bypasses the
/// provider's live-session guard (claude). Returns `{ ok: true }` on success;
/// stderr says `ACTIVE` so the frontend can offer a force-retry.
fn delete(provider: Option<&String>, id: Option<&String>, flag: Option<&String>) {
    let Some(p) = find(provider) else {
        fail("uso: delete <provider> <session-id> [--force]");
    };
    let Some(id) = id else {
        fail("uso: delete <provider> <session-id> [--force]");
    };
    let force = matches!(flag.map(String::as_str), Some("--force") | Some("-f"));
    match p.delete_session(id, force) {
        Ok(()) => emit(&serde_json::json!({ "ok": true })),
        Err(DeleteError::Active) => fail("ACTIVE"),
        Err(e) => fail(&format!("delete falló: {}", e.to_user_string())),
    }
}

/// Import a `.zip` back into Claude's project tree, routing each session to
/// its recorded cwd (with a `aterm-imported` fallback). Other providers'
/// on-disk layouts aren't wired in `transfer` yet, so this is claude-only.
fn import(zip: Option<&String>) {
    let Some(zip) = zip else {
        fail("uso: import <zip>");
    };
    let zip = PathBuf::from(zip);
    let projects = home_dir().join(".claude/projects");
    let fallback = projects.join("aterm-imported");
    match import_archive_routed(&zip, &projects, &fallback, encode_cwd) {
        Ok(o) => emit(&serde_json::to_value(o).unwrap_or(serde_json::Value::Null)),
        Err(e) => fail(&format!("import falló: {e}")),
    }
}

/// Flip a session's `persisted` flag in the shared metadata store.
fn set_persisted(provider: &str, id: &str, value: bool) {
    let path = metadata_path();
    let mut store = MetadataStore::load(&path);
    store.update(provider, id, |m| m.persisted = value);
    let _ = store.save(&path);
}

/// `archive <provider> <id>` → durable snapshot under
/// `~/.config/aterm/archive/<provider>/<id>.zip` (reusing the export format),
/// then mark the session `persisted`. Written atomically via a `.tmp` rename
/// so a crash can't leave a truncated zip.
fn archive_cmd(provider: Option<&String>, id: Option<&String>) {
    let (p, sid) = match (find(provider), id) {
        (Some(p), Some(id)) => (p, id.clone()),
        _ => fail("uso: archive <provider> <session-id>"),
    };
    let session = p
        .list_sessions()
        .unwrap_or_else(|e| fail(&format!("scan falló: {e}")))
        .into_iter()
        .find(|s| s.id == sid)
        .unwrap_or_else(|| fail("sesión no encontrada"));
    let store = MetadataStore::load(&metadata_path());
    let meta = store.get(p.id(), &sid);
    let item = ExportItem {
        session_id: sid.clone(),
        display_name: meta.and_then(|m| m.name.clone()),
        tags: meta.map(|m| m.tags.clone()).unwrap_or_default(),
    };
    let dir = archive_dir().join(p.id());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        fail(&format!("no se pudo crear el archive: {e}"));
    }
    let dest = dir.join(format!("{sid}.zip"));
    let tmp = dir.join(format!("{sid}.zip.tmp"));
    let written = match export_sessions(&[(session, item)], |id| p.locate(id), &tmp) {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            fail(&format!("archive falló: {e}"));
        }
    };
    if written == 0 {
        // Nothing on disk to snapshot (provider doesn't store a locatable file).
        let _ = std::fs::remove_file(&tmp);
        emit(&serde_json::json!({ "written": 0 }));
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        fail(&format!("no se pudo finalizar el archive: {e}"));
    }
    set_persisted(p.id(), &sid, true);
    emit(&serde_json::json!({ "written": written, "dest": dest }));
}

/// `unarchive <provider> <id>` → clear `persisted` and drop the snapshot.
fn unarchive_cmd(provider: Option<&String>, id: Option<&String>) {
    let (provider, id) = match (provider, id) {
        (Some(p), Some(i)) => (p.clone(), i.clone()),
        _ => fail("uso: unarchive <provider> <session-id>"),
    };
    set_persisted(&provider, &id, false);
    let zip = archive_dir().join(&provider).join(format!("{id}.zip"));
    let _ = std::fs::remove_file(&zip);
    emit(&serde_json::json!({ "ok": true }));
}

/// `archive-restore <provider> <id>` → re-materialise the snapshot into
/// Claude's project tree so `--resume` works again. Claude-only (the import
/// router assumes Claude's on-disk layout). The snapshot is kept as backup.
fn archive_restore_cmd(provider: Option<&String>, id: Option<&String>) {
    let (provider, id) = match (provider, id) {
        (Some(p), Some(i)) => (p.clone(), i.clone()),
        _ => fail("uso: archive-restore <provider> <session-id>"),
    };
    // The import router only knows Claude's on-disk layout; reject other
    // providers here so the CLI is safe regardless of caller (it's also an
    // MCP surface), not only when driven by the extension's TS guard.
    if provider != "claude" {
        fail("restaurar solo está soportado para Claude por ahora");
    }
    let zip = archive_dir().join(&provider).join(format!("{id}.zip"));
    if !zip.exists() {
        fail("no hay copia archivada de esta sesión");
    }
    let projects = home_dir().join(".claude/projects");
    let fallback = projects.join("aterm-imported");
    match import_archive_routed(&zip, &projects, &fallback, encode_cwd) {
        Ok(o) => emit(&serde_json::to_value(o).unwrap_or(serde_json::Value::Null)),
        Err(e) => fail(&format!("restore falló: {e}")),
    }
}

/// List every archived snapshot by reading each zip's manifest. Used by `scan`
/// to merge persisted sessions into the panel even when the original is gone.
fn archive_entries() -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let root = archive_dir();
    let Ok(providers) = std::fs::read_dir(&root) else {
        return out;
    };
    for pe in providers.flatten() {
        if !pe.path().is_dir() {
            continue;
        }
        let provider = pe.file_name().to_string_lossy().to_string();
        let Ok(zips) = std::fs::read_dir(pe.path()) else {
            continue;
        };
        for ze in zips.flatten() {
            let path = ze.path();
            if path.extension().and_then(|e| e.to_str()) != Some("zip") {
                continue;
            }
            let archived_at = ze
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            // A corrupt/unreadable snapshot is skipped rather than aborting the scan.
            let Ok(infos) = read_manifest(&path) else {
                continue;
            };
            for info in infos {
                out.push(serde_json::json!({
                    "provider": provider,
                    "id": info.id,
                    "cwd": info.cwd,
                    "title": info.first_prompt.clone().or_else(|| info.display_name.clone()),
                    "displayName": info.display_name,
                    "firstPrompt": info.first_prompt,
                    "tags": info.tags,
                    "archivedAt": archived_at,
                }));
            }
        }
    }
    out
}

fn provider_info(p: &dyn AgentProvider) -> AgentProviderInfo {
    AgentProviderInfo {
        id: p.id().to_string(),
        display_name: p.display_name().to_string(),
        available: p.detect(),
        binary_found: binary_in_path(p.binary()),
        new_session_argv: p.new_session_argv(),
    }
}

/// Resolve a provider by its id (`claude` | `codex` | `opencode` | `gemini`).
fn find(id: Option<&String>) -> Option<Box<dyn AgentProvider>> {
    let id = id?;
    all_providers().into_iter().find(|p| p.id() == id)
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Shared with the native `aterm` app — both UIs read/write the same store so
/// rename/tags/colour set in one show up in the other.
fn metadata_path() -> PathBuf {
    home_dir().join(".config/aterm/session-metadata.json")
}

/// Project aliases + colours. Same path as the native app uses for its
/// `ProjectNames` store, so renames are visible across UIs.
fn projects_path() -> PathBuf {
    home_dir().join(".config/aterm/project-names.json")
}

/// Durable session snapshots, one `.zip` per session under
/// `<archive>/<provider>/<id>.zip`. Survives provider-side cleanup; shared
/// with the native app.
fn archive_dir() -> PathBuf {
    home_dir().join(".config/aterm/archive")
}

/// Pro-licence file written by the VS Code extension whenever the licence/trial
/// state changes: `{ "pro": bool, "expiresAt": <ms since epoch> }`. The MCP
/// server reads it to gate its tools behind Pro. (The `serve` binary is OSS, so
/// this is a courtesy gate, not tamper-proof — the source can be recompiled.)
fn pro_license_path() -> PathBuf {
    home_dir().join(".config/aterm/pro-license.json")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// True only when the extension has recorded a valid Pro state (licence or
/// active trial) that has not yet expired. Absent/invalid/expired → fail closed.
fn pro_active() -> bool {
    let raw = match std::fs::read_to_string(pro_license_path()) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if json.get("pro").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    // `expiresAt` bounds how long we trust the file without a refresh from the
    // extension; missing → treat as expired.
    match json.get("expiresAt").and_then(|v| v.as_i64()) {
        Some(expires) => now_ms() < expires,
        None => false,
    }
}

const PRO_REQUIRED_MSG: &str = "Las tools MCP de Agent Sessions requieren la edición Pro. \
Actívala en la extensión de VS Code (Agent Sessions: Activar licencia Pro…) o durante la prueba de 14 días.";

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string(value).unwrap_or_else(|_| "null".into())
    );
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

// ── Live-session poll (cheap) ─────────────────────────────────────────────
//
// Just the providers' live registries — no session parsing, no transcript
// reads. Used by the extension to poll for state transitions (busy → idle,
// alive → gone) without the cost of a full `scan`.

fn live_cmd() {
    let providers = all_providers();
    let mut out = Vec::new();
    for p in &providers {
        for l in p.live_sessions() {
            out.push(l);
        }
    }
    emit(&serde_json::json!(out));
}

/// Full-text search across session transcripts. Uses each provider's
/// `fts_content()` (already implemented in the vendor) and returns up to 50
/// hits with a small snippet around the first match. Heavier than `scan` —
/// the frontend calls it only when the user explicitly searches content.
fn search_content_cmd(query: Option<&String>) {
    let Some(q) = query else {
        fail("uso: search-content <query>");
    };
    let needle = q.to_lowercase();
    if needle.is_empty() {
        emit(&serde_json::json!([]));
        return;
    }
    let providers = all_providers();
    let mut hits = Vec::new();
    for p in &providers {
        let Ok(sessions) = p.list_sessions() else {
            continue;
        };
        for s in sessions {
            let Some(text) = p.fts_content(&s.id) else {
                continue;
            };
            let lo = text.to_lowercase();
            if let Some(pos) = lo.find(&needle) {
                // `pos` indexes the lowercased string, whose byte length can
                // differ from `text`; clamp every offset to `text` length AND
                // to a char boundary so a multibyte char (acentos, emojis)
                // can't trigger a slice-on-non-boundary panic.
                let len = text.len();
                let mut start = pos.saturating_sub(60).min(len);
                let mut end = (pos + needle.len() + 100).min(len);
                while start > 0 && !text.is_char_boundary(start) {
                    start -= 1;
                }
                while end < len && !text.is_char_boundary(end) {
                    end += 1;
                }
                if start > end {
                    start = end;
                }
                let snippet = text[start..end].replace('\n', " ");
                hits.push(serde_json::json!({
                    "provider": s.provider,
                    "id": s.id,
                    "title": s.title,
                    "cwd": s.cwd,
                    "snippet": snippet,
                    "lastActivity": s.last_activity,
                }));
                if hits.len() >= 50 {
                    break;
                }
            }
        }
        if hits.len() >= 50 {
            break;
        }
    }
    emit(&serde_json::json!(hits));
}

// ── Launch templates ──────────────────────────────────────────────────────
//
// User-saved "recipes" for starting an agent: provider + initial prompt +
// optional cwd + tags. Stored sidecar-private (the native app doesn't use
// them yet) at `~/.config/aterm/templates.json`. Trivial CRUD; the frontend
// picks a template and the launch logic is its concern.

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct LaunchTemplate {
    id: String,
    name: String,
    provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct TemplateStore {
    #[serde(default)]
    templates: Vec<LaunchTemplate>,
}

impl TemplateStore {
    fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }
    fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }
}

fn templates_get() {
    let store = TemplateStore::load(&templates_path());
    emit(&serde_json::to_value(&store.templates).unwrap_or(serde_json::Value::Null));
}

fn templates_set(id: Option<&String>) {
    let Some(id) = id else {
        fail("uso: templates-set <id>  (JSON por stdin)");
    };
    let mut raw = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw).is_err() {
        fail("no se pudo leer stdin");
    }
    let mut t: LaunchTemplate =
        serde_json::from_str(&raw).unwrap_or_else(|e| fail(&format!("JSON inválido: {e}")));
    t.id = id.clone();
    if t.name.trim().is_empty() {
        fail("`name` es obligatorio");
    }
    if t.provider.trim().is_empty() {
        fail("`provider` es obligatorio");
    }
    let path = templates_path();
    let mut store = TemplateStore::load(&path);
    if let Some(existing) = store.templates.iter_mut().find(|x| x.id == t.id) {
        *existing = t.clone();
    } else {
        store.templates.push(t.clone());
    }
    if let Err(e) = store.save(&path) {
        fail(&format!("no se pudo guardar templates: {e}"));
    }
    emit(&serde_json::to_value(&t).unwrap_or(serde_json::Value::Null));
}

fn templates_delete(id: Option<&String>) {
    let Some(id) = id else {
        fail("uso: templates-delete <id>");
    };
    let path = templates_path();
    let mut store = TemplateStore::load(&path);
    store.templates.retain(|t| &t.id != id);
    if let Err(e) = store.save(&path) {
        fail(&format!("no se pudo guardar templates: {e}"));
    }
    emit(&serde_json::Value::Null);
}

// ── Claude usage via the official OAuth usage endpoint (self-contained) ────
//
// Replicates what plugins like `claude-hud` do, WITHOUT depending on them:
// reads Claude's own OAuth token (`~/.claude/.credentials.json`), queries the
// official Anthropic usage endpoint and caches the result (~5 min) so we don't
// hit the API on every scan. Read-only, best-effort. The token is only ever
// sent to api.anthropic.com and is passed to curl via a 0600 config file (kept
// out of the process arg list); expired tokens are ignored.

const USAGE_CACHE_TTL_MS: i64 = 5 * 60_000;

fn usage_cache_path() -> PathBuf {
    home_dir().join(".config/aterm/usage-cache.json")
}

fn claude_config_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".claude"))
}

/// Claude's OAuth access token, only when present and not expired.
fn claude_oauth_token() -> Option<String> {
    let raw = std::fs::read_to_string(claude_config_dir().join(".credentials.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let token = oauth.get("accessToken")?.as_str()?.to_string();
    if token.is_empty() {
        return None;
    }
    // `expiresAt` is unix ms; if present and already past, don't use the token.
    if let Some(exp) = oauth.get("expiresAt").and_then(|x| x.as_i64()) {
        if exp > 0 && exp <= now_ms() {
            return None;
        }
    }
    Some(token)
}

fn quota_from_cache_value(v: &serde_json::Value) -> Option<ProviderQuota> {
    let windows: Vec<QuotaWindow> = v
        .get("windows")?
        .as_array()?
        .iter()
        .filter_map(|w| {
            Some(QuotaWindow {
                label: w.get("label")?.as_str()?.to_string(),
                used_percent: w.get("used_percent")?.as_f64()?,
                resets_at: w.get("resets_at").and_then(|x| x.as_u64()),
            })
        })
        .collect();
    if windows.is_empty() {
        return None;
    }
    Some(ProviderQuota {
        provider: "claude".to_string(),
        windows,
        as_of: v.get("as_of").and_then(|x| x.as_f64()),
    })
}

/// Read our own usage cache. With `fresh_only`, returns None past the TTL.
fn read_usage_cache(fresh_only: bool) -> Option<ProviderQuota> {
    let raw = std::fs::read_to_string(usage_cache_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    if fresh_only {
        let ts = v.get("ts_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        if now_ms() - ts > USAGE_CACHE_TTL_MS {
            return None;
        }
    }
    quota_from_cache_value(&v)
}

fn write_usage_cache(q: &ProviderQuota) {
    let windows: Vec<serde_json::Value> = q
        .windows
        .iter()
        .map(|w| {
            serde_json::json!({
                "label": w.label,
                "used_percent": w.used_percent,
                "resets_at": w.resets_at,
            })
        })
        .collect();
    let obj = serde_json::json!({ "ts_ms": now_ms(), "as_of": q.as_of, "windows": windows });
    let path = usage_cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, obj.to_string());
}

/// GET https://api.anthropic.com/api/oauth/usage with the OAuth token. The token
/// goes in a 0600 curl config file (not the arg list), removed right after.
fn fetch_oauth_usage(token: &str) -> Option<String> {
    let cfg_path = std::env::temp_dir().join(format!("aterm-usage-{}.curl", std::process::id()));
    let cfg = format!(
        "silent\nlocation\nmax-time = 8\nurl = \"https://api.anthropic.com/api/oauth/usage\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\n"
    );
    std::fs::write(&cfg_path, &cfg).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600));
    }
    let out = std::process::Command::new("curl")
        .arg("-K")
        .arg(&cfg_path)
        .output();
    let _ = std::fs::remove_file(&cfg_path);
    let out = out.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn parse_oauth_usage(body: &str) -> Option<ProviderQuota> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", "session"), ("seven_day", "weekly")] {
        if let Some(w) = v.get(key) {
            if let Some(u) = w.get("utilization").and_then(|x| x.as_f64()) {
                windows.push(QuotaWindow {
                    label: label.to_string(),
                    used_percent: u.clamp(0.0, 100.0),
                    resets_at: w
                        .get("resets_at")
                        .and_then(|x| x.as_str())
                        .and_then(parse_iso8601_to_unix),
                });
            }
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(ProviderQuota {
        provider: "claude".to_string(),
        windows,
        as_of: Some(now_ms() as f64 / 1000.0),
    })
}

/// Self-contained Claude usage: fresh cache → API refresh → stale cache.
fn claude_oauth_usage_quota() -> Option<ProviderQuota> {
    if let Some(q) = read_usage_cache(true) {
        return Some(q);
    }
    let token = match claude_oauth_token() {
        Some(t) => t,
        None => return read_usage_cache(false), // sin token: último valor conocido
    };
    match fetch_oauth_usage(&token).and_then(|b| parse_oauth_usage(&b)) {
        Some(q) => {
            write_usage_cache(&q);
            Some(q)
        }
        None => read_usage_cache(false), // API caída: último valor conocido
    }
}

/// Tiny ISO 8601 → unix-seconds parser (`YYYY-MM-DDTHH:MM:SS(.fff)?Z`, UTC).
fn parse_iso8601_to_unix(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(secs).ok()
}

// ── Provider service status (statuspage v2 via curl) ──────────────────────
//
// Mirror of the `service_status.rs` module in the native panel: shells out to
// curl instead of pulling a TLS HTTP stack. Best-effort — providers without a
// statuspage (opencode, gemini) yield no entry; failures yield no entry. We
// fan out across providers in parallel so the worst-case latency is one HTTP
// request, not the sum.

fn statuspage_endpoint(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "claude" => Some("https://status.claude.com/api/v2/status.json"),
        "codex" => Some("https://status.openai.com/api/v2/status.json"),
        _ => None,
    }
}

fn fetch_service_status(provider_id: &str) -> Option<ServiceStatus> {
    let url = statuspage_endpoint(provider_id)?;
    let output = std::process::Command::new("curl")
        .args(["-sL", "-m", "5", url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let status = json.get("status")?;
    let indicator = status.get("indicator")?.as_str()?.to_string();
    let description = status
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    Some(ServiceStatus {
        provider: provider_id.to_string(),
        indicator,
        description,
    })
}

fn service_status_cmd() {
    // Fan out across the known statuspage providers. Threads are fine here:
    // each is one curl, bounded by the -m timeout.
    let ids = ["claude", "codex"];
    let handles: Vec<_> = ids
        .iter()
        .map(|id| {
            let id = id.to_string();
            std::thread::spawn(move || fetch_service_status(&id))
        })
        .collect();
    let statuses: Vec<ServiceStatus> = handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect();
    emit(&serde_json::json!(statuses));
}

// ── Backup / restore the local catalog ────────────────────────────────────
//
// Snapshot the user-edited overlay so a new machine can pick up where the
// old one left off. Sessions themselves are *not* included — they belong to
// each provider's data dir on the new machine; the manifest only carries our
// metadata (rename/tags/colour/notes/favourite), project aliases/colours,
// and (later) templates. Read-write paths only, never the providers'.

use std::io::Read as BackupRead;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

const BACKUP_FORMAT: &str = "aterm/catalog-backup";
const BACKUP_VERSION: u64 = 1;

fn backup(dest: Option<&String>) {
    let Some(dest) = dest else {
        fail("uso: backup <dest.zip>");
    };
    let dest = PathBuf::from(dest);
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let file = std::fs::File::create(&dest)
        .unwrap_or_else(|e| fail(&format!("no se pudo crear {}: {e}", dest.display())));
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default();

    let manifest = serde_json::json!({
        "format": BACKUP_FORMAT,
        "version": BACKUP_VERSION,
        "created_at_unix": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let _ = zip.start_file("manifest.json", opts);
    let _ = std::io::Write::write_all(
        &mut zip,
        serde_json::to_string_pretty(&manifest)
            .unwrap_or_default()
            .as_bytes(),
    );

    let mut written = 0usize;
    for (name, path) in [
        ("session-metadata.json", metadata_path()),
        ("project-names.json", projects_path()),
        ("templates.json", templates_path()),
    ] {
        if let Ok(bytes) = std::fs::read(&path) {
            let _ = zip.start_file(format!("config/{name}"), opts);
            let _ = std::io::Write::write_all(&mut zip, &bytes);
            written += 1;
        }
    }
    let _ = zip.finish();
    emit(&serde_json::json!({ "written": written, "dest": dest }));
}

fn restore(source: Option<&String>) {
    let Some(source) = source else {
        fail("uso: restore <source.zip>");
    };
    let source = PathBuf::from(source);
    let file = std::fs::File::open(&source)
        .unwrap_or_else(|e| fail(&format!("no se pudo abrir {}: {e}", source.display())));
    let mut zip = ZipArchive::new(file).unwrap_or_else(|e| fail(&format!("zip inválido: {e}")));

    // Validate the manifest before touching anything on disk.
    let mut raw = String::new();
    {
        let mut entry = zip
            .by_name("manifest.json")
            .unwrap_or_else(|_| fail("backup sin manifest.json"));
        BackupRead::read_to_string(&mut entry, &mut raw)
            .unwrap_or_else(|e| fail(&format!("manifest ilegible: {e}")));
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| fail(&format!("manifest corrupto: {e}")));
    if manifest.get("format").and_then(|v| v.as_str()) != Some(BACKUP_FORMAT) {
        fail("este zip no parece un backup de aterm");
    }
    if manifest.get("version").and_then(|v| v.as_u64()) != Some(BACKUP_VERSION) {
        fail(&format!(
            "versión de backup no soportada: {}",
            manifest
                .get("version")
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        ));
    }

    let cfg = home_dir().join(".config/aterm");
    let _ = std::fs::create_dir_all(&cfg);
    let mut restored = Vec::new();
    for (name, dest) in [
        ("session-metadata.json", metadata_path()),
        ("project-names.json", projects_path()),
        ("templates.json", templates_path()),
    ] {
        let member = format!("config/{name}");
        let Ok(mut entry) = zip.by_name(&member) else {
            continue;
        };
        let mut bytes = Vec::new();
        if BackupRead::read_to_end(&mut entry, &mut bytes).is_ok() {
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&dest, &bytes).is_ok() {
                restored.push(name.to_string());
            }
        }
    }
    emit(&serde_json::json!({ "restored": restored }));
}

/// Local launch templates (not in the vendor; sidecar-private).
fn templates_path() -> PathBuf {
    home_dir().join(".config/aterm/templates.json")
}

// ── MCP server (JSON-RPC 2.0 over stdio) ──────────────────────────────────
//
// Lets an agent (Claude/Codex/…) read its *own* session history through tool
// calls. The MCP wire format here is the 2024-11-05 revision: newline-framed
// JSON-RPC, no Content-Length headers. We implement just enough by hand to
// avoid pulling in a full MCP SDK — `initialize`, `tools/list`, `tools/call`,
// and the `notifications/*` no-ops the client sends.

use std::io::{BufRead, Write};

const MCP_PROTOCOL: &str = "2024-11-05";
const SERVER_NAME: &str = "agent-sessions";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Read JSON-RPC requests from stdin and reply on stdout, one message per
/// line, until EOF.
fn serve() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut line = String::new();
    let reader = stdin.lock();
    let mut lines = reader.lines();
    while let Some(Ok(raw)) = lines.next() {
        line.clear();
        line.push_str(&raw);
        if line.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Notifications carry no `id` and expect no reply (just absorb them).
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let response = match method {
            "initialize" => Some(rpc_ok(id.clone(), mcp_initialize())),
            "initialized" | "notifications/initialized" => None,
            "ping" => Some(rpc_ok(id.clone(), serde_json::json!({}))),
            "tools/list" => Some(rpc_ok(id.clone(), tools_list())),
            "tools/call" => Some(handle_tool_call(id.clone(), &params)),
            _ => id.as_ref().map(|_| {
                rpc_err(
                    id.clone(),
                    -32601,
                    &format!("método no soportado: {method}"),
                )
            }),
        };
        if let Some(r) = response {
            let _ = writeln!(stdout, "{}", serde_json::to_string(&r).unwrap_or_default());
            let _ = stdout.flush();
        }
    }
}

fn mcp_initialize() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
    })
}

fn tools_list() -> serde_json::Value {
    // Pro gate: without a valid licence/trial the server advertises no tools.
    if !pro_active() {
        return serde_json::json!({ "tools": [] });
    }
    serde_json::json!({
        "tools": [
            {
                "name": "list_sessions",
                "description": "Lista las sesiones del usuario (Claude Code, Codex, OpenCode, Gemini). Devuelve título, cwd, branch, modelo, última actividad. Filtra por proveedor o por cwd si se indica.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": ["claude","codex","opencode","gemini"] },
                        "cwd": { "type": "string", "description": "Filtra a sesiones cuyo working directory contiene esta ruta." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 }
                    }
                }
            },
            {
                "name": "get_session_turns",
                "description": "Devuelve los turnos (user/assistant) de una sesión concreta, en orden cronológico.",
                "inputSchema": {
                    "type": "object",
                    "required": ["provider","id"],
                    "properties": {
                        "provider": { "type": "string", "enum": ["claude","codex","opencode","gemini"] },
                        "id": { "type": "string", "description": "Session id del proveedor." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }
                    }
                }
            },
            {
                "name": "search_sessions",
                "description": "Busca sesiones cuyo título/cwd/branch/tags coincidan con la consulta (case-insensitive).",
                "inputSchema": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                    }
                }
            }
        ]
    })
}

fn handle_tool_call(
    id: Option<serde_json::Value>,
    params: &serde_json::Value,
) -> serde_json::Value {
    // Pro gate: even if a client cached the tool list, calls fail without Pro.
    if !pro_active() {
        return rpc_ok(
            id,
            serde_json::json!({
                "content": [{ "type": "text", "text": PRO_REQUIRED_MSG }],
                "isError": true
            }),
        );
    }
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let result = match name {
        "list_sessions" => tool_list_sessions(&args),
        "get_session_turns" => tool_get_session_turns(&args),
        "search_sessions" => tool_search_sessions(&args),
        _ => return rpc_err(id, -32601, &format!("tool desconocida: {name}")),
    };
    match result {
        Ok(payload) => rpc_ok(
            id,
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&payload).unwrap_or_default()
                }],
                "isError": false
            }),
        ),
        Err(e) => rpc_ok(
            id,
            serde_json::json!({
                "content": [{ "type": "text", "text": e }],
                "isError": true
            }),
        ),
    }
}

fn tool_list_sessions(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let provider_filter = args
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let cwd_filter = args.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .min(200) as usize;
    let providers = all_providers();
    let mut all = Vec::new();
    for p in &providers {
        if let Some(ref f) = provider_filter {
            if p.id() != f {
                continue;
            }
        }
        if let Ok(ss) = p.list_sessions() {
            for s in ss {
                if let Some(ref f) = cwd_filter {
                    if !s.cwd.as_deref().unwrap_or("").contains(f) {
                        continue;
                    }
                }
                all.push(s);
            }
        }
    }
    all.sort_by(|a, b| b.last_activity.total_cmp(&a.last_activity));
    all.truncate(limit);
    Ok(serde_json::json!(all))
}

fn tool_get_session_turns(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let provider = args
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or("`provider` es obligatorio")?
        .to_string();
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("`id` es obligatorio")?
        .to_string();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .min(500) as usize;
    let providers = all_providers();
    let p = providers
        .iter()
        .find(|p| p.id() == provider)
        .ok_or_else(|| format!("provider desconocido: {provider}"))?;
    let mut turns = p.preview(&id).map_err(|e| format!("preview falló: {e}"))?;
    if turns.len() > limit {
        turns.drain(..turns.len() - limit);
    }
    Ok(serde_json::json!(turns))
}

fn tool_search_sessions(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("`query` es obligatorio")?
        .to_lowercase();
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .min(100) as usize;
    let store = MetadataStore::load(&metadata_path());
    let mut matches = Vec::new();
    for p in all_providers() {
        if let Ok(ss) = p.list_sessions() {
            for s in ss {
                let meta = store.get(p.id(), &s.id);
                let hay = [
                    s.title.clone().unwrap_or_default(),
                    meta.and_then(|m| m.name.clone()).unwrap_or_default(),
                    s.cwd.clone().unwrap_or_default(),
                    s.branch.clone().unwrap_or_default(),
                    meta.map(|m| m.tags.join(" ")).unwrap_or_default(),
                ]
                .join("\n")
                .to_lowercase();
                if hay.contains(&query) {
                    matches.push(s);
                }
            }
        }
    }
    matches.sort_by(|a, b| b.last_activity.total_cmp(&a.last_activity));
    matches.truncate(limit);
    Ok(serde_json::json!(matches))
}

fn rpc_ok(id: Option<serde_json::Value>, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "result": result,
    })
}

fn rpc_err(id: Option<serde_json::Value>, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(serde_json::Value::Null),
        "error": { "code": code, "message": message },
    })
}
