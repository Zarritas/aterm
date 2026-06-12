//! JSON sidecar over `agent-sessions`.
//!
//! A stateless, read-only binary the VS Code extension spawns. Every subcommand
//! prints a single JSON document to stdout and exits; errors go to stderr with
//! a non-zero status. The session-discovery logic lives entirely in the shared
//! `agent-sessions` core — this layer is just argv parsing + serialisation, so
//! the native `aterm` app and the extension stay byte-for-byte consistent.
//!
//! Subcommands:
//!   scan                         → { providers: [...], sessions: [...] }
//!   preview      <provider> <id> → [ { role, text }, ... ]
//!   resume-argv  <provider> <id> → [ "claude", "--resume", "<id>" ]
//!   new-argv     <provider>      → [ "claude" ]
//!   providers                    → [ { id, displayName, available, ... } ]

use agent_sessions::all_providers;
use agent_sessions::provider::{binary_in_path, AgentProvider};
use agent_sessions::types::AgentProviderInfo;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("scan");
    match cmd {
        "scan" => scan(),
        "providers" => providers(),
        "preview" => preview(args.get(1), args.get(2)),
        "resume-argv" => argv_cmd(args.get(1), args.get(2), false),
        "new-argv" => argv_cmd(args.get(1), None, true),
        other => fail(&format!(
            "comando desconocido: {other:?}\nuso: agent-sessions-cli \
             <scan|providers|preview|resume-argv|new-argv> [args]"
        )),
    }
}

/// Full discovery: every provider's metadata plus all sessions, newest first,
/// with `resumeArgv` stamped in (the trait leaves it empty on purpose).
fn scan() {
    let providers = all_providers();
    let infos: Vec<AgentProviderInfo> = providers.iter().map(provider_info).collect();
    let mut sessions = Vec::new();
    for p in &providers {
        if let Ok(mut ss) = p.list_sessions() {
            for s in &mut ss {
                s.resume_argv = p.resume_argv(&s.id);
            }
            sessions.append(&mut ss);
        }
    }
    sessions.sort_by(|a, b| b.last_activity.total_cmp(&a.last_activity));
    emit(&serde_json::json!({ "providers": infos, "sessions": sessions }));
}

/// Provider metadata only (cheap: no session parsing).
fn providers() {
    let infos: Vec<AgentProviderInfo> = all_providers().iter().map(provider_info).collect();
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

fn provider_info(p: &Box<dyn AgentProvider>) -> AgentProviderInfo {
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

fn emit(value: &serde_json::Value) {
    println!("{}", serde_json::to_string(value).unwrap_or_else(|_| "null".into()));
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}
