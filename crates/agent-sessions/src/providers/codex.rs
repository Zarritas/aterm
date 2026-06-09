// Codex CLI session provider.
//
// Layout: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`. The first
// line is a `session_meta` event carrying the session id and cwd; the first
// `event_msg`/`user_message` event carries the first real prompt. Archived
// sessions live under `~/.codex/archived_sessions` and are excluded (they are
// protected from resume until restored). Optional `~/.codex/session_index.jsonl`
// maps ids to user-given thread names (append-only, last entry wins).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::extract::{self, strip_command_wrappers, truncate_title};
use crate::provider::{AgentProvider, FileScanCache};
use crate::types::{AgentSession, DeleteError, PreviewTurn, ProviderQuota, QuotaWindow};

const HEADER_SCAN_LINES: usize = 40;

pub struct CodexProvider {
    home: PathBuf,
    cache: FileScanCache,
}

impl CodexProvider {
    pub fn new() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_default().join(".codex"),
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

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn binary(&self) -> &'static str {
        "codex"
    }

    fn detect(&self) -> bool {
        self.sessions_dir().is_dir()
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let names = load_session_names(&self.home.join("session_index.jsonl"));
        let mut sessions = Vec::new();
        for rollout in collect_rollouts(&self.sessions_dir()) {
            let Some(mtime) = mtime_secs(&rollout) else {
                continue;
            };
            let Some(mut session) = self
                .cache
                .get_or_build(&rollout, mtime, || build_session(&rollout))
            else {
                continue;
            };
            // Thread names live in the index, not the rollout: stamp fresh so
            // a rename is reflected without invalidating the cached parse.
            if let Some(name) = names.get(&session.id) {
                session.title = Some(name.clone());
            }
            sessions.push(session);
        }
        self.cache.retain_existing();
        Ok(sessions)
    }

    fn resume_argv(&self, session_id: &str) -> Vec<String> {
        vec![
            "codex".to_string(),
            "resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn locate(&self, session_id: &str) -> Option<PathBuf> {
        // The rollout filename ends with the session uuid; prefilter by name
        // and confirm against the session_meta payload defensively.
        let suffix = format!("{session_id}.jsonl");
        for rollout in collect_rollouts(&self.sessions_dir()) {
            let name = rollout.file_name()?.to_string_lossy().into_owned();
            if !name.ends_with(&suffix) {
                continue;
            }
            if rollout_id(&rollout).as_deref() == Some(session_id) {
                return Some(rollout);
            }
        }
        None
    }

    fn preview(&self, session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        let path = self
            .locate(session_id)
            .ok_or_else(|| "session not found".to_string())?;
        Ok(extract::preview_turns(&path, codex_turn))
    }

    fn fts_content(&self, session_id: &str) -> Option<String> {
        let path = self.locate(session_id)?;
        extract::fts_text(&path, codex_turn)
    }

    /// Codex stamps account rate limits into every token_count event; read
    /// the freshest rollout's latest populated one. Windows map by their
    /// minute span (~300 = the rolling session window, ~10080 = weekly).
    fn quota(&self) -> Option<ProviderQuota> {
        let mut rollouts = collect_rollouts(&self.sessions_dir());
        rollouts.sort_by(|a, b| {
            mtime_secs(b)
                .unwrap_or(0.0)
                .total_cmp(&mtime_secs(a).unwrap_or(0.0))
        });
        for rollout in rollouts.iter().take(5) {
            let Some(line) = extract::tail_lines(rollout, 60)
                .into_iter()
                .rev()
                .find(|l| l.contains("\"used_percent\""))
            else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(limits) = event.get("payload").and_then(|p| p.get("rate_limits")) else {
                continue;
            };
            let mut windows = Vec::new();
            for key in ["primary", "secondary"] {
                let Some(w) = limits.get(key).filter(|w| !w.is_null()) else {
                    continue;
                };
                let Some(used) = w.get("used_percent").and_then(Value::as_f64) else {
                    continue;
                };
                let minutes = w.get("window_minutes").and_then(Value::as_u64).unwrap_or(0);
                windows.push(QuotaWindow {
                    label: if minutes > 0 && minutes <= 600 {
                        "session".to_string()
                    } else {
                        "weekly".to_string()
                    },
                    used_percent: used,
                    resets_at: w.get("resets_at").and_then(Value::as_u64),
                });
            }
            if !windows.is_empty() {
                windows.sort_by_key(|w| w.label.clone()); // session before weekly
                return Some(ProviderQuota {
                    provider: "codex".to_string(),
                    windows,
                    as_of: mtime_secs(rollout),
                });
            }
        }
        None
    }

    /// Codex "delete" archives: the rollout moves to archived_sessions/
    /// preserving its YYYY/MM/DD subpath, matching codex's own flow and
    /// keeping the session recoverable (`codex unarchive`).
    fn delete_session(&self, session_id: &str, _force: bool) -> Result<(), DeleteError> {
        let Some(rollout) = self.locate(session_id) else {
            return Ok(()); // already gone/archived
        };
        let relative = rollout
            .strip_prefix(self.sessions_dir())
            .map_err(|e| DeleteError::Io(e.to_string()))?
            .to_path_buf();
        let target = self.home.join("archived_sessions").join(&relative);
        if target.exists() {
            // Same rollout already archived: drop the live copy.
            std::fs::remove_file(&rollout).map_err(|e| DeleteError::Io(e.to_string()))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| DeleteError::Io(e.to_string()))?;
            }
            std::fs::rename(&rollout, &target).map_err(|e| DeleteError::Io(e.to_string()))?;
        }
        self.cache.retain_existing();
        Ok(())
    }
}

