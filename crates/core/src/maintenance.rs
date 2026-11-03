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

/// Stage a restore the way Python's backup.py does: the backup's
/// rss.db + config.json land in `<data_dir>/restore_pending/` and
/// are applied at the next start, before the database opens —
/// swapping the DB live under an open connection corrupts state.
pub fn stage_restore(zip_path: &std::path::Path, data_dir: &std::path::Path) -> Result<(), String> {
    let pending = data_dir.join("restore_pending");
    std::fs::create_dir_all(&pending).map_err(|e| format!("stage dir: {e}"))?;
    restore_backup(
        zip_path,
        &pending.join("rss.db"),
        &pending.join("config.json"),
    )
}

/// Apply a staged restore, if one is pending. Call before opening
/// the database. `db_path` is the database the app is actually going
/// to open (which may be a `BLINDRSS_DB` path or the app folder, not
/// `<data_dir>/rss.db`). Returns true when a restore was applied; the
/// replaced files are kept as `.pre-restore` siblings.
pub fn apply_pending_restore(data_dir: &std::path::Path, db_path: &std::path::Path) -> bool {
    let pending = data_dir.join("restore_pending");
    let staged_db = pending.join("rss.db");
    if !staged_db.exists() {
        return false;
    }
    // Windows refuses to rename onto an existing file, so a second
    // restore had to clear the previous backup first — it used to
    // leave the old .pre-restore in place, fail both renames, and
    // still report success while deleting the staged copy.
    let db_backup = db_path.with_file_name(format!(
        "{}.pre-restore",
        db_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "rss.db".to_string())
    ));
    let _ = std::fs::remove_file(&db_backup);
    if db_path.exists() && std::fs::rename(db_path, &db_backup).is_err() {
        // The live database could not be moved aside (open elsewhere,
        // permissions); keep everything staged so the next start can
        // try again instead of dropping the restore.
        return false;
    }
    if std::fs::rename(&staged_db, db_path).is_err() {
        // Put the previous database back; the staged copy stays so the
        // restore is not lost.
        let _ = std::fs::rename(&db_backup, db_path);
        return false;
    }
    let staged_cfg = pending.join("config.json");
    if staged_cfg.exists() {
        let cfg_path = data_dir.join("config.json");
        if cfg_path.exists() {
            let _ = std::fs::remove_file(data_dir.join("config.json.pre-restore"));
            let _ = std::fs::rename(&cfg_path, data_dir.join("config.json.pre-restore"));
        }
        let _ = std::fs::rename(&staged_cfg, &cfg_path);
    }
    let _ = std::fs::remove_dir_all(&pending);
    true
}

/// The platform asset name suffix for update downloads.
fn platform_asset_suffix() -> &'static str {
    if cfg!(target_os = "windows") {
        "-Windows.zip"
    } else if cfg!(target_os = "macos") {
        "-macOS.zip"
    } else {
        "-Linux.zip"
    }
}

/// The release signing public key (the file the release
/// workflow ships; the secret half lives only in the
/// MINISIGN_KEY repository secret, never in git).
const MINISIGN_PUB: &str = include_str!("../assets/minisign.pub");

/// Verify a downloaded update against its `.minisig` (the
/// minisign signature the release workflow writes beside
/// every asset). Fail closed: any parse or verify error is
/// an error, never a skip.
pub fn verify_update_signature(bytes: &[u8], sig_text: &str) -> Result<(), String> {
    let pub_b64 = MINISIGN_PUB
        .lines()
        .nth(1)
        .map(str::trim)
        .ok_or("embedded minisign.pub has no key line")?;
    let public_key = minisign_verify::PublicKey::from_base64(pub_b64)
        .map_err(|e| format!("update public key: {e}"))?;
    let signature = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| format!("update signature: {e}"))?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|_| "update download failed signature verification".to_string())
}

/// The downloadable pieces of the latest release for this
/// platform.
pub struct LatestReleaseAssets {
    pub tag: String,
    pub zip_url: Option<String>,
    pub sums_url: Option<String>,
    pub sig_url: Option<String>,
}

