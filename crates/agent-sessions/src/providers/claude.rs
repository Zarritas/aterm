// Claude Code session provider.
//
// Layout: `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, one event per
// line. The dir name is the session cwd with every non-alphanumeric char
// replaced by `-`, so it can't be decoded losslessly — the real cwd is read
// back from the sessions' own `cwd` events, preferring the candidate whose
// re-encoding matches the dir name (sessions moved across cwds record a stale
// first cwd and would otherwise flip the project identity).

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::extract::{self, strip_command_wrappers, truncate_title};
use crate::live;
use crate::provider::{AgentProvider, FileScanCache};
use crate::types::{
    AgentSession, DeleteError, LiveAgentSession, PreviewTurn, ProviderQuota, QuotaWindow,
};

const HEADER_SCAN_LINES: usize = 80;
const PROMPT_MAX_CHARS: usize = 120;
/// Cap when scanning long sessions for the latest `/rename`.
const RENAME_SCAN_LINES: usize = 50_000;
/// Files larger than this skip the exact line count (message_count = None)
/// rather than stalling a scan on a runaway session log.
const LINE_COUNT_MAX_BYTES: u64 = 50 * 1024 * 1024;

pub struct ClaudeProvider {
    home: PathBuf,
    cache: FileScanCache,
}

impl ClaudeProvider {
    pub fn new() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_default().join(".claude"),
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

impl Default for ClaudeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn binary(&self) -> &'static str {
        "claude"
    }

    fn detect(&self) -> bool {
        self.projects_dir().is_dir()
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let projects = self.projects_dir();
        let Ok(entries) = std::fs::read_dir(&projects) else {
            return Ok(Vec::new());
        };
        let active: std::collections::HashMap<String, Option<String>> = self
            .live_sessions()
            .into_iter()
            .map(|l| (l.session_id, l.status))
            .collect();
        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let jsonl_files = jsonl_files_newest_first(&project_dir);
            if jsonl_files.is_empty() {
                continue;
            }
            let project_cwd = resolve_real_cwd(&project_dir, &jsonl_files);
            for jsonl in &jsonl_files {
                let Some(mtime) = mtime_secs(jsonl) else {
                    continue;
                };
                // The expensive parse (header + line count + rename scan) is
                // cached by mtime; activity is stamped fresh on every call.
                let Some(mut session) = self.cache.get_or_build(jsonl, mtime, || {
                    build_session(jsonl, project_cwd.as_deref())
                }) else {
                    continue;
                };
                if let Some(status) = active.get(&session.id) {
                    session.is_active = true;
                    session.live_status = status.clone();
                }
                sessions.push(session);
            }
        }
        self.cache.retain_existing();
        Ok(sessions)
    }

    fn live_sessions(&self) -> Vec<LiveAgentSession> {
        live::claude_live_sessions(&self.home)
    }

    fn resume_argv(&self, session_id: &str) -> Vec<String> {
        vec![
            "claude".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    /// `/compact` runs for real in print mode despite the docs claiming
    /// slash commands are interactive-only (verified: it appends a
    /// `system/compact_boundary` + `isCompactSummary` pair to the SAME
    /// session id, no fork).
    fn compact_argv(&self, session_id: &str) -> Option<Vec<String>> {
        Some(vec![
            "claude".to_string(),
            "--resume".to_string(),
            session_id.to_string(),
            "-p".to_string(),
            "/compact".to_string(),
        ])
    }

    fn locate(&self, session_id: &str) -> Option<PathBuf> {
        let target = format!("{session_id}.jsonl");
        let entries = std::fs::read_dir(self.projects_dir()).ok()?;
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
        Ok(extract::preview_turns(&path, claude_turn))
    }

    fn fts_content(&self, session_id: &str) -> Option<String> {
        let path = self.locate(session_id)?;
        extract::fts_text(&path, claude_turn)
    }

    /// Claude Code only hands account rate limits to the statusline at
    /// runtime; a small statusline addition mirrors them to
    /// `~/.claude/rate-limits-cache.json`, which this reads.
    fn quota(&self) -> Option<ProviderQuota> {
        let raw = std::fs::read_to_string(self.home.join("rate-limits-cache.json")).ok()?;
        let data = serde_json::from_str::<Value>(&raw).ok()?;
        let window = |key: &str, label: &str| -> Option<QuotaWindow> {
            let w = data.get(key)?;
            Some(QuotaWindow {
                label: label.to_string(),
                used_percent: w.get("used_percentage").and_then(Value::as_f64)?,
                resets_at: w.get("resets_at").and_then(Value::as_u64),
            })
        };
        let windows: Vec<QuotaWindow> = [
            window("five_hour", "session"),
            window("seven_day", "weekly"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if windows.is_empty() {
            return None;
        }
        Some(ProviderQuota {
            provider: "claude".to_string(),
            windows,
            as_of: data.get("updated_at").and_then(Value::as_f64),
        })
    }

    /// Port of multi-claude's delete_session: jsonl + `<id>/` subagents
    /// subdir + `session-env/<id>`, guarded against live sessions.
    fn delete_session(&self, session_id: &str, force: bool) -> Result<(), DeleteError> {
        if !force
            && self
                .live_sessions()
                .iter()
                .any(|l| l.session_id == session_id)
        {
            return Err(DeleteError::Active);
        }
        if let Some(jsonl) = self.locate(session_id) {
            std::fs::remove_file(&jsonl).map_err(|e| DeleteError::Io(e.to_string()))?;
            let subdir = jsonl.with_extension("");
            if subdir.is_dir() {
                std::fs::remove_dir_all(&subdir).map_err(|e| DeleteError::Io(e.to_string()))?;
            }
        }
        let env_path = self.home.join("session-env").join(session_id);
        if env_path.is_dir() {
            std::fs::remove_dir_all(&env_path).map_err(|e| DeleteError::Io(e.to_string()))?;
        } else if env_path.exists() {
            std::fs::remove_file(&env_path).map_err(|e| DeleteError::Io(e.to_string()))?;
        }
        self.cache.retain_existing();
        Ok(())
    }
}

/// Per-event extractor: user/assistant turns from `message.content`
/// (plain string or text blocks; tool payloads are skipped).
pub(crate) fn claude_turn(event: &Value) -> Option<(&'static str, String)> {
    let role = match event.get("type").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let content = event.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return Some((role, text.to_string()));
    }
    let mut parts: Vec<&str> = Vec::new();
    for block in content.as_array()? {
        if block.get("type").and_then(Value::as_str) == Some("text") {
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

fn jsonl_files_newest_first(project_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return Vec::new();
    };
    let mut files: Vec<(f64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return None;
            }
            let mtime = mtime_secs(&path)?;
            Some((mtime, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.total_cmp(&a.0));
    files.into_iter().map(|(_, p)| p).collect()
}

fn build_session(jsonl: &Path, project_cwd: Option<&str>) -> Option<AgentSession> {
    let id = jsonl.file_stem()?.to_string_lossy().to_string();
    let meta = std::fs::metadata(jsonl).ok()?;
    let header = parse_session_header(jsonl);
    let message_count = if meta.len() <= LINE_COUNT_MAX_BYTES {
        count_lines(jsonl).map(|n| n as u32)
    } else {
        None
    };
    let deep = deep_scan(jsonl);
    let title = deep
        .embedded_name
        .or(header.display_name)
        .or(header.first_prompt);
    Some(AgentSession {
        provider: "claude".to_string(),
        is_active: false, // stamped per call from the live registry
        live_status: None,
        resume_argv: Vec::new(),
        context_window: deep.context_tokens.map(infer_claude_window),
        context_tokens: deep.context_tokens,
        model: deep.model,
        started_at: header.started_at,
        cost_usd: deep.cost_usd,
        id,
        title,
        // Resume must run under the cwd the project dir was named after;
        // the session's own header cwd is only a fallback.
        cwd: project_cwd.map(str::to_string).or(header.cwd),
        branch: header.branch,
        message_count,
        size_bytes: Some(meta.len()),
        last_activity: mtime_secs(jsonl).unwrap_or(0.0),
    })
}

/// Replicate Claude Code's project-dir encoding: every non-alphanumeric → '-'.
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Resolve the project's real cwd from its sessions' `cwd` events, newest
/// first, preferring the candidate whose encoding matches the dir name.
fn resolve_real_cwd(project_dir: &Path, jsonl_newest_first: &[PathBuf]) -> Option<String> {
    let dir_name = project_dir.file_name()?.to_string_lossy();
    let mut fallback: Option<String> = None;
    for jsonl in jsonl_newest_first {
        let Some(cwd) = first_cwd(jsonl) else {
            continue;
        };
        if fallback.is_none() {
            fallback = Some(cwd.clone());
        }
        if encode_cwd(&cwd) == dir_name {
            return Some(cwd);
        }
    }
    fallback
}

fn first_cwd(jsonl: &Path) -> Option<String> {
    let file = File::open(jsonl).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(HEADER_SCAN_LINES) {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(cwd) = event.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

#[derive(Default)]
struct SessionHeader {
    first_prompt: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    display_name: Option<String>,
    started_at: Option<f64>,
}

fn parse_session_header(jsonl: &Path) -> SessionHeader {
    let mut header = SessionHeader::default();
    let Ok(file) = File::open(jsonl) else {
        return header;
    };
    let reader = BufReader::new(file);
    for line in reader.lines().take(HEADER_SCAN_LINES) {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if header.started_at.is_none() {
            header.started_at = event
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_iso_seconds);
        }
        if header.cwd.is_none() {
            header.cwd = non_empty_str(event.get("cwd"));
        }
        if header.branch.is_none() {
            header.branch = non_empty_str(event.get("gitBranch"));
        }
        if header.display_name.is_none() {
            header.display_name = non_empty_str(event.get("name"));
        }
        if header.first_prompt.is_none() {
            if let Some(prompt) = extract_user_prompt(&event) {
                header.first_prompt = Some(truncate_title(
                    &strip_command_wrappers(&prompt),
                    PROMPT_MAX_CHARS,
                ));
            }
        }
        if header.first_prompt.is_some()
            && header.cwd.is_some()
            && header.branch.is_some()
            && header.display_name.is_some()
        {
            break;
        }
    }
    header
}

/// "2026-06-04T10:00:00.000Z" -> unix seconds, without a chrono dependency.
fn parse_iso_seconds(iso: &str) -> Option<f64> {
    let date = iso.get(0..10)?;
    let time = iso.get(11..19)?;
    let mut dp = date.split('-');
    let (y, m, d): (i64, i64, i64) = (
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
        dp.next()?.parse().ok()?,
    );
    let mut tp = time.split(':');
    let (hh, mm, ss): (i64, i64, i64) = (
        tp.next()?.parse().ok()?,
        tp.next()?.parse().ok()?,
        tp.next()?.parse().ok()?,
    );
    // Civil-to-days (Howard Hinnant), inverse of the export timestamp.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + hh * 3600 + mm * 60 + ss) as f64)
}

fn non_empty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// If this event is a user message with text content, return that content.
fn extract_user_prompt(event: &Value) -> Option<String> {
    if event.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let message = event.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    // Some user messages come as a list of blocks; pick the first text block.
    for block in content.as_array()? {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
    }
    None
}

#[derive(Default)]
struct DeepScan {
    embedded_name: Option<String>,
    context_tokens: Option<u64>,
    model: Option<String>,
    cost_usd: Option<f64>,
}

/// Approximate API rates per million tokens (input, output, cache write,
/// cache read) by model family. Estimates: subscription plans don't bill
/// per token, and rates drift over time.
fn model_rates(model: &str) -> (f64, f64, f64, f64) {
    if model.contains("opus") {
        (15.0, 75.0, 18.75, 1.5)
    } else if model.contains("haiku") {
        (1.0, 5.0, 1.25, 0.1)
    } else {
        // sonnet and unknown families
        (3.0, 15.0, 3.75, 0.3)
    }
}

/// One bounded pass over the log capturing the latest `/rename` (later
/// renames win — `system/local_command` stdout markers, plus a deprecated
/// top-level `name` string) and the latest `usage` block, whose input +
/// cache tokens equal the session's current context size. Only candidate
/// lines are JSON-parsed; the usage candidate is kept as a string and parsed
/// once at the end.
fn deep_scan(jsonl: &Path) -> DeepScan {
    const MARKER: &str = "Session renamed to:";
    let mut result = DeepScan::default();
    let Ok(file) = File::open(jsonl) else {
        return result;
    };
    let reader = BufReader::new(file);
    let mut latest: Option<String> = None;
    let mut latest_usage_line: Option<String> = None;
    let mut cost = 0.0f64;
    for line in reader.lines().take(RENAME_SCAN_LINES) {
        let Ok(line) = line else { break };
        if line.contains("\"usage\"") {
            if let Some((model, usage_cost)) = parse_usage_cost(&line) {
                cost += usage_cost;
                result.model = Some(model);
            }
            latest_usage_line = Some(line.clone());
        }
        // Cheap pre-filter: full JSON parse only for candidate lines.
        let has_marker = line.contains(MARKER);
        let has_name = line.contains("\"name\"");
        if !has_marker && !has_name {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(top_name) = event.get("name").and_then(Value::as_str) {
            let trimmed = top_name.trim();
            if !trimmed.is_empty() {
                latest = Some(trimmed.to_string());
                continue;
            }
        }
        if !has_marker
            || event.get("type").and_then(Value::as_str) != Some("system")
            || event.get("subtype").and_then(Value::as_str) != Some("local_command")
        {
            continue;
        }
        let Some(content) = event.get("content").and_then(Value::as_str) else {
            continue;
        };
        if let Some(name) = parse_rename_stdout(content) {
            latest = Some(name);
        }
    }
    result.embedded_name = latest;
    result.context_tokens = latest_usage_line.as_deref().and_then(parse_context_tokens);
    result.cost_usd = (cost > 0.0).then_some(cost);
    result
}

/// Cost contribution of one assistant event plus its model name.
fn parse_usage_cost(line: &str) -> Option<(String, f64)> {
    let event = serde_json::from_str::<Value>(line).ok()?;
    let message = event.get("message")?;
    let model = message.get("model").and_then(Value::as_str)?.to_string();
    let usage = message.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0) as f64;
    let (rate_in, rate_out, rate_cw, rate_cr) = model_rates(&model);
    let cost = (field("input_tokens") * rate_in
        + field("output_tokens") * rate_out
        + field("cache_creation_input_tokens") * rate_cw
        + field("cache_read_input_tokens") * rate_cr)
        / 1_000_000.0;
    Some((model, cost))
}

/// Claude's logs don't record the window size. Assume the 200k baseline and
/// upgrade to the 1M beta once usage proves it — conservative on purpose: a
/// 1M user sees a pessimistic percentage below 200k, never an optimistic one.
fn infer_claude_window(tokens: u64) -> u64 {
    if tokens > 200_000 {
        1_000_000
    } else {
        200_000
    }
}

/// Context size of one assistant event: usage input + cache read + cache
/// creation tokens (what Claude Code itself reports as context).
fn parse_context_tokens(line: &str) -> Option<u64> {
    let event = serde_json::from_str::<Value>(line).ok()?;
    let usage = event.get("message")?.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let total = field("input_tokens")
        + field("cache_read_input_tokens")
        + field("cache_creation_input_tokens");
    (total > 0).then_some(total)
}

/// Extract X from `<local-command-stdout>Session renamed to: X</local-command-stdout>`.
fn parse_rename_stdout(content: &str) -> Option<String> {
    let open = content.find("<local-command-stdout>")?;
    let rest = &content[open + "<local-command-stdout>".len()..];
    let close = rest.find("</local-command-stdout>")?;
    let inner = rest[..close].trim();
    let name = inner.strip_prefix("Session renamed to:")?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Streaming newline count, 64 KB chunks, no full file in memory.
pub fn count_lines(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 64 * 1024];
    let mut count: u64 = 0;
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        count += buf[..n].iter().filter(|&&b| b == b'\n').count() as u64;
    }
    Some(count)
}

fn mtime_secs(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn user_event(cwd: &str, prompt: &str) -> String {
        format!(
            "{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"gitBranch\":\"main\",\
             \"message\":{{\"role\":\"user\",\"content\":\"{prompt}\"}}}}"
        )
    }

    #[test]
    fn encode_cwd_replaces_non_alphanumerics() {
        assert_eq!(encode_cwd("/home/me/WS/repo"), "-home-me-WS-repo");
        assert_eq!(encode_cwd("/a/b.c_d"), "-a-b-c-d");
    }

    #[test]
    fn count_lines_counts_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.jsonl");
        std::fs::write(&path, "a\nb\nc\n").unwrap();
        assert_eq!(count_lines(&path), Some(3));
    }

    #[test]
    fn header_extracts_prompt_cwd_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session(
            tmp.path(),
            "s1",
            &[
                "{\"type\":\"summary\"}",
                &user_event("/work/x", "arregla el bug"),
            ],
        );
        let header = parse_session_header(&path);
        assert_eq!(header.first_prompt.as_deref(), Some("arregla el bug"));
        assert_eq!(header.cwd.as_deref(), Some("/work/x"));
        assert_eq!(header.branch.as_deref(), Some("main"));
    }

    #[test]
    fn embedded_rename_latest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let rename = |n: &str| {
            format!(
                "{{\"type\":\"system\",\"subtype\":\"local_command\",\"content\":\
                 \"<local-command-stdout>Session renamed to: {n}</local-command-stdout>\"}}"
            )
        };
        let path = write_session(
            tmp.path(),
            "s2",
            &[
                &user_event("/w", "p"),
                &rename("primero"),
                &rename("segundo"),
            ],
        );
        assert_eq!(deep_scan(&path).embedded_name.as_deref(), Some("segundo"));
    }

    #[test]
    fn resolve_real_cwd_prefers_encode_match_over_newest() {
        let tmp = tempfile::tempdir().unwrap();
        // Project dir named after /work/sub, but the NEWEST session records a
        // stale parent cwd (moved/resumed session). The encode match must win.
        let project = tmp.path().join(encode_cwd("/work/sub"));
        let old = write_session(&project, "old", &[&user_event("/work/sub", "a")]);
        let newer = write_session(&project, "new", &[&user_event("/work", "b")]);
        let t = std::time::SystemTime::now();
        let ft_old = filetime::FileTime::from_system_time(t - std::time::Duration::from_secs(100));
        let ft_new = filetime::FileTime::from_system_time(t);
        filetime::set_file_mtime(&old, ft_old).unwrap();
        filetime::set_file_mtime(&newer, ft_new).unwrap();
        let files = jsonl_files_newest_first(&project);
        assert_eq!(
            resolve_real_cwd(&project, &files).as_deref(),
            Some("/work/sub")
        );
    }

    #[test]
    fn list_sessions_builds_full_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".claude");
        let project = home.join("projects").join(encode_cwd("/work/proj"));
        write_session(
            &project,
            "abc-123",
            &[
                &user_event("/work/proj", "implementa la feature"),
                "{\"type\":\"assistant\"}",
            ],
        );
        let provider = ClaudeProvider::with_home(home);
        let sessions = provider.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.provider, "claude");
        assert_eq!(s.id, "abc-123");
        assert_eq!(s.title.as_deref(), Some("implementa la feature"));
        assert_eq!(s.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.message_count, Some(2));
        assert!(!s.is_active);
    }

    #[test]
    fn resume_argv_shape() {
        let p = ClaudeProvider::new();
        assert_eq!(p.resume_argv("xyz"), vec!["claude", "--resume", "xyz"]);
        assert_eq!(p.new_session_argv(), vec!["claude"]);
        assert_eq!(
            p.compact_argv("xyz"),
            Some(
                ["claude", "--resume", "xyz", "-p", "/compact"]
                    .map(String::from)
                    .to_vec()
            )
        );
    }

    #[test]
    fn quota_reads_the_statusline_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".claude");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("rate-limits-cache.json"),
            "{\"five_hour\":{\"used_percentage\":50,\"resets_at\":1780600000},\
             \"seven_day\":{\"used_percentage\":25,\"resets_at\":1780900000},\
             \"updated_at\":1780568594.3}",
        )
        .unwrap();
        let provider = ClaudeProvider::with_home(home.clone());
        let quota = provider.quota().unwrap();
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "session");
        assert_eq!(quota.windows[0].used_percent, 50.0);
        assert_eq!(quota.windows[1].label, "weekly");
        assert_eq!(quota.windows[1].resets_at, Some(1_780_900_000));
        // Missing cache -> None.
        std::fs::remove_file(home.join("rate-limits-cache.json")).unwrap();
        assert!(provider.quota().is_none());
    }

    #[test]
    fn deep_scan_captures_latest_context_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let usage = |inp: u64, read: u64| {
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"usage\":{{\"input_tokens\":{inp},\
                 \"cache_read_input_tokens\":{read},\"cache_creation_input_tokens\":100,\
                 \"output_tokens\":5}}}}}}"
            )
        };
        let path = write_session(
            tmp.path(),
            "s-ctx",
            &[&user_event("/w", "p"), &usage(10, 1000), &usage(2, 856_101)],
        );
        let deep = deep_scan(&path);
        // Latest usage wins: 2 + 856101 + 100.
        assert_eq!(deep.context_tokens, Some(856_203));
    }

    #[test]
    fn delete_removes_jsonl_subdir_and_session_env() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".claude");
        let project = home.join("projects").join(encode_cwd("/work/p"));
        let jsonl = write_session(&project, "sid-del", &[&user_event("/work/p", "x")]);
        std::fs::create_dir_all(project.join("sid-del")).unwrap();
        std::fs::write(project.join("sid-del/agent.jsonl"), "{}").unwrap();
        let env_dir = home.join("session-env");
        std::fs::create_dir_all(&env_dir).unwrap();
        std::fs::write(env_dir.join("sid-del"), "VAR=1").unwrap();

        let provider = ClaudeProvider::with_home(home.clone());
        provider.delete_session("sid-del", false).unwrap();
        assert!(!jsonl.exists());
        assert!(!project.join("sid-del").exists());
        assert!(!env_dir.join("sid-del").exists());
        // Idempotent on repeat.
        provider.delete_session("sid-del", false).unwrap();
    }

    #[test]
    fn delete_blocks_live_sessions_unless_forced() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".claude");
        let project = home.join("projects").join(encode_cwd("/work/p"));
        write_session(&project, "sid-live", &[&user_event("/work/p", "x")]);
        let registry = home.join("sessions");
        std::fs::create_dir_all(&registry).unwrap();
        let pid = std::process::id();
        std::fs::write(
            registry.join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"sid-live\"}}"),
        )
        .unwrap();

        let provider = ClaudeProvider::with_home(home);
        assert_eq!(
            provider.delete_session("sid-live", false),
            Err(DeleteError::Active)
        );
        provider.delete_session("sid-live", true).unwrap();
        assert!(provider.locate("sid-live").is_none());
    }

    #[test]
    fn locate_and_preview_read_the_session_log() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".claude");
        let project = home.join("projects").join(encode_cwd("/work/p"));
        write_session(
            &project,
            "sid-prev",
            &[
                &user_event("/work/p", "pregunta inicial"),
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"respuesta\"}]}}",
            ],
        );
        let provider = ClaudeProvider::with_home(home);
        assert!(provider.locate("sid-prev").is_some());
        assert!(provider.locate("missing").is_none());
        let turns = provider.preview("sid-prev").unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].text, "respuesta");
        let fts = provider.fts_content("sid-prev").unwrap();
        assert!(fts.contains("pregunta inicial") && fts.contains("respuesta"));
    }
}
