//! Source-feed family (Batch 35): subscriptions that have no native
//! RSS and are enumerated instead — Odysee, Rumble, YouTube search,
//! SoundCloud, Mixcloud, Google Groups, Groups.io archive search —
//! plus the YouTube channel-feed listing fallback, and the NPR / Sky
//! media attach Python runs inside its generic parse path.
//!
//! Ports Python `core/odysee.py`, `core/rumble.py`,
//! `core/forum_sources.py` (subscription side), `core/groups_io.py`
//! (search side), the listing fetchers of `core/discovery.py`,
//! `core/npr.py` and `core/sky.py`. Listing articles store an empty
//! body and no enclosure (YouTube search/channel items carry the
//! watch URL as `video/youtube` media), exactly like Python: playback
//! resolves them through the yt-dlp path on activation.

use crate::extract::{ElemSpan, attr_value, find_elements, has_class, leaf_text};
use crate::feeds::ParsedEntry;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct SourceItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub author: Option<String>,
    /// RFC 3339 date string when the source provides one.
    pub published: Option<String>,
    pub content: String,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Odysee,
    Rumble,
    YoutubeSearch,
    SoundCloud,
    Mixcloud,
    GoogleGroup,
    GroupsIoSearch,
}

/// Split a URL into (scheme, host, path, query) — same helper shape
/// as discovery.rs (kept local; the two modules evolve separately).
fn split_url(url: &str) -> Option<(String, String, String, String)> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (host, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.to_lowercase();
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p.to_string(), q.split('#').next().unwrap_or("").to_string()),
        None => (
            tail.split('#').next().unwrap_or("").to_string(),
            String::new(),
        ),
    };
    Some((scheme, host, path, query))
}

fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == name {
            return Some(url_decode(v));
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(bytes[i]),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Seconds since epoch -> RFC 3339 UTC (civil algorithm, no chrono).
pub(crate) fn epoch_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    epoch_rfc3339(secs)
}

/// Listing dates (Python `normalize_date` listing behaviour): an
/// ISO-shaped date when the source gave one, otherwise now — an
/// undated listing item must not sort as year 0001 at the bottom.
fn listing_date(published: Option<String>) -> Option<String> {
    match published {
        Some(p) if p.len() >= 10 && p.as_bytes().get(4) == Some(&b'-') => Some(p),
        _ => Some(now_rfc3339()),
    }
}

/// UUIDv5 with the URL namespace (Python `uuid.uuid5(NAMESPACE_URL, …)`).
pub fn uuid5_url(seed: &str) -> String {
    use sha1::{Digest, Sha1};
    const NAMESPACE_URL: [u8; 16] = [
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ];
    let mut hasher = Sha1::new();
    hasher.update(NAMESPACE_URL);
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x50; // version 5
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

// ---------------------------------------------------------------------------
// Classifiers + subscription normalization (Python discovery/local rules)
// ---------------------------------------------------------------------------

pub fn is_odysee_url(url: &str) -> bool {
    if url.trim().to_lowercase().starts_with("lbry://") {
        return true;
    }
    match split_url(url) {
        Some((_, host, _, _)) => host.contains("odysee.com") || host.contains("lbry.tv"),
        None => false,
    }
}

/// Strip query/fragment from an Odysee or Rumble URL (stable ids).
pub fn strip_query_fragment(url: &str) -> String {
    match split_url(url) {
        Some((scheme, host, path, _)) => format!("{scheme}://{host}{path}"),
        None => url.to_string(),
    }
}

pub fn normalize_odysee_url(url: &str) -> String {
    if is_odysee_url(url) {
        strip_query_fragment(url)
    } else {
        url.to_string()
    }
}

pub fn is_rumble_url(url: &str) -> bool {
    match split_url(url) {
        Some((_, host, _, _)) => host.contains("rumble.com"),
        None => false,
    }
}

/// Python `normalize_rumble_feed_url`: channel/user pages get the
/// `/videos` listing view; search pages default to `sort=date`.
pub fn normalize_rumble_feed_url(url: &str) -> String {
    let Some((scheme, host, path, query)) = split_url(url) else {
        return url.to_string();
    };
    if !host.contains("rumble.com") {
        return url.to_string();
    }
    let path_t = path.trim_end_matches('/');
    let parts: Vec<&str> = path_t.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() == 2 && (parts[0] == "c" || parts[0] == "user") {
        return format!("{scheme}://{host}{path_t}/videos");
    }
    if parts.first().copied() == Some("search") && query_param(&query, "sort").is_none() {
        let q = if query.is_empty() {
            "sort=date".to_string()
        } else {
            format!("{query}&sort=date")
        };
        return format!("{scheme}://{host}{path}?{q}");
    }
    url.to_string()
}

fn is_listing_page_url(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    !lower.ends_with(".xml") && !lower.ends_with(".rss") && !lower.ends_with(".atom")
}

/// YouTube search-results URL: youtube.com/results?search_query=…
pub fn youtube_search_query(url: &str) -> Option<String> {
    let (_, host, path, query) = split_url(url)?;
    if !host.contains("youtube.com") || path.trim_end_matches('/') != "/results" {
        return None;
    }
    let q = query_param(&query, "search_query")?;
    let q = q.trim().to_string();
    if q.is_empty() { None } else { Some(q) }
}

/// The channel id of a native `feeds/videos.xml?channel_id=UC…` URL.
pub fn youtube_channel_id(url: &str) -> Option<String> {
    let (_, host, path, query) = split_url(url)?;
    if !host.contains("youtube.com") || path.trim_end_matches('/') != "/feeds/videos.xml" {
        return None;
    }
    let cid = query_param(&query, "channel_id")?;
    let ok = cid.len() == 24
        && cid.starts_with("UC")
        && cid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Some(cid) } else { None }
}

/// SoundCloud URL kind: "user" | "playlist" | "track" | "" (Python
/// `soundcloud_listing_kind`).
pub fn soundcloud_listing_kind(url: &str) -> &'static str {
    let Some((_, host, path, _)) = split_url(url) else {
        return "";
    };
    if !host.contains("soundcloud.com") {
        return "";
    }
    let parts: Vec<String> = path
        .trim_matches('/')
        .to_lowercase()
        .split('/')
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    if parts.is_empty() {
        return "";
    }
    if parts.iter().any(|p| p == "sets") {
        return "playlist";
    }
    if [
        "you", "search", "discover", "stream", "upload", "settings", "pages", "tags",
    ]
    .contains(&parts[0].as_str())
    {
        return "";
    }
    match parts.len() {
        1 => "user",
        2 => "track",
        _ => "",
    }
}

/// Mixcloud URL kind + API key path (Python `mixcloud_listing_kind`
/// / `_mixcloud_key_from_url`).
pub fn mixcloud_listing(url: &str) -> Option<(&'static str, String)> {
    let (_, host, path, _) = split_url(url)?;
    if !host.contains("mixcloud.com") {
        return None;
    }
    let mut key = path.clone();
    if !key.starts_with('/') {
        key = format!("/{key}");
    }
    if !key.ends_with('/') {
        key.push('/');
    }
    let parts: Vec<&str> = key.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let kind = if parts.len() == 1 {
        "user"
    } else if parts.len() >= 3 && parts[1] == "playlists" {
        "playlist"
    } else if parts.len() == 2 {
        "cloudcast"
    } else {
        return None;
    };
    Some((kind, key))
}

