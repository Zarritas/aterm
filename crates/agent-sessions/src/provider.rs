use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::types::{AgentSession, DeleteError, LiveAgentSession, PreviewTurn, ProviderQuota};

/// One coding-agent CLI whose on-disk sessions we know how to read.
///
/// Implementations are read-only and derive every path from the home directory;
/// nothing here accepts paths from the frontend.
pub trait AgentProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    /// Name of the executable used to resume sessions (looked up in PATH).
    fn binary(&self) -> &'static str;
    /// The provider's data directory exists on this machine.
    fn detect(&self) -> bool;
    /// Every known session, unsorted; callers sort/group.
    fn list_sessions(&self) -> Result<Vec<AgentSession>, String>;
    /// Sessions the provider reports as currently running. Most providers
    /// keep no such registry and return the default empty vec.
    fn live_sessions(&self) -> Vec<LiveAgentSession> {
        Vec::new()
    }
    /// Argv to resume `session_id` (e.g. `["claude", "--resume", id]`).
    fn resume_argv(&self, session_id: &str) -> Vec<String>;
    /// Absolute path of the on-disk artefact backing `session_id`, if this
    /// provider stores one. None for providers without per-session files
    /// (opencode) or when the session isn't found.
    fn locate(&self, _session_id: &str) -> Option<PathBuf> {
        None
    }
    /// Last conversation turns for a preview. Err("unsupported") for
    /// providers whose content isn't readable from disk (opencode).
    fn preview(&self, _session_id: &str) -> Result<Vec<PreviewTurn>, String> {
        Err("unsupported".to_string())
    }
    /// Concatenated user+assistant text for the FTS index, capped. None when
    /// unsupported or the session can't be found.
    fn fts_content(&self, _session_id: &str) -> Option<String> {
        None
    }
    /// Delete every on-disk artefact for `session_id`. Idempotent: an
    /// already-absent session is success. `force` bypasses the live-session
    /// guard (only claude keeps a live registry). Deliberately has no default
    /// so every provider spells out its destructive flow.
    fn delete_session(&self, session_id: &str, force: bool) -> Result<(), DeleteError>;
    /// Account-level usage windows (session/weekly), when the provider
    /// records them somewhere we can read. None otherwise.
    fn quota(&self) -> Option<ProviderQuota> {
        None
    }
    /// Drop any internal result caches so the next scan is fully fresh.
    /// Called on user-initiated refresh; mtime-keyed file caches don't need
    /// it (they self-invalidate), so the default is a no-op.
    fn invalidate_caches(&self) {}
    /// Argv to start a fresh session (usually just the binary).
    fn new_session_argv(&self) -> Vec<String> {
        vec![self.binary().to_string()]
    }
    /// Argv that compacts `session_id`'s context without a terminal, when
    /// the CLI supports it. None: compaction only works inside a live TUI.
    fn compact_argv(&self, _session_id: &str) -> Option<Vec<String>> {
        None
    }
}

/// Per-file scan cache keyed by mtime. Session logs are append-only and big
/// (a real ~/.claude tops 100 MB); re-parsing only files whose mtime moved
/// turns the steady-state scan into a stat() walk.
///
/// Cached sessions are stored provider-shaped but *neutral*: `is_active`
/// false and `resume_argv` empty — both are stamped per call by the consumer.
#[derive(Default)]
pub struct FileScanCache {
    entries: Mutex<HashMap<PathBuf, (f64, AgentSession)>>,
}

impl FileScanCache {
    /// Return the cached session for `path` when `mtime` matches; otherwise
    /// run `build`, cache its result, and return it.
    pub fn get_or_build(
        &self,
        path: &Path,
        mtime: f64,
        build: impl FnOnce() -> Option<AgentSession>,
    ) -> Option<AgentSession> {
        if let Ok(entries) = self.entries.lock() {
            if let Some((cached_mtime, session)) = entries.get(path) {
                if *cached_mtime == mtime {
                    return Some(session.clone());
                }
            }
        }
        let session = build()?;
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(path.to_path_buf(), (mtime, session.clone()));
        }
        Some(session)
    }

    /// Drop entries whose file no longer exists (deleted sessions).
    pub fn retain_existing(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|path, _| path.exists());
        }
    }
}

/// Resolve `name` against PATH like the shell would, including Windows
/// extensions (`.exe`, `.cmd`, `.bat`) since agent CLIs ship as npm shims there.
///
/// GUI launches inherit a PATH without the entries that interactive-shell rc
/// files add (nvm, fnm, volta, bun…) — exactly where npm-installed agent CLIs
/// live. The new-session PTY runs the user's shell, so those binaries *are*
/// launchable even when this process can't see them; well-known per-user bin
/// dirs are searched as a fallback so the menu matches what the shell can run.
pub fn binary_in_path(name: &str) -> bool {
    resolve_binary(name).is_some()
}

