// OpenCode session provider.
//
// OpenCode's source of truth is a SQLite database whose schema is internal and
// has already migrated once (JSON storage → DB). Instead of binding to it, we
// shell out to its own stable CLI: `opencode session list --format json`, which
// emits `{id, title, updated, created, projectId, directory}` per session
// (timestamps in milliseconds). Fixed argv, no shell, short timeout.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::proc::hide_console;
use crate::provider::{binary_in_path, AgentProvider};
use crate::types::{AgentSession, DeleteError};

const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// Floor between CLI runs even when the database keeps changing (an active
/// opencode writes its WAL constantly); each run costs 4-9s and ~245 MB RSS.
const RELIST_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Combined mtimes of opencode's database (+ WAL): if they didn't move, the
/// sessions didn't change and the cached result is exact, no matter how old.
#[derive(PartialEq, Clone, Copy, Debug, Default)]
struct DbStamp {
    db_mtime: f64,
    wal_mtime: f64,
}

struct CachedList {
    stamp: DbStamp,
    at: std::time::Instant,
    sessions: Vec<AgentSession>,
}

#[derive(Deserialize)]
struct OpencodeSessionRow {
    id: String,
    title: Option<String>,
    updated: Option<f64>,
    created: Option<f64>,
    #[serde(rename = "directory")]
    directory: Option<String>,
}

pub struct OpencodeProvider {
    data_dir: PathBuf,
    /// Result cache keyed by the database stamp. The mutex is held across the
    /// subprocess call on purpose (single-flight): concurrent scans wait for
    /// the in-flight CLI run and reuse its result instead of stacking 245 MB
    /// processes.
    cache: std::sync::Mutex<Option<CachedList>>,
}

impl OpencodeProvider {
    pub fn new() -> Self {
        // xdg-basedir semantics: $XDG_DATA_HOME or ~/.local/share, all platforms.
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".local")
                    .join("share")
            });
        Self {
            data_dir: base.join("opencode"),
            cache: std::sync::Mutex::new(None),
        }
    }

    fn db_stamp(&self) -> DbStamp {
        let mtime = |name: &str| -> f64 {
            std::fs::metadata(self.data_dir.join(name))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        };
        DbStamp {
            db_mtime: mtime("opencode.db"),
            wal_mtime: mtime("opencode.db-wal"),
        }
    }
}

impl Default for OpencodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentProvider for OpencodeProvider {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
    }

    fn binary(&self) -> &'static str {
        "opencode"
    }

    fn detect(&self) -> bool {
        // Listing shells out to the binary, so require both artifacts and binary.
        self.data_dir.is_dir() && binary_in_path(self.binary())
    }

    fn list_sessions(&self) -> Result<Vec<AgentSession>, String> {
        let mut cache = self.cache.lock().map_err(|e| e.to_string())?;
        let stamp = self.db_stamp();
        if let Some(cached) = cache.as_ref() {
            let unchanged = cached.stamp == stamp;
            let too_soon = cached.at.elapsed() < RELIST_MIN_INTERVAL;
            if unchanged || too_soon {
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
            // Re-stamp after the run: the CLI itself may touch the WAL, and
            // caching the pre-run stamp would invalidate immediately.
            stamp: self.db_stamp(),
            at: std::time::Instant::now(),
            sessions: sessions.clone(),
        });
        Ok(sessions)
    }

    fn resume_argv(&self, session_id: &str) -> Vec<String> {
        vec![
            "opencode".to_string(),
            "--session".to_string(),
            session_id.to_string(),
        ]
    }

    fn invalidate_caches(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
    }

    /// OpenCode owns its storage (SQLite); deletion goes through its CLI.
    fn delete_session(&self, session_id: &str, _force: bool) -> Result<(), DeleteError> {
        run_with_timeout(
            self.binary(),
            &["session", "delete", session_id],
            LIST_TIMEOUT,
        )
        .map_err(DeleteError::Subprocess)?;
        self.invalidate_caches();
        Ok(())
    }
}

/// Run `bin args..` from the home dir with a hard timeout. The child is
/// spawned on a helper thread so a hung CLI can't wedge the scan; on timeout
/// it is killed and an error returned. Shared with other CLI-backed providers
/// (e.g. goose).
pub(crate) fn run_with_timeout(
    bin: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("{bin}: {e}"))?;
    let stdout = child.stdout.take();

    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_string(&mut buf);
        }
        let _ = tx.send(buf);
    });

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = rx.recv().unwrap_or_default();
                let _ = reader.join();
                if !status.success() {
                    return Err(format!("{bin} exited with {status}"));
                }
                return Ok(output);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(format!("{bin} timed out after {timeout:?}"));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn parse_session_list(raw: &str) -> Result<Vec<AgentSession>, String> {
    // The CLI may prepend log noise; parse from the first '['.
    let start = raw
        .find('[')
        .ok_or("unexpected opencode session list output")?;
    let rows: Vec<OpencodeSessionRow> =
        serde_json::from_str(raw[start..].trim()).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let last_ms = row.updated.or(row.created).unwrap_or(0.0);
            AgentSession {
                provider: "opencode".to_string(),
                id: row.id,
                title: row.title.filter(|t| !t.is_empty()),
                cwd: row.directory,
                branch: None,
                message_count: None,
                size_bytes: None,
                last_activity: last_ms / 1000.0,
                is_active: false,
                context_tokens: None,
                context_window: None,
                model: None,
                started_at: None,
                cost_usd: None,
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
        let raw = r#"[
          {"id":"ses_abc","title":"Mi refactor","updated":1780490606640,
           "created":1780490603780,"projectId":"global","directory":"/tmp/x"}
        ]"#;
        let sessions = parse_session_list(raw).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "ses_abc");
        assert_eq!(s.title.as_deref(), Some("Mi refactor"));
        assert_eq!(s.cwd.as_deref(), Some("/tmp/x"));
        // Milliseconds converted to seconds.
        assert!((s.last_activity - 1_780_490_606.640).abs() < 0.001);
    }

    #[test]
    fn tolerates_leading_log_noise() {
        let raw = "INFO something\n[{\"id\":\"ses_x\",\"title\":\"t\",\"updated\":1000}]";
        let sessions = parse_session_list(raw).unwrap();
        assert_eq!(sessions[0].id, "ses_x");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_session_list("no json here").is_err());
    }

    #[test]
    fn resume_argv_shape() {
        let p = OpencodeProvider::new();
        assert_eq!(
            p.resume_argv("ses_1"),
            vec!["opencode", "--session", "ses_1"]
        );
    }
}