/// Canonical Google Groups subscription URL for a group page or a
/// `name@googlegroups.com` address; thread URLs are excluded (Python
/// `google_group_subscription_url`).
pub fn google_group_subscription_url(value: &str) -> Option<String> {
    let v = value.trim();
    if let Some((name, domain)) = v.rsplit_once('@') {
        if domain.eq_ignore_ascii_case("googlegroups.com") && valid_group_name(name) {
            return Some(format!("https://groups.google.com/g/{name}"));
        }
        return None;
    }
    let (_, host, path, _) = split_url(v)?;
    if host != "groups.google.com" {
        return None;
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() == 2 && parts[0] == "g" && valid_group_name(parts[1]) {
        return Some(format!("https://groups.google.com/g/{}", parts[1]));
    }
    None
}

fn valid_group_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c))
}

/// Groups.io group search URL parts: (origin, group, query).
pub fn groups_io_search(url: &str) -> Option<(String, String, String)> {
    let (scheme, host, path, query) = split_url(url)?;
    if host != "groups.io" && !host.ends_with(".groups.io") {
        return None;
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() == 3 && parts[0] == "g" && parts[2] == "search" {
        let q = query_param(&query, "q").unwrap_or_default();
        return Some((format!("{scheme}://{host}"), parts[1].to_string(), q));
    }
    None
}

/// A Groups.io group's native RSS URL for a group landing/search URL
/// (Python `group_feed_url`).
pub fn groups_io_feed_url(url: &str) -> Option<String> {
    let (_, host, path, _) = split_url(url)?;
    if host != "groups.io" && !host.ends_with(".groups.io") {
        return None;
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2
        && parts[0] == "g"
        && (parts.len() == 2
            || (parts.len() == 3 && ["topics", "messages", "rss", "search"].contains(&parts[2])))
    {
        return Some(format!("https://{host}/g/{}/rss", parts[1]));
    }
    None
}

/// Classify a feed URL into a listing source, if it is one. Mirrors
/// the branch order of Python `providers/local.py::_refresh_single_feed`.
pub fn classify(url: &str) -> Option<SourceKind> {
    if is_odysee_url(url) && is_listing_page_url(url) {
        return Some(SourceKind::Odysee);
    }
    if is_rumble_url(url) && is_listing_page_url(url) {
        return Some(SourceKind::Rumble);
    }
    if youtube_search_query(url).is_some() {
        return Some(SourceKind::YoutubeSearch);
    }
    match soundcloud_listing_kind(url) {
        "user" | "playlist" => return Some(SourceKind::SoundCloud),
        _ => {}
    }
    if let Some((kind, _)) = mixcloud_listing(url)
        && (kind == "user" || kind == "playlist")
    {
        return Some(SourceKind::Mixcloud);
    }
    if google_group_subscription_url(url).is_some() {
        return Some(SourceKind::GoogleGroup);
    }
    if groups_io_search(url).is_some() {
        return Some(SourceKind::GroupsIoSearch);
    }
    None
}

// ---------------------------------------------------------------------------
// Fetch plumbing
// ---------------------------------------------------------------------------

fn fetch_text(url: &str) -> Result<String, String> {
    let (bytes, _final) = crate::extract::fetch_page_bytes(url, "")?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn fetch_json(url: &str) -> Result<Value, String> {
    let text = fetch_text(url)?;
    serde_json::from_str(&text).map_err(|e| format!("JSON parse failed for {url}: {e}"))
}

/// Run yt-dlp flat-playlist and parse the JSON-lines output (Python
/// `_run_ytdlp_flat_listing` / odysee + youtube listing commands).
fn ytdlp_flat_listing(
    url: &str,
    max_items: usize,
    timeout_s: u64,
    youtube: bool,
) -> Result<Vec<Value>, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let mut args: Vec<String> = vec![
        "--ignore-config".into(),
        "--dump-json".into(),
        "--flat-playlist".into(),
        "--ignore-errors".into(),
        "--no-warnings".into(),
        "--playlist-end".into(),
        max_items.to_string(),
    ];
    if youtube {
        args.push("--extractor-args".into());
        args.push("youtubetab:approximate_date".into());
    }
    args.push(url.to_string());
    // F20: owned subprocess — a timed-out listing is killed and
    // reaped via the shared runner, never abandoned on a thread.
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = crate::dependency::run_tool(
        &exe,
        &arg_refs,
        std::time::Duration::from_secs(timeout_s.max(5)),
    )
    .map_err(|e| {
        if e.contains("timed out") {
            "yt-dlp listing timed out".to_string()
        } else {
            format!("yt-dlp: {e}")
        }
    })?;
    let mut entries = Vec::new();
    for line in out.stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            entries.push(v);
        }
    }
    if entries.is_empty() && !out.success {
        let last = out.stderr.lines().last().unwrap_or("failed").trim();
        return Err(format!("yt-dlp: {last}"));
    }
    Ok(entries)
}

fn json_str(v: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    None
}

/// yt-dlp entry date: upload_date YYYYMMDD, else timestamp epoch.
fn entry_date(e: &Value) -> Option<String> {
    if let Some(d) = e.get("upload_date").and_then(|v| v.as_str())
        && d.len() == 8
        && d.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("{}-{}-{}T00:00:00Z", &d[0..4], &d[4..6], &d[6..8]));
    }
    for k in ["timestamp", "release_timestamp"] {
        if let Some(t) = e.get(k).and_then(|v| v.as_f64()) {
            return Some(epoch_rfc3339(t as i64));
        }
    }
    None
}

/// One yt-dlp flat entry -> SourceItem. `site` shapes the URL repair
/// and fallbacks (Python's per-module `_extract_listing_from_info`).
fn flat_entry_item(e: &Value, site: &str) -> Option<SourceItem> {
    let entry_type = json_str(e, &["_type"]).unwrap_or_default().to_lowercase();
    if entry_type == "channel" || entry_type == "playlist" {
        return None;
    }
    let raw_url = json_str(e, &["webpage_url", "url"])?;
    let url = match site {
        "youtube" => {
            let vid = json_str(e, &["id"]).unwrap_or_default();
            if !vid.is_empty() {
                format!("https://www.youtube.com/watch?v={vid}")
            } else if raw_url.starts_with("http") {
                raw_url.clone()
            } else {
                format!("https://www.youtube.com/watch?v={raw_url}")
            }
        }
        "odysee" => normalize_odysee_url(&raw_url),
        _ => raw_url.clone(),
    };
    let fallback_title = match site {
        "youtube" => "YouTube Video",
        "odysee" => "Odysee Video",
        "soundcloud" => "SoundCloud Track",
        _ => "Video",
    };
    let title = json_str(e, &["title"]).unwrap_or_else(|| fallback_title.to_string());
    let author = json_str(e, &["channel", "uploader", "uploader_id"]);
    Some(SourceItem {
        id: url.clone(),
        url,
        title,
        author,
        published: entry_date(e),
        content: String::new(),
        media_url: None,
        media_type: None,
    })
}

