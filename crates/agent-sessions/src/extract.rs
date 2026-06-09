// Shared machinery for reading conversation content out of session logs:
// preview turns (last N user/assistant messages) and FTS text extraction.
// Each provider supplies a per-event extractor; caps and shaping live here.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

use crate::types::PreviewTurn;

/// Tail window read for previews. Much deeper than multi-claude's 60: agentic
/// sessions emit dozens of assistant/tool events per user message, so a short
/// tail shows only ASSISTANT turns (observed in real use). The turn limit
/// below still caps what renders.
pub const PREVIEW_TAIL_LINES: usize = 400;
/// Most recent turns shown.
pub const PREVIEW_TURN_LIMIT: usize = 12;
/// Per-turn character cap.
pub const PREVIEW_TEXT_LIMIT: usize = 800;
/// FTS extraction caps: lines scanned and total characters kept.
pub const FTS_MAX_LINES: usize = 2_000;
pub const FTS_MAX_CHARS: usize = 64_000;

/// A provider-specific extractor: given one parsed jsonl event, return the
/// (role, text) it contributes to the conversation, if any. Role is
/// "user" | "assistant".
pub type TurnExtractor = fn(&Value) -> Option<(&'static str, String)>;

/// Last `PREVIEW_TURN_LIMIT` turns from the tail of `path`.
pub fn preview_turns(path: &Path, extract: TurnExtractor) -> Vec<PreviewTurn> {
    let lines = tail_lines(path, PREVIEW_TAIL_LINES);
    let mut turns: Vec<PreviewTurn> = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| extract(&event))
        .map(|(role, text)| PreviewTurn {
            role: role.to_string(),
            text: cap_text(&strip_command_wrappers(&text), PREVIEW_TEXT_LIMIT),
        })
        .filter(|t| !t.text.is_empty())
        .collect();
    if turns.len() > PREVIEW_TURN_LIMIT {
        turns.drain(..turns.len() - PREVIEW_TURN_LIMIT);
    }
    turns
}

/// Concatenated conversation text for the FTS index, capped by lines/chars.
pub fn fts_text(path: &Path, extract: TurnExtractor) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut out = String::new();
    for line in reader.lines().take(FTS_MAX_LINES) {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some((_, text)) = extract(&event) else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&text);
        if out.len() >= FTS_MAX_CHARS {
            out.truncate(floor_char_boundary(&out, FTS_MAX_CHARS));
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Last `n` lines of `path`. Reads the file once; previews are on-demand
/// (a click), so a single sequential read of even a large log is acceptable.
pub fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = content
        .rsplit('\n')
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .map(str::to_string)
        .collect();
    lines.reverse();
    lines
}

/// Char-safe truncation with ellipsis (keeps markdown mostly intact).
pub fn cap_text(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(limit - 1).collect();
    format!("{}…", cut.trim_end())
}

fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Convert slash-command wrappers into a human-friendly summary:
/// `<command-name>/x</command-name><command-args>y</command-args>` → `/x y`.
/// Plain prompts pass through with inline `<tag>...</tag>` blocks stripped.
pub fn strip_command_wrappers(text: &str) -> String {
    if let Some(name) = extract_tag(text, "command-name") {
        let args = extract_tag(text, "command-args").unwrap_or_default();
        return format!("{} {}", name.trim(), args.trim())
            .trim()
            .to_string();
    }
    strip_inline_tags(text).trim().to_string()
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].to_string())
}

/// Remove every `<tag>...</tag>` block (non-nested).
fn strip_inline_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open_at) = rest.find('<') {
        let Some(name_end) = rest[open_at..].find('>') else {
            break;
        };
        let tag_name = &rest[open_at + 1..open_at + name_end];
        if tag_name.is_empty() || tag_name.starts_with('/') || tag_name.contains('<') {
            out.push_str(&rest[..open_at + 1]);
            rest = &rest[open_at + 1..];
            continue;
        }
        let close = format!("</{tag_name}>");
        let Some(close_at) = rest[open_at..].find(&close) else {
            out.push_str(&rest[..open_at + 1]);
            rest = &rest[open_at + 1..];
            continue;
        };
        out.push_str(&rest[..open_at]);
        rest = &rest[open_at + close_at + close.len()..];
    }
    out.push_str(rest);
    out
}

/// Collapse whitespace and cap at `limit` (char-safe, with ellipsis). Used by
/// providers for one-line titles.
pub fn truncate_title(text: &str, limit: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(limit - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_like_extractor(event: &Value) -> Option<(&'static str, String)> {
        let role = match event.get("type").and_then(Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => return None,
        };
        let text = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)?;
        Some((role, text.to_string()))
    }

    fn write_log(dir: &Path, lines: &[String]) -> std::path::PathBuf {
        let path = dir.join("log.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn event(role: &str, text: &str) -> String {
        format!("{{\"type\":\"{role}\",\"message\":{{\"content\":\"{text}\"}}}}")
    }

    #[test]
    fn preview_keeps_last_turns_only() {
        let tmp = tempfile::tempdir().unwrap();
        let lines: Vec<String> = (0..20)
            .map(|i| {
                event(
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("m{i}"),
                )
            })
            .collect();
        let path = write_log(tmp.path(), &lines);
        let turns = preview_turns(&path, claude_like_extractor);
        assert_eq!(turns.len(), PREVIEW_TURN_LIMIT);
        assert_eq!(turns.last().unwrap().text, "m19");
        assert_eq!(turns.first().unwrap().text, "m8");
    }

    #[test]
    fn preview_caps_long_texts_and_skips_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let long = "x".repeat(2000);
        let lines = vec![
            "{\"type\":\"summary\"}".to_string(),
            "not json at all".to_string(),
            event("user", &long),
        ];
        let path = write_log(tmp.path(), &lines);
        let turns = preview_turns(&path, claude_like_extractor);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text.chars().count(), PREVIEW_TEXT_LIMIT);
        assert!(turns[0].text.ends_with('…'));
    }

    #[test]
    fn fts_text_concatenates_and_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let lines = vec![
            event("user", "primera pregunta"),
            "{\"type\":\"tool_use\"}".to_string(),
            event("assistant", "una respuesta"),
        ];
        let path = write_log(tmp.path(), &lines);
        let text = fts_text(&path, claude_like_extractor).unwrap();
        assert_eq!(text, "primera pregunta\nuna respuesta");
    }

    #[test]
    fn fts_text_none_when_nothing_extractable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_log(tmp.path(), &["{\"type\":\"meta\"}".to_string()]);
        assert!(fts_text(&path, claude_like_extractor).is_none());
    }

    #[test]
    fn tail_lines_returns_last_n_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        let path = write_log(tmp.path(), &lines);
        let tail = tail_lines(&path, 3);
        assert_eq!(tail, vec!["line7", "line8", "line9"]);
    }

    #[test]
    fn strip_command_wrappers_summarizes_and_strips() {
        let cmd = "<command-message>r</command-message>\
                   <command-name>/refine</command-name><command-args>x</command-args>";
        assert_eq!(strip_command_wrappers(cmd), "/refine x");
        assert_eq!(strip_command_wrappers("hola"), "hola");
        assert_eq!(strip_command_wrappers("a <hint>b</hint> c"), "a  c");
    }
}
