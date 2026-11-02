//! Cookie export auto-import — port of Python core/cookies_import.py
//! plus the Downloads/browser-profile halves of core/site_cookies.py's
//! auto-import. A CookieImportWatcher equivalent runs these on a
//! slow tick in the app: a fresh YouTube cookies.txt export becomes
//! the yt-dlp cookie file, any fresh export merges into the site
//! jar, and changed Chromium profiles are re-read into the site jar.

use std::path::{Path, PathBuf};

use crate::chromium::{JarRecord, merge_into_jar};
use crate::config::Config;

pub const MAX_JAR_BYTES: u64 = 4 * 1024 * 1024;
pub const DEFAULT_MAX_AGE_S: f64 = 15.0 * 60.0;
pub const IMPORTED_COOKIE_FILENAME: &str = "youtube_cookies.txt";
pub const SITE_JAR_FILENAME: &str = "site_cookies.txt";

const YOUTUBE_DOMAINS: &[&str] = &["youtube.com", "google.com"];
const MANAGED_JAR_MARKERS: &[&str] = &[
    "# Imported by BlindRSS",
    "# Harvested from installed browsers by BlindRSS",
];

/// True when this cookies.txt is one BlindRSS wrote itself.
pub fn is_blindrss_managed_jar(text: &str) -> bool {
    text.lines()
        .take(20)
        .any(|line| MANAGED_JAR_MARKERS.iter().any(|m| line.contains(m)))
}

fn read_jar_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_JAR_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Parse Netscape cookie records from jar text.
pub fn parse_netscape_records(text: &str) -> Vec<JarRecord> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let (http_only, line) = match raw.strip_prefix("#HttpOnly_") {
            Some(rest) => (true, rest),
            None => {
                if raw.starts_with('#') || raw.trim().is_empty() {
                    continue;
                }
                (false, raw)
            }
        };
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        out.push(JarRecord {
            host: fields[0].to_string(),
            include_subdomains: fields[1].eq_ignore_ascii_case("TRUE"),
            path: fields[2].to_string(),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires: fields[4].parse().unwrap_or(0),
            name: fields[5].to_string(),
            value: fields[6].to_string(),
            http_only,
        });
    }
    out
}

pub fn is_netscape_cookie_jar(text: &str) -> bool {
    !parse_netscape_records(text).is_empty()
}

pub fn cookie_jar_has_youtube(text: &str) -> bool {
    parse_netscape_records(text).iter().any(|r| {
        let host = r.host.trim_start_matches('.').to_lowercase();
        YOUTUBE_DOMAINS
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
    })
}

/// (ok, message): ok means a usable YouTube cookie jar.
pub fn validate_cookie_file(path: &Path) -> (bool, String) {
    if !path.is_file() {
        return (false, "File not found.".to_string());
    }
    let Some(text) = read_jar_text(path) else {
        return (
            false,
            "Could not read the file (too large or unreadable).".to_string(),
        );
    };
    if !is_netscape_cookie_jar(&text) {
        return (
            false,
            "This does not look like a Netscape cookies.txt export.".to_string(),
        );
    }
    if !cookie_jar_has_youtube(&text) {
        return (
            false,
            "No YouTube/Google login cookies found in this file.".to_string(),
        );
    }
    (
        true,
        "Looks like a valid YouTube cookie export.".to_string(),
    )
}

fn is_importable_export(path: &Path) -> bool {
    let Some(text) = read_jar_text(path) else {
        return false;
    };
    !is_blindrss_managed_jar(&text)
        && is_netscape_cookie_jar(&text)
        && cookie_jar_has_youtube(&text)
}

/// Likely locations of a freshly exported cookies.txt.
pub fn default_download_dirs(extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |d: PathBuf| {
        if d.is_dir() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    for d in extra_dirs {
        add(d.clone());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        add(home.join("Downloads"));
        add(home);
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        add(PathBuf::from(profile).join("Downloads"));
    }
    dirs
}

/// Newest .txt across search_dirs that validates as a YouTube
/// cookie jar, gated by `since_ts` and the recency window
/// (Python's find_latest_youtube_cookie_export).
pub fn find_latest_youtube_cookie_export(
    search_dirs: &[PathBuf],
    since_ts: Option<f64>,
    max_age_s: f64,
    now: f64,
) -> Option<PathBuf> {
    let mut cutoff = now - max_age_s;
    if let Some(since) = since_ts {
        cutoff = cutoff.max(since);
    }
    let mut best: Option<(PathBuf, f64)> = None;
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in search_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_txt = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
            if !is_txt || seen.contains(&path) {
                continue;
            }
            seen.push(path.clone());
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            if mtime < cutoff || best.as_ref().is_some_and(|(_, b)| mtime <= *b) {
                continue;
            }
            if is_importable_export(&path) {
                best = Some((path, mtime));
            }
        }
    }
    best.map(|(p, _)| p)
}

