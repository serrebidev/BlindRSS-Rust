//! Site cookies (port of `core/site_cookies.py`).
//!
//! Python keeps a Netscape-format cookie jar (`site_cookies.txt`)
//! beside the config, harvested from the user's browsers or edited
//! by hand, and sends matching cookies on article fetches so
//! paywalled/clearance sites return real pages. This port covers
//! the jar format + per-URL matching + the header budget rule
//! (Python issue #101: keep the Cookie header under the smallest
//! common server limit, 8 KB), plus the identity sidecars Python
//! keeps beside the jar: the user's global User-Agent
//! (`site_cookies_ua.txt`) and the per-host User-Agents a harvested
//! clearance was issued to (`site_cookies_ua_hosts.json`). A
//! clearance cookie is only valid together with the exact UA that
//! earned it, so the fetch layer must be able to ask which UA a
//! URL's cookies belong to (Audit 8 B05).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const HEADER_BUDGET: usize = 8000;

pub const UA_FILENAME: &str = "site_cookies_ua.txt";
pub const HOST_UA_FILENAME: &str = "site_cookies_ua_hosts.json";

/// Cookie names worth harvesting from a browser session
/// (Python `_is_harvestable`): the bot-check clearance cookies.
/// Matching is prefix-based on the trimmed lowercase name, so
/// `incap_ses_123` counts via its `incap_ses` prefix.
pub fn is_clearance_cookie_name(name: &str) -> bool {
    const WANT: &[&str] = &[
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
    ];
    let low = name.trim().to_ascii_lowercase();
    WANT.iter().any(|want| low.starts_with(want))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: i64, // unix seconds; 0 = session
    pub name: String,
    pub value: String,
}

/// Parse Netscape cookie-jar text. `#HttpOnly_` lines count as
/// cookies; plain `#` lines are comments.
pub fn parse_netscape_jar(text: &str) -> Vec<Cookie> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let line = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
            rest
        } else if line.starts_with('#') {
            continue;
        } else {
            line
        };
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        out.push(Cookie {
            domain: fields[0].trim().trim_start_matches('.').to_lowercase(),
            // Python `_parse_jar`: a leading dot in the stored
            // domain means include-subdomains on its own, whatever
            // the flag column says.
            include_subdomains: fields[0].trim().starts_with('.')
                || fields[1].eq_ignore_ascii_case("TRUE"),
            path: fields[2].to_string(),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires: fields[4].trim().parse().unwrap_or(0),
            name: fields[5].to_string(),
            value: fields[6..].join("\t"),
        });
    }
    out
}

fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_lowercase()
}

fn path_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    match rest.find('/') {
        Some(i) => rest[i..]
            .split(['?', '#'])
            .next()
            .unwrap_or("/")
            .to_string(),
        None => "/".to_string(),
    }
}

fn cookie_matches(cookie: &Cookie, host: &str, path: &str, https: bool, now: i64) -> bool {
    if cookie.expires != 0 && cookie.expires < now {
        return false;
    }
    if cookie.secure && !https {
        return false;
    }
    let domain_ok = host == cookie.domain
        || (cookie.include_subdomains && host.ends_with(&format!(".{}", cookie.domain)));
    if !domain_ok {
        return false;
    }
    // Python `cookies_for` path rule (Audit 8 B07): a stored path
    // matches the request path exactly or at a slash boundary —
    // never as a bare string prefix, so `/private` must not leak
    // into `/private-other`.
    if cookie.path.is_empty() || cookie.path == "/" {
        return true;
    }
    path == cookie.path || path.starts_with(&format!("{}/", cookie.path.trim_end_matches('/')))
}

