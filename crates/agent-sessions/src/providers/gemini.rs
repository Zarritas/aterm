// Gemini CLI session provider.
//
// Layout: `~/.gemini/tmp/<projectId>/chats/session-<ts>-<id>.jsonl`, where the
// first line is a metadata record ({sessionId, projectHash, startTime,
// lastUpdated}) and each following line is one message. `~/.gemini/projects.json`
// maps real project paths to their short ids, letting us recover the cwd.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::extract;
use crate::provider::{AgentProvider, FileScanCache};
use crate::types::{AgentSession, DeleteError, PreviewTurn};

pub struct GeminiProvider {
    home: PathBuf,
    cache: FileScanCache,
}

impl GeminiProvider {
    pub fn new() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_default().join(".gemini"),
            cache: FileScanCache::default(),
        }
    }

    #[cfg(test)]
    pub fn with_home(home: PathBuf) -> Self {
        Self {
            home,
            cache: FileScanCache::default(),
        }
    }

    fn tmp_dir(&self) -> PathBuf {
        self.home.join("tmp")
    }
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn binary(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self) -> bool {
        self.tmp_dir().is_dir()
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let project_paths = load_project_registry(&self.home.join("projects.json"));
        let Ok(entries) = std::fs::read_dir(self.tmp_dir()) else {
            return Ok(Vec::new());
        };
        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let project_id = entry.file_name().to_string_lossy().to_string();
            let cwd = project_paths.get(&project_id).cloned();
            let chats = project_dir.join("chats");
            let Ok(chat_files) = std::fs::read_dir(&chats) else {
                continue;
            };
            for chat in chat_files.flatten() {
                let path = chat.path();
                let name = chat.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("session-") || !name.ends_with(".jsonl") {
                    continue;
                }
                let Some(mtime) = mtime_secs(&path) else {
                    continue;
                };
                if let Some(session) = self
                    .cache
                    .get_or_build(&path, mtime, || build_session(&path, cwd.clone()))
                {
                    sessions.push(session);
                }
            }
        }
        self.cache.retain_existing();
        Ok(sessions)
    }

    fn resume_argv(&self, session_id: &str) -> Vec<String> {
        vec![
            "gemini".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn locate(&self, session_id: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(self.tmp_dir()).ok()?;
        for entry in entries.flatten() {
            let chats = entry.path().join("chats");
            let Ok(chat_files) = std::fs::read_dir(&chats) else {
                continue;
            };
            for chat in chat_files.flatten() {
                let path = chat.path();
                let name = chat.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("session-") || !name.ends_with(".jsonl") {
                    continue;
                }
                if chat_session_id(&path).as_deref() == Some(session_id) {
                    return Some(path);
                }
            }
        }
        None
    }

    fn preview(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let path = self
            .locate(session_id)
            .ok_or_else(|| "session not found".to_string())?;
        Ok(extract::preview_turns(&path, gemini_turn))
    }

    fn fts_content(&self, session_id: &str) -> Option<String> {
        let path = self.locate(session_id)?;
        extract::fts_text(&path, gemini_turn)
    }

    fn delete_session(&self, session_id: &str, _force: bool) -> Result<(), DeleteError> {
        let Some(path) = self.locate(session_id) else {
            return Ok(()); // already gone
        };
        std::fs::remove_file(&path).map_err(|e| DeleteError::Io(e.to_string()))?;
        self.cache.retain_existing();
        Ok(())
    }
}

fn chat_session_id(chat_file: &Path) -> Option<String> {
    let file = File::open(chat_file).ok()?;
    let first = BufReader::new(file).lines().next()?.ok()?;
    let head = serde_json::from_str::<Value>(&first).ok()?;
    head.get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Defensive extractor for Gemini's MessageRecord lines: accepts
/// `{role|type, content}` with content as string or `{text}` blocks; lines
/// without a recognizable role/content yield nothing (never an error).
pub(crate) fn gemini_turn(event: &Value) -> Option<(&'static str, String)> {
    if event.get("sessionId").is_some() {
        return None; // metadata head line
    }
    let role_raw = event
        .get("role")
        .or_else(|| event.get("type"))
        .and_then(Value::as_str)?;
    let role = match role_raw {
        "user" => "user",
        "assistant" | "model" | "gemini" => "assistant",
        _ => return None,
    };
    let content = event.get("content")?;
    if let Some(text) = content.as_str() {
        return Some((role, text.to_string()));
    }
    let mut parts: Vec<&str> = Vec::new();
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
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

/// Parse `~/.gemini/projects.json` ({"projects": {"<path>": "<shortId>"}}) and
/// invert it to shortId → path.
fn load_project_registry(path: &Path) -> HashMap<String, String> {
    let mut by_id = HashMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return by_id;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return by_id;
    };
    let Some(projects) = parsed.get("projects").and_then(Value::as_object) else {
        return by_id;
    };
    for (real_path, short_id) in projects {
        if let Some(id) = short_id.as_str() {
            by_id.insert(id.to_string(), real_path.clone());
        }
    }
    by_id
}

fn build_session(chat_file: &Path, cwd: Option<String>) -> Option<AgentSession> {
    let meta = std::fs::metadata(chat_file).ok()?;
    let file = File::open(chat_file).ok()?;
    let first = BufReader::new(file).lines().next()?.ok()?;
    let head = serde_json::from_str::<Value>(&first).ok()?;
    let id = head.get("sessionId").and_then(Value::as_str)?.to_string();
    // Message count: lines after the metadata record.
    let message_count = super::claude::count_lines(chat_file).map(|n| n.saturating_sub(1) as u32);
    Some(AgentSession {
        provider: "gemini".to_string(),
        id,
        title: None,
        cwd,
        branch: None,
        message_count,
        size_bytes: Some(meta.len()),
        last_activity: mtime_secs(chat_file).unwrap_or(0.0),
        is_active: false,
        context_tokens: None,
        context_window: None,
        model: None,
        started_at: None,
        cost_usd: None,
        live_status: None,
        resume_argv: Vec::new(),
    })
}

fn mtime_secs(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs_f64(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_home(tmp: &Path) -> PathBuf {
        let home = tmp.join(".gemini");
        let chats = home.join("tmp/proj-a/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(
            home.join("projects.json"),
            "{\"projects\":{\"/work/proj-a\":\"proj-a\"}}",
        )
        .unwrap();
        std::fs::write(
            chats.join("session-2026-06-03-abcd.jsonl"),
            "{\"sessionId\":\"uuid-1234\",\"projectHash\":\"proj-a\",\
             \"startTime\":\"2026-06-03T10:00:00Z\"}\n\
             {\"id\":\"m1\",\"content\":\"hola\"}\n\
             {\"id\":\"m2\",\"content\":\"adios\"}\n",
        )
        .unwrap();
        home
    }

    #[test]
    fn lists_sessions_with_registry_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = GeminiProvider::with_home(setup_home(tmp.path()));
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "uuid-1234");
        assert_eq!(s.cwd.as_deref(), Some("/work/proj-a"));
        assert_eq!(s.message_count, Some(2));
        assert_eq!(s.provider, "gemini");
    }

    #[test]
    fn unknown_project_id_yields_no_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let home = setup_home(tmp.path());
        std::fs::write(home.join("projects.json"), "{}").unwrap();
        let provider = GeminiProvider::with_home(home);
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions[0].cwd, None);
    }

    #[test]
    fn ignores_non_session_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = setup_home(tmp.path());
        std::fs::write(
            home.join("tmp/proj-a/chats/checkpoint-tag.json"),
            "{\"history\":[]}",
        )
        .unwrap();
        let provider = GeminiProvider::with_home(home);
        assert_eq!(provider.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn resume_argv_shape() {
        let p = GeminiProvider::new();
        assert_eq!(p.resume_argv("u1"), vec!["gemini", "--resume", "u1"]);
    }

    #[test]
    fn delete_unlinks_the_chat_file() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = GeminiProvider::with_home(setup_home(tmp.path()));
        provider.delete_session("uuid-1234", false).unwrap();
        assert!(provider.locate("uuid-1234").is_none());
        provider.delete_session("uuid-1234", false).unwrap(); // idempotent
    }

    #[test]
    fn preview_is_defensive_about_record_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let home = setup_home(tmp.path());
        std::fs::write(
            home.join("tmp/proj-a/chats/session-2026-06-03-abcd.jsonl"),
            "{\"sessionId\":\"uuid-1234\",\"projectHash\":\"proj-a\"}\n\
             {\"role\":\"user\",\"content\":\"hola gemini\"}\n\
             {\"type\":\"model\",\"content\":[{\"text\":\"respuesta\"}]}\n\
             {\"thoughts\":\"sin role ni type\"}\n",
        )
        .unwrap();
        let provider = GeminiProvider::with_home(home);
        assert!(provider.locate("uuid-1234").is_some());
        let turns = provider.preview("uuid-1234").unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].text, "hola gemini");
        assert_eq!(turns[1].role, "assistant");
    }
}
