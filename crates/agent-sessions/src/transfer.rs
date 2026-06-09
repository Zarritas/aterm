// Export, import and move for Claude Code sessions — a port of multi-claude's
// transfer.py with a byte-compatible archive format, so .claude-session.zip
// files are interchangeable between the multi-claude TUI and Terax.
//
// Archive layout: `manifest.json` (snake_case, format/version below) plus
// `sessions/<id>.jsonl` and the optional `sessions/<id>/**` subagents subdir.
// The session-env entry is deliberately excluded (machine-local, may hold
// secrets; Claude recreates it on resume) and so is any index (it's a cache).

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::types::AgentSession;

pub const FORMAT: &str = "multi-claude/session-export";
pub const VERSION: u64 = 1;
const ARCHIVE_ROOT: &str = "sessions";
const MANIFEST_NAME: &str = "manifest.json";

/// Per-session metadata the frontend contributes to an export (local rename
/// and tags live in its store, not on disk).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExportItem {
    pub session_id: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Manifest entry surfaced to the frontend (import preview and outcome).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSessionInfo {
    pub id: String,
    /// Working directory recorded at export time, used to route imports back
    /// to the original project.
    #[serde(default)]
    pub cwd: Option<String>,
    pub display_name: Option<String>,
    pub first_prompt: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub imported: Vec<ManifestSessionInfo>,
    pub skipped_existing: Vec<String>,
    pub skipped_missing: Vec<String>,
}

/// Write `dest_zip` with the manifest plus each session's jsonl and subdir.
/// Sessions whose jsonl can't be located are silently dropped from the
/// archive (mirrors multi-claude); returns how many were written. Writes
/// nothing when none are eligible.
pub fn export_sessions(
    entries: &[(AgentSession, ExportItem)],
    locate: impl Fn(&str) -> Option<PathBuf>,
    dest_zip: &Path,
) -> Result<usize, String> {
    let eligible: Vec<(&AgentSession, &ExportItem, PathBuf)> = entries
        .iter()
        .filter_map(|(s, item)| locate(&s.id).map(|path| (s, item, path)))
        .collect();
    if eligible.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = dest_zip.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(dest_zip).map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    // Manifest keys are snake_case for byte-compat with multi-claude.
    let manifest = json!({
        "format": FORMAT,
        "version": VERSION,
        "exported_at": chrono_free_timestamp(),
        "sessions": eligible.iter().map(|(s, item, _)| json!({
            "id": s.id,
            "cwd": s.cwd,
            "branch": s.branch,
            "display_name": item.display_name,
            "tags": item.tags,
            "first_prompt": s.title,
            "message_count": s.message_count.unwrap_or(0),
            "size_bytes": s.size_bytes.unwrap_or(0),
        })).collect::<Vec<_>>(),
    });
    zip.start_file(MANIFEST_NAME, options)
        .map_err(|e| e.to_string())?;
    zip.write_all(
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;

    for (session, _, jsonl) in &eligible {
        add_file(
            &mut zip,
            jsonl,
            &format!("{ARCHIVE_ROOT}/{}.jsonl", session.id),
        )?;
        let subdir = jsonl.with_extension("");
        if subdir.is_dir() {
            add_dir_recursive(&mut zip, &subdir, &format!("{ARCHIVE_ROOT}/{}", session.id))?;
        }
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(eligible.len())
}

/// ISO-8601 UTC without pulling in chrono: seconds resolution is plenty.
fn chrono_free_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days-to-civil conversion (Howard Hinnant's algorithm), UTC.
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}+00:00")
}

fn add_file<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    name: &str,
) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    zip.start_file(name, SimpleFileOptions::default())
        .map_err(|e| e.to_string())?;
    std::io::copy(&mut file, zip).map_err(|e| e.to_string())?;
    Ok(())
}

fn add_dir_recursive<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        if path.is_dir() {
            add_dir_recursive(zip, &path, &name)?;
        } else {
            add_file(zip, &path, &name)?;
        }
    }
    Ok(())
}

