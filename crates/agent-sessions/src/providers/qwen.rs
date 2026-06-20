// Qwen Code session provider.
//
// Qwen Code began as a Gemini CLI fork but diverged: its on-disk layout is
// actually Claude-style — `~/.qwen/projects/<encoded-cwd>/chats/<sessionId>.jsonl`,
// one event per line, with each line carrying `type`, `cwd`, `gitBranch`,
// `timestamp` and (for turns) `message: { role, parts: [{ text }] }`. `system`
// lines (snapshots) are skipped. The real cwd is read straight from the events'
// own `cwd` field (no registry needed). A sibling `<id>.runtime.json` per chat is
// ignored (not `.jsonl`).

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::extract::{self, truncate_title};
use crate::provider::{AgentProvider, FileScanCache};
use crate::types::{AgentSession, DeleteError, PreviewTurn};

const PROMPT_MAX_CHARS: usize = 120;

pub struct QwenProvider {
    home: PathBuf,
    cache: FileScanCache,
}

impl QwenProvider {
    pub fn new() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_default().join(".qwen"),
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

    fn projects_dir(&self) -> PathBuf {
        self.home.join("projects")
    }
}

impl Default for QwenProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for QwenProvider {
    fn id(&self) -> &'static str {
        "qwen"
    }

    fn display_name(&self) -> &'static str {
        "Qwen Code"
    }

    fn binary(&self) -> &'static str {
        "qwen"
    }

    fn detect(&self) -> bool {
        self.projects_dir().is_dir()
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let Ok(entries) = std::fs::read_dir(self.projects_dir()) else {
            return Ok(Vec::new());
        };
        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let chats = entry.path().join("chats");
            let Ok(chat_files) = std::fs::read_dir(&chats) else {
                continue;
            };
            for chat in chat_files.flatten() {
                let path = chat.path();
                if path.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                let Some(mtime) = mtime_secs(&path) else {
                    continue;
                };
                if let Some(session) =
                    self.cache.get_or_build(&path, mtime, || build_session(&path))
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
            "qwen".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn locate(&self, session_id: &str) -> Option<PathBuf> {
        let target = format!("{session_id}.jsonl");
        let entries = std::fs::read_dir(self.projects_dir()).ok()?;
        for entry in entries.flatten() {
            let candidate = entry.path().join("chats").join(&target);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn preview(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let path = self
            .locate(session_id)
            .ok_or_else(|| "session not found".to_string())?;
        Ok(extract::preview_turns(&path, qwen_turn))
    }

    fn transcript(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let path = self
            .locate(session_id)
            .ok_or_else(|| "session not found".to_string())?;
        Ok(extract::transcript_turns(&path, qwen_turn))
    }

    fn fts_content(&self, session_id: &str) -> Option<String> {
        let path = self.locate(session_id)?;
        extract::fts_text(&path, qwen_turn)
    }

    fn delete_session(&self, session_id: &str, _force: bool) -> Result<(), DeleteError> {
        let Some(path) = self.locate(session_id) else {
            return Ok(()); // already gone
        };
        std::fs::remove_file(&path).map_err(|e| DeleteError::Io(e.to_string()))?;
        // Drop the sibling runtime sidecar too, if present.
        let runtime = path.with_extension("runtime.json");
        let _ = std::fs::remove_file(runtime);
        self.cache.retain_existing();
        Ok(())
    }
}

/// Per-event extractor: user/assistant turns from `message.parts[].text`
/// (also tolerates a plain `message.content` string or text blocks). `system`
/// snapshot lines carry no `message` and are skipped.
pub(crate) fn qwen_turn(event: &Value) -> Option<(&'static str, String)> {
    let role = match event.get("type").and_then(Value::as_str) {
        Some("user") => "user",
        Some("assistant") => "assistant",
        // Some forks tag the role only inside `message`; fall back to that.
        _ => match event
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
        {
            Some("user") => "user",
            Some("assistant") | Some("model") | Some("qwen") => "assistant",
            _ => return None,
        },
    };
    let message = event.get("message")?;
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        return Some((role, text.to_string()));
    }
    let blocks = message
        .get("parts")
        .or_else(|| message.get("content"))
        .and_then(Value::as_array)?;
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            parts.push(text.to_string());
        } else if let Some(text) = block.as_str() {
            parts.push(text.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some((role, parts.join("\n")))
    }
}

fn build_session(jsonl: &Path) -> Option<AgentSession> {
    let id = jsonl.file_stem()?.to_string_lossy().to_string();
    let meta = std::fs::metadata(jsonl).ok()?;
    let file = File::open(jsonl).ok()?;

    let mut cwd: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut title: Option<String> = None;
    let mut message_count: u32 = 0;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = event.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if branch.is_none() {
            branch = event
                .get("gitBranch")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if let Some((role, text)) = qwen_turn(&event) {
            message_count += 1;
            if title.is_none() && role == "user" && !text.trim().is_empty() {
                title = Some(truncate_title(text.trim(), PROMPT_MAX_CHARS));
            }
        }
    }

    Some(AgentSession {
        provider: "qwen".to_string(),
        id,
        title,
        cwd,
        branch,
        message_count: Some(message_count),
        size_bytes: Some(meta.len()),
        last_activity: mtime_secs(jsonl).unwrap_or(0.0),
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

    // Real shape captured from a live Qwen Code session (user turn) plus an
    // assistant turn in the same `message.parts` form the fork uses.
    const SAMPLE: &str = concat!(
        "{\"uuid\":\"u1\",\"sessionId\":\"sess-1\",\"type\":\"user\",\"cwd\":\"/work/proj-a\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"parts\":[{\"text\":\"hola\"}]}}\n",
        "{\"uuid\":\"s1\",\"sessionId\":\"sess-1\",\"type\":\"system\",\"cwd\":\"/work/proj-a\",\"subtype\":\"file_history_snapshot\",\"systemPayload\":{}}\n",
        "{\"uuid\":\"a1\",\"sessionId\":\"sess-1\",\"type\":\"assistant\",\"cwd\":\"/work/proj-a\",\"gitBranch\":\"main\",\"message\":{\"role\":\"assistant\",\"parts\":[{\"text\":\"¡Hola! ¿En qué te ayudo?\"}]}}\n",
    );

    fn setup_home(tmp: &Path) -> PathBuf {
        let home = tmp.join(".qwen");
        let chats = home.join("projects/-work-proj-a/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(chats.join("sess-1.jsonl"), SAMPLE).unwrap();
        // A runtime sidecar that must be ignored.
        std::fs::write(chats.join("sess-1.runtime.json"), "{\"pid\":1}").unwrap();
        home
    }

    #[test]
    fn lists_sessions_with_cwd_branch_and_title() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = QwenProvider::with_home(setup_home(tmp.path()));
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.provider, "qwen");
        assert_eq!(s.cwd.as_deref(), Some("/work/proj-a"));
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.title.as_deref(), Some("hola"));
        assert_eq!(s.message_count, Some(2)); // user + assistant, system skipped
    }

    #[test]
    fn ignores_runtime_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = QwenProvider::with_home(setup_home(tmp.path()));
        // Only the .jsonl is listed, never the .runtime.json.
        assert_eq!(provider.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn preview_extracts_user_and_assistant_skips_system() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = QwenProvider::with_home(setup_home(tmp.path()));
        assert!(provider.locate("sess-1").is_some());
        let turns = provider.preview("sess-1").unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].text, "hola");
        assert_eq!(turns[1].role, "assistant");
        assert!(turns[1].text.contains("Hola"));
    }

    #[test]
    fn resume_argv_shape() {
        let p = QwenProvider::new();
        assert_eq!(p.resume_argv("x1"), vec!["qwen", "--resume", "x1"]);
    }

    #[test]
    fn delete_unlinks_chat_and_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let home = setup_home(tmp.path());
        let provider = QwenProvider::with_home(home.clone());
        provider.delete_session("sess-1", false).unwrap();
        assert!(provider.locate("sess-1").is_none());
        assert!(!home
            .join("projects/-work-proj-a/chats/sess-1.runtime.json")
            .exists());
        provider.delete_session("sess-1", false).unwrap(); // idempotent
    }
}