/// The ordered (name, value) pairs `url` would send from the jar
/// (Python `cookies_for`). When several stored cookies share a
/// name, the LAST value wins but the pair keeps its FIRST position
/// — Python builds an insertion-ordered dict keyed by name.
pub fn cookies_for(jar: &[Cookie], url: &str, now: i64) -> Vec<(String, String)> {
    let host = host_of(url);
    let path = path_of(url);
    let https = url.starts_with("https://");
    let mut out: Vec<(String, String)> = Vec::new();
    for cookie in jar {
        if !cookie_matches(cookie, &host, &path, https, now) {
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|(name, _)| *name == cookie.name) {
            existing.1 = cookie.value.clone();
        } else {
            out.push((cookie.name.clone(), cookie.value.clone()));
        }
    }
    out
}

/// Python `_trim_cookies_to_budget` (Audit 8 B06): when the full
/// header would exceed the budget, prefer clearance cookies first,
/// then the smallest rendered entries; an entry that does not fit
/// is SKIPPED, not a stop — a huge first cookie must not starve the
/// clearance behind it. Kept cookies render in their original
/// relative order.
fn trim_to_budget(pairs: &[(String, String)]) -> Vec<(String, String)> {
    let rendered: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let total: usize =
        rendered.iter().map(|p| p.len()).sum::<usize>() + rendered.len().saturating_sub(1) * 2;
    if total <= HEADER_BUDGET {
        return pairs.to_vec();
    }
    let mut order: Vec<usize> = (0..pairs.len()).collect();
    order.sort_by_key(|&i| {
        (
            if is_clearance_cookie_name(&pairs[i].0) {
                0
            } else {
                1
            },
            rendered[i].len(),
        )
    });
    let mut used = 0usize;
    let mut keep = vec![false; pairs.len()];
    for i in order {
        let cost = rendered[i].len() + if used > 0 { 2 } else { 0 };
        if used + cost > HEADER_BUDGET {
            continue;
        }
        used += cost;
        keep[i] = true;
    }
    pairs
        .iter()
        .zip(keep.iter())
        .filter(|(_, k)| **k)
        .map(|(pair, _)| pair.clone())
        .collect()
}