/// Fetch the latest release and pick this platform's zip, the
/// SHA256SUMS asset and the zip's `.minisig`.
pub fn latest_release_assets() -> Result<LatestReleaseAssets, String> {
    let mut resp = crate::http::agent()
        .get("https://api.github.com/repos/serrebidev/BlindRSS-Rust/releases/latest")
        .call()
        .map_err(|e| format!("update check: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("update check: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("update response: {e}"))?;
    let tag = value
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or("no tag in release response")?
        .to_string();
    let suffix = platform_asset_suffix();
    let sig_suffix = format!("{suffix}.minisig");
    let mut zip_url = None;
    let mut sums_url = None;
    let mut sig_url = None;
    if let Some(assets) = value.get("assets").and_then(|a| a.as_array()) {
        for a in assets {
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let url = a
                .get("browser_download_url")
                .and_then(|u| u.as_str())
                .map(|s| s.to_string());
            if name.ends_with(&sig_suffix) {
                sig_url = url.clone();
            } else if name.ends_with(suffix) {
                zip_url = url.clone();
            }
            if name == "SHA256SUMS" {
                sums_url = url;
            }
        }
    }
    Ok(LatestReleaseAssets {
        tag,
        zip_url,
        sums_url,
        sig_url,
    })
}

/// Download the latest platform zip, verify it against the
/// release's SHA256SUMS and its minisign signature, and stage it under
/// `<data_dir>/update_pending/` for the next start to apply.
/// Returns the staged tag.
pub fn download_and_stage_update(data_dir: &std::path::Path) -> Result<String, String> {
    use sha2::Digest;
    let latest = latest_release_assets()?;
    let tag = latest.tag;
    let zip_url = latest
        .zip_url
        .ok_or("no platform zip in the latest release")?;
    let sums_url = latest
        .sums_url
        .ok_or("release has no SHA256SUMS to verify against")?;
    let sig_url = latest
        .sig_url
        .ok_or("release has no signature to verify against")?;
    let mut resp = crate::http::agent()
        .get(&sums_url)
        .call()
        .map_err(|e| format!("checksums: {e}"))?;
    let sums = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("checksums: {e}"))?;
    let zip_name = zip_url.rsplit('/').next().unwrap_or("");
    let expected = sums
        .lines()
        .find_map(|line| {
            let (hash, name) = line.split_once(char::is_whitespace)?;
            if name.trim().trim_start_matches('*') == zip_name {
                Some(hash.trim().to_lowercase())
            } else {
                None
            }
        })
        .ok_or("platform zip not listed in SHA256SUMS")?;
    let mut resp = crate::http::agent()
        .get(&zip_url)
        .call()
        .map_err(|e| format!("update download: {e}"))?;
    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| format!("update download: {e}"))?;
    let digest = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if digest != expected {
        return Err("update download failed checksum verification".to_string());
    }
    let mut resp = crate::http::agent()
        .get(&sig_url)
        .call()
        .map_err(|e| format!("update signature: {e}"))?;
    let sig_text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("update signature: {e}"))?;
    verify_update_signature(&bytes, &sig_text)?;
    let pending = data_dir.join("update_pending");
    if pending.exists() {
        let _ = std::fs::remove_dir_all(&pending);
    }
    std::fs::create_dir_all(&pending).map_err(|e| format!("stage dir: {e}"))?;
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("update zip: {e}"))?;
    archive
        .extract(&pending)
        .map_err(|e| format!("update extract: {e}"))?;
    std::fs::write(pending.join("VERSION"), &tag).map_err(|e| format!("stage: {e}"))?;
    Ok(tag)
}

#[cfg(test)]
mod signature_tests {
    use super::verify_update_signature;

    /// A real signature made by the release key (BlindRadio
    /// v0.2.45's BlindRadio.tar.gz.minisig, public release
    /// data; same key ID as the embedded public key).
    const REAL_SIG: &str = concat!(
        "untrusted comment: signature from rsign secret key\n",
        "RUQ/pjmJi4QeFfDwFtbcmjzYhf6KKpipIoYxMcM1FwDONDjm+kZ8AOuBUR4GqZi+vg0tVeJMlwW7pf/A9o/7TAgr9TWhfNIOUw4=\n",
        "trusted comment: timestamp:1793660993\tfile:BlindRadio.tar.gz\thashed\n",
        "db1orUnRYjuxaADwHx2ksQ9dz7oABgGrFLxd87IyuEL1RHV8hF+N3k2SHcGjx7/PGRcmij+LMSj1E/7qtxBFCg==\n",
    );

    #[test]
    fn garbage_signature_is_rejected() {
        assert!(verify_update_signature(b"bytes", "not a signature").is_err());
        assert!(verify_update_signature(b"bytes", "").is_err());
    }

    #[test]
    fn real_signature_over_other_bytes_is_rejected() {
        // The key ID matches the embedded public key, so this
        // exercises the Ed25519 check itself, not a parse or
        // key-mismatch path: signed bytes != these bytes.
        let err = verify_update_signature(b"not the signed archive", REAL_SIG)
            .expect_err("a signature must not verify over different bytes");
        assert!(
            err.contains("failed signature verification"),
            "unexpected error: {err}"
        );
    }
}
