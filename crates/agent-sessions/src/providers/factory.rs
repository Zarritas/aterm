// Factory Droid session provider.
//
// Layout: `~/.factory/sessions/<encoded-cwd>/<sessionId>.jsonl`, one event per
// line. A `session_start` line carries `id`, `cwd` and `sessionTitle`; each
// `message` line carries `message: { role, content: [{ type: "text", text }] }`
// (Claude-style content blocks). A sibling `<id>.settings.json` is ignored.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::extract::{self, truncate_title};
use crate::provider::{AgentProvider, FileScanCache};
use crate::types::{AgentSession, DeleteError, PreviewTurn};

const PROMPT_MAX_CHARS: usize = 120;

pub struct FactoryProvider {
    home: PathBuf,
    cache: FileScanCache,
}

impl FactoryProvider {
    pub fn new() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_default().join(".factory"),
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

    fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }
}

impl Default for FactoryProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for FactoryProvider {
    fn id(&self) -> &'static str {
        "factory"
    }

    fn display_name(&self) -> &'static str {
        "Factory Droid"
    }

    fn binary(&self) -> &'static str {
        "droid"
    }

    fn detect(&self) -> bool {
        self.sessions_dir().is_dir()
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let Ok(entries) = std::fs::read_dir(self.sessions_dir()) else {
            return Ok(Vec::new());
        };
        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&project_dir) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
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
            "droid".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn locate(&self, session_id: &str) -> Option<PathBuf> {
        let target = format!("{session_id}.jsonl");
        let entries = std::fs::read_dir(self.sessions_dir()).ok()?;
        for entry in entries.flatten() {
            let candidate = entry.path().join(&target);
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
        Ok(extract::preview_turns(&path, factory_turn))
    }

    fn transcript(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let path = self
            .locate(session_id)
            .ok_or_else(|| "session not found".to_string())?;
        Ok(extract::transcript_turns(&path, factory_turn))
    }

    fn fts_content(&self, session_id: &str) -> Option<String> {
        let path = self.locate(session_id)?;
        extract::fts_text(&path, factory_turn)
    }

    fn delete_session(&self, session_id: &str, _force: bool) -> Result<(), DeleteError> {
        let Some(path) = self.locate(session_id) else {
            return Ok(()); // already gone
        };
        std::fs::remove_file(&path).map_err(|e| DeleteError::Io(e.to_string()))?;
        // Drop the sibling settings sidecar too, if present.
        let settings = path.with_extension("settings.json");
        let _ = std::fs::remove_file(settings);
        self.cache.retain_existing();
        Ok(())
    }
}

/// Per-event extractor: user/assistant turns from a `message` line's
/// `message.content` (text blocks or a plain string). `session_start` and any
/// non-message line yield nothing.
pub(crate) fn factory_turn(event: &Value) -> Option<(&'static str, String)> {
    if event.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = event.get("message")?;
    let role = match message.get("role").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        return Some((role, text.to_string()));
    }
    let blocks = message.get("content").and_then(Value::as_array)?;
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            }
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
    let mut explicit_title: Option<String> = None;
    let mut first_user: Option<String> = None;
    let mut message_count: u32 = 0;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("session_start") => {
                if cwd.is_none() {
                    cwd = event.get("cwd").and_then(Value::as_str).map(str::to_string);
                }
                explicit_title = event
                    .get("sessionTitle")
                    .or_else(|| event.get("title"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty() && *t != "New Session")
                    .map(str::to_string);
            }
            _ => {
                if let Some((role, text)) = factory_turn(&event) {
                    message_count += 1;
                    let trimmed = text.trim();
                    // Skip harness-injected system reminders when picking a title.
                    if first_user.is_none()
                        && role == "user"
                        && !trimmed.is_empty()
                        && !trimmed.starts_with("<system-reminder>")
                    {
                        first_user = Some(truncate_title(trimmed, PROMPT_MAX_CHARS));
                    }
                }
            }
        }
    }

    Some(AgentSession {
        provider: "factory".to_string(),
        id,
        title: explicit_title.or(first_user),
        cwd,
        branch: None,
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

    // Shape captured from a live Droid session (session_start + message lines,
    // Claude-style content blocks).
    const SAMPLE: &str = concat!(
        "{\"type\":\"session_start\",\"id\":\"sess-1\",\"sessionTitle\":\"New Session\",\"cwd\":\"/work/proj-a\",\"version\":\"1\"}\n",
        "{\"type\":\"message\",\"id\":\"m1\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"arregla el build\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"m2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Hecho.\"}]}}\n",
    );

    fn setup_home(tmp: &Path) -> PathBuf {
        let home = tmp.join(".factory");
        let project = home.join("sessions/-work-proj-a");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("sess-1.jsonl"), SAMPLE).unwrap();
        std::fs::write(project.join("sess-1.settings.json"), "{\"x\":1}").unwrap();
        home
    }

    #[test]
    fn lists_session_with_cwd_and_first_prompt_title() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = FactoryProvider::with_home(setup_home(tmp.path()));
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.provider, "factory");
        assert_eq!(s.cwd.as_deref(), Some("/work/proj-a"));
        // "New Session" is ignored → falls back to the first user prompt.
        assert_eq!(s.title.as_deref(), Some("arregla el build"));
        assert_eq!(s.message_count, Some(2));
    }

    #[test]
    fn ignores_settings_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = FactoryProvider::with_home(setup_home(tmp.path()));
        assert_eq!(provider.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn preview_extracts_message_turns_skips_session_start() {
        let tmp = tempfile::tempdir().unwrap();
        let provider = FactoryProvider::with_home(setup_home(tmp.path()));
        let turns = provider.preview("sess-1").unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].text, "arregla el build");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].text, "Hecho.");
    }

    #[test]
    fn resume_argv_shape() {
        let p = FactoryProvider::new();
        assert_eq!(p.resume_argv("s9"), vec!["droid", "--resume", "s9"]);
    }

    #[test]
    fn delete_unlinks_chat_and_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let home = setup_home(tmp.path());
        let provider = FactoryProvider::with_home(home.clone());
        provider.delete_session("sess-1", false).unwrap();
        assert!(provider.locate("sess-1").is_none());
        assert!(!home
            .join("sessions/-work-proj-a/sess-1.settings.json")
            .exists());
        provider.delete_session("sess-1", false).unwrap(); // idempotent
    }
}