fn dedupe_items(items: Vec<SourceItem>) -> Vec<SourceItem> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for it in items {
        if seen.insert(it.url.clone()) {
            out.push(it);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Per-source fetchers
// ---------------------------------------------------------------------------

/// Run yt-dlp for a single info document (`-J --flat-playlist`):
/// the listing title plus its flat entries in one JSON value
/// (Python's yt_dlp-library `extract_flat="in_playlist"` shape).
/// `extra_args` carries one attempt's cookie arguments
/// (Audit 8 F14).
fn ytdlp_info_document_attempt(
    url: &str,
    max_items: usize,
    timeout_s: u64,
    extra_args: &[String],
) -> Result<Value, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let mut args: Vec<String> = vec![
        "--ignore-config".into(),
        "-J".into(),
        "--flat-playlist".into(),
        "--ignore-errors".into(),
        "--no-warnings".into(),
        "--playlist-end".into(),
        max_items.to_string(),
    ];
    args.extend(extra_args.iter().cloned());
    args.push(url.to_string());
    // F20: owned subprocess via the shared runner (kill + reap
    // on timeout), same as `ytdlp_flat_listing`.
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = crate::dependency::run_tool(
        &exe,
        &arg_refs,
        std::time::Duration::from_secs(timeout_s.max(5)),
    )
    .map_err(|e| {
        if e.contains("timed out") {
            "yt-dlp listing timed out".to_string()
        } else {
            format!("yt-dlp: {e}")
        }
    })?;
    serde_json::from_str::<Value>(&out.stdout).map_err(|e| {
        let last = out.stderr.lines().last().unwrap_or("failed").trim();
        format!("yt-dlp: {last} ({e})")
    })
}

fn fetch_odysee(url: &str, max_items: usize) -> Result<(String, Vec<SourceItem>), String> {
    // Python `fetch_listing_items` (Audit 8 F14): the
    // anonymous attempt first; when it fails, one retry per
    // configured browser cookie source, in order. yt-dlp
    // reads the browser cookies itself — no cookie value is
    // logged or stored here.
    let mut attempts: Vec<Vec<String>> = vec![Vec::new()];
    for source in crate::ytdlp_recovery::cookie_sources() {
        attempts.push(vec!["--cookies-from-browser".to_string(), source]);
    }
    let mut last_err = String::new();
    for extra in &attempts {
        match ytdlp_info_document_attempt(url, max_items, 60, extra) {
            Ok(info) => {
                // Python `_extract_listing_from_info`: the feed title is the
                // listing's own title from the info document.
                let title = json_str(&info, &["title"]).unwrap_or_else(|| "Odysee".to_string());
                let entries = info
                    .get("entries")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let items: Vec<SourceItem> = entries
                    .iter()
                    .filter_map(|e| flat_entry_item(e, "odysee"))
                    .collect();
                return Ok((title, dedupe_items(items)));
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn fetch_youtube_search(
    query: &str,
    max_items: usize,
) -> Result<(String, Vec<SourceItem>), String> {
    let search_url = format!(
        "https://www.youtube.com/results?search_query={}&sp=CAI%3D",
        url_encode(query)
    );
    let entries = ytdlp_flat_listing(&search_url, max_items.min(100), 60, true)?;
    let mut items: Vec<SourceItem> = entries
        .iter()
        .filter_map(|e| flat_entry_item(e, "youtube"))
        .map(|mut it| {
            it.published = youtube_date_only(it.published);
            it
        })
        .collect();
    for it in items.iter_mut() {
        it.media_url = Some(it.url.clone());
        it.media_type = Some("video/youtube".to_string());
    }
    Ok((format!("YouTube: {query}"), dedupe_items(items)))
}

/// Python `_fetch_youtube_listing_items` dates are date-only
/// ("2026-11-01", from upload_date or the timestamp's UTC date),
/// never datetimes: truncate the RFC 3339 form `entry_date` builds.
fn youtube_date_only(published: Option<String>) -> Option<String> {
    published.map(|p| {
        if p.len() > 10 && p.as_bytes().get(10) == Some(&b'T') {
            p[..10].to_string()
        } else {
            p
        }
    })
}

/// Enumerate a channel's uploads playlist (Python
/// `fetch_youtube_channel_items`, the #107 RSS-failure fallback and
/// the #109 one-time history backfill source).
pub fn fetch_youtube_channel_items(
    channel_id: &str,
    max_items: usize,
) -> Result<(String, Vec<SourceItem>), String> {
    fetch_youtube_channel_items_with_timeout(channel_id, max_items, 120)
}

pub fn fetch_youtube_channel_items_with_timeout(
    channel_id: &str,
    max_items: usize,
    timeout_s: u64,
) -> Result<(String, Vec<SourceItem>), String> {
    let listing_url = format!(
        "https://www.youtube.com/playlist?list=UU{}",
        &channel_id[2..]
    );
    let entries = ytdlp_flat_listing(&listing_url, max_items, timeout_s, true)?;
    let mut title = String::new();
    let mut items = Vec::new();
    for e in &entries {
        if title.is_empty()
            && let Some(u) = json_str(e, &["playlist_uploader", "channel", "uploader"])
        {
            title = u;
        }
        if let Some(mut it) = flat_entry_item(e, "youtube") {
            it.published = youtube_date_only(it.published);
            // Stable id: the same one the native RSS feed uses, so
            // the two sources never duplicate each other (issue #107).
            if let Some(vid) = it.url.rsplit('=').next()
                && !vid.is_empty()
                && !vid.contains('/')
            {
                it.id = format!("yt:video:{vid}");
            }
            it.media_url = Some(it.url.clone());
            it.media_type = Some("video/youtube".to_string());
            items.push(it);
        }
    }
    if title.is_empty() {
        title = "YouTube".to_string();
    }
    Ok((title, dedupe_items(items)))
}

fn fetch_soundcloud(
    url: &str,
    kind: &str,
    max_items: usize,
) -> Result<(String, Vec<SourceItem>), String> {
    match soundcloud_via_api(url, kind, max_items) {
        Ok((title, items)) if !items.is_empty() => Ok((title, items)),
        _ => {
            let entries = ytdlp_flat_listing(url, max_items, 60, false)?;
            let items: Vec<SourceItem> = entries
                .iter()
                .filter_map(|e| flat_entry_item(e, "soundcloud"))
                .collect();
            Ok(("SoundCloud".to_string(), dedupe_items(items)))
        }
    }
}

/// Scrape a client_id from SoundCloud's own web assets (the same
/// approach yt-dlp uses; nothing is hardcoded).
pub(crate) fn soundcloud_client_id() -> Option<String> {
    static CACHE: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some(id) = guard.clone()
    {
        return Some(id);
    }
    let home = fetch_text("https://soundcloud.com/").ok()?;
    let re = Regex::new(r#"https://[a-z0-9\-]+\.sndcdn\.com/assets/[^"']+\.js"#).ok()?;
    let mut js_urls: Vec<String> = re
        .find_iter(&home)
        .map(|m| m.as_str().to_string())
        .collect();
    js_urls.reverse(); // the client_id lives in the later app bundles
    let id_re = Regex::new(r#"client_id\s*[:=]\s*"([A-Za-z0-9]{16,})""#).ok()?;
    let mut found = None;
    for js in js_urls.iter().take(6) {
        if let Ok(text) = fetch_text(js)
            && let Some(cap) = id_re.captures(&text)
        {
            found = Some(cap[1].to_string());
            break;
        }
    }
    if let Ok(mut guard) = cache.lock() {
        *guard = found.clone();
    }
    found
}

fn soundcloud_api_get(path_and_query: &str, client_id: &str) -> Option<Value> {
    let sep = if path_and_query.contains('?') {
        '&'
    } else {
        '?'
    };
    let url = format!("https://api-v2.soundcloud.com{path_and_query}{sep}client_id={client_id}");
    fetch_json(&url).ok()
}

fn soundcloud_track_item(t: &Value, fallback_author: &str) -> Option<SourceItem> {
    let url = json_str(t, &["permalink_url"])?;
    let title = json_str(t, &["title"]).unwrap_or_else(|| url.clone());
    let author = t
        .get("user")
        .and_then(|u| json_str(u, &["username"]))
        .or_else(|| {
            if fallback_author.is_empty() {
                None
            } else {
                Some(fallback_author.to_string())
            }
        });
    let published = json_str(t, &["created_at", "display_date"]);
    Some(SourceItem {
        id: url.clone(),
        url,
        title,
        author,
        published,
        content: String::new(),
        media_url: None,
        media_type: None,
    })
}

fn soundcloud_via_api(
    url: &str,
    kind: &str,
    max_items: usize,
) -> Result<(String, Vec<SourceItem>), String> {
    let client_id = soundcloud_client_id().ok_or("SoundCloud client_id unavailable")?;
    let resolved = soundcloud_api_get(&format!("/resolve?url={}", url_encode(url)), &client_id)
        .ok_or("SoundCloud resolve failed")?;
    if kind == "user" {
        let uid = resolved
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or("SoundCloud user id missing")?;
        let feed_title = json_str(&resolved, &["username"]).unwrap_or_default();
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        let mut page = soundcloud_api_get(
            &format!(
                "/users/{uid}/tracks?limit={}&linked_partitioning=1",
                max_items.min(200)
            ),
            &client_id,
        );
        let mut guard = 0;
        while let Some(p) = page {
            guard += 1;
            if guard > 8 {
                break;
            }
            if let Some(rows) = p.get("collection").and_then(|v| v.as_array()) {
                for t in rows {
                    if let Some(it) = soundcloud_track_item(t, &feed_title)
                        && seen.insert(it.url.clone())
                    {
                        items.push(it);
                    }
                    if items.len() >= max_items {
                        break;
                    }
                }
            }
            let next = json_str(&p, &["next_href"]);
            match next {
                Some(n) if items.len() < max_items => {
                    page = fetch_json(&n).ok();
                }
                _ => break,
            }
        }
        return Ok((feed_title, items));
    }
    // Playlist/set: the resolve payload carries the ordered track
    // list; leading tracks are hydrated, the rest are id stubs we
    // batch-fetch (Python's approach).
    let mut feed_title = json_str(&resolved, &["title"]).unwrap_or_default();
    let owner = resolved
        .get("user")
        .and_then(|u| json_str(u, &["username"]))
        .unwrap_or_default();
    if !feed_title.is_empty() && !owner.is_empty() {
        feed_title = format!("{feed_title} ({owner})");
    }
    let empty = Vec::new();
    let raw_tracks = resolved
        .get("tracks")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let mut ordered_ids: Vec<i64> = Vec::new();
    let mut hydrated: std::collections::HashMap<i64, Value> = std::collections::HashMap::new();
    for t in raw_tracks {
        let Some(tid) = t.get("id").and_then(|v| v.as_i64()) else {
            continue;
        };
        ordered_ids.push(tid);
        if t.get("title").is_some() || t.get("permalink_url").is_some() {
            hydrated.insert(tid, t.clone());
        }
        if ordered_ids.len() >= max_items {
            break;
        }
    }
    let stub_ids: Vec<i64> = ordered_ids
        .iter()
        .filter(|id| !hydrated.contains_key(id))
        .cloned()
        .collect();
    for chunk in stub_ids.chunks(50) {
        let ids = chunk
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        if let Some(data) = soundcloud_api_get(&format!("/tracks?ids={ids}"), &client_id)
            && let Some(rows) = data.as_array()
        {
            for t in rows {
                if let Some(tid) = t.get("id").and_then(|v| v.as_i64()) {
                    hydrated.insert(tid, t.clone());
                }
            }
        }
    }
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for tid in &ordered_ids {
        if let Some(t) = hydrated.get(tid)
            && let Some(it) = soundcloud_track_item(t, &owner)
            && seen.insert(it.url.clone())
        {
            items.push(it);
        }
        if items.len() >= max_items {
            break;
        }
    }
    Ok((feed_title, items))
}

fn fetch_mixcloud(key: &str, max_items: usize) -> Result<(String, Vec<SourceItem>), String> {
    let meta = fetch_json(&format!("https://api.mixcloud.com{key}"))?;
    let mut feed_title = json_str(&meta, &["name"]).unwrap_or_default();
    let owner = meta
        .get("user")
        .and_then(|u| json_str(u, &["name", "username"]))
        .unwrap_or_default();
    if key.contains("/playlists/") && !feed_title.is_empty() && !owner.is_empty() {
        feed_title = format!("{feed_title} ({owner})");
    }
    let mut items = Vec::new();
    let mut params = format!("limit={}", max_items.min(100));
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 6 || items.len() >= max_items {
            break;
        }
        let data = fetch_json(&format!(
            "https://api.mixcloud.com{key}cloudcasts/?{params}"
        ))?;
        let empty = Vec::new();
        let rows = data
            .get("data")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        if rows.is_empty() {
            break;
        }
        for it in rows {
            let Some(u) = json_str(it, &["url"]) else {
                continue;
            };
            let title = json_str(it, &["name"]).unwrap_or_else(|| u.clone());
            let author = it
                .get("user")
                .and_then(|x| json_str(x, &["name", "username"]))
                .or_else(|| {
                    if feed_title.is_empty() {
                        Some("Mixcloud".to_string())
                    } else {
                        Some(feed_title.clone())
                    }
                });
            items.push(SourceItem {
                id: u.clone(),
                url: u,
                title,
                author,
                published: json_str(it, &["created_time"]),
                content: String::new(),
                media_url: None,
                media_type: None,
            });
            if items.len() >= max_items {
                break;
            }
        }
        let next = data.get("paging").and_then(|p| json_str(p, &["next"]));
        match next {
            Some(n) if items.len() < max_items => {
                let offset = n.split('?').nth(1).and_then(|q| query_param(q, "offset"));
                params = match offset {
                    Some(o) => format!("limit={}&offset={o}", max_items.min(100)),
                    None => break,
                };
            }
            _ => break,
        }
    }
    Ok((feed_title, items))
}

// ---------------------------------------------------------------------------
// HTML-scraped sources (Rumble, Google Groups, Groups.io search)
// ---------------------------------------------------------------------------

fn span_inner<'a>(html: &'a str, sp: &ElemSpan) -> &'a str {
    &html[sp.inner_start..sp.inner_end]
}

fn span_open<'a>(html: &'a str, sp: &ElemSpan) -> &'a str {
    &html[sp.start..sp.inner_start]
}

/// Text of the first element (any tag) carrying class `cls`.
fn class_text(html: &str, cls: &str) -> Option<String> {
    let spans = find_elements(html, None, &|open: &str| has_class(open, cls));
    for sp in spans {
        let t = leaf_text(span_inner(html, &sp));
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

fn rumble_video_path(href: &str) -> bool {
    // Python _VIDEO_PATH_RE: ^/v(?!ideos)[^/]+\.html(?:\?.*)?$
    let path = href.split(['?', '#']).next().unwrap_or("");
    if !path.starts_with("/v") || path.starts_with("/videos") {
        return false;
    }
    path.ends_with(".html") && path.matches('/').count() == 1
}

pub fn parse_rumble_listing(html: &str, base: &str) -> Vec<SourceItem> {
    let mut items = Vec::new();
    let cards = find_elements(html, None, &|open: &str| {
        attr_value(open, "class")
            .map(|c| c.to_lowercase().contains("videostream"))
            .unwrap_or(false)
    });
    let push = |items: &mut Vec<SourceItem>, href: &str, title: String, frag: &str| {
        let full = if href.starts_with('/') {
            format!("{}{href}", base.trim_end_matches('/'))
        } else {
            href.to_string()
        };
        let url = if is_rumble_url(&full) {
            strip_query_fragment(&full)
        } else {
            full
        };
        items.push(SourceItem {
            id: url.clone(),
            url,
            title,
            author: None,
            published: find_time_datetime(frag),
            content: String::new(),
            media_url: None,
            media_type: None,
        });
    };
    if !cards.is_empty() {
        for card in &cards {
            let frag = span_inner(html, card);
            for sp in find_elements(frag, Some("a"), &|_| true) {
                if let Some(href) = attr_value(span_open(frag, &sp), "href")
                    && rumble_video_path(&href)
                {
                    let title = class_text(frag, "thumbnail__title")
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| "Rumble Video".to_string());
                    push(&mut items, &href, title, frag);
                    break;
                }
            }
        }
    } else {
        for sp in find_elements(html, Some("a"), &|_| true) {
            let open = span_open(html, &sp);
            if let Some(href) = attr_value(open, "href")
                && rumble_video_path(&href)
            {
                let text = leaf_text(span_inner(html, &sp));
                let title = if !text.is_empty() {
                    text
                } else if let Some(t) = attr_value(open, "title") {
                    t
                } else {
                    "Rumble Video".to_string()
                };
                let frag = span_inner(html, &sp).to_string();
                push(&mut items, &href, title, &frag);
            }
        }
    }
    dedupe_items(items)
}

fn find_time_datetime(fragment: &str) -> Option<String> {
    for sp in find_elements(fragment, Some("time"), &|_| true) {
        if let Some(dt) = attr_value(span_open(fragment, &sp), "datetime") {
            return Some(dt);
        }
    }
    None
}

/// Fetch a page's text plus its final (post-redirect) URL
/// (Python `_fetch_text`'s pair).
fn fetch_text_final(url: &str) -> Result<(String, String), String> {
    let mut resp = crate::http::agent()
        .get(url)
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    use ureq::ResponseExt as _;
    let final_url = resp.get_uri().to_string();
    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))?;
    Ok((String::from_utf8_lossy(&bytes).to_string(), final_url))
}

/// Python rumble `_looks_like_login_page`: a redirect to a
/// login route, or a sign-in document.
pub fn rumble_looks_like_login_page(html: &str, final_url: &str) -> bool {
    let fu = final_url.to_lowercase();
    if fu.contains("/login") || fu.contains("/signin") {
        return true;
    }
    let low = html.to_lowercase();
    if low.contains("<title>sign in") || low.contains("<title>login") {
        return true;
    }
    low.contains("name=\"password\"") && (low.contains("sign in") || low.contains("log in"))
}

/// The managed-jar Cookie header for `url`, when the app
/// has one (Audit 8 F14's authorized retry source): only
/// cookies the user already imported or the app harvested
/// under the Task 25 consent model are ever sent — no live
/// browser read happens here.
fn managed_cookie_header(url: &str) -> String {
    let cfg = crate::source_feeds::config();
    let Some(path) = cfg.source_path() else {
        return String::new();
    };
    crate::cookies::site_headers_for(path, url).0
}

fn fetch_rumble(url: &str, max_pages: usize) -> Result<(String, Vec<SourceItem>), String> {
    let mut items = Vec::new();
    let mut title = String::new();
    for page in 1..=max_pages.max(1) {
        let page_url = if page == 1 {
            url.to_string()
        } else if url.contains('?') {
            format!("{url}&page={page}")
        } else {
            format!("{url}?page={page}")
        };
        let html = if page == 1 {
            let (html, final_url) = fetch_text_final(&page_url)?;
            // Python `fetch_listing_items` (Audit 8 F14): a
            // login page means the anonymous fetch hit the
            // sign-in wall — retry once with the configured
            // cookies, when the app holds any for Rumble.
            if rumble_looks_like_login_page(&html, &final_url) {
                let header = managed_cookie_header(&page_url);
                if !header.is_empty() {
                    match crate::extract::fetch_page_bytes(&page_url, &header) {
                        Ok((bytes, _)) => String::from_utf8_lossy(&bytes).to_string(),
                        Err(_) => html,
                    }
                } else {
                    html
                }
            } else {
                html
            }
        } else {
            fetch_text(&page_url)?
        };
        if title.is_empty() {
            title = html_title(&html).unwrap_or_default();
        }
        let before = items.len();
        items.extend(parse_rumble_listing(&html, "https://rumble.com"));
        items = dedupe_items(items);
        if items.len() == before {
            break;
        }
    }
    if title.is_empty() {
        title = "Rumble".to_string();
    }
    Ok((title, items))
}

fn html_title(html: &str) -> Option<String> {
    for sp in find_elements(html, Some("title"), &|_| true) {
        let t = leaf_text(span_inner(html, &sp));
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

pub fn parse_google_group_items(html: &str, max_items: usize) -> Vec<SourceItem> {
    let mut items = Vec::new();
    let rows = find_elements(html, None, &|open: &str| {
        attr_value(open, "data-rowid").is_some()
    });
    for row in rows {
        let open = span_open(html, &row);
        let conv_id = attr_value(open, "data-rowid").unwrap_or_default();
        if conv_id.is_empty() {
            continue;
        }
        let frag = span_inner(html, &row);
        // Subject anchor: an <a> whose href contains /c/ and whose
        // subtree carries the subject class (or any text).
        let mut subject: Option<(String, String)> = None;
        for sp in find_elements(frag, Some("a"), &|_| true) {
            let href = attr_value(span_open(frag, &sp), "href").unwrap_or_default();
            if !href.contains("/c/") {
                continue;
            }
            let inner = span_inner(frag, &sp);
            let text = class_text(inner, "o1DPKc")
                .or_else(|| class_text(inner, "t17a0d"))
                .or_else(|| {
                    let t = leaf_text(inner);
                    if t.is_empty() { None } else { Some(t) }
                });
            if let Some(t) = text {
                subject = Some((href, t));
                break;
            }
        }
        let Some((href, title)) = subject else {
            continue;
        };
        let item_url = resolve_href("https://groups.google.com/", &href);
        // Authors: every .z0zUgf node, deduplicated.
        let mut authors: Vec<String> = Vec::new();
        for sp in find_elements(frag, None, &|open: &str| has_class(open, "z0zUgf")) {
            let t = leaf_text(span_inner(frag, &sp));
            if !t.is_empty() && !authors.contains(&t) {
                authors.push(t);
            }
        }
        items.push(SourceItem {
            id: conv_id,
            url: item_url,
            title,
            author: if authors.is_empty() {
                None
            } else {
                Some(authors.join(", "))
            },
            published: class_text(frag, "tRlaM"),
            content: class_text(frag, "WzoK").unwrap_or_default(),
            media_url: None,
            media_type: None,
        });
        if items.len() >= max_items {
            break;
        }
    }
    items
}

fn resolve_href(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Some(stripped) = href.strip_prefix("./") {
        return format!("{base}{stripped}");
    }
    if href.starts_with('/') {
        // Resolve against the site root, as Python does for Groups.
        let root = base.split('/').take(3).collect::<Vec<_>>().join("/");
        return format!("{root}{href}");
    }
    // Page-relative: resolve against the base's directory.
    let dir = base.rsplit('/').next().map(|_| {
        let idx = base.rfind('/').unwrap_or(base.len());
        &base[..=idx]
    });
    match dir {
        Some(d) => format!("{d}{href}"),
        None => format!("{base}{href}"),
    }
}

fn fetch_google_group(url: &str, max_items: usize) -> Result<(String, Vec<SourceItem>), String> {
    let canonical = google_group_subscription_url(url)
        .ok_or_else(|| "Not a Google Groups group address".to_string())?;
    let group = canonical.rsplit('/').next().unwrap_or("").to_string();
    let html = fetch_text(&format!("{canonical}?hl=en"))?;
    let text = leaf_text(&html).to_lowercase();
    if text.contains("you don't have permission to access this content")
        || text.contains("you don’t have permission to access this content")
    {
        return Err(
            "This Google Group requires sign-in. Import its browser cookies with Tools > Import Site Cookies, then refresh the feed."
                .to_string(),
        );
    }
    let title = html_title(&html)
        .map(|t| t.trim_end_matches("- Google Groups").trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or(group.clone());
    let items = parse_google_group_items(&html, max_items.clamp(1, 100));
    Ok((title, items))
}

fn canonical_topic_url(url: &str) -> String {
    if let Some((scheme, host, path, _)) = split_url(url) {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        // /g/<group>/topic/<slug>/<id> or /g/<group>/topic/<id>
        if parts.len() >= 4 && parts[0] == "g" && parts[2] == "topic" {
            let last = parts[parts.len() - 1];
            if last.chars().all(|c| c.is_ascii_digit()) {
                return format!("https://{host}/g/{}/topic/{last}", parts[1]);
            }
        }
        return format!("{scheme}://{host}{path}");
    }
    url.to_string()
}

pub fn parse_groups_io_search_page(html: &str, page_url: &str) -> Vec<SourceItem> {
    static TIME_RE: OnceLock<Regex> = OnceLock::new();
    let time_re =
        TIME_RE.get_or_init(|| Regex::new(r"DisplayShortTime\s*\(\s*([0-9]{10,})").unwrap());
    let mut items = Vec::new();
    // Result cells: <td id=...> elements.
    for cell in find_elements(html, Some("td"), &|open: &str| {
        attr_value(open, "id").is_some()
    }) {
        let frag = span_inner(html, &cell);
        // Subject anchor: first <a> whose href contains /topic/.
        let mut topic: Option<(String, String)> = None;
        for sp in find_elements(frag, Some("a"), &|_| true) {
            let href = attr_value(span_open(frag, &sp), "href").unwrap_or_default();
            if href.contains("/topic/") {
                topic = Some((href, leaf_text(span_inner(frag, &sp))));
                break;
            }
        }
        let Some((href, raw_title)) = topic else {
            continue;
        };
        let topic_url = canonical_topic_url(&resolve_href(page_url, &href));
        let title = if raw_title.trim().is_empty() {
            "Groups.io topic".to_string()
        } else {
            raw_title.trim().to_string()
        };
        // Groups.io uses Unix nanoseconds in DisplayShortTime(...).
        let published = time_re.captures(frag).and_then(|cap| {
            cap[1]
                .parse::<i64>()
                .ok()
                .map(|ns| epoch_rfc3339(ns / 1_000_000_000))
        });
        items.push(SourceItem {
            id: topic_url.clone(),
            url: topic_url,
            title,
            author: class_text(frag, "user-chip-name"),
            published,
            content: class_text(frag, "truncate-two-lines").unwrap_or_default(),
            media_url: None,
            media_type: None,
        });
    }
    items
}

fn groups_io_next_page(html: &str, page_url: &str) -> Option<String> {
    for sp in find_elements(html, Some("a"), &|_| true) {
        let label = leaf_text(span_inner(html, &sp)).to_lowercase();
        let rel = attr_value(span_open(html, &sp), "rel").unwrap_or_default();
        if (label == "next" || label == "next page" || label == "older" || rel == "next")
            && let Some(href) = attr_value(span_open(html, &sp), "href")
        {
            return Some(resolve_href(page_url, &href));
        }
    }
    None
}

fn fetch_groups_io_search(
    url: &str,
    max_items: usize,
) -> Result<(String, Vec<SourceItem>), String> {
    let (_, group, q) = groups_io_search(url).ok_or("Not a Groups.io group search URL")?;
    let limit = max_items.clamp(1, 1000);
    let mut items: Vec<SourceItem> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut next_url = url.to_string();
    let mut pages = 0;
    while !next_url.is_empty()
        && visited.insert(next_url.clone())
        && pages < 500
        && items.len() < limit
    {
        pages += 1;
        let html = fetch_text(&next_url)?;
        for it in parse_groups_io_search_page(&html, &next_url) {
            if !items.iter().any(|x| x.url == it.url) {
                items.push(it);
            }
            if items.len() >= limit {
                break;
            }
        }
        next_url = groups_io_next_page(&html, &next_url).unwrap_or_default();
    }
    let title = if q.is_empty() {
        format!("{group} search")
    } else {
        format!("{group} search: {q}")
    };
    Ok((title, items))
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn listing_limit(config: &crate::config::Config, kind: SourceKind, initial: bool) -> usize {
    let (key, default, maximum) = match kind {
        SourceKind::Odysee if initial => ("odysee_max_items_initial", 150, 500),
        SourceKind::Odysee => ("odysee_max_items_refresh", 60, 500),
        SourceKind::Rumble if initial => ("rumble_max_pages_initial", 3, 10),
        SourceKind::Rumble => ("rumble_max_pages_refresh", 1, 10),
        SourceKind::SoundCloud | SourceKind::Mixcloud if initial => {
            ("audio_listing_max_items_initial", 80, 300)
        }
        SourceKind::SoundCloud | SourceKind::Mixcloud => {
            ("audio_listing_max_items_refresh", 40, 300)
        }
        SourceKind::YoutubeSearch => ("youtube_search_max_items", 30, 100),
        SourceKind::GoogleGroup => ("google_groups_max_items", 30, 100),
        SourceKind::GroupsIoSearch => ("groups_io_search_max_items", 100, 1000),
    };
    let value = config.raw().get(key);
    value
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        })
        .unwrap_or(default)
        .clamp(1, maximum) as usize
}

/// Fetch a listing feed's items. `existing_count` selects Python's
/// initial vs refresh caps. Returns (feed title, items).
pub fn fetch_listing(
    url: &str,
    kind: SourceKind,
    feed_id: &str,
    existing_count: i64,
) -> Result<(String, Vec<SourceItem>), String> {
    let initial = existing_count == 0;
    let limit = listing_limit(&crate::source_feeds::config(), kind, initial);
    let (title, mut items) = match kind {
        SourceKind::Odysee => fetch_odysee(url, limit),
        SourceKind::Rumble => {
            let normalized = normalize_rumble_feed_url(url);
            fetch_rumble(&normalized, limit)
        }
        SourceKind::YoutubeSearch => {
            let q = youtube_search_query(url).ok_or("Not a YouTube search URL")?;
            fetch_youtube_search(&q, limit)
        }
        SourceKind::SoundCloud => {
            let kind = soundcloud_listing_kind(url);
            fetch_soundcloud(url, kind, limit)
        }
        SourceKind::Mixcloud => {
            let (_, key) = mixcloud_listing(url).ok_or("Not a Mixcloud listing URL")?;
            fetch_mixcloud(&key, limit)
        }
        SourceKind::GoogleGroup => fetch_google_group(url, limit),
        SourceKind::GroupsIoSearch => fetch_groups_io_search(url, limit),
    }?;
    // Per-source article-id shaping (Python's store paths).
    match kind {
        SourceKind::YoutubeSearch => {
            for it in items.iter_mut() {
                it.id = uuid5_url(&format!("blindrss:youtube-search:{feed_id}:{}", it.url));
            }
        }
        SourceKind::GroupsIoSearch => {
            for it in items.iter_mut() {
                it.id = uuid5_url(&format!("blindrss:groups-io-search:{feed_id}:{}", it.url));
            }
        }
        _ => {}
    }
    Ok((title, items))
}

/// Convert listing items into parsed-feed entries for `store_parsed`.
pub fn items_to_entries(items: Vec<SourceItem>) -> Vec<ParsedEntry> {
    items
        .into_iter()
        .map(|it| ParsedEntry {
            id: it.id,
            title: it.title,
            url: it.url,
            content_text: it.content.clone(),
            summary: it.content,
            date: listing_date(it.published),
            author: it.author,
            media_url: it.media_url,
            media_type: it.media_type,
            tags: Vec::new(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// NPR / Sky media attach (Python's generic-parse-path behaviour)
// ---------------------------------------------------------------------------

pub fn is_npr_url(url: &str) -> bool {
    url.to_lowercase().contains("npr.org")
}

pub fn npr_story_id(url: &str) -> Option<String> {
    static ID_RE: OnceLock<Regex> = OnceLock::new();
    let id_re = ID_RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:[a-z]{1,3}-[a-z][0-9]+-[0-9]+(?:-[a-z0-9]+)*|[0-9]{6,})$").unwrap()
    });
    if !is_npr_url(url) {
        return None;
    }
    let (_, host, path, query) = split_url(url)?;
    if host == "text.npr.org" {
        return None;
    }
    let segments: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    // Dated paths: the ID is the segment straight after YYYY/MM/DD.
    for (i, seg) in segments.iter().enumerate() {
        if seg.len() == 4 && seg.chars().all(|c| c.is_ascii_digit()) && i + 3 < segments.len() {
            let candidate = segments[i + 3];
            if id_re.is_match(candidate) {
                return Some(candidate.to_string());
            }
        }
    }
    if segments.len() == 2
        && segments[0].eq_ignore_ascii_case("transcripts")
        && id_re.is_match(segments[1])
    {
        return Some(segments[1].to_string());
    }
    if segments.len() == 1 && id_re.is_match(segments[0]) {
        return Some(segments[0].to_string());
    }
    if let Some(id) = query_param(&query, "storyId")
        && id_re.is_match(&id)
    {
        return Some(id);
    }
    None
}

/// The on-demand MP3 for an NPR story (Python `extract_npr_audio`):
/// the syndication embed player first (TollBit does not meter it),
/// then the story page's own audio markers.
pub fn npr_audio(url: &str) -> Option<(String, String)> {
    static MP3_RE: OnceLock<Regex> = OnceLock::new();
    let mp3_re = MP3_RE.get_or_init(|| {
        Regex::new(
            r#"https?:(?:\\?/){2}ondemand\.npr\.org(?:[^\s"'<>]|\\/)+?\.mp3(?:[^\s"'<>]|\\/)*"#,
        )
        .unwrap()
    });
    let unescape_mp3 = |raw: &str| -> String { raw.replace("\\/", "/").replace("&amp;", "&") };
    if let Some(story_id) = npr_story_id(url) {
        let embed_url = format!("https://www.npr.org/player/embed/{story_id}/{story_id}");
        if let Ok(html) = fetch_text(&embed_url)
            && let Some(m) = mp3_re.find(&html)
        {
            return Some((unescape_mp3(m.as_str()), "audio/mpeg".to_string()));
        }
    }
    let html = fetch_text(url).ok()?;
    // data-audio JSON (new NPR CMS / Brightspot).
    for sp in find_elements(&html, None, &|open: &str| {
        attr_value(open, "data-audio").is_some()
    }) {
        if let Some(raw) = attr_value(span_open(&html, &sp), "data-audio")
            && let Ok(v) = serde_json::from_str::<Value>(&raw.replace("&quot;", "\""))
            && let Some(u) = json_str(&v, &["audioUrl"])
        {
            return Some((u.replace("\\/", "/"), "audio/mpeg".to_string()));
        }
    }
    // a.audio-module-listen download link.
    for sp in find_elements(&html, Some("a"), &|open: &str| {
        has_class(open, "audio-module-listen")
    }) {
        if let Some(href) = attr_value(span_open(&html, &sp), "href")
            && href.contains(".mp3")
        {
            return Some((href, "audio/mpeg".to_string()));
        }
    }
    if let Some(m) = mp3_re.find(&html) {
        return Some((unescape_mp3(m.as_str()), "audio/mpeg".to_string()));
    }
    None
}

/// Hosted-provider NPR attach (Python's HostedArticleMediaMixin):
/// hosted readers only relay the feed's enclosures, and NPR program
/// feeds carry none, so the audio is resolved client-side and
/// persisted. A recorded miss is retried after 6 hours (a story's
/// audio can be posted after the story). At most 8 uncached
/// resolutions run per call — Python bounds the same work with a
/// 4-worker pool and lets the rest land on the next load.
pub fn attach_hosted_npr_media(
    db: &crate::db::Db,
    provider: &str,
    articles: &mut [crate::db::Article],
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut resolved = 0;
    for article in articles.iter_mut() {
        if article.media_url.is_some() || !is_npr_url(&article.url) {
            continue;
        }
        let key = format!("{provider}:{}", article.id);
        if let Ok(Some((mu, mt, checked))) = db.hosted_article_media(&key) {
            if mu.is_some() {
                article.media_url = mu;
                article.media_type = mt;
                continue;
            }
            if now - checked < 6 * 3600 {
                continue; // recorded miss, not yet due for a retry
            }
        }
        if resolved >= 8 {
            continue;
        }
        resolved += 1;
        let found = npr_audio(&article.url);
        if let Some((mu, mt)) = &found {
            article.media_url = Some(mu.clone());
            article.media_type = Some(mt.clone());
            let _ = db.update_article_media(&article.id, mu, Some(mt));
        }
        let _ = db.set_hosted_article_media(
            &key,
            found.as_ref().map(|f| f.0.as_str()),
            found.as_ref().map(|f| f.1.as_str()),
            now,
        );
    }
}

pub fn is_sky_news_url(url: &str) -> bool {
    match split_url(url) {
        Some((_, host, _, _)) => host == "news.sky.com",
        None => false,
    }
}

fn podfollow_url(html: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r#"(?i)https?://(?:www\.)?podfollow\.com/[^\s"'<>]+"#).unwrap());
    let decoded = html.replace("&amp;", "&");
    re.find(&decoded).map(|m| {
        m.as_str()
            .trim_end_matches(['.', ',', ')', ';', ']'])
            .to_string()
    })
}

fn normalize_title_key(title: &str) -> String {
    title
        .replace(['’', '‘'], "'")
        .replace(['“', '”'], "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Resolve a Sky podcast article to its matching RSS audio enclosure
/// (Python `sky.extract_podcast_audio`): follow the article's explicit
/// Podfollow series link and attach only the enclosure whose
/// normalized episode title exactly matches the article's.
pub fn sky_podcast_audio(
    article_url: &str,
    title: &str,
    html_hint: &str,
) -> Option<(String, String)> {
    if !is_sky_news_url(article_url) {
        return None;
    }
    let mut page_html = html_hint.to_string();
    let mut podfollow = podfollow_url(&page_html);
    if podfollow.is_none() {
        // Direct fetch, then Sky's Google Translate proxy when gated.
        if let Ok(html) = fetch_text(article_url)
            && podfollow_url(&html).is_some()
        {
            page_html = html;
            podfollow = podfollow_url(&page_html);
        }
        if podfollow.is_none()
            && let Some((_, _, path, query)) = split_url(article_url)
        {
            let q = if query.is_empty() {
                "_x_tr_sl=auto&_x_tr_tl=en&_x_tr_hl=en".to_string()
            } else {
                format!("{query}&_x_tr_sl=auto&_x_tr_tl=en&_x_tr_hl=en")
            };
            let proxy_url = format!("https://news-sky-com.translate.goog{path}?{q}");
            if let Ok(html) = fetch_text(&proxy_url) {
                page_html = html;
                podfollow = podfollow_url(&page_html);
            }
        }
    }
    let podfollow = podfollow?;
    let page_title = if title.trim().is_empty() {
        let mut t = String::new();
        for sp in find_elements(&page_html, Some("h1"), &|_| true) {
            t = leaf_text(span_inner(&page_html, &sp));
            if !t.is_empty() {
                break;
            }
        }
        t
    } else {
        title.to_string()
    };
    let target = normalize_title_key(&page_title);
    if target.is_empty() {
        return None;
    }
    let podfollow_html = fetch_text(&podfollow).ok()?;
    // The series RSS feed: JSON-LD webFeed, else a pcast:// link.
    let mut feed_url: Option<String> = None;
    for sp in find_elements(&podfollow_html, Some("script"), &|open: &str| {
        attr_value(open, "type").as_deref() == Some("application/ld+json")
    }) {
        if let Ok(v) = serde_json::from_str::<Value>(span_inner(&podfollow_html, &sp)) {
            let objs: Vec<&Value> = match &v {
                Value::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            for o in objs {
                if let Some(f) = json_str(o, &["webFeed"]) {
                    feed_url = Some(f);
                    break;
                }
            }
        }
        if feed_url.is_some() {
            break;
        }
    }
    if feed_url.is_none() {
        for sp in find_elements(&podfollow_html, Some("a"), &|_| true) {
            if let Some(href) = attr_value(span_open(&podfollow_html, &sp), "href")
                && href.to_lowercase().starts_with("pcast://")
            {
                feed_url = Some(format!("https://{}", &href[8..]));
                break;
            }
        }
    }
    let feed_url = feed_url?;
    let parsed = crate::feeds::fetch_and_parse(&feed_url).ok()?;
    for e in &parsed.entries {
        if normalize_title_key(&e.title) == target
            && let Some(mu) = &e.media_url
        {
            let mt = e.media_type.clone();
            let is_audio = mt
                .as_deref()
                .map(|t| t.to_lowercase().starts_with("audio/"))
                .unwrap_or(false)
                || mu
                    .to_lowercase()
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .ends_with(".mp3");
            if is_audio {
                return Some((mu.clone(), mt.unwrap_or_else(|| "audio/mpeg".to_string())));
            }
        }
    }
    None
}

#[cfg(test)]
mod listing_limit_tests {
    use super::*;

    #[test]
    fn python_listing_caps_use_shared_audio_keys_and_clamp_configured_values() {
        for (kind, key, initial, default, maximum) in [
            (
                SourceKind::Odysee,
                "odysee_max_items_initial",
                true,
                150,
                500,
            ),
            (
                SourceKind::Odysee,
                "odysee_max_items_refresh",
                false,
                60,
                500,
            ),
            (SourceKind::Rumble, "rumble_max_pages_initial", true, 3, 10),
            (SourceKind::Rumble, "rumble_max_pages_refresh", false, 1, 10),
            (
                SourceKind::SoundCloud,
                "audio_listing_max_items_initial",
                true,
                80,
                300,
            ),
            (
                SourceKind::SoundCloud,
                "audio_listing_max_items_refresh",
                false,
                40,
                300,
            ),
            (
                SourceKind::Mixcloud,
                "audio_listing_max_items_initial",
                true,
                80,
                300,
            ),
            (
                SourceKind::Mixcloud,
                "audio_listing_max_items_refresh",
                false,
                40,
                300,
            ),
            (
                SourceKind::YoutubeSearch,
                "youtube_search_max_items",
                false,
                30,
                100,
            ),
            (
                SourceKind::GoogleGroup,
                "google_groups_max_items",
                false,
                30,
                100,
            ),
            (
                SourceKind::GroupsIoSearch,
                "groups_io_search_max_items",
                false,
                100,
                1000,
            ),
        ] {
            let mut config = crate::config::Config::default();
            assert_eq!(listing_limit(&config, kind, initial), default);
            for (value, expected) in [
                (serde_json::json!(0), 1),
                (serde_json::json!(-10), 1),
                (serde_json::json!(100_000), maximum),
                (serde_json::json!(" 7 "), 7),
                (serde_json::json!("invalid"), default),
                (serde_json::Value::Null, default),
            ] {
                config.raw_mut()[key] = value;
                assert_eq!(listing_limit(&config, kind, initial), expected, "{key}");
            }
        }
    }
}
