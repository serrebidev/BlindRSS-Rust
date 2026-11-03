//! Backup / restore, retention, and update checks (slice 8).
//!
//! Backup = a ZIP holding a consistent snapshot of the database
//! (VACUUM INTO), config.json and the four optional site-cookie /
//! user-agent files, following Python's `core/backup.py`. The ZIP
//! is written to a unique partial sibling and renamed over the
//! destination, so a failed backup never truncates a good one.
//! Restore validates the full staged contents (SQLite
//! `integrity_check`, config must be a JSON object) in a unique
//! staging directory and only then swaps it into
//! `restore_pending/`; the swap into the live files happens at the
//! next start, before the database opens, and removes the old
//! database's WAL/SHM sidecars first. Retention prunes old read
//! articles and old tombstones (favorites and unread always kept).
//! Update check reads the GitHub latest-release endpoint for the
//! Rust repo.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};

/// The database member name inside a backup ZIP.
pub const DB_NAME: &str = "rss.db";
/// The config member name inside a backup ZIP.
pub const CONFIG_NAME: &str = "config.json";
/// Small state files in the data directory that a backup carries
/// (Python `EXTRA_FILES`). `chromium_v20_keys.json` is deliberately
/// absent: it is DPAPI-bound to one Windows account.
pub const EXTRA_FILES: [&str; 4] = [
    "site_cookies.txt",
    "site_cookies_ua.txt",
    "site_cookies_ua_hosts.json",
    "youtube_cookies.txt",
];
/// Automatic backups use Python's timestamped names so both apps
/// rotate one shared folder correctly.
pub const AUTO_BACKUP_PREFIX: &str = "BlindRSS-auto-backup-";
/// How many automatic backups rotation keeps (Python `AUTO_KEEP`).
pub const AUTO_BACKUP_KEEP: usize = 7;

static TEMP_CTR: AtomicU64 = AtomicU64::new(0);

/// A process-unique scratch path inside `dir` (pid + nanoseconds +
/// per-process counter, the AGENTS.md temp-name rule).
fn unique_temp_path(dir: &std::path::Path, prefix: &str, ext: &str) -> std::path::PathBuf {
    let n = TEMP_CTR.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("{prefix}{}_{nanos}_{n}{ext}", std::process::id()))
}

/// Create a fresh unique temporary directory inside `base`.
fn unique_temp_dir(base: &std::path::Path, prefix: &str) -> Result<std::path::PathBuf, String> {
    for _ in 0..16 {
        let candidate = unique_temp_path(base, prefix, "");
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("temp dir: {e}")),
        }
    }
    Err("temp dir: could not allocate a unique name".to_string())
}

/// Write `data` to `target` via a unique sibling temp file and an
/// atomic rename, so readers never see a half-written file.
fn write_atomic(target: &std::path::Path, data: &[u8]) -> Result<(), String> {
    let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = unique_temp_path(parent, ".blindrss-tmp-", "");
    let result = std::fs::write(&tmp, data)
        .map_err(|e| format!("write {}: {e}", target.display()))
        .and_then(|()| {
            std::fs::rename(&tmp, target).map_err(|e| format!("replace {}: {e}", target.display()))
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Validate a staged database file the way Python does: SQLite
/// header plus a full `PRAGMA integrity_check`.
fn validate_db_file(path: &std::path::Path) -> Result<(), String> {
    let damaged = || "The database in this backup is damaged.".to_string();
    let head = std::fs::read(path).map_err(|_| damaged())?;
    if !head.starts_with(b"SQLite format 3\0") {
        return Err(damaged());
    }
    let conn = rusqlite::Connection::open(path).map_err(|_| damaged())?;
    let mut stmt = conn
        .prepare("PRAGMA integrity_check")
        .map_err(|_| damaged())?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|_| damaged())?
        .collect::<Result<_, _>>()
        .map_err(|_| damaged())?;
    if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(damaged())
    }
}

/// Validate a staged config file: it must parse as a JSON object.
fn validate_config_file(path: &std::path::Path) -> Result<(), String> {
    let damaged = || "The settings in this backup are damaged.".to_string();
    let text = std::fs::read_to_string(path).map_err(|_| damaged())?;
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.is_object() => Ok(()),
        _ => Err(damaged()),
    }
}