/// Copy a validated jar into dest_dir as the managed cookie file.
pub fn import_cookie_file(src_path: &Path, dest_dir: &Path) -> Result<PathBuf, String> {
    let (ok, message) = validate_cookie_file(src_path);
    if !ok {
        return Err(message);
    }
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(IMPORTED_COOKIE_FILENAME);
    let same =
        std::fs::canonicalize(src_path).ok() == std::fs::canonicalize(&dest).ok() && dest.exists();
    if !same {
        std::fs::copy(src_path, &dest).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

fn file_mtime(path: &Path) -> f64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Detect a freshly exported YouTube cookies.txt and import it.
/// Config is updated in place (caller saves). Returns the imported
/// destination when a new export was imported.
pub fn auto_import_youtube_cookies(
    config: &mut Config,
    data_dir: &Path,
    search_dirs: Option<Vec<PathBuf>>,
    now: f64,
) -> Option<PathBuf> {
    if !config.get_bool("auto_import_browser_cookies", true) {
        return None;
    }
    let last_mtime = config
        .raw()
        .get("ytdlp_cookies_last_import_mtime")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let managed = data_dir.join(IMPORTED_COOKIE_FILENAME);
    let mut extra: Vec<PathBuf> = Vec::new();
    let current = config.get_str("ytdlp_cookies_file", "").to_string();
    if !current.is_empty()
        && let Some(parent) = Path::new(&current).parent()
    {
        extra.push(parent.to_path_buf());
    }
    let dirs = search_dirs.unwrap_or_else(|| default_download_dirs(&extra));
    let found = find_latest_youtube_cookie_export(
        &dirs,
        if last_mtime > 0.0 {
            Some(last_mtime)
        } else {
            None
        },
        DEFAULT_MAX_AGE_S,
        now,
    )?;
    if found == managed {
        return None;
    }
    let src_mtime = file_mtime(&found);
    if src_mtime <= last_mtime {
        return None;
    }
    let dest = import_cookie_file(&found, data_dir).ok()?;
    config.set_path(
        "ytdlp_cookies_file",
        serde_json::json!(dest.to_string_lossy()),
    );
    config.set_path(
        "ytdlp_cookies_last_import_mtime",
        serde_json::json!(src_mtime),
    );
    Some(dest)
}

/// Merge every fresh (non-managed) Netscape export in the search
/// dirs into the site jar, oldest first; returns the newest
/// imported source (Python's site_cookies.auto_import_downloads).
pub fn auto_import_downloads(
    config: &mut Config,
    jar_path: &Path,
    search_dirs: Option<Vec<PathBuf>>,
    _now: f64,
) -> Option<PathBuf> {
    if !config.get_bool("auto_import_browser_cookies", true) {
        return None;
    }
    let last_mtime = config
        .raw()
        .get("site_cookies_last_import_mtime")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let dirs = search_dirs.unwrap_or_else(|| default_download_dirs(&[]));
    let mut candidates: Vec<(PathBuf, f64)> = Vec::new();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
            {
                continue;
            }
            if path == jar_path {
                continue;
            }
            let mtime = file_mtime(&path);
            if mtime <= last_mtime {
                continue;
            }
            let Some(text) = read_jar_text(&path) else {
                continue;
            };
            if is_blindrss_managed_jar(&text) || !is_netscape_cookie_jar(&text) {
                continue;
            }
            candidates.push((path, mtime));
        }
    }
    candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut newest: Option<PathBuf> = None;
    let mut newest_mtime = last_mtime;
    for (path, mtime) in &candidates {
        let Some(text) = read_jar_text(path) else {
            continue;
        };
        let records = parse_netscape_records(&text);
        if records.is_empty() {
            continue;
        }
        let existing = std::fs::read_to_string(jar_path).unwrap_or_default();
        let merged = merge_into_jar(&existing, &records, _now as i64);
        if std::fs::write(jar_path, merged).is_err() {
            continue;
        }
        newest = Some(path.clone());
        newest_mtime = newest_mtime.max(*mtime);
    }
    if newest.is_some() {
        config.set_path(
            "site_cookies_last_import_mtime",
            serde_json::json!(newest_mtime),
        );
    }
    newest
}

fn profile_import_mode(config: &Config, now: i64) -> Option<bool> {
    if config.get_bool("installed_browser_cookie_import_enabled", false) {
        let last = config
            .raw()
            .get("site_cookies_full_import_last_run")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if last <= 0.0 || !(0.0..21600.0).contains(&(now as f64 - last)) {
            return Some(true);
        }
    }
    config
        .get_bool("auto_import_browser_cookies", true)
        .then_some(false)
}