/// Parse and validate the archive manifest; user-facing error strings mirror
/// multi-claude's ArchiveError cases.
pub fn read_manifest(zip_path: &Path) -> Result<Vec<ManifestSessionInfo>, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| "the file is not a valid .zip".to_string())?;
    let mut raw = String::new();
    archive
        .by_name(MANIFEST_NAME)
        .map_err(|_| "the archive has no manifest.json".to_string())?
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    let manifest: Value = serde_json::from_str(&raw)
        .map_err(|_| "manifest.json is corrupt (invalid JSON)".to_string())?;
    if manifest.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err("this does not look like a session export".to_string());
    }
    if manifest.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(format!(
            "unsupported archive version: {}",
            manifest.get("version").cloned().unwrap_or(Value::Null)
        ));
    }
    let sessions = manifest
        .get("sessions")
        .and_then(Value::as_array)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "the manifest lists no sessions".to_string())?;
    let parsed: Vec<ManifestSessionInfo> = sessions
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?.to_string();
            Some(ManifestSessionInfo {
                id,
                cwd: entry.get("cwd").and_then(Value::as_str).map(str::to_string),
                display_name: entry
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                first_prompt: entry
                    .get("first_prompt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tags: entry
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|tags| {
                        tags.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    if parsed.is_empty() {
        return Err("the manifest contains no valid sessions".to_string());
    }
    Ok(parsed)
}

/// Extract the archive's sessions into `dest_dir` (an encoded project dir).
/// Existing ids are never overwritten; extraction rejects entries whose path
/// escapes the destination.
pub fn import_archive(zip_path: &Path, dest_dir: &Path) -> Result<ImportOutcome, String> {
    let manifest = read_manifest(zip_path)?;
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| "the file is not a valid .zip".to_string())?;
    let member_names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let mut outcome = ImportOutcome::default();
    for session in manifest {
        let jsonl_member = format!("{ARCHIVE_ROOT}/{}.jsonl", session.id);
        if !member_names.iter().any(|n| n == &jsonl_member) {
            outcome.skipped_missing.push(session.id);
            continue;
        }
        if dest_dir.join(format!("{}.jsonl", session.id)).exists() {
            outcome.skipped_existing.push(session.id);
            continue;
        }
        let subdir_prefix = format!("{ARCHIVE_ROOT}/{}/", session.id);
        for name in member_names
            .iter()
            .filter(|n| *n == &jsonl_member || n.starts_with(&subdir_prefix))
        {
            extract_member(&mut archive, name, dest_dir)?;
        }
        outcome.imported.push(session);
    }
    Ok(outcome)
}

/// Like [`import_archive`], but routes each session to the Claude project dir
/// derived from its recorded `cwd` (`projects_dir/encode(cwd)`), restoring it
/// to its original project. Sessions without a cwd go to `fallback_dir`.
pub fn import_archive_routed(
    zip_path: &Path,
    projects_dir: &Path,
    fallback_dir: &Path,
    encode: impl Fn(&str) -> String,
) -> Result<ImportOutcome, String> {
    let manifest = read_manifest(zip_path)?;
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive =
        ZipArchive::new(file).map_err(|_| "the file is not a valid .zip".to_string())?;
    let member_names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let mut outcome = ImportOutcome::default();
    for session in manifest {
        let dest_dir = session
            .cwd
            .as_deref()
            .map(|c| projects_dir.join(encode(c)))
            .unwrap_or_else(|| fallback_dir.to_path_buf());
        let jsonl_member = format!("{ARCHIVE_ROOT}/{}.jsonl", session.id);
        if !member_names.iter().any(|n| n == &jsonl_member) {
            outcome.skipped_missing.push(session.id);
            continue;
        }
        if dest_dir.join(format!("{}.jsonl", session.id)).exists() {
            outcome.skipped_existing.push(session.id);
            continue;
        }
        std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
        let subdir_prefix = format!("{ARCHIVE_ROOT}/{}/", session.id);
        for name in member_names
            .iter()
            .filter(|n| *n == &jsonl_member || n.starts_with(&subdir_prefix))
        {
            extract_member(&mut archive, name, &dest_dir)?;
        }
        outcome.imported.push(session);
    }
    Ok(outcome)
}

fn extract_member(
    archive: &mut ZipArchive<std::fs::File>,
    member: &str,
    dest_dir: &Path,
) -> Result<(), String> {
    let rel = &member[ARCHIVE_ROOT.len() + 1..];
    if rel.is_empty() {
        return Ok(());
    }
    let rel_path = Path::new(rel);
    // Reject traversal before touching the filesystem: no parent/root/prefix
    // components may appear in an archive-relative path.
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("suspicious path in archive: {member}"));
    }
    let target = dest_dir.join(rel_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut entry = archive.by_name(member).map_err(|e| e.to_string())?;
    if entry.is_dir() {
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
    std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    Ok(())
}

/// Relocate a session's jsonl (plus its subagents subdir) from one encoded
/// project dir into another. The source dir is explicit (the caller knows the
/// session's current project) so a same-id file at the destination is always
/// a collision, never mistaken for the session itself. Error markers "ACTIVE"
/// and "COLLISION" are stable strings the frontend maps to messages.
pub fn move_session(
    projects_dir: &Path,
    session_id: &str,
    source_encoded_dir: &str,
    dest_encoded_dir: &str,
    is_live: impl Fn(&str) -> bool,
) -> Result<(), String> {
    if is_live(session_id) {
        return Err("ACTIVE".to_string());
    }
    if source_encoded_dir == dest_encoded_dir {
        return Ok(());
    }
    let source = projects_dir
        .join(source_encoded_dir)
        .join(format!("{session_id}.jsonl"));
    if !source.is_file() {
        return Err("session not found".to_string());
    }
    let dest_dir = projects_dir.join(dest_encoded_dir);
    let dest_jsonl = dest_dir.join(format!("{session_id}.jsonl"));
    if dest_jsonl.exists() {
        return Err("COLLISION".to_string());
    }
    std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    std::fs::rename(&source, &dest_jsonl).map_err(|e| e.to_string())?;
    let source_subdir = source.with_extension("");
    if source_subdir.is_dir() {
        let dest_subdir = dest_dir.join(session_id);
        if !dest_subdir.exists() {
            std::fs::rename(&source_subdir, &dest_subdir).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_session(id: &str) -> AgentSession {
        AgentSession {
            provider: "claude".to_string(),
            id: id.to_string(),
            title: Some("primer prompt".to_string()),
            cwd: Some("/work/p".to_string()),
            branch: Some("main".to_string()),
            message_count: Some(3),
            size_bytes: Some(42),
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

    fn item(id: &str, name: Option<&str>, tags: &[&str]) -> ExportItem {
        ExportItem {
            session_id: id.to_string(),
            display_name: name.map(str::to_string),
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn setup_source(dir: &Path, id: &str) -> PathBuf {
        let project = dir.join("projects/-work-p");
        std::fs::create_dir_all(project.join(id)).unwrap();
        let jsonl = project.join(format!("{id}.jsonl"));
        std::fs::write(&jsonl, "{\"type\":\"user\"}\n").unwrap();
        std::fs::write(project.join(id).join("agent.jsonl"), "{}\n").unwrap();
        jsonl
    }

    #[test]
    fn export_import_roundtrip_preserves_payload_and_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = setup_source(tmp.path(), "sid-1");
        let zip_path = tmp.path().join("out/export.claude-session.zip");

        let entries = vec![(
            dummy_session("sid-1"),
            item("sid-1", Some("Mi nombre"), &["bug"]),
        )];
        let written = export_sessions(&entries, |_| Some(jsonl.clone()), &zip_path).unwrap();
        assert_eq!(written, 1);

        // Manifest is multi-claude byte-compatible (snake_case, format, version).
        let preview = read_manifest(&zip_path).unwrap();
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].display_name.as_deref(), Some("Mi nombre"));
        assert_eq!(preview[0].tags, vec!["bug"]);

        let dest = tmp.path().join("dest-project");
        let outcome = import_archive(&zip_path, &dest).unwrap();
        assert_eq!(outcome.imported.len(), 1);
        assert!(dest.join("sid-1.jsonl").is_file());
        assert!(dest.join("sid-1/agent.jsonl").is_file());

        // Re-import: everything already present.
        let again = import_archive(&zip_path, &dest).unwrap();
        assert!(again.imported.is_empty());
        assert_eq!(again.skipped_existing, vec!["sid-1"]);
    }

    #[test]
    fn export_with_nothing_eligible_writes_no_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let zip_path = tmp.path().join("none.zip");
        let entries = vec![(dummy_session("ghost"), item("ghost", None, &[]))];
        let written = export_sessions(&entries, |_| None, &zip_path).unwrap();
        assert_eq!(written, 0);
        assert!(!zip_path.exists());
    }

    #[test]
    fn manifest_validation_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let not_zip = tmp.path().join("fake.zip");
        std::fs::write(&not_zip, "not a zip").unwrap();
        assert!(read_manifest(&not_zip).unwrap_err().contains("not a valid"));

        // Zip without manifest.
        let no_manifest = tmp.path().join("nm.zip");
        let mut zip = ZipWriter::new(std::fs::File::create(&no_manifest).unwrap());
        zip.start_file("other.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        assert!(read_manifest(&no_manifest)
            .unwrap_err()
            .contains("manifest"));

        // Wrong format string.
        let bad = tmp.path().join("bad.zip");
        let mut zip = ZipWriter::new(std::fs::File::create(&bad).unwrap());
        zip.start_file(MANIFEST_NAME, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"{\"format\":\"other\",\"version\":1,\"sessions\":[{\"id\":\"x\"}]}")
            .unwrap();
        zip.finish().unwrap();
        assert!(read_manifest(&bad).unwrap_err().contains("does not look"));
    }

    #[test]
    fn traversal_paths_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let evil = tmp.path().join("evil.zip");
        let mut zip = ZipWriter::new(std::fs::File::create(&evil).unwrap());
        let options = SimpleFileOptions::default();
        zip.start_file(MANIFEST_NAME, options).unwrap();
        zip.write_all(
            format!(
                "{{\"format\":\"{FORMAT}\",\"version\":1,\
                 \"sessions\":[{{\"id\":\"x\"}}]}}"
            )
            .as_bytes(),
        )
        .unwrap();
        zip.start_file("sessions/x.jsonl", options).unwrap();
        zip.write_all(b"{}\n").unwrap();
        zip.start_file("sessions/x/../../escape.txt", options)
            .unwrap();
        zip.write_all(b"pwned").unwrap();
        zip.finish().unwrap();

        let dest = tmp.path().join("dest");
        let err = import_archive(&evil, &dest).unwrap_err();
        assert!(err.contains("suspicious path"));
        assert!(!tmp.path().join("escape.txt").exists());
    }

    #[test]
    fn move_session_relocates_with_guards() {
        let tmp = tempfile::tempdir().unwrap();
        setup_source(tmp.path(), "sid-mv");
        let projects = tmp.path().join("projects");

        // Live guard.
        assert_eq!(
            move_session(&projects, "sid-mv", "-work-p", "-work-q", |_| true).unwrap_err(),
            "ACTIVE"
        );

        move_session(&projects, "sid-mv", "-work-p", "-work-q", |_| false).unwrap();
        assert!(projects.join("-work-q/sid-mv.jsonl").is_file());
        assert!(projects.join("-work-q/sid-mv/agent.jsonl").is_file());
        assert!(!projects.join("-work-p/sid-mv.jsonl").exists());

        // Collision guard: a same-id session at the destination is never overwritten.
        setup_source(tmp.path(), "sid-mv"); // recreate at -work-p
        assert_eq!(
            move_session(&projects, "sid-mv", "-work-p", "-work-q", |_| false).unwrap_err(),
            "COLLISION"
        );

        // No-op when source and destination are the same project.
        move_session(&projects, "sid-mv", "-work-p", "-work-p", |_| false).unwrap();
        assert!(projects.join("-work-p/sid-mv.jsonl").is_file());
    }

    #[test]
    fn exported_manifest_is_snake_case_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = setup_source(tmp.path(), "sid-sc");
        let zip_path = tmp.path().join("sc.zip");
        let entries = vec![(dummy_session("sid-sc"), item("sid-sc", Some("N"), &[]))];
        export_sessions(&entries, |_| Some(jsonl.clone()), &zip_path).unwrap();

        let mut archive = ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        let mut raw = String::new();
        archive
            .by_name(MANIFEST_NAME)
            .unwrap()
            .read_to_string(&mut raw)
            .unwrap();
        assert!(raw.contains("\"display_name\""));
        assert!(raw.contains("\"first_prompt\""));
        assert!(raw.contains("\"message_count\""));
        assert!(raw.contains(&format!("\"format\": \"{FORMAT}\"")));
    }
}