/// Full path of `name` under PATH + the shell-only fallback dirs, or None.
/// Spawning through this instead of the bare name keeps headless runs
/// working when the binary is only reachable from an interactive shell.
pub fn resolve_binary(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect())
        .unwrap_or_default();
    dirs.extend(shell_only_bin_dirs());
    find_binary(name, &dirs)
}

fn find_binary(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let candidates: Vec<String> = if cfg!(windows) {
        ["", ".exe", ".cmd", ".bat"]
            .iter()
            .map(|ext| format!("{name}{ext}"))
            .collect()
    } else {
        vec![name.to_string()]
    };
    dirs.iter().find_map(|dir| {
        candidates.iter().find_map(|c| {
            let full: PathBuf = dir.join(c);
            full.is_file().then_some(full)
        })
    })
}

/// Per-user bin dirs that interactive shells put on PATH but GUI launches miss.
fn shell_only_bin_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut out = vec![
        home.join(".local/bin"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".volta/bin"),
        home.join(".asdf/shims"),
        home.join(".local/share/mise/shims"),
        home.join(".npm-global/bin"),
    ];
    // Node version managers nest binaries under per-version directories.
    for versions_root in [
        home.join(".nvm/versions/node"),
        home.join(".config/nvm/versions/node"),
    ] {
        out.extend(version_bin_dirs(&versions_root, "bin"));
    }
    out.extend(version_bin_dirs(
        &home.join(".local/share/fnm/node-versions"),
        "installation/bin",
    ));
    out
}

/// Expand `<root>/<version>/<suffix>` for every version dir under `root`.
fn version_bin_dirs(root: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join(suffix))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_in_path_finds_sh_on_unix() {
        #[cfg(unix)]
        assert!(binary_in_path("sh"));
    }

    #[test]
    fn binary_in_path_rejects_nonexistent() {
        assert!(!binary_in_path("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn find_binary_locates_file_outside_path() {
        let tmp = std::env::temp_dir().join("terax-test-bin-dirs");
        std::fs::create_dir_all(&tmp).unwrap();
        let bin = tmp.join("fake-agent-cli");
        std::fs::write(&bin, b"").unwrap();
        assert_eq!(
            find_binary("fake-agent-cli", &[tmp.clone()]),
            Some(bin.clone())
        );
        assert_eq!(find_binary("fake-agent-cli", &[tmp.join("nope")]), None);
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn version_bin_dirs_expands_versions_and_skips_missing_root() {
        let root = std::env::temp_dir().join("terax-test-nvm/versions/node");
        let bin = root.join("v25.0.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dirs = version_bin_dirs(&root, "bin");
        assert_eq!(dirs, vec![bin]);
        assert!(version_bin_dirs(Path::new("/definitely/missing"), "bin").is_empty());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("terax-test-nvm"));
    }

    fn dummy_session(title: &str) -> AgentSession {
        AgentSession {
            provider: "claude".to_string(),
            id: "x".to_string(),
            title: Some(title.to_string()),
            cwd: None,
            branch: None,
            message_count: None,
            size_bytes: None,
            last_activity: 0.0,
            is_active: false,
            context_tokens: None,
            context_window: None,
            model: None,
            started_at: None,
            cost_usd: None,
            live_status: None,
            resume_argv: Vec::new(),
        }
    }

    #[test]
    fn file_cache_hits_on_same_mtime_and_rebuilds_on_change() {
        let cache = FileScanCache::default();
        let path = Path::new("/fake/session.jsonl");
        let mut builds = 0;
        for _ in 0..3 {
            let s = cache.get_or_build(path, 100.0, || {
                builds += 1;
                Some(dummy_session("v1"))
            });
            assert_eq!(s.unwrap().title.as_deref(), Some("v1"));
        }
        assert_eq!(builds, 1, "same mtime must reuse the cached entry");

        let s = cache.get_or_build(path, 200.0, || Some(dummy_session("v2")));
        assert_eq!(s.unwrap().title.as_deref(), Some("v2"));
    }

    #[test]
    fn file_cache_failed_build_is_not_cached() {
        let cache = FileScanCache::default();
        let path = Path::new("/fake/broken.jsonl");
        assert!(cache.get_or_build(path, 1.0, || None).is_none());
        // A later successful build must still run.
        let s = cache.get_or_build(path, 1.0, || Some(dummy_session("ok")));
        assert!(s.is_some());
    }
}
