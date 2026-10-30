//! Backup / restore, retention, and update checks (slice 8).
//!
//! Backup = a ZIP holding a consistent copy of the database
//! (VACUUM INTO) plus config.json. Restore accepts only those two
//! members. Retention prunes old read articles and old tombstones
//! (favorites and unread always kept). Update check reads the
//! GitHub latest-release endpoint for the Rust repo.

use std::io::{Read, Write};

/// Create a backup ZIP at `out_path` from the live DB + config.
pub fn create_backup(
    db_path: &std::path::Path,
    config_path: &std::path::Path,
    out_path: &std::path::Path,
) -> Result<(), String> {
    let tmp_db = std::env::temp_dir().join(format!("blindrss_backup_{}.db", std::process::id()));
    {
        let conn = rusqlite::Connection::open(db_path).map_err(|e| format!("open db: {e}"))?;
        conn.execute(
            "VACUUM INTO ?1",
            rusqlite::params![tmp_db.to_string_lossy()],
        )
        .map_err(|e| format!("snapshot db: {e}"))?;
    }
    let db_bytes = std::fs::read(&tmp_db).map_err(|e| format!("read snapshot: {e}"))?;
    let _ = std::fs::remove_file(&tmp_db);
    let cfg_bytes = std::fs::read(config_path).unwrap_or_else(|_| b"{}".to_vec());
    let file = std::fs::File::create(out_path).map_err(|e| format!("create backup: {e}"))?;
    let mut zw = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("rss.db", opts)
        .map_err(|e| format!("backup write: {e}"))?;
    zw.write_all(&db_bytes)
        .map_err(|e| format!("backup write: {e}"))?;
    zw.start_file("config.json", opts)
        .map_err(|e| format!("backup write: {e}"))?;
    zw.write_all(&cfg_bytes)
        .map_err(|e| format!("backup write: {e}"))?;
    zw.finish().map_err(|e| format!("backup finish: {e}"))?;
    Ok(())
}

/// Restore a backup ZIP over the given DB + config paths. The DB
/// is replaced only when the archive holds a plausible SQLite file.
pub fn restore_backup(
    zip_path: &std::path::Path,
    db_path: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<(), String> {
    let bytes = std::fs::read(zip_path).map_err(|e| format!("read backup: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("backup zip: {e}"))?;
    let mut restored_db = false;
    for i in 0..archive.len() {
        let mut member = archive
            .by_index(i)
            .map_err(|e| format!("backup member: {e}"))?;
        let name = member.name().to_string();
        let mut data = Vec::new();
        member
            .read_to_end(&mut data)
            .map_err(|e| format!("backup member read: {e}"))?;
        match name.as_str() {
            "rss.db" => {
                if !data.starts_with(b"SQLite format 3\0") {
                    return Err("backup rss.db is not a SQLite database".to_string());
                }
                std::fs::write(db_path, &data).map_err(|e| format!("restore db: {e}"))?;
                restored_db = true;
            }
            "config.json" => {
                std::fs::write(config_path, &data).map_err(|e| format!("restore config: {e}"))?;
            }
            _ => {} // ignore anything else in the archive
        }
    }
    if !restored_db {
        return Err("backup contains no rss.db".to_string());
    }
    Ok(())
}

/// Parse the GitHub latest-release JSON into (tag, first asset URL).
pub fn parse_latest_release(json: &str) -> Option<(String, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("tag_name")?.as_str()?.to_string();
    let asset = value
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.get("browser_download_url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());
    Some((tag, asset))
}

/// Fetch the latest release of the Rust repo from GitHub.
pub fn check_update() -> Result<(String, Option<String>), String> {
    let mut resp =
        ureq::get("https://api.github.com/repos/serrebidev/BlindRSS-Rust/releases/latest")
            .header("User-Agent", "BlindRSS-Rust")
            .call()
            .map_err(|e| format!("update check: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("update check: {e}"))?;
    parse_latest_release(&text).ok_or_else(|| "unexpected release response".to_string())
}
