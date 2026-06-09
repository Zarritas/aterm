// Live-session detection for providers that persist a registry of running
// processes. Today only Claude Code does (`~/.claude/sessions/<pid>.json`).
//
// Registry files can outlive their process (crashes, power loss), so an entry
// only counts when its pid is still alive — and, on Linux, when the recorded
// `procStart` still matches `/proc/<pid>/stat` field 22 (guards against pid
// reuse after reboots or long uptimes).

use std::path::Path;

use serde::Deserialize;

use crate::types::LiveAgentSession;

#[derive(Deserialize)]
struct ClaudeRegistryEntry {
    pid: Option<u32>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "procStart")]
    proc_start: Option<String>,
    status: Option<String>,
}

/// Parse Claude Code's live registry under `claude_home/sessions/`.
pub fn claude_live_sessions(claude_home: &Path) -> Vec<LiveAgentSession> {
    let dir = claude_home.join("sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<ClaudeRegistryEntry>(&raw) else {
            continue;
        };
        let (Some(pid), Some(session_id)) = (parsed.pid, parsed.session_id) else {
            continue;
        };
        if !pid_alive(pid) {
            continue;
        }
        if let Some(proc_start) = parsed.proc_start.as_deref() {
            if !proc_start_matches(pid, proc_start) {
                continue;
            }
        }
        live.push(LiveAgentSession {
            provider: "claude".to_string(),
            session_id,
            pid,
            status: parsed.status,
        });
    }
    live
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) probes existence without signalling. EPERM still means alive.
    let res = unsafe { libc::kill(pid as libc::pid_t, 0) };
    res == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

/// Compare the registry's `procStart` with the live process start time.
/// Only meaningful where procfs exists (Linux); elsewhere trust the pid check.
fn proc_start_matches(pid: u32, proc_start: &str) -> bool {
    let stat_path = format!("/proc/{pid}/stat");
    if !Path::new(&stat_path).exists() {
        return true;
    }
    let Ok(stat) = std::fs::read_to_string(&stat_path) else {
        return true;
    };
    match parse_stat_starttime(&stat) {
        Some(starttime) => starttime == proc_start,
        None => true,
    }
}

/// Extract field 22 (`starttime`) from a `/proc/<pid>/stat` line. The comm
/// field (2) may contain spaces and parens, so split after the LAST `)`:
/// what follows is whitespace-separated starting at field 3, putting
/// starttime at index 19.
pub fn parse_stat_starttime(stat: &str) -> Option<&str> {
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_registry(dir: &Path, pid: u32, session_id: &str, proc_start: Option<&str>) {
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let proc_start_field = proc_start
            .map(|p| format!(",\"procStart\":\"{p}\""))
            .unwrap_or_default();
        std::fs::write(
            sessions.join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"{session_id}\"{proc_start_field}}}"),
        )
        .unwrap();
    }

    #[test]
    fn parses_starttime_with_spaces_and_parens_in_comm() {
        let stat = "1234 (we)ird (name) S 1 1234 1234 0 -1 4194560 0 0 0 0 0 0 0 0 \
                    20 0 1 0 9876543 0 0 18446744073709551615";
        assert_eq!(parse_stat_starttime(stat), Some("9876543"));
    }

    #[test]
    fn starttime_none_on_garbage() {
        assert_eq!(parse_stat_starttime("no parens here"), None);
        assert_eq!(parse_stat_starttime("1 (x) S 1"), None);
    }

    #[test]
    fn dead_pid_is_not_live() {
        let tmp = tempfile::tempdir().unwrap();
        // Pids cycle below ~4 million on Linux; this one can't be alive.
        write_registry(tmp.path(), 4_000_000, "sid-dead", None);
        assert!(claude_live_sessions(tmp.path()).is_empty());
    }

    #[test]
    fn own_pid_is_live() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        write_registry(tmp.path(), pid, "sid-self", None);
        let live = claude_live_sessions(tmp.path());
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].session_id, "sid-self");
        assert_eq!(live[0].provider, "claude");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_start_mismatch_marks_entry_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        write_registry(tmp.path(), pid, "sid-reused", Some("1"));
        // Our own starttime is never 1 (that's the boot-time init range).
        assert!(claude_live_sessions(tmp.path()).is_empty());
    }

    #[test]
    fn corrupt_and_foreign_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("bad.json"), "{not json").unwrap();
        std::fs::write(sessions.join("notes.txt"), "ignored").unwrap();
        assert!(claude_live_sessions(tmp.path()).is_empty());
    }

    #[test]
    fn missing_registry_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(claude_live_sessions(&tmp.path().join("nope")).is_empty());
    }
}