/// The `Cookie` header value for `url` from the jar, trimmed to
/// the header budget with Python's prioritised selection.
pub fn cookie_header_for(jar: &[Cookie], url: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    trim_to_budget(&cookies_for(jar, url, now))
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Whether any clearance-shaped cookie in the jar applies to
/// `url` (Python `has_clearance_for`).
pub fn has_clearance_for(jar: &[Cookie], url: &str, now: i64) -> bool {
    cookies_for(jar, url, now)
        .iter()
        .any(|(name, _)| is_clearance_cookie_name(name))
}

// ---------------------------------------------------------------------
// Identity sidecars (Python `site_cookies.py` UA half, Audit 8 B05)

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sidecar_path(config_path: &Path, name: &str) -> Option<PathBuf> {
    config_path.parent().map(|dir| dir.join(name))
}

/// The user's global browser User-Agent (Python `get_user_agent`):
/// the UA stored beside the jar, "" when unset or unreadable.
pub fn get_global_user_agent(config_path: &Path) -> String {
    let Some(path) = sidecar_path(config_path, UA_FILENAME) else {
        return String::new();
    };
    std::fs::read_to_string(path)
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// Store (or, for an empty UA, remove) the global browser
/// User-Agent (Python `set_user_agent`).
pub fn set_global_user_agent(config_path: &Path, user_agent: &str) {
    let Some(path) = sidecar_path(config_path, UA_FILENAME) else {
        return;
    };
    let ua = user_agent.trim();
    if ua.is_empty() {
        let _ = std::fs::remove_file(path);
    } else {
        let _ = std::fs::write(path, format!("{ua}\n"));
    }
}

/// The per-host User-Agent map (Python
/// `site_cookies_ua_hosts.json`): host → UA the host's clearance
/// was issued to. Keys are stored lowercase without leading dots.
pub fn load_host_user_agents(config_path: &Path) -> Vec<(String, String)> {
    let Some(path) = sidecar_path(config_path, HOST_UA_FILENAME) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(map) = value.as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(host, ua)| {
            let ua = ua.as_str()?.trim().to_string();
            let host = host.trim().to_ascii_lowercase();
            let host = host.trim_start_matches('.').to_string();
            if host.is_empty() || ua.is_empty() {
                None
            } else {
                Some((host, ua))
            }
        })
        .collect()
}

fn save_host_user_agents(config_path: &Path, entries: &[(String, String)]) {
    let Some(path) = sidecar_path(config_path, HOST_UA_FILENAME) else {
        return;
    };
    let map: BTreeMap<&str, &str> = entries
        .iter()
        .map(|(host, ua)| (host.as_str(), ua.as_str()))
        .collect();
    let Ok(text) = serde_json::to_string_pretty(&map) else {
        return;
    };
    let _ = std::fs::write(path, text);
}

/// Pin (or, for an empty UA, unpin) the User-Agent for one host
/// (Python `set_host_user_agent`).
pub fn set_host_user_agent(config_path: &Path, host: &str, user_agent: &str) {
    let host = host.trim().to_ascii_lowercase();
    let host = host.trim_start_matches('.').to_string();
    if host.is_empty() {
        return;
    }
    let mut entries = load_host_user_agents(config_path);
    entries.retain(|(h, _)| *h != host);
    let ua = user_agent.trim();
    if !ua.is_empty() {
        entries.push((host, ua.to_string()));
    }
    save_host_user_agents(config_path, &entries);
}

/// The User-Agent pinned to `url`'s host, longest stored suffix
/// winning (Python `host_user_agent_for`): a UA pinned for
/// `example.com` covers `news.example.com`, and a longer stored
/// host beats a shorter one.
pub fn host_user_agent_for(entries: &[(String, String)], url: &str) -> String {
    let host = host_of(url);
    let mut best: Option<(&str, usize)> = None;
    for (candidate, ua) in entries {
        if (host == *candidate || host.ends_with(&format!(".{candidate}")))
            && best.map(|(_, len)| candidate.len() > len).unwrap_or(true)
        {
            best = Some((ua, candidate.len()));
        }
    }
    best.map(|(ua, _)| ua.to_string()).unwrap_or_default()
}

/// The User-Agent a request to `url` must use so its cookies are
/// accepted (Python `user_agent_for`): the host-pinned UA when the
/// jar actually sends cookies for the URL; the global UA only when
/// the URL holds clearance; otherwise "" (callers keep their
/// default identity — unrelated requests stay untouched).
pub fn user_agent_for(config_path: &Path, jar: &[Cookie], url: &str, now: i64) -> String {
    let host_ua = host_user_agent_for(&load_host_user_agents(config_path), url);
    if !host_ua.is_empty() {
        return if cookies_for(jar, url, now).is_empty() {
            String::new()
        } else {
            host_ua
        };
    }
    if !has_clearance_for(jar, url, now) {
        return String::new();
    }
    get_global_user_agent(config_path)
}

/// Remove ONLY the clearance cookies that apply to `url`'s host,
/// plus the host UAs pinned for matching domains (Python
/// `forget_clearance_for`). Ordinary session cookies are never
/// touched. Returns whether anything was removed.
pub fn forget_clearance_for(config_path: &Path, url: &str) -> bool {
    let host = host_of(url);
    if host.is_empty() {
        return false;
    }
    let Some(jar_path) = sidecar_path(config_path, "site_cookies.txt") else {
        return false;
    };
    // A missing jar reads as empty (Python's `_jar_records`); the
    // host-UA cleanup below still applies.
    let text = std::fs::read_to_string(&jar_path).unwrap_or_default();
    let mut kept_lines: Vec<String> = Vec::new();
    let mut removed = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let data = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if data.starts_with('#') || data.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = data.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        let domain = fields[0].trim().to_ascii_lowercase();
        let bare = domain.trim_start_matches('.');
        let include_sub = domain.starts_with('.') || fields[1].eq_ignore_ascii_case("TRUE");
        let host_matches = host == bare || (include_sub && host.ends_with(&format!(".{bare}")));
        if host_matches && is_clearance_cookie_name(fields[5]) {
            removed = true;
            continue;
        }
        kept_lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            fields[0],
            fields[1],
            fields[2],
            fields[3],
            fields[4],
            fields[5],
            fields[6..].join("\t")
        ));
    }
    if removed {
        let mut out = String::from("# Netscape HTTP Cookie File\n");
        for line in &kept_lines {
            out.push_str(line);
            out.push('\n');
        }
        let tmp = jar_path.with_extension("txt.tmp");
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &jar_path);
        }
    }
    // Python also drops host UAs pinned for the forgotten domains.
    let entries = load_host_user_agents(config_path);
    let kept: Vec<(String, String)> = entries
        .into_iter()
        .filter(|(domain, _)| !(host == *domain || host.ends_with(&format!(".{domain}"))))
        .collect();
    if kept.len() != load_host_user_agents(config_path).len() {
        save_host_user_agents(config_path, &kept);
        removed = true;
    }
    removed
}

