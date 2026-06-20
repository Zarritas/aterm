// Goose (Block) session provider.
//
// Goose stores sessions in a SQLite database
// (`~/.local/share/goose/sessions/sessions.db`). Rather than bind to its schema
// we shell out to its stable CLI, like the opencode provider:
//   - `goose session list --format json`  → session metadata
//   - `goose session export --session-id <id> --format json` → `{conversation}`
// Fixed argv, no shell, short timeout; results cached by the db mtime.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::extract::{cap_text, PREVIEW_TEXT_LIMIT, PREVIEW_TURN_LIMIT, TRANSCRIPT_TEXT_LIMIT};
use crate::provider::{binary_in_path, AgentProvider};
use crate::types::{AgentSession, DeleteError, PreviewTurn};

use super::claude::parse_iso_seconds;
use super::opencode::run_with_timeout;

const LIST_TIMEOUT: Duration = Duration::from_secs(10);
const EXPORT_TIMEOUT: Duration = Duration::from_secs(15);
const RELIST_MIN_INTERVAL: Duration = Duration::from_secs(60);

#[derive(PartialEq, Clone, Copy, Debug, Default)]
struct DbStamp {
    db: f64,
    wal: f64,
}

struct CachedList {
    stamp: DbStamp,
    at: std::time::Instant,
    sessions: Vec<AgentSession>,
}

#[derive(Deserialize)]
struct GooseRow {
    id: String,
    name: Option<String>,
    working_dir: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    message_count: Option<u32>,
    total_tokens: Option<u64>,
    accumulated_cost: Option<f64>,
    provider_name: Option<String>,
    model_config: Option<Value>,
}

pub struct GooseProvider {
    sessions_dir: PathBuf,
    cache: std::sync::Mutex<Option<CachedList>>,
}

impl GooseProvider {
    pub fn new() -> Self {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".local")
                    .join("share")
            });
        Self {
            sessions_dir: base.join("goose").join("sessions"),
            cache: std::sync::Mutex::new(None),
        }
    }

    fn db_stamp(&self) -> DbStamp {
        let mtime = |name: &str| -> f64 {
            std::fs::metadata(self.sessions_dir.join(name))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        };
        DbStamp {
            db: mtime("sessions.db"),
            wal: mtime("sessions.db-wal"),
        }
    }
}

impl Default for GooseProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for GooseProvider {
    fn id(&self) -> &'static str {
        "goose"
    }

    fn display_name(&self) -> &'static str {
        "Goose"
    }

    fn binary(&self) -> &'static str {
        "goose"
    }

    fn detect(&self) -> bool {
        self.sessions_dir.join("sessions.db").is_file() && binary_in_path(self.binary())
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let mut cache = self.cache.lock().map_err(|e| e.to_string())?;
        let stamp = self.db_stamp();
        if let Some(cached) = cache.as_ref() {
            if cached.stamp == stamp || cached.at.elapsed() < RELIST_MIN_INTERVAL {
                return Ok(cached.sessions.clone());
            }
        }
        let raw = run_with_timeout(
            self.binary(),
            &["session", "list", "--format", "json"],
            LIST_TIMEOUT,
        )?;
        let sessions = parse_session_list(&raw)?;
        *cache = Some(CachedList {
            stamp: self.db_stamp(),
            at: std::time::Instant::now(),
            sessions: sessions.clone(),
        });
        Ok(sessions)
    }

    fn resume_argv(&self, session_id: &str) -> Vec<String> {
        vec![
            "goose".to_string(),
            "session".to_string(),
            "--resume".to_string(),
            "--session-id".to_string(),
            session_id.to_string(),
        ]
    }

    fn new_session_argv(&self) -> Vec<String> {
        vec!["goose".to_string(), "session".to_string()]
    }

    fn preview(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let turns = self.export_turns(session_id)?;
        Ok(cap_for_preview(turns))
    }

    fn transcript(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        self.export_turns(session_id)
    }

    fn invalidate_caches(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
    }

    fn delete_session(&self, session_id: &str, _force: bool) -> Result<(), DeleteError> {
        run_with_timeout(
            self.binary(),
            &["session", "remove", "--session-id", session_id],
            LIST_TIMEOUT,
        )
        .map_err(DeleteError::Subprocess)?;
        self.invalidate_caches();
        Ok(())
    }
}

