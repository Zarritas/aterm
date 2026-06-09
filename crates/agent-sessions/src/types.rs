use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    /// Provider id: "claude" | "codex" | "opencode" | "gemini".
    pub provider: String,
    /// The id the provider's own CLI accepts in its resume command.
    pub id: String,
    /// Best available label: explicit rename/title, else first prompt.
    pub title: Option<String>,
    /// Working directory to resume under. None when the provider can't recover it.
    pub cwd: Option<String>,
    pub branch: Option<String>,
    /// None when the provider doesn't track it (or counting was capped).
    pub message_count: Option<u32>,
    pub size_bytes: Option<u64>,
    /// Unix seconds of the last activity (file mtime).
    pub last_activity: f64,
    /// True when the provider's live registry reports the session as running.
    /// Only Claude Code persists such a registry today.
    pub is_active: bool,
    /// Context-window tokens of the latest model turn (input + cache read +
    /// cache creation), when the provider records usage. None otherwise.
    pub context_tokens: Option<u64>,
    /// Model of the latest turn (e.g. "claude-opus-4-8"), when recorded.
    pub model: Option<String>,
    /// Unix seconds of the first event, for wall-clock session duration.
    pub started_at: Option<f64>,
    /// Estimated API-equivalent cost in USD, summed over every usage block
    /// with per-model rates. An estimate by nature (subscription plans don't
    /// bill per token); None when the provider records no usage.
    pub cost_usd: Option<f64>,
    /// Live state from the provider registry: "busy" | "idle" | other.
    pub live_status: Option<String>,
    /// Size of the model's context window. Exact for codex (session_meta
    /// reports it); inferred for claude (200k, upgraded to 1M once usage
    /// exceeds it — conservative, so the warning never under-fires).
    pub context_window: Option<u64>,
    /// Argv the frontend writes into a new terminal to resume this session.
    /// Filled centrally from `AgentProvider::resume_argv` so the command
    /// surface stays in one place; providers construct it empty.
    pub resume_argv: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderInfo {
    pub id: String,
    pub display_name: String,
    /// The provider's data directory exists (it has been used on this machine).
    pub available: bool,
    /// The provider's binary is resolvable in PATH (resume will work).
    pub binary_found: bool,
    /// Argv the frontend writes into a new terminal to start a fresh session.
    pub new_session_argv: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LiveAgentSession {
    pub provider: String,
    pub session_id: String,
    pub pid: u32,
    /// "busy" | "idle" | "shell" … as the registry reports it.
    pub status: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTurn {
    /// "user" | "assistant"
    pub role: String,
    pub text: String,
}

/// Failure modes of a session deletion, mapped to user-facing strings by the
/// command layer. `Active` carries a stable marker the frontend detects to
/// offer a force-confirmation.
#[derive(Debug, Clone, PartialEq)]
pub enum DeleteError {
    /// The provider's live registry reports the session as running.
    Active,
    Io(String),
    /// The provider's own CLI failed to delete (opencode).
    Subprocess(String),
}

impl DeleteError {
    pub fn to_user_string(&self) -> String {
        match self {
            DeleteError::Active => "ACTIVE".to_string(),
            DeleteError::Io(e) => format!("delete failed: {e}"),
            DeleteError::Subprocess(e) => format!("provider CLI failed: {e}"),
        }
    }
}

/// Lightweight reference to a session, used by search results.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRef {
    pub provider: String,
    pub session_id: String,
}

/// One rolling rate-limit window of a provider account.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    /// "session" (the ~5h window) or "weekly".
    pub label: String,
    pub used_percent: f64,
    /// Unix seconds when the window resets, when known.
    pub resets_at: Option<u64>,
}

/// Account-level usage availability for one provider.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuota {
    pub provider: String,
    pub windows: Vec<QuotaWindow>,
    /// Unix seconds of the snapshot these numbers come from (cache mtime /
    /// rollout mtime); None when truly live.
    pub as_of: Option<f64>,
}

/// Health of a provider's hosted service (statuspage.io shape).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub provider: String,
    /// "none" | "minor" | "major" | "critical" | "unknown".
    pub indicator: String,
    pub description: String,
}