/// Record the cookies a solved in-app browser challenge produced
/// (Python `record_browser_session`): ONLY clearance-shaped
/// cookies whose domain applies to the request host are stored —
/// a challenge solve must never silently harvest the whole
/// session. Session cookies get a one-hour expiry, and the UA the
/// browser ran with is pinned to the request host. Returns the
/// number of cookies stored.
pub fn record_browser_session(
    config_path: &Path,
    url: &str,
    records: &[crate::chromium::JarRecord],
    user_agent: &str,
    now: i64,
) -> usize {
    let host = host_of(url);
    if host.is_empty() {
        return 0;
    }
    let mut kept: Vec<crate::chromium::JarRecord> = Vec::new();
    for record in records {
        if !is_clearance_cookie_name(&record.name) {
            continue;
        }
        let bare = record
            .host
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        let bare = if bare.is_empty() { host.clone() } else { bare };
        if !(host == bare || host.ends_with(&format!(".{bare}"))) {
            continue;
        }
        let mut record = record.clone();
        record.host = bare;
        if record.expires <= 0 {
            record.expires = now + 3600;
        }
        kept.push(record);
    }
    let stored = kept.len();
    if stored > 0 {
        let Some(jar_path) = sidecar_path(config_path, "site_cookies.txt") else {
            return 0;
        };
        let existing = std::fs::read_to_string(&jar_path).unwrap_or_default();
        let merged = crate::chromium::merge_into_jar(&existing, &kept, now);
        if std::fs::write(&jar_path, merged).is_err() {
            return 0;
        }
    }
    if !user_agent.trim().is_empty() {
        set_host_user_agent(config_path, &host, user_agent);
    }
    stored
}

/// The shared URL-aware header pair for fetch call sites
/// (Python `core/utils.py` attachment rule): the Cookie header for
/// `url`, and the User-Agent the cookies were issued to when one
/// applies (`None` = keep the caller's default identity).
pub fn site_headers_for(config_path: &Path, url: &str) -> (String, Option<String>) {
    let jar = load_jar_beside(config_path);
    let cookie = cookie_header_for(&jar, url);
    let ua = user_agent_for(config_path, &jar, url, now_unix());
    (cookie, if ua.is_empty() { None } else { Some(ua) })
}

// ---------------------------------------------------------------------
// Targeted recovery (Python `refresh_*_from_browsers`, Audit 8 B05).
// When a stored session or clearance stops working, Python re-reads
// the installed Firefox profiles for JUST that site and merges the
// best copy. Each key is rate-limited to one scan per 20 seconds.

static RECOVERY_LAST: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn recovery_allowed(key: &str) -> bool {
    let mut map = RECOVERY_LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if let Some(last) = map.get(key)
        && now.duration_since(*last).as_secs() < 20
    {
        return false;
    }
    map.insert(key.to_string(), now);
    true
}

fn record_key(record: &crate::chromium::JarRecord) -> (String, String, String) {
    (
        record.host.trim().trim_start_matches('.').to_lowercase(),
        record.name.clone(),
        record.path.clone(),
    )
}

