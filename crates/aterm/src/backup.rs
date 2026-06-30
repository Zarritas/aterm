//! Catalog backup/restore: a single zip snapshot of the local catalog —
//! session metadata, project names and launch templates.
//!
//! Byte-compatible with the sidecar's `backup`/`restore` (same manifest and
//! `config/<file>` layout, format `aterm/catalog-backup` v1) so a snapshot
//! taken by the native app restores in the extension and vice versa.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

const BACKUP_FORMAT: &str = "aterm/catalog-backup";
const BACKUP_VERSION: u64 = 1;

fn config_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm")
}

/// The catalog files, as `(zip entry name, path on disk)`.
fn catalog_files() -> [(&'static str, PathBuf); 3] {
    let d = config_dir();
    [
        ("session-metadata.json", d.join("session-metadata.json")),
        ("project-names.json", d.join("project-names.json")),
        ("templates.json", d.join("templates.json")),
    ]
}

/// Write a catalog snapshot to `dest`. Returns how many catalog files were
/// included (those that exist on disk).
pub fn backup(dest: &Path) -> Result<usize, String> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default();

    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let manifest = serde_json::json!({
        "format": BACKUP_FORMAT,
        "version": BACKUP_VERSION,
        "created_at_unix": created_at,
    });
    zip.start_file("manifest.json", opts)
        .map_err(|e| e.to_string())?;
    zip.write_all(
        serde_json::to_string_pretty(&manifest)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;

    let mut written = 0usize;
    for (name, path) in catalog_files() {
        if let Ok(bytes) = std::fs::read(&path) {
            zip.start_file(format!("config/{name}"), opts)
                .map_err(|e| e.to_string())?;
            zip.write_all(&bytes).map_err(|e| e.to_string())?;
            written += 1;
        }
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(written)
}

/// Restore a catalog snapshot from `source`, overwriting the local catalog
/// files present in the zip. Returns the names of the files restored.
pub fn restore(source: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(source).map_err(|e| e.to_string())?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("zip inválido: {e}"))?;

    // Validate the manifest before touching anything on disk.
    let mut raw = String::new();
    {
        let mut entry = zip
            .by_name("manifest.json")
            .map_err(|_| "backup sin manifest.json".to_string())?;
        entry
            .read_to_string(&mut raw)
            .map_err(|e| format!("manifest ilegible: {e}"))?;
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("manifest corrupto: {e}"))?;
    if manifest.get("format").and_then(|v| v.as_str()) != Some(BACKUP_FORMAT) {
        return Err("este zip no parece un backup de aterm".to_string());
    }
    if manifest.get("version").and_then(|v| v.as_u64()) != Some(BACKUP_VERSION) {
        return Err("versión de backup no soportada".to_string());
    }

    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let mut restored = Vec::new();
    for (name, dest) in catalog_files() {
        let member = format!("config/{name}");
        let Ok(mut entry) = zip.by_name(&member) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
            restored.push(name.to_string());
        }
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_then_restore_roundtrips_a_file() {
        // Point HOME at a temp dir so we touch a sandbox catalog.
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let cfg = config_dir();
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("templates.json"), b"{\"templates\":[]}").unwrap();

        let zip_path = tmp.path().join("snap.zip");
        let n = backup(&zip_path).unwrap();
        assert_eq!(n, 1);

        std::fs::remove_file(cfg.join("templates.json")).unwrap();
        let restored = restore(&zip_path).unwrap();
        assert_eq!(restored, vec!["templates.json"]);
        assert!(cfg.join("templates.json").exists());
    }

    #[test]
    fn restore_rejects_a_non_backup_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("x.zip");
        let f = std::fs::File::create(&bogus).unwrap();
        let mut z = ZipWriter::new(f);
        z.start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"{\"format\":\"other\"}").unwrap();
        z.finish().unwrap();
        assert!(restore(&bogus).is_err());
    }
}