/// The members of a backup ZIP that BlindRSS knows about, in
/// archive order. Unknown members are ignored; only the flat known
/// names are ever read, so a hostile archive cannot traverse paths.
fn read_backup_members(zip_path: &std::path::Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bytes = std::fs::read(zip_path).map_err(|e| format!("read backup: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| "This file is not a BlindRSS backup.".to_string())?;
    let mut members = Vec::new();
    for i in 0..archive.len() {
        let mut member = archive
            .by_index(i)
            .map_err(|_| "This file is not a BlindRSS backup.".to_string())?;
        let name = member.name().to_string();
        if name != DB_NAME && name != CONFIG_NAME && !EXTRA_FILES.contains(&name.as_str()) {
            continue;
        }
        let mut data = Vec::new();
        member
            .read_to_end(&mut data)
            .map_err(|_| "This file is not a BlindRSS backup.".to_string())?;
        members.push((name, data));
    }
    if !members
        .iter()
        .any(|(n, _)| n == DB_NAME || n == CONFIG_NAME)
    {
        return Err("This file is not a BlindRSS backup.".to_string());
    }
    Ok(members)
}

/// Create a backup ZIP at `out_path` from the live DB + config +
/// whichever of the optional cookie/UA files exist in `data_dir`.
/// Atomic: the ZIP is built in a unique temporary snapshot and a
/// unique `.partial` sibling, then renamed over `out_path`; on any
/// failure the previous archive at `out_path` is untouched and no
/// partial is left behind.
pub fn create_backup(
    db_path: &std::path::Path,
    config_path: &std::path::Path,
    data_dir: &std::path::Path,
    out_path: &std::path::Path,
) -> Result<(), String> {
    let tmp_root = unique_temp_dir(&std::env::temp_dir(), "blindrss-backup-")?;
    let result = create_backup_inner(db_path, config_path, data_dir, out_path, &tmp_root);
    let _ = std::fs::remove_dir_all(&tmp_root);
    result
}

fn create_backup_inner(
    db_path: &std::path::Path,
    config_path: &std::path::Path,
    data_dir: &std::path::Path,
    out_path: &std::path::Path,
    tmp_root: &std::path::Path,
) -> Result<(), String> {
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    if db_path.exists() {
        // VACUUM INTO writes a consistent snapshot even while the
        // app holds the database open (Python uses the SQLite
        // backup API for the same guarantee).
        let snapshot = tmp_root.join(DB_NAME);
        {
            let conn = rusqlite::Connection::open(db_path).map_err(|e| format!("open db: {e}"))?;
            conn.execute(
                "VACUUM INTO ?1",
                rusqlite::params![snapshot.to_string_lossy()],
            )
            .map_err(|e| format!("snapshot db: {e}"))?;
        }
        let db_bytes = std::fs::read(&snapshot).map_err(|e| format!("read snapshot: {e}"))?;
        members.push((DB_NAME.to_string(), db_bytes));
    }
    if config_path.exists() {
        let cfg_bytes = std::fs::read(config_path).map_err(|e| format!("read config: {e}"))?;
        members.push((CONFIG_NAME.to_string(), cfg_bytes));
    }
    for name in EXTRA_FILES {
        let path = data_dir.join(name);
        if path.is_file() {
            let data = std::fs::read(&path).map_err(|e| format!("read {name}: {e}"))?;
            members.push((name.to_string(), data));
        }
    }
    let out_dir = out_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let out_name = out_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup.zip".to_string());
    let partial = unique_temp_path(out_dir, &format!("{out_name}.partial-"), "");
    let write_result = (|| -> Result<(), String> {
        let file = std::fs::File::create(&partial).map_err(|e| format!("create backup: {e}"))?;
        let mut zw = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in &members {
            zw.start_file(name, opts)
                .map_err(|e| format!("backup write: {e}"))?;
            zw.write_all(data)
                .map_err(|e| format!("backup write: {e}"))?;
        }
        zw.finish().map_err(|e| format!("backup finish: {e}"))?;
        std::fs::rename(&partial, out_path).map_err(|e| format!("replace backup: {e}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    write_result
}

/// Restore a backup ZIP directly over the given DB + config paths
/// (the optional cookie/UA files land beside the config). Every
/// member is validated before anything is written, and each file
/// is replaced atomically, so a bad archive changes nothing.
/// Archives without a database (config-only) restore fine.
pub fn restore_backup(
    zip_path: &std::path::Path,
    db_path: &std::path::Path,
    config_path: &std::path::Path,
) -> Result<(), String> {
    let dir = unique_temp_dir(&std::env::temp_dir(), "blindrss-restore-")?;
    let result = restore_backup_inner(zip_path, db_path, config_path, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn restore_backup_inner(
    zip_path: &std::path::Path,
    db_path: &std::path::Path,
    config_path: &std::path::Path,
    scratch: &std::path::Path,
) -> Result<(), String> {
    let members = read_backup_members(zip_path)?;
    // Validate everything before writing anything.
    for (name, data) in &members {
        let staged = scratch.join(name);
        std::fs::write(&staged, data).map_err(|e| format!("stage member: {e}"))?;
        match name.as_str() {
            DB_NAME => validate_db_file(&staged)?,
            CONFIG_NAME => validate_config_file(&staged)?,
            _ => {}
        }
    }
    let extra_dir = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for (name, data) in &members {
        let target = match name.as_str() {
            DB_NAME => db_path.to_path_buf(),
            CONFIG_NAME => config_path.to_path_buf(),
            other => extra_dir.join(other),
        };
        write_atomic(&target, data)?;
    }
    Ok(())
}

/// Whether an automatic backup is due (Python `auto_backup_due`:
/// at least 24 hours since the last one).
pub fn auto_backup_due(last_ts: f64, now: f64) -> bool {
    now - last_ts >= 86_400.0
}

/// Format a Unix timestamp the way Python's
/// `strftime("%Y-%m-%d-%H%M%S")` does, in UTC (the app has no
/// timezone dependency; the fixed-width format is what rotation
/// sorts on, and it sorts correctly mixed with Python's local-time
/// names from the same folder).
fn auto_backup_stamp(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days conversion (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{year:04}-{m:02}-{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Write a timestamped automatic backup into `folder` and delete
/// all but the newest `keep` automatic backups (Python
/// `run_auto_backup`). Rotation only ever touches files matching
/// the auto-backup prefix AND the `.zip` suffix; manual backups
/// and unrelated files are left alone.
pub fn run_auto_backup(
    folder: &std::path::Path,
    data_dir: &std::path::Path,
    config_path: &std::path::Path,
    db_path: &std::path::Path,
    keep: usize,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("backup folder: {e}"))?;
    let dest = folder.join(format!(
        "{AUTO_BACKUP_PREFIX}{}.zip",
        auto_backup_stamp(std::time::SystemTime::now())
    ));
    create_backup(db_path, config_path, data_dir, &dest)?;
    if keep > 0 {
        let mut autos: Vec<std::path::PathBuf> = std::fs::read_dir(folder)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .map(|n| {
                                let n = n.to_string_lossy();
                                n.starts_with(AUTO_BACKUP_PREFIX) && n.ends_with(".zip")
                            })
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        autos.sort();
        let excess = autos.len().saturating_sub(keep);
        for old in autos.iter().take(excess) {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(dest)
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
/// members are unpacked into a unique staging directory inside
/// `data_dir`, the FULL staged contents are validated (database
/// integrity, config is a JSON object), and only then is the
/// staging directory swapped into `<data_dir>/restore_pending/`
/// for the next start to apply — swapping the DB live under an
/// open connection corrupts state. A failed stage leaves any
/// earlier staged restore exactly as it was.
pub fn stage_restore(zip_path: &std::path::Path, data_dir: &std::path::Path) -> Result<(), String> {
    let members = read_backup_members(zip_path)?;
    std::fs::create_dir_all(data_dir).map_err(|e| format!("stage dir: {e}"))?;
    let staging = unique_temp_dir(data_dir, "restore-")?;
    let result = stage_restore_inner(&members, &staging, data_dir);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn stage_restore_inner(
    members: &[(String, Vec<u8>)],
    staging: &std::path::Path,
    data_dir: &std::path::Path,
) -> Result<(), String> {
    for (name, data) in members {
        std::fs::write(staging.join(name), data).map_err(|e| format!("stage member: {e}"))?;
    }
    // Validate the staged files themselves, as Python does, before
    // the pending directory is touched at all.
    let staged_db = staging.join(DB_NAME);
    if staged_db.exists() {
        validate_db_file(&staged_db)?;
    }
    let staged_cfg = staging.join(CONFIG_NAME);
    if staged_cfg.exists() {
        validate_config_file(&staged_cfg)?;
    }
    // Swap the validated staging directory into place. The old
    // pending directory is first renamed aside so a failed swap
    // can put it back.
    let pending = data_dir.join("restore_pending");
    let trash = unique_temp_path(data_dir, "restore-old-", "");
    let had_pending = pending.exists();
    if had_pending && std::fs::rename(&pending, &trash).is_err() {
        return Err("stage restore: could not move the previous staged restore".to_string());
    }
    if std::fs::rename(staging, &pending).is_err() {
        if had_pending {
            let _ = std::fs::rename(&trash, &pending);
        }
        return Err("stage restore: could not install the staged restore".to_string());
    }
    if had_pending {
        let _ = std::fs::remove_dir_all(&trash);
    }
    Ok(())
}

/// Apply a staged restore, if one is pending. Call before opening
/// the database — this is the only place WAL/SHM sidecars are
/// handled, because at this boundary the database is stopped.
/// `db_path` is the database the app is actually going to open
/// (which may be a `BLINDRSS_DB` path or the app folder, not
/// `<data_dir>/rss.db`); config and the optional cookie/UA files
/// land in `data_dir`. A config-only stage applies without
/// touching the database, and files the stage omits are preserved.
/// Returns true when a restore was applied; the replaced database
/// and config are kept as `.pre-restore` siblings.
pub fn apply_pending_restore(data_dir: &std::path::Path, db_path: &std::path::Path) -> bool {
    let pending = data_dir.join("restore_pending");
    if !pending.is_dir() {
        return false;
    }
    let staged_db = pending.join(DB_NAME);
    let staged_cfg = pending.join(CONFIG_NAME);
    let staged_extras: Vec<std::path::PathBuf> = EXTRA_FILES
        .iter()
        .map(|name| pending.join(name))
        .filter(|p| p.exists())
        .collect();
    if !staged_db.exists() && !staged_cfg.exists() && staged_extras.is_empty() {
        return false;
    }
    if staged_db.exists() {
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
        // A WAL left from the old database would be replayed onto
        // the restored one and corrupt it (Python backup.py O01
        // fix): remove the old database's sidecars before the
        // restored file lands. Safe only here, before the database
        // is opened.
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = db_path.as_os_str().to_owned();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
        }
        if std::fs::rename(&staged_db, db_path).is_err() {
            // Put the previous database back; the staged copy stays so the
            // restore is not lost.
            let _ = std::fs::rename(&db_backup, db_path);
            return false;
        }
    }
    if staged_cfg.exists() {
        let cfg_path = data_dir.join("config.json");
        if cfg_path.exists() {
            let _ = std::fs::remove_file(data_dir.join("config.json.pre-restore"));
            let _ = std::fs::rename(&cfg_path, data_dir.join("config.json.pre-restore"));
        }
        let _ = std::fs::rename(&staged_cfg, &cfg_path);
    }
    for staged in &staged_extras {
        if let Some(name) = staged.file_name() {
            let _ = std::fs::rename(staged, data_dir.join(name));
        }
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