/// Session id from a rollout's session_meta head line.
fn rollout_id(rollout: &Path) -> Option<String> {
    let file = File::open(rollout).ok()?;
    let first = BufReader::new(file).lines().next()?.ok()?;
    let head = serde_json::from_str::<Value>(&first).ok()?;
    head.get("payload")?
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Per-event extractor: `event_msg` user_message/agent_message carry plain
/// `message` strings (verified on real rollouts, both directions).
pub(crate) fn codex_turn(event: &Value) -> Option<(&'static str, String)> {
    if event.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = event.get("payload")?;
    let role = match payload.get("type").and_then(Value::as_str)? {
        "user_message" => "user",
        "agent_message" => "assistant",
        _ => return None,
    };
    let message = payload.get("message").and_then(Value::as_str)?;
    Some((role, message.to_string()))
}

/// Walk `sessions/YYYY/MM/DD/` collecting `rollout-*.jsonl` files. The layout
/// is exactly three levels deep, so a bounded manual walk avoids a recursion
/// dependency.
fn collect_rollouts(sessions_dir: &Path) -> Vec<PathBuf> {
    let mut rollouts = Vec::new();
    let years = read_subdirs(sessions_dir);
    for year in years {
        for month in read_subdirs(&year) {
            for day in read_subdirs(&month) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                        rollouts.push(path);
                    }
                }
            }
        }
    }
    rollouts
}

fn read_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Parse the append-only `session_index.jsonl` ({id, thread_name, updated_at});
/// later entries win. Missing file → empty map (older codex versions).
fn load_session_names(index_path: &Path) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(file) = File::open(index_path) else {
        return names;
    };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(name)) = (
            entry.get("id").and_then(Value::as_str),
            entry.get("thread_name").and_then(Value::as_str),
        ) else {
            continue;
        };
        if !name.is_empty() {
            names.insert(id.to_string(), name.to_string());
        }
    }
    names
}

fn build_session(rollout: &Path) -> Option<AgentSession> {
    let meta = std::fs::metadata(rollout).ok()?;
    let file = File::open(rollout).ok()?;
    let mut reader = BufReader::new(file).lines();

    // First line must be the session_meta event; anything else isn't a rollout.
    let first = reader.next()?.ok()?;
    let head = serde_json::from_str::<Value>(&first).ok()?;
    if head.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = head.get("payload")?;
    let id = payload.get("id").and_then(Value::as_str)?.to_string();
    let context_window = payload.get("model_context_window").and_then(Value::as_u64);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut first_prompt = None;
    for line in reader.take(HEADER_SCAN_LINES) {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let Some(payload) = event.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("user_message") {
            continue;
        }
        if let Some(message) = payload.get("message").and_then(Value::as_str) {
            first_prompt = Some(truncate_title(&strip_command_wrappers(message), 120));
            break;
        }
    }

    Some(AgentSession {
        provider: "codex".to_string(),
        id,
        title: first_prompt,
        cwd,
        branch: None,
        message_count: None,
        size_bytes: Some(meta.len()),
        last_activity: mtime_secs(rollout).unwrap_or(0.0),
        is_active: false,
        live_status: None,
        context_tokens: latest_context_tokens(rollout),
        context_window,
        model: None,
        started_at: None,
        cost_usd: None,
        resume_argv: Vec::new(),
    })
}

