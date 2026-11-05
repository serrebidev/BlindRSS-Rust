//! Firefox-family cookie profiles (port of the Firefox half of
//! Python `core/site_cookies.py`, Audit 8 B03).
//!
//! Firefox and its forks (LibreWolf, Waterfox, Floorp, Zen, Pale
//! Moon) keep cookies in a plain SQLite database
//! (`cookies.sqlite`, table `moz_cookies`) — no encryption layer —
//! so they are readable directly, under exactly the same consent
//! model as the Chromium profiles: the automatic pass takes only
//! clearance-shaped cookies, and full session import happens only
//! through the explicit, consented paths.

use std::path::{Path, PathBuf};

use crate::chromium::JarRecord;

#[derive(Debug, Clone)]
pub struct FirefoxProfile {
    pub browser: String,
    pub profile: String,
    pub profile_dir: PathBuf,
    pub cookie_db: PathBuf,
    pub mtime: f64,
}

#[cfg(not(target_os = "windows"))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// (browser label, profile root) pairs for this platform
/// (Python `_firefox_roots`).
fn roots() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            for (label, rel) in [
                ("Firefox", "Mozilla/Firefox"),
                ("LibreWolf", "librewolf"),
                ("Waterfox", "Waterfox"),
                ("Floorp", "Floorp"),
                ("Zen Browser", "zen"),
                ("Pale Moon", "Moonchild Productions/Pale Moon"),
                ("Thunderbird", "Thunderbird"),
            ] {
                out.push((label.to_string(), appdata.join(rel)));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            let base = home.join("Library/Application Support");
            for (label, rel) in [
                ("Firefox", "Firefox"),
                ("LibreWolf", "librewolf"),
                ("Waterfox", "Waterfox"),
                ("Floorp", "Floorp"),
                ("Zen", "zen"),
            ] {
                out.push((label.to_string(), base.join(rel)));
            }
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(home) = home_dir() {
            for (label, rel) in [
                ("Firefox", ".mozilla/firefox"),
                (
                    "Firefox (Flatpak)",
                    ".var/app/org.mozilla.firefox/.mozilla/firefox",
                ),
                ("Firefox (Snap)", "snap/firefox/common/.mozilla/firefox"),
                ("LibreWolf", ".librewolf"),
                ("Waterfox", ".waterfox"),
                ("Floorp", ".floorp"),
            ] {
                out.push((label.to_string(), home.join(rel)));
            }
        }
    }
    out
}

/// Importable Firefox-family profiles, newest cookie DB first
/// (Python `_firefox_profiles`).
pub fn list_profiles() -> Vec<FirefoxProfile> {
    list_profiles_in(&roots())
}

/// The discovery half of `list_profiles` over explicit roots, so
/// tests can point it at fixture trees. Profiles live either one
/// level under the root or under its `Profiles/` directory; each
/// profile directory is considered once.
pub fn list_profiles_in(roots: &[(String, PathBuf)]) -> Vec<FirefoxProfile> {
    let mut found: Vec<FirefoxProfile> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (label, root) in roots {
        let mut parents = vec![root.clone(), root.join("Profiles")];
        while let Some(parent) = parents.pop() {
            let Ok(entries) = std::fs::read_dir(&parent) else {
                continue;
            };
            for entry in entries.flatten() {
                let profile_dir = entry.path();
                if !profile_dir.is_dir() {
                    continue;
                }
                let cookie_db = profile_dir.join("cookies.sqlite");
                if !cookie_db.is_file() {
                    continue;
                }
                let key = profile_dir.to_string_lossy().to_lowercase();
                if !seen.insert(key) {
                    continue;
                }
                let mtime = std::fs::metadata(&cookie_db)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                found.push(FirefoxProfile {
                    browser: label.clone(),
                    profile: profile_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    profile_dir,
                    cookie_db,
                    mtime,
                });
            }
        }
    }
    found.sort_by(|a, b| {
        b.mtime
            .partial_cmp(&a.mtime)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found
}

/// Read one profile's cookies (Python `_read_firefox_cookies`).
/// The live database is copied (with its WAL sidecars) into a
/// unique per-operation temporary directory first, so a running
/// browser's lock never blocks the read and concurrent imports
/// never share cleanup (Audit 8 B12 discipline).
pub fn read_profile_cookies(profile: &FirefoxProfile) -> Result<Vec<JarRecord>, String> {
    read_cookie_db(&profile.cookie_db)
}

/// The database half of `read_profile_cookies` over an explicit
/// path, so tests can read a fixture database directly.
pub fn read_cookie_db(db_path: &Path) -> Result<Vec<JarRecord>, String> {
    if !db_path.is_file() {
        return Err("cookies database not found".to_string());
    }
    let tmp_dir = std::env::temp_dir().join(format!(
        "blindrss-cookies-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    let result = read_cookie_db_inner(db_path, &tmp_dir);
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

fn read_cookie_db_inner(db_path: &Path, tmp_dir: &Path) -> Result<Vec<JarRecord>, String> {
    let dst = tmp_dir.join("cookies.sqlite");
    crate::chromium::copy_with_retries(db_path, &dst)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if side.is_file() {
            let _ = std::fs::copy(&side, format!("{}{suffix}", dst.display()));
        }
    }
    let conn = rusqlite::Connection::open(&dst).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT host, path, isSecure, isHttpOnly, expiry, name, value \
             FROM moz_cookies",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    for row in rows {
        let (host, path, secure, http_only, expiry, name, value) =
            row.map_err(|e| e.to_string())?;
        let host = host.trim().to_string();
        if host.is_empty() || name.is_empty() {
            continue;
        }
        records.push(JarRecord {
            include_subdomains: host.starts_with('.'),
            host,
            http_only: http_only != 0,
            path: if path.is_empty() {
                "/".to_string()
            } else {
                path
            },
            secure: secure != 0,
            // moz_cookies.expiry is already Unix seconds.
            expires: expiry,
            name,
            value,
        });
    }
    Ok(records)
}

/// The User-Agent a Firefox-family profile's cookies were issued
/// to (Python `firefox_profile_user_agent`): the major version
/// from the profile's `compatibility.ini` on this platform's
/// token. Empty when the version cannot be determined.
pub fn profile_user_agent(profile_dir: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(profile_dir.join("compatibility.ini")) else {
        return String::new();
    };
    let mut in_compatibility = false;
    let mut last_version = "";
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_compatibility = line.eq_ignore_ascii_case("[Compatibility]");
            continue;
        }
        if in_compatibility
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("LastVersion")
        {
            last_version = value.trim();
        }
    }
    let major = last_version.split('_').next().unwrap_or("");
    let major = major.split('.').next().unwrap_or("");
    if major.is_empty() || !major.bytes().all(|b| b.is_ascii_digit()) {
        return String::new();
    }
    let token = if cfg!(target_os = "windows") {
        "Windows NT 10.0; Win64; x64"
    } else if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10.15"
    } else {
        "X11; Linux x86_64"
    };
    format!("Mozilla/5.0 ({token}; rv:{major}.0) Gecko/20100101 Firefox/{major}.0")
}