fn merge_records_into_jar(config_path: &Path, records: &[crate::chromium::JarRecord], now: i64) {
    if records.is_empty() {
        return;
    }
    let Some(jar_path) = sidecar_path(config_path, "site_cookies.txt") else {
        return;
    };
    let existing = std::fs::read_to_string(&jar_path).unwrap_or_default();
    let merged = crate::chromium::merge_into_jar(&existing, records, now);
    let _ = std::fs::write(&jar_path, merged);
}

/// Re-harvest clearance for `url` from every Firefox profile,
/// ignoring the watcher's mtime markers (Python
/// `refresh_clearance_from_browsers`). Returns whether the
/// cookies `url` sends actually changed.
pub fn refresh_clearance_from_browsers(config_path: &Path, url: &str) -> bool {
    let host = host_of(url);
    if host.is_empty() || !recovery_allowed(&format!("clearance:{host}")) {
        return false;
    }
    let profiles = crate::firefox::list_profiles();
    refresh_clearance_with_profiles(config_path, url, &profiles)
}

/// The scan half of `refresh_clearance_from_browsers` over
/// explicit profiles, for fixture tests.
pub fn refresh_clearance_with_profiles(
    config_path: &Path,
    url: &str,
    profiles: &[crate::firefox::FirefoxProfile],
) -> bool {
    let now = now_unix();
    let before = cookies_for(&load_jar_beside(config_path), url, now);
    let mut best: Vec<(crate::chromium::JarRecord, String)> = Vec::new();
    for profile in profiles {
        let Ok(records) = crate::firefox::read_profile_cookies(profile) else {
            continue;
        };
        let ua = crate::firefox::profile_user_agent(&profile.profile_dir);
        for record in records {
            if !is_clearance_cookie_name(&record.name)
                || record.value.is_empty()
                || (record.expires != 0 && record.expires < now)
            {
                continue;
            }
            match best
                .iter_mut()
                .find(|(existing, _)| record_key(existing) == record_key(&record))
            {
                Some(slot) => {
                    if record.expires > slot.0.expires {
                        *slot = (record, ua.clone());
                    }
                }
                None => best.push((record, ua.clone())),
            }
        }
    }
    if best.is_empty() {
        return false;
    }
    let records: Vec<crate::chromium::JarRecord> =
        best.iter().map(|(record, _)| record.clone()).collect();
    merge_records_into_jar(config_path, &records, now);
    for (record, ua) in &best {
        if !ua.is_empty() {
            let host = record.host.trim().trim_start_matches('.');
            if !host.is_empty() {
                set_host_user_agent(config_path, host, ua);
            }
        }
    }
    cookies_for(&load_jar_beside(config_path), url, now) != before
}

/// The shared Groups.io / Reddit session recovery (Python
/// `refresh_groups_io_cookies_from_browsers` /
/// `refresh_reddit_cookies_from_browsers`): only that site's
/// unexpired cookies are considered; the profile carrying the
/// strongest login (auth cookies first, then newest expiry, then
/// most cookies) wins whole — profiles are never blended.
fn refresh_site_session_with_profiles(
    config_path: &Path,
    url: &str,
    profiles: &[crate::firefox::FirefoxProfile],
    site_domain: &str,
    auth_names: &[&str],
    auth_contains_session: bool,
) -> bool {
    let now = now_unix();
    let host = host_of(url);
    if !(host == site_domain || host.ends_with(&format!(".{site_domain}"))) {
        return false;
    }
    type SiteScore = (bool, usize, i64, usize);
    let mut winner: Option<(Vec<crate::chromium::JarRecord>, String, SiteScore)> = None;
    for profile in profiles {
        let Ok(records) = crate::firefox::read_profile_cookies(profile) else {
            continue;
        };
        let site_records: Vec<crate::chromium::JarRecord> = records
            .into_iter()
            .filter(|record| {
                let bare = record.host.trim().trim_start_matches('.').to_lowercase();
                (bare == site_domain || bare.ends_with(&format!(".{site_domain}")))
                    && !record.value.is_empty()
                    && (record.expires == 0 || record.expires >= now)
            })
            .collect();
        if site_records.is_empty() {
            continue;
        }
        let is_auth = |name: &str| {
            let low = name.trim().to_ascii_lowercase();
            auth_names.contains(&low.as_str()) || (auth_contains_session && low.contains("session"))
        };
        let auth_count = site_records.iter().filter(|r| is_auth(&r.name)).count();
        let newest = site_records.iter().map(|r| r.expires).max().unwrap_or(0);
        let score = (auth_count > 0, auth_count, newest, site_records.len());
        if winner
            .as_ref()
            .map(|(_, _, best)| score > *best)
            .unwrap_or(true)
        {
            winner = Some((
                site_records,
                crate::firefox::profile_user_agent(&profile.profile_dir),
                score,
            ));
        }
    }
    let Some((records, ua, _)) = winner else {
        return false;
    };
    merge_records_into_jar(config_path, &records, now);
    if !ua.is_empty() {
        set_host_user_agent(config_path, site_domain, &ua);
    }
    !cookies_for(&load_jar_beside(config_path), url, now).is_empty()
}

