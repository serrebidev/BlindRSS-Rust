//! Anonymous YouTube browser session for one final yt-dlp retry
//! (Python `core/youtube_browser_session.py`): a hidden,
//! globally-serialized Chromium visit mints a visitor identity
//! and session cookies, written to a Netscape jar for yt-dlp's
//! `--cookies` retry with visitor data and the browser UA. Never
//! the personal profile; a good identity is reused for 30
//! minutes so streaming and downloading do not launch Chromium
//! twice for the same failure.
//!
//! Nothing here logs cookie contents, cookie values, tokens, or
//! the visitor identity.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Jar filename beside the config file (Python `_COOKIE_FILENAME`).
const COOKIE_FILENAME: &str = "youtube_browser_cookies.txt";

/// Identity reuse window (Python `_CACHE_SECONDS`).
const CACHE_SECONDS: u64 = 30 * 60;

static CONFIG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static CACHED: Mutex<Option<(YouTubeBrowserSession, Instant)>> = Mutex::new(None);

/// Point the session cache at the app config (the jar lives
/// beside it). Unconfigured, bootstrap fails closed.
pub fn configure(config_path: &Path) {
    *CONFIG_PATH.lock().unwrap_or_else(|p| p.into_inner()) = Some(config_path.to_path_buf());
}

fn jar_path() -> Option<PathBuf> {
    let path = CONFIG_PATH
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    Some(
        path.parent()
            .unwrap_or(Path::new("."))
            .join(COOKIE_FILENAME),
    )
}

/// An anonymous playable identity (Python `YouTubeBrowserSession`).
#[derive(Clone, Debug)]
pub struct YouTubeBrowserSession {
    pub cookie_file: PathBuf,
    pub visitor_data: String,
    pub user_agent: String,
    pub title: String,
}

/// True for cookies the jar keeps (Python `_is_youtube_cookie`).
pub fn is_youtube_cookie(domain: &str) -> bool {
    let domain = domain.trim().to_lowercase();
    let domain = domain.strip_prefix('.').unwrap_or(&domain);
    domain == "youtube.com"
        || domain == "youtube-nocookie.com"
        || domain.ends_with(".youtube.com")
        || domain.ends_with(".youtube-nocookie.com")
}

fn safe_field(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\t' | '\r' | '\n' => ' ',
            c => c,
        })
        .collect()
}

/// Atomically write only the YouTube cookies (Python
/// `_write_netscape_cookie_file`). CDP records carry `expires`;
/// Selenium-style records carry `expiry` — both are honored.
pub fn write_netscape_cookie_file(
    cookies: &[serde_json::Value],
    path: &Path,
) -> Result<PathBuf, String> {
    let mut text = String::from("# Netscape HTTP Cookie File\n");
    text.push_str("# Generated from BlindRSS's anonymous YouTube browser profile.\n");
    for cookie in cookies {
        let Some(obj) = cookie.as_object() else {
            continue;
        };
        let raw_domain = obj
            .get("domain")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !is_youtube_cookie(raw_domain) {
            continue;
        }
        let raw_domain = safe_field(raw_domain);
        let mut domain = raw_domain.clone();
        if obj
            .get("httpOnly")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !domain.starts_with("#HttpOnly_")
        {
            domain = format!("#HttpOnly_{domain}");
        }
        let expires = obj
            .get("expiry")
            .or_else(|| obj.get("expires"))
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse::<f64>().ok()))
            .filter(|v| v.is_finite())
            .map(|v| v.max(0.0) as i64)
            .unwrap_or(0);
        let path_field = safe_field(obj.get("path").and_then(|v| v.as_str()).unwrap_or("/"));
        let secure = obj.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);
        let name = safe_field(obj.get("name").and_then(|v| v.as_str()).unwrap_or(""));
        let value = safe_field(obj.get("value").and_then(|v| v.as_str()).unwrap_or(""));
        text.push_str(
            &[
                domain,
                if raw_domain.starts_with('.') {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                },
                path_field,
                if secure {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                },
                expires.to_string(),
                name,
                value,
            ]
            .join("\t"),
        );
        text.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cookie dir: {e}"))?;
    }
    crate::config::atomic_write(path, &text).map_err(|e| format!("cookie jar: {e}"))?;
    Ok(path.to_path_buf())
}