fn permitted_profile_records(config: &Config, records: Vec<JarRecord>, now: i64) -> Vec<JarRecord> {
    let full = profile_import_mode(config, now) == Some(true);
    let clearance = config.get_bool("auto_import_browser_cookies", true);
    records
        .into_iter()
        .filter(|record| {
            let name = record.name.trim().to_ascii_lowercase();
            let harvestable = [
                "cf_clearance",
                "__cf_bm",
                "datadome",
                "reese84",
                "incap_ses",
                "visid_incap",
                "_px3",
                "_pxvid",
                "bm_sz",
                "ak_bmsc",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix));
            (full || (clearance && harvestable))
                && !record.value.is_empty()
                && (record.expires == 0 || record.expires >= now)
        })
        .collect()
}

/// Re-read Chromium profiles whose cookie DB changed since the
/// last pass and merge their cookies into the site jar. Returns
/// the number of cookies stored (Python's
/// site_cookies.auto_import_browser_profiles).
pub fn auto_import_browser_profiles(config: &mut Config, jar_path: &Path, now: i64) -> usize {
    let Some(full) = profile_import_mode(config, now) else {
        return 0;
    };
    let marker = if full {
        "site_cookies_full_profile_mtimes"
    } else {
        "site_cookies_profile_mtimes"
    };
    let mut seen: serde_json::Map<String, serde_json::Value> = config
        .raw()
        .get(marker)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let profiles = crate::chromium::list_profiles();
    let mut stored = 0usize;
    let mut changed = false;
    for profile in &profiles {
        let db_mtime = profile.mtime;
        let key = profile.cookie_db.to_string_lossy().to_string();
        let previous = seen.get(&key).and_then(|v| v.as_f64()).unwrap_or(0.0);
        if db_mtime <= previous {
            continue;
        }
        let outcome = crate::chromium::read_profile_cookies(profile);
        if outcome.records.is_empty() {
            continue;
        }
        let records = permitted_profile_records(config, outcome.records, now);
        let existing = std::fs::read_to_string(jar_path).unwrap_or_default();
        let count = records.len();
        let merged = merge_into_jar(&existing, &records, now);
        if std::fs::write(jar_path, merged).is_ok() {
            stored += count;
            seen.insert(key, serde_json::json!(db_mtime));
            changed = true;
        }
    }
    if changed {
        config.set_path(marker, serde_json::Value::Object(seen));
    }
    if full {
        config.set_path("site_cookies_full_import_last_run", serde_json::json!(now));
    }
    stored
}

#[cfg(test)]
mod consent_tests {
    use super::*;

    fn record(name: &str) -> JarRecord {
        JarRecord {
            host: ".example.com".into(),
            include_subdomains: true,
            path: "/".into(),
            secure: true,
            expires: 0,
            name: name.into(),
            value: "fixture".into(),
            http_only: true,
        }
    }

    #[test]
    fn automatic_import_keeps_only_clearance_without_separate_session_consent() {
        let mut config = Config::default();
        let records = vec![
            record("SID"),
            record("cf_clearance"),
            record("incap_ses_123"),
        ];
        let selected = permitted_profile_records(&config, records.clone(), 100);
        assert_eq!(
            selected.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["cf_clearance", "incap_ses_123"]
        );
        config.set_path(
            "installed_browser_cookie_import_enabled",
            serde_json::json!(true),
        );
        assert_eq!(
            permitted_profile_records(&config, records.clone(), 100).len(),
            3
        );
        config.set_path("auto_import_browser_cookies", serde_json::json!(false));
        assert_eq!(
            permitted_profile_records(&config, records.clone(), 100).len(),
            3
        );
        config.set_path(
            "installed_browser_cookie_import_enabled",
            serde_json::json!(false),
        );
        assert!(permitted_profile_records(&config, records, 100).is_empty());
    }

    #[test]
    fn expired_or_empty_cookies_are_never_imported() {
        let mut expired = record("cf_clearance");
        expired.expires = 50;
        let mut empty = record("__cf_bm");
        empty.value.clear();
        assert!(
            permitted_profile_records(&Config::default(), vec![expired, empty], 100).is_empty()
        );
    }

    #[test]
    fn full_import_cadence_does_not_stop_clearance_refreshes() {
        let mut config = Config::default();
        config.set_path(
            "installed_browser_cookie_import_enabled",
            serde_json::json!(true),
        );
        config.set_path("site_cookies_full_import_last_run", serde_json::json!(100));
        assert_eq!(profile_import_mode(&config, 101), Some(false));
        let selected =
            permitted_profile_records(&config, vec![record("SID"), record("cf_clearance")], 101);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "cf_clearance");
        config.set_path("auto_import_browser_cookies", serde_json::json!(false));
        assert_eq!(profile_import_mode(&config, 101), None);
        assert_eq!(profile_import_mode(&config, 21_700), Some(true));
    }
}