impl GooseProvider {
    /// Full conversation via `goose session export ... --format json`.
    fn export_turns(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let raw = run_with_timeout(
            self.binary(),
            &["session", "export", "--session-id", session_id, "--format", "json"],
            EXPORT_TIMEOUT,
        )?;
        let start = raw.find('{').ok_or("unexpected goose export output")?;
        let doc: Value = serde_json::from_str(raw[start..].trim()).map_err(|e| e.to_string())?;
        let conv = doc
            .get("conversation")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(conv
            .iter()
            .filter_map(turn_from_message)
            .map(|(role, text)| PreviewTurn {
                role: role.to_string(),
                text: cap_text(text.trim(), TRANSCRIPT_TEXT_LIMIT),
            })
            .filter(|t| !t.text.is_empty())
            .collect())
    }
}

/// One conversation entry → (role, text). Content is text blocks
/// (`[{type:"text", text}]`) or a plain string; tool/other blocks are skipped.
fn turn_from_message(msg: &Value) -> Option<(&'static str, String)> {
    let role = match msg.get("role").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let content = msg.get("content")?;
    if let Some(text) = content.as_str() {
        return Some((role, text.to_string()));
    }
    let mut parts: Vec<&str> = Vec::new();
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                parts.push(text);
            } else if let Some(text) = block.as_str() {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some((role, parts.join("\n")))
    }
}

fn cap_for_preview(mut turns: Vec<PreviewTurn>) -> Vec<PreviewTurn> {
    for t in &mut turns {
        t.text = cap_text(&t.text, PREVIEW_TEXT_LIMIT);
    }
    if turns.len() > PREVIEW_TURN_LIMIT {
        turns.drain(..turns.len() - PREVIEW_TURN_LIMIT);
    }
    turns
}

fn parse_session_list(raw: &str) -> Result<Vec<AgentSession>, String> {
    let start = raw.find('[').ok_or("unexpected goose session list output")?;
    let rows: Vec<GooseRow> =
        serde_json::from_str(raw[start..].trim()).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let last_activity = row
                .updated_at
                .as_deref()
                .or(row.created_at.as_deref())
                .and_then(parse_iso_seconds)
                .unwrap_or(0.0);
            let model = row
                .model_config
                .as_ref()
                .and_then(|c| c.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(row.provider_name);
            AgentSession {
                provider: "goose".to_string(),
                id: row.id,
                title: row.name.filter(|t| !t.is_empty() && t != "CLI Session"),
                cwd: row.working_dir,
                branch: None,
                message_count: row.message_count,
                size_bytes: None,
                last_activity,
                is_active: false,
                context_tokens: row.total_tokens,
                context_window: None,
                model,
                started_at: row.created_at.as_deref().and_then(parse_iso_seconds),
                cost_usd: row.accumulated_cost,
                live_status: None,
                resume_argv: Vec::new(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_list_json() {
        let raw = r#"INFO noise
        [{"id":"20260620_3","name":"CLI Session","working_dir":"/work/p",
          "created_at":"2026-06-20T09:54:14Z","updated_at":"2026-06-20T09:54:17Z",
          "message_count":4,"total_tokens":1200,"accumulated_cost":0.01,
          "provider_name":"anthropic","model_config":{"model":"claude-opus-4-8"}}]"#;
        let sessions = parse_session_list(raw).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "20260620_3");
        assert_eq!(s.cwd.as_deref(), Some("/work/p"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s.message_count, Some(4));
        // "CLI Session" is the default name → suppressed.
        assert_eq!(s.title, None);
        assert!(s.last_activity > 1_780_000_000.0);
    }

    #[test]
    fn export_conversation_to_turns() {
        let raw = r#"{"id":"s1","conversation":[
          {"role":"user","content":[{"type":"text","text":"hola"}]},
          {"role":"assistant","content":[{"type":"text","text":"¡hola!"}]},
          {"role":"assistant","content":[{"type":"tool_use","name":"x"}]}
        ]}"#;
        let start = raw.find('{').unwrap();
        let doc: Value = serde_json::from_str(&raw[start..]).unwrap();
        let conv = doc.get("conversation").unwrap().as_array().unwrap();
        let turns: Vec<_> = conv.iter().filter_map(turn_from_message).collect();
        assert_eq!(turns.len(), 2); // tool_use-only message dropped
        assert_eq!(turns[0], ("user", "hola".to_string()));
        assert_eq!(turns[1].0, "assistant");
    }

    #[test]
    fn resume_argv_shape() {
        let p = GooseProvider::new();
        assert_eq!(
            p.resume_argv("20260620_3"),
            vec!["goose", "session", "--resume", "--session-id", "20260620_3"]
        );
    }
}