/// Recover a Groups.io login from the installed Firefox profiles
/// (Python `refresh_groups_io_cookies_from_browsers`). Any other
/// host returns false immediately.
pub fn refresh_groups_io_cookies(config_path: &Path, url: &str) -> bool {
    let host = host_of(url);
    if !(host == "groups.io" || host.ends_with(".groups.io")) {
        return false;
    }
    if !recovery_allowed(&format!("groupsio-login:{host}")) {
        return !cookies_for(&load_jar_beside(config_path), url, now_unix()).is_empty();
    }
    refresh_site_session_with_profiles(
        config_path,
        url,
        &crate::firefox::list_profiles(),
        "groups.io",
        &["groupsio", "groupsio_session", "session", "sessionid"],
        true,
    )
}

/// The Groups.io recovery over explicit profiles, for fixture
/// tests (no rate limit — that lives in the public wrapper).
pub fn refresh_groups_io_cookies_with_profiles(
    config_path: &Path,
    url: &str,
    profiles: &[crate::firefox::FirefoxProfile],
) -> bool {
    refresh_site_session_with_profiles(
        config_path,
        url,
        profiles,
        "groups.io",
        &["groupsio", "groupsio_session", "session", "sessionid"],
        true,
    )
}

/// The Reddit recovery over explicit profiles, for fixture
/// tests (no rate limit — that lives in the public wrapper).
pub fn refresh_reddit_cookies_with_profiles(
    config_path: &Path,
    url: &str,
    profiles: &[crate::firefox::FirefoxProfile],
) -> bool {
    refresh_site_session_with_profiles(
        config_path,
        url,
        profiles,
        "reddit.com",
        &[
            "reddit_session",
            "token_v2",
            "csrf_token",
            "session_tracker",
        ],
        false,
    )
}

/// Recover a Reddit login from the installed Firefox profiles
/// (Python `refresh_reddit_cookies_from_browsers`). Any other
/// host returns false immediately.
pub fn refresh_reddit_cookies(config_path: &Path, url: &str) -> bool {
    let host = host_of(url);
    if !(host == "reddit.com" || host.ends_with(".reddit.com")) {
        return false;
    }
    if !recovery_allowed(&format!("reddit-login:{host}")) {
        return !cookies_for(&load_jar_beside(config_path), url, now_unix()).is_empty();
    }
    refresh_site_session_with_profiles(
        config_path,
        url,
        &crate::firefox::list_profiles(),
        "reddit.com",
        &[
            "reddit_session",
            "token_v2",
            "csrf_token",
            "session_tracker",
        ],
        false,
    )
}

/// Load the jar that sits beside the config file
/// (`site_cookies.txt`); empty when absent.
pub fn load_jar_beside(config_path: &std::path::Path) -> Vec<Cookie> {
    let Some(dir) = config_path.parent() else {
        return Vec::new();
    };
    match std::fs::read_to_string(dir.join("site_cookies.txt")) {
        Ok(text) => parse_netscape_jar(&text),
        Err(_) => Vec::new(),
    }
}
