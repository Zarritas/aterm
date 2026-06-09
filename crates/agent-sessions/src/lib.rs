//! Discover and describe coding-agent CLI sessions persisted on disk (Claude
//! Code, Codex, OpenCode, Gemini), so a Warp panel can list, preview and
//! resume them.
//!
//! Read-only by design: providers derive every path from the home directory
//! and never accept paths from the caller. Ported from the standalone
//! multi-claude / Terax agent-sessions work; the on-disk formats and parsing
//! are provider-stable.

pub mod extract;
pub mod live;
pub mod metadata;
pub mod proc;
pub mod provider;
pub mod providers;
pub mod transfer;
pub mod types;
// `service_status` (HTTP via reqwest) intentionally omitted from the aterm vendor.
// Re-add `pub mod service_status;` + the reqwest dep behind a feature when wiring service health.

pub use metadata::{parse_tags, MetadataStore, SessionMetadata};
pub use provider::{binary_in_path, resolve_binary, AgentProvider};
pub use providers::all_providers;
pub use providers::claude::encode_cwd;
pub use transfer::{
    export_sessions, import_archive, import_archive_routed, read_manifest, ExportItem,
    ImportOutcome, ManifestSessionInfo,
};
pub use types::{
    AgentProviderInfo, AgentSession, DeleteError, LiveAgentSession, PreviewTurn, ProviderQuota,
    QuotaWindow, ServiceStatus, SessionRef,
};
