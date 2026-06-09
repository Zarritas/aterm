//! Provider service health from public statuspage.io pages (status.claude.com,
//! status.openai.com …).
//!
//! Deliberately shells out to `curl` instead of pulling a heavy HTTP/TLS crate
//! (see CLAUDE.md: reqwest was intentionally left out). It's best-effort: any
//! failure — no curl, no network, unexpected JSON — yields `None` and the panel
//! simply shows no badge. Always called from the background scan thread.

use agent_sessions::types::ServiceStatus;

/// The statuspage v2 endpoint for a provider, when it publishes one.
fn endpoint(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "claude" => Some("https://status.claude.com/api/v2/status.json"),
        "codex" => Some("https://status.openai.com/api/v2/status.json"),
        // opencode and gemini don't expose a statuspage v2 page.
        _ => None,
    }
}

/// Fetch and parse `{ "status": { "indicator", "description" } }`.
pub fn fetch(provider_id: &str) -> Option<ServiceStatus> {
    let url = endpoint(provider_id)?;
    let output = std::process::Command::new("curl")
        .args(["-sL", "-m", "8", url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse(provider_id, &output.stdout)
}

/// Parse a statuspage v2 `status.json` body. Pure (no I/O) so it's testable.
fn parse(provider_id: &str, body: &[u8]) -> Option<ServiceStatus> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let status = json.get("status")?;
    let indicator = status.get("indicator")?.as_str()?.to_string();
    let description = status
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    Some(ServiceStatus {
        provider: provider_id.to_string(),
        indicator,
        description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_statuspage_v2_body() {
        let body = br#"{"page":{"name":"Claude"},
            "status":{"indicator":"none","description":"All Systems Operational"}}"#;
        let s = parse("claude", body).expect("parse");
        assert_eq!(s.provider, "claude");
        assert_eq!(s.indicator, "none");
        assert_eq!(s.description, "All Systems Operational");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("claude", b"not json").is_none());
        assert!(parse("claude", b"{}").is_none());
    }

    #[test]
    fn providers_without_a_statuspage_have_no_endpoint() {
        assert!(endpoint("opencode").is_none());
        assert!(endpoint("gemini").is_none());
        assert!(endpoint("claude").is_some());
        assert!(endpoint("codex").is_some());
    }
}