/// Read the identity script's JSON value: (visitor, status, title, UA).
pub fn identity_from_value(value: &serde_json::Value) -> Option<(String, String, String, String)> {
    let obj = value.as_object()?;
    let field = |key: &str| {
        obj.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Some((
        field("visitor"),
        field("status"),
        field("title"),
        field("ua"),
    ))
}

/// Visitor data is safe to place on a command line only without
/// `;`, CR or LF (Python's malformed-data rejection, plus the
/// whitespace/control rule `youtube_player_client_arg` enforces).
pub fn valid_visitor_data(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !value.contains([';', '\r', '\n'])
        && value.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Return a fresh browser-derived anonymous identity for a yt-dlp
/// retry (Python `bootstrap_youtube_session`): the hidden visit
/// is globally serialized with the feed fallback, a good
/// identity is reused for 30 minutes, and every failure —
/// unconfigured paths, no browser, no playable status —
/// returns None.
pub fn bootstrap_youtube_session(url: &str, timeout_s: f64) -> Option<YouTubeBrowserSession> {
    let target = url.trim();
    if target.is_empty() {
        return None;
    }
    {
        let cached = CACHED.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((session, until)) = cached.as_ref()
            && Instant::now() < *until
            && session.cookie_file.is_file()
        {
            return Some(session.clone());
        }
    }
    let timeout_s = if timeout_s.is_finite() {
        timeout_s.clamp(15.0, 90.0)
    } else {
        45.0
    };
    let jar = jar_path()?;
    let identity = crate::browser::youtube_identity(target, timeout_s)?;
    if !identity.status.trim().eq_ignore_ascii_case("OK")
        || !valid_visitor_data(&identity.visitor_data)
    {
        return None;
    }
    let cookie_file = write_netscape_cookie_file(&identity.cookies, &jar).ok()?;
    let session = YouTubeBrowserSession {
        cookie_file,
        visitor_data: identity.visitor_data.trim().to_string(),
        user_agent: identity.user_agent.trim().to_string(),
        title: identity.title.trim().to_string(),
    };
    *CACHED.lock().unwrap_or_else(|p| p.into_inner()) = Some((
        session.clone(),
        Instant::now() + Duration::from_secs(CACHE_SECONDS),
    ));
    Some(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static JAR_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn youtube_cookie_domains_match_python() {
        for hit in [
            "youtube.com",
            ".youtube.com",
            "www.youtube.com",
            ".www.youtube.com",
            "youtube-nocookie.com",
            "www.youtube-nocookie.com",
            ".music.youtube.com",
        ] {
            assert!(is_youtube_cookie(hit), "missed: {hit}");
        }
        for miss in ["", "google.com", "notyoutube.com", "youtube.com.evil.com"] {
            assert!(!is_youtube_cookie(miss), "false hit: {miss}");
        }
    }

    #[test]
    fn jar_keeps_youtube_only_with_python_shape() {
        let dir = std::env::temp_dir().join(format!(
            "blindrss-ytjar-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            JAR_TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("cookies.txt");
        let cookies = serde_json::json!([
            {"domain": ".youtube.com", "path": "/", "secure": true, "expires": 2000000000.0, "name": "VISITOR_INFO1_LIVE", "value": "abc"},
            {"domain": ".youtube.com", "path": "/", "secure": false, "expiry": 100, "name": "PREF", "value": "x\ty", "httpOnly": true},
            {"domain": ".google.com", "path": "/", "name": "NID", "value": "drop-me"},
            "not-an-object",
        ]);
        let out = write_netscape_cookie_file(cookies.as_array().unwrap(), &path).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.starts_with("# Netscape HTTP Cookie File\n"));
        // Only "# " lines are comments: an httpOnly cookie's
        // `#HttpOnly_` domain prefix is a data row (curl's own
        // Netscape convention, as Python writes it).
        let rows: Vec<&str> = text.lines().filter(|l| !l.starts_with("# ")).collect();
        assert_eq!(rows.len(), 2, "{text}");
        assert!(
            rows[0].starts_with(".youtube.com\tTRUE\t/\tTRUE\t2000000000\tVISITOR_INFO1_LIVE\tabc"),
            "{text}"
        );
        assert!(
            rows[1].starts_with("#HttpOnly_.youtube.com\tTRUE\t/\tFALSE\t100\tPREF\tx y"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_parse_reads_the_script_shape() {
        let value =
            serde_json::json!({"visitor": " CgX ", "status": "OK", "title": "T", "ua": "U"});
        assert_eq!(
            identity_from_value(&value),
            Some((
                "CgX".to_string(),
                "OK".to_string(),
                "T".to_string(),
                "U".to_string()
            ))
        );
        assert_eq!(identity_from_value(&serde_json::json!([1])), None);
        assert_eq!(
            identity_from_value(&serde_json::json!({})),
            Some((String::new(), String::new(), String::new(), String::new()))
        );
    }

    #[test]
    fn malformed_visitor_data_is_rejected() {
        assert!(valid_visitor_data("CgXabc123"));
        for bad in ["", "a;b", "a\rb", "a\nb", "a b", "a\tb"] {
            assert!(!valid_visitor_data(bad), "accepted: {bad:?}");
        }
    }

    #[test]
    fn bootstrap_fails_closed_without_a_target() {
        assert!(bootstrap_youtube_session("   ", 45.0).is_none());
    }
}