/// Latest token_count event from the rollout tail, parsed defensively: the
/// payload `info` is often null (short exec sessions) and its populated
/// shape varies across codex versions, so any known total field wins.
fn latest_context_tokens(rollout: &Path) -> Option<u64> {
    let line = extract::tail_lines(rollout, 40)
        .into_iter()
        .rev()
        .find(|l| l.contains("\"token_count\""))?;
    let event = serde_json::from_str::<Value>(&line).ok()?;
    let info = event.get("payload")?.get("info")?;
    for source in ["total_token_usage", "last_token_usage"] {
        if let Some(usage) = info.get(source) {
            let field = |n: &str| usage.get(n).and_then(Value::as_u64).unwrap_or(0);
            let total = field("input_tokens") + field("cached_input_tokens");
            if total > 0 {
                return Some(total);
            }
            if let Some(t) = usage.get("total_tokens").and_then(Value::as_u64) {
                if t > 0 {
                    return Some(t);
                }
            }
        }
    }
    None
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

    fn write_rollout(home: &Path, id: &str, prompt: Option<&str>) -> PathBuf {
        let day = home.join("sessions/2026/06/03");
        std::fs::create_dir_all(&day).unwrap();
        let mut lines = vec![format!(
            "{{\"timestamp\":\"2026-06-03T10:00:00.000Z\",\"type\":\"session_meta\",\
             \"payload\":{{\"id\":\"{id}\",\"cwd\":\"/work/proj\",\"originator\":\"codex_cli\"}}}}"
        )];
        lines.push("{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}".to_string());
        if let Some(p) = prompt {
            lines.push(format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\
                 \"message\":\"{p}\"}}}}"
            ));
        }
        let path = day.join(format!("rollout-2026-06-03T10-00-00-{id}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    #[test]
    fn lists_rollouts_with_meta_and_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(tmp.path(), "0199-aaaa", Some("arregla el parser"));
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "0199-aaaa");
        assert_eq!(s.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(s.title.as_deref(), Some("arregla el parser"));
        assert_eq!(s.provider, "codex");
    }

    #[test]
    fn session_index_name_wins_over_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(tmp.path(), "0199-bbbb", Some("prompt"));
        std::fs::write(
            tmp.path().join("session_index.jsonl"),
            "{\"id\":\"0199-bbbb\",\"thread_name\":\"viejo\",\"updated_at\":1}\n\
             {\"id\":\"0199-bbbb\",\"thread_name\":\"mi refactor\",\"updated_at\":2}\n",
        )
        .unwrap();
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions[0].title.as_deref(), Some("mi refactor"));
    }

    #[test]
    fn archived_sessions_are_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(tmp.path(), "0199-cccc", None);
        // Same shape under archived_sessions must not be picked up.
        let archived_day = tmp.path().join("archived_sessions/2026/06/03");
        std::fs::create_dir_all(&archived_day).unwrap();
        std::fs::write(
            archived_day.join("rollout-2026-06-03T09-00-00-0199-dddd.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"0199-dddd\",\"cwd\":\"/x\"}}\n",
        )
        .unwrap();
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        let ids: Vec<String> = provider
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["0199-cccc"]);
    }

    #[test]
    fn non_rollout_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/06/03");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("notes.jsonl"), "{}\n").unwrap();
        std::fs::write(day.join("rollout-bad.jsonl"), "{\"type\":\"other\"}\n").unwrap();
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        assert!(provider.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn resume_argv_shape() {
        let p = CodexProvider::new();
        assert_eq!(p.resume_argv("abc"), vec!["codex", "resume", "abc"]);
    }

    #[test]
    fn delete_archives_preserving_date_subpath() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollout(tmp.path(), "0199-arch", None);
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        provider.delete_session("0199-arch", false).unwrap();
        let archived = tmp
            .path()
            .join("archived_sessions/2026/06/03/rollout-2026-06-03T10-00-00-0199-arch.jsonl");
        assert!(archived.exists());
        assert!(provider.locate("0199-arch").is_none());
        // Idempotent: nothing left to archive.
        provider.delete_session("0199-arch", false).unwrap();
    }

    #[test]
    fn quota_reads_latest_rate_limits_from_rollouts() {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/06/04");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("rollout-2026-06-04T10-00-00-0199-qqqq.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"0199-qqqq\",\"cwd\":\"/w\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"rate_limits\":\
             {\"primary\":{\"used_percent\":52.0,\"window_minutes\":10080,\"resets_at\":1779690525},\
             \"secondary\":{\"used_percent\":12.5,\"window_minutes\":300,\"resets_at\":1779600000}}}}\n",
        )
        .unwrap();
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        let quota = provider.quota().unwrap();
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "session");
        assert_eq!(quota.windows[0].used_percent, 12.5);
        assert_eq!(quota.windows[1].label, "weekly");
        assert_eq!(quota.windows[1].used_percent, 52.0);
    }

    #[test]
    fn locate_and_preview_both_directions() {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/06/03");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("rollout-2026-06-03T10-00-00-0199-pppp.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"0199-pppp\",\"cwd\":\"/w\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"hola\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"respuesta codex\"}}\n",
        )
        .unwrap();
        let provider = CodexProvider::with_home(tmp.path().to_path_buf());
        assert!(provider.locate("0199-pppp").is_some());
        let turns = provider.preview("0199-pppp").unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].text, "respuesta codex");
        assert!(provider.fts_content("0199-pppp").unwrap().contains("hola"));
    }
}
