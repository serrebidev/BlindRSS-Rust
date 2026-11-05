//! Structured YouTube full text (Batch 35b): ports Python
//! `core/youtube_fulltext.py`. A watch page progressively hydrates
//! its description and comments, collapses reply branches, and
//! exposes subtitles separately, so the complete accessible
//! document is reconstructed through yt-dlp instead of scraping:
//! Description, Chapters, Subtitles (cue text only — timecodes are
//! noise to a screen reader), and Comments as parent/reply trees.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const YOUTUBE_HOSTS: [&str; 6] = [
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "www.youtu.be",
];

/// Strict YouTube video id for a watch/shorts/embed/live/youtu.be
/// URL, or None (Python `video_id_from_url`).
pub fn video_id_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (_, rest) = url.split_once("://")?;
    let (host, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.to_lowercase();
    if !YOUTUBE_HOSTS.contains(&host.as_str()) {
        return None;
    }
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, q),
        None => (tail, ""),
    };
    let candidate = if host.ends_with("youtu.be") {
        path.trim_matches('/')
            .split('/')
            .next()
            .unwrap_or("")
            .to_string()
    } else if path == "/watch" {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("v="))
            .unwrap_or("")
            .to_string()
    } else {
        let mut found = String::new();
        for prefix in ["/shorts/", "/embed/", "/live/"] {
            if let Some(rest) = path.strip_prefix(prefix) {
                found = rest.split(['/', '?', '#']).next().unwrap_or("").to_string();
                break;
            }
        }
        found
    };
    let ok = (6..=20).contains(&candidate.len())
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Some(candidate) } else { None }
}

pub fn is_youtube_video_url(url: &str) -> bool {
    video_id_from_url(url).is_some()
}

fn ui_language() -> String {
    if let Ok(v) = std::env::var("BLINDRSS_UI_LANGUAGE") {
        let v = v.trim().to_lowercase();
        if !v.is_empty() && v != "auto" {
            return v;
        }
    }
    for var in ["LANG", "LANGUAGE", "LC_ALL"] {
        if let Ok(v) = std::env::var(var) {
            let tag = v.split(['_', '.', '@']).next().unwrap_or("").to_lowercase();
            if !tag.is_empty() {
                return tag;
            }
        }
    }
    "en".to_string()
}

/// Subtitle language rank: exact tag, Chinese script, bare base, other variant.
pub(crate) fn language_match(code: &str, wanted: &str) -> Option<u8> {
    let code = code.to_ascii_lowercase().replace('_', "-");
    let code = code.strip_suffix("-orig").unwrap_or(&code);
    let wanted = wanted.to_ascii_lowercase().replace('_', "-");
    if code.is_empty() || wanted.is_empty() {
        return None;
    }
    if code == wanted {
        return Some(0);
    }
    let script = match wanted.as_str() {
        "zh-cn" | "zh-sg" => "zh-hans",
        "zh-tw" | "zh-hk" | "zh-mo" => "zh-hant",
        _ => "",
    };
    if code == script {
        return Some(1);
    }
    let base = wanted.split('-').next().unwrap_or("");
    if code == base {
        Some(2)
    } else if code.split('-').next() == Some(base) {
        Some(3)
    } else {
        None
    }
}

fn json_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Seconds budget for one comment-complete CLI extraction
/// (Python `_CLI_TIMEOUT_WITH_COMMENTS_S`: comment-heavy
/// videos can take minutes; a hung CLI must still end).
const CLI_TIMEOUT_WITH_COMMENTS_S: u64 = 900;

/// Run one yt-dlp `-J` extraction with comments (Python
/// `_extract_info_via_cli` with include_comments=True) through
/// the owned-subprocess policy (`dependency::run_tool` — the
/// full-output variant: a comment-complete dump legitimately
/// exceeds the shared cap, and Python caps nothing here).
/// Returns the info dict and whether a SUCCESSFUL run's
/// stderr reported incomplete comment data — the signal
/// Python's retry keys on.
fn run_info_once(exe: &Path, extract_url: &str) -> Result<(Value, bool), String> {
    let extractor_args = format!(
        "{};max_comments=all,all,all,all,all",
        crate::ytdlp_recovery::youtube_player_client_arg(
            &crate::ytdlp_recovery::YOUTUBE_PLAYER_CLIENTS,
            None,
        )
    );
    let mut args: Vec<String> = crate::media::ytdlp_env_args();
    args.extend(
        [
            "-J",
            "--ignore-config",
            "--no-playlist",
            "--skip-download",
            "--socket-timeout",
            "20",
            "--retries",
            "2",
            "--extractor-retries",
            "6",
            "--extractor-args",
            &extractor_args,
            "--write-comments",
            "--",
            extract_url,
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    let arg_refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
    let run = crate::dependency::run_tool_full_output(
        exe,
        &arg_refs,
        std::time::Duration::from_secs(CLI_TIMEOUT_WITH_COMMENTS_S),
    )
    .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    if !run.success {
        let last = run
            .stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("yt-dlp failed")
            .trim()
            .to_string();
        return Err(last);
    }
    let incomplete = run
        .stderr
        .to_lowercase()
        .contains("incomplete data received");
    let parsed: Value = serde_json::from_str(&run.stdout)
        .map_err(|e| format!("yt-dlp output parse failed: {e}"))?;
    let info = if parsed.is_object() {
        parsed
    } else {
        Value::Object(serde_json::Map::new())
    };
    Ok((info, incomplete))
}

/// Canonical JSON with recursively sorted object keys — the
/// dedupe key for a comment carrying no id (Python's
/// `json.dumps(item, sort_keys=True)` fallback).
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Merge two extractions' comment lists into the retry's
/// info (Python `extract_video_info`): the first extraction's
/// comments in order, then the retry's, deduped by comment
/// id. The retry's info wins as the document source; only
/// its comment list is replaced by the union.
fn union_comments(first: &Value, retry: &mut Value) {
    let mut merged: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for source in [first, &*retry] {
        if let Some(items) = source.get("comments").and_then(|v| v.as_array()) {
            for item in items {
                if !item.is_object() {
                    continue;
                }
                let id = json_str(item, "id");
                let key = if id.is_empty() {
                    canonical_json(item)
                } else {
                    id
                };
                if seen.insert(key) {
                    merged.push(item.clone());
                }
            }
        }
    }
    retry["comments"] = Value::Array(merged);
}

/// The ONE YouTube info pipeline (Python `extract_video_info`
/// with comments), shared by the plain reader, the rich
/// reader and the main-window full-text path. A run whose
/// stderr reports incomplete comment data is retried once
/// and the two comment sets are unioned by id; when the
/// retry still reports incomplete data (or fails outright),
/// the info carries `_blindrss_comments_incomplete` so the
/// document can say so honestly instead of silently
/// presenting a partial discussion as complete.
pub fn extract_video_info(url: &str) -> Result<Value, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    extract_video_info_with(&exe, url)
}

/// `extract_video_info` against an explicit executable (the
/// seam the regression tests drive with a fake yt-dlp).
pub fn extract_video_info_with(exe: &Path, url: &str) -> Result<Value, String> {
    if !is_youtube_video_url(url) {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let extract_url = crate::ytdlp_recovery::normalize_ytdlp_single_item_url(url);
    let (info, incomplete) = run_info_once(exe, &extract_url)?;
    if !incomplete {
        return Ok(info);
    }
    let mut info = info;
    let mut incomplete = true;
    if let Ok((mut retry_info, retry_incomplete)) = run_info_once(exe, &extract_url) {
        incomplete = retry_incomplete;
        union_comments(&info, &mut retry_info);
        info = retry_info;
    }
    if incomplete {
        info["_blindrss_comments_incomplete"] = Value::Bool(true);
    }
    Ok(info)
}

fn timestamp(secs: f64) -> String {
    let total = secs.max(0.0) as i64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
    pub href: String,
}

/// Chapters from the info dict, else parsed from description
/// timestamp lines (Python `chapters_from_info` + the description
/// grammar yt-dlp itself uses).
pub fn chapters_from_info(info: &Value, url: &str) -> Vec<Chapter> {
    let video_id = {
        let id = json_str(info, "id");
        if id.is_empty() {
            video_id_from_url(url).unwrap_or_default()
        } else {
            id
        }
    };
    let base = if video_id.is_empty() {
        url.to_string()
    } else {
        format!("https://www.youtube.com/watch?v={video_id}")
    };
    let sep = if base.contains('?') { "&" } else { "?" };
    let mut out = Vec::new();
    if let Some(chaps) = info.get("chapters").and_then(|v| v.as_array()) {
        for c in chaps {
            let start = c
                .get("start_time")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                .max(0.0);
            let mut title = json_str(c, "title");
            if title.is_empty() {
                title = "Chapter".to_string();
            }
            out.push(Chapter {
                start,
                title,
                href: format!("{base}{sep}t={}s", start as i64),
            });
        }
    }
    if !out.is_empty() {
        return out;
    }
    // Description grammar: lines like "0:00 Intro" / "1:02:03 Middle".
    static LINE_RE: OnceLock<Regex> = OnceLock::new();
    let line_re = LINE_RE
        .get_or_init(|| Regex::new(r"^\s*((?:\d+:)?\d{1,2}:\d{2})\b\W*\s(.+?)\s*$").unwrap());
    let description = json_str(info, "description");
    let mut parsed: Vec<(f64, String)> = Vec::new();
    for line in description.lines() {
        if let Some(cap) = line_re.captures(line)
            && let Some(secs) = parse_clock(&cap[1])
        {
            parsed.push((secs, cap[2].trim().to_string()));
        }
    }
    // yt-dlp requires at least 3 timestamped lines to call them chapters.
    if parsed.len() >= 3 {
        for (start, title) in parsed {
            out.push(Chapter {
                start,
                title,
                href: format!("{base}{sep}t={}s", start as i64),
            });
        }
    }
    out
}

fn parse_clock(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.split(':').collect();
    match parts.len() {
        2 => {
            let m: f64 = parts[0].parse().ok()?;
            let s: f64 = parts[1].parse().ok()?;
            Some(m * 60.0 + s)
        }
        3 => {
            let h: f64 = parts[0].parse().ok()?;
            let m: f64 = parts[1].parse().ok()?;
            let s: f64 = parts[2].parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        }
        _ => None,
    }
}

fn clean_subtitle_text(value: &str) -> String {
    static WS_RE: OnceLock<Regex> = OnceLock::new();
    let ws_re = WS_RE.get_or_init(|| Regex::new(r"\s+").unwrap());
    let unescaped = value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let flat = ws_re.replace_all(&unescaped, " ").trim().to_string();
    // Strip leading speaker chevrons (">>"): NVDA announces each one.
    let mut out = flat.as_str();
    loop {
        let trimmed = out.trim_start();
        if let Some(rest) = trimmed.strip_prefix('>') {
            out = rest;
        } else {
            out = trimmed;
            break;
        }
    }
    out.trim().to_string()
}

fn transcript_from_json3(payload: &Value) -> Vec<(f64, String)> {
    let mut entries: Vec<(f64, String)> = Vec::new();
    let empty = Vec::new();
    let events = payload
        .get("events")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    for event in events {
        let Some(segs) = event.get("segs").and_then(|v| v.as_array()) else {
            continue;
        };
        let raw: String = segs
            .iter()
            .filter_map(|s| s.get("utf8").and_then(|v| v.as_str()))
            .collect();
        let text = clean_subtitle_text(&raw);
        if text.is_empty() {
            continue;
        }
        if entries.last().map(|e| &e.1) == Some(&text) {
            continue;
        }
        let start = event
            .get("tStartMs")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            / 1000.0;
        entries.push((start, text));
    }
    entries
}

fn transcript_from_vtt(body: &str) -> Vec<(f64, String)> {
    static CUE_RE: OnceLock<Regex> = OnceLock::new();
    let cue_re =
        CUE_RE.get_or_init(|| Regex::new(r"(?:(\d+):)?(\d{2}):(\d{2})[.,](\d{3})\s+-->").unwrap());
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let mut entries: Vec<(f64, String)> = Vec::new();
    for block in body.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        let cue_idx = lines.iter().position(|l| cue_re.is_match(l));
        let Some(idx) = cue_idx else {
            continue;
        };
        let Some(cap) = cue_re.captures(lines[idx]) else {
            continue;
        };
        let h: f64 = cap
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0.0);
        let m: f64 = cap[2].parse().unwrap_or(0.0);
        let s: f64 = cap[3].parse().unwrap_or(0.0);
        let ms: f64 = cap[4].parse().unwrap_or(0.0);
        let start = h * 3600.0 + m * 60.0 + s + ms / 1000.0;
        let joined = lines[idx + 1..].join(" ");
        let text = clean_subtitle_text(&tag_re.replace_all(&joined, ""));
        if !text.is_empty() && entries.last().map(|e| &e.1) != Some(&text) {
            entries.push((start, text));
        }
    }
    entries
}

/// The track's machine-translation target language, if the track
/// URL is a translated one (`tlang=` in the timedtext URL).
fn track_translation(track: &Value) -> String {
    let url = json_str(track, "url");
    let Ok(parsed) = url::Url::parse(&url) else {
        return String::new();
    };
    parsed
        .query_pairs()
        .find(|(key, _)| key == "tlang")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

struct SubCandidate {
    rank: (i32, u8),
    kind_rank: i32,
    language: String,
    track: Value,
}

/// Whether a format entry names a real subtitle file: a
/// format carrying a truthy "protocol" is not one (a
/// livestream replay's "live_chat" entry points at the watch
/// page — Python's `not item.get("protocol")` test).
fn track_has_file(track: &Value) -> bool {
    if json_str(track, "url").is_empty() {
        return false;
    }
    match track.get("protocol") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Bool(b)) => !*b,
        Some(_) => false,
    }
}

/// Subtitle tracks best-first (Python `_subtitle_candidates`): UI
/// language, English, machine translation into the UI language, the
/// video's original language, other authored tracks; authored
/// before automatic within each step.
fn subtitle_candidates(info: &Value, ui: &str) -> Vec<SubCandidate> {
    let authored = info.get("subtitles");
    let automatic = info.get("automatic_captions");
    let mut original = String::new();
    if let Some(auto) = automatic.and_then(|v| v.as_object()) {
        for key in auto.keys() {
            if let Some(prefix) = key.strip_suffix("-orig") {
                original = prefix.to_string();
                break;
            }
        }
    }
    let mut out: Vec<SubCandidate> = Vec::new();
    for (tracks, kind_rank) in [(authored, 0), (automatic, 1)] {
        let Some(tracks) = tracks.and_then(|v| v.as_object()) else {
            continue;
        };
        for (language, formats) in tracks {
            let Some(formats) = formats.as_array() else {
                continue;
            };
            let mut usable: Vec<&Value> = formats.iter().filter(|f| track_has_file(f)).collect();
            usable.sort_by_key(|f| match json_str(f, "ext").as_str() {
                "json3" => 0,
                "vtt" => 1,
                _ => 2,
            });
            let Some(track) = usable.first() else {
                continue;
            };
            let translation = track_translation(track);
            let rank = if !translation.is_empty() {
                match language_match(&translation, ui) {
                    Some(m) => (2, m),
                    None => continue, // never offer other-language translations
                }
            } else if let Some(m) = language_match(language, ui) {
                (0, m)
            } else if let Some(m) = language_match(language, "en") {
                (1, m)
            } else if language_match(language, &original).is_some() {
                (3, 0)
            } else {
                (4, 0)
            };
            out.push(SubCandidate {
                rank,
                kind_rank,
                language: language
                    .strip_suffix("-orig")
                    .unwrap_or(language)
                    .to_string(),
                track: (*track).clone(),
            });
        }
    }
    out.sort_by_key(|a| (a.rank, a.kind_rank));
    out
}

/// The info's extracted HTTP headers (yt-dlp's
/// `http_headers`), forwarded to subtitle requests exactly
/// as Python forwards them — some tracks refuse requests
/// that arrive without them.
fn info_http_headers(info: &Value) -> Vec<(String, String)> {
    let Some(map) = info.get("http_headers").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(name, value)| value.as_str().map(|v| (name.clone(), v.to_string())))
        .collect()
}

/// GET one subtitle track: Ok(body) on success,
/// Err(Some(status)) on an HTTP error status, Err(None) on a
/// transport or read failure. Only the status is ever
/// surfaced from a failure — the error text carries the
/// signed track URL, which is never logged or returned.
fn fetch_subtitle_track(url: &str, headers: &[(String, String)]) -> Result<String, Option<u16>> {
    let mut req = crate::http::agent().get(url);
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let mut resp = req.call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => Some(code),
        _ => None,
    })?;
    resp.body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_string()
        .map_err(|_| None)
}

/// (language, cue texts) for the best subtitle track — THE
/// shared subtitle downloader (Python `transcript_from_info`),
/// used by every reader path: candidates are deduplicated
/// by URL, at most two tracks are attempted, the extracted
/// headers ride every request, and a 429 on an untranslated
/// track stops the walk (another request would be
/// rate-limited too) while a translated track's 429 falls
/// through to the next candidate.
fn transcript_from_info(info: &Value, ui: &str) -> (String, Vec<String>) {
    let headers = info_http_headers(info);
    let mut first_language = String::new();
    let mut tried: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cand in subtitle_candidates(info, ui) {
        let track_url = json_str(&cand.track, "url");
        if track_url.is_empty() || tried.contains(&track_url) {
            continue;
        }
        if tried.len() >= 2 {
            break;
        }
        tried.insert(track_url.clone());
        if first_language.is_empty() {
            first_language = cand.language.clone();
        }
        let translation = track_translation(&cand.track);
        let body = match fetch_subtitle_track(&track_url, &headers) {
            Ok(body) => body,
            Err(status) => {
                if status == Some(429) && translation.is_empty() {
                    break;
                }
                continue;
            }
        };
        let entries = if json_str(&cand.track, "ext") == "json3" {
            match serde_json::from_str::<Value>(&body) {
                Ok(payload) => transcript_from_json3(&payload),
                // Python's parse lives inside the same try as
                // the download: a body that does not parse is
                // a failed attempt, not an empty transcript.
                Err(_) => continue,
            }
        } else {
            transcript_from_vtt(&body)
        };
        return (
            cand.language.clone(),
            entries.into_iter().map(|e| e.1).collect(),
        );
    }
    (first_language, Vec::new())
}

fn comment_text(comment: &Value) -> String {
    static WS_RE: OnceLock<Regex> = OnceLock::new();
    let ws_re = WS_RE.get_or_init(|| Regex::new(r"\s+").unwrap());
    let raw = {
        let t = json_str(comment, "text");
        if t.is_empty() {
            json_str(comment, "html")
        } else {
            t
        }
    };
    let unescaped = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    ws_re.replace_all(&unescaped, " ").trim().to_string()
}

/// Format the flat yt-dlp comment list as parent/reply subtrees
/// (Python `format_comments`).
pub fn format_comments(comments: &Value) -> Vec<String> {
    let empty = Vec::new();
    let items = comments.as_array().unwrap_or(&empty);
    let normalized: Vec<&Value> = items
        .iter()
        .filter(|c| !comment_text(c).is_empty())
        .collect();
    let known_ids: std::collections::HashSet<String> =
        normalized.iter().map(|c| json_str(c, "id")).collect();
    let mut by_parent: HashMap<String, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, item) in normalized.iter().enumerate() {
        let parent = json_str(item, "parent");
        if parent.is_empty() || parent == "root" || parent == "None" || !known_ids.contains(&parent)
        {
            roots.push(index);
        } else {
            by_parent.entry(parent).or_default().push(index);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    fn append_tree(
        normalized: &[&Value],
        by_parent: &HashMap<String, Vec<usize>>,
        index: usize,
        depth: usize,
        out: &mut Vec<String>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        let item = normalized[index];
        let mut id = json_str(item, "id");
        if id.is_empty() {
            id = format!("index:{index}");
        }
        if !seen.insert(id.clone()) {
            return;
        }
        let mut author = json_str(item, "author");
        if author.is_empty() {
            author = "Unknown author".to_string();
        }
        let label = if depth == 0 {
            "Comment".to_string()
        } else {
            format!("Reply level {depth}")
        };
        out.push(format!("{label} by {author}:"));
        out.push(comment_text(item));
        if let Some(children) = by_parent.get(&json_str(item, "id")) {
            for child in children.clone() {
                append_tree(normalized, by_parent, child, depth + 1, out, seen);
            }
        }
    }
    for root in &roots {
        append_tree(&normalized, &by_parent, *root, 0, &mut out, &mut seen);
    }
    for (index, item) in normalized.iter().enumerate() {
        let depth = item
            .get("depth")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .max(0.0) as usize;
        append_tree(&normalized, &by_parent, index, depth, &mut out, &mut seen);
    }
    out
}

/// Build the accessible document from a yt-dlp info dict (Python
/// `article_fields_from_info`): (title, author, text).
pub fn article_fields_from_info(info: &Value, url: &str) -> (String, String, String) {
    article_fields_from_info_in_language(info, url, &ui_language())
}

/// `article_fields_from_info` with the subtitle-ranking
/// language given explicitly (the main-window path passes
/// the app's UI language, as Python's `app_ui_language`
/// supplies it internally).
pub fn article_fields_from_info_in_language(
    info: &Value,
    url: &str,
    ui: &str,
) -> (String, String, String) {
    let mut sections: Vec<String> = Vec::new();
    let description = json_str(info, "description");
    sections.push("Description".to_string());
    sections.push(if description.is_empty() {
        "No video description was provided.".to_string()
    } else {
        description
    });
    let chapters = chapters_from_info(info, url);
    sections.push("Chapters".to_string());
    if chapters.is_empty() {
        sections.push("No chapters were provided.".to_string());
    } else {
        for ch in &chapters {
            sections.push(format!("{} {}", timestamp(ch.start), ch.title));
        }
    }
    let (language, transcript) = transcript_from_info(info, ui);
    sections.push(if language.is_empty() {
        "Subtitles".to_string()
    } else {
        format!("Subtitles ({language})")
    });
    if transcript.is_empty() {
        sections.push("No subtitles were available.".to_string());
    } else {
        sections.extend(transcript);
    }
    sections.push("Comments".to_string());
    // Honest incompleteness (Python article_fields_from_info):
    // the retry could not complete the discussion, so the
    // document says so before showing what did arrive.
    if info
        .get("_blindrss_comments_incomplete")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        sections.push(
            "YouTube returned an incomplete comment page after retry; all comments received are shown below."
                .to_string(),
        );
    }
    let empty = Value::Null;
    let comments = info.get("comments").unwrap_or(&empty);
    let lines = format_comments(comments);
    if lines.is_empty() {
        sections.push("No public comments were available.".to_string());
    } else {
        sections.extend(lines);
    }
    let text = sections
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let title = json_str(info, "title");
    let author = {
        let a = json_str(info, "channel");
        if a.is_empty() {
            json_str(info, "uploader")
        } else {
            a
        }
    };
    (title, author, text)
}

/// Full structured extraction for a YouTube video URL.
/// Returns (title, author, text); None when yt-dlp cannot serve it.
pub fn extract_article(url: &str) -> Option<(String, String, String)> {
    let exe = crate::media::find_ytdlp()?;
    extract_article_with(&exe, url)
}

/// `extract_article` against an explicit executable (the
/// seam the regression tests drive with a fake yt-dlp).
pub fn extract_article_with(exe: &Path, url: &str) -> Option<(String, String, String)> {
    if !is_youtube_video_url(url) {
        return None;
    }
    let info = extract_video_info_with(exe, url).ok()?;
    // Python extract_article returns nothing for an empty
    // info dict, however the fields builder would fill it.
    if info.as_object().is_some_and(|o| o.is_empty()) {
        return None;
    }
    let (title, author, text) = article_fields_from_info(&info, url);
    if text.trim().is_empty() {
        None
    } else {
        Some((title, author, text))
    }
}

#[cfg(test)]
mod language_tests {
    use super::language_match;

    #[test]
    fn subtitle_language_ranks_match_python() {
        for (code, wanted, rank) in [
            ("en_US-orig", "en-US", Some(0)),
            ("zh-Hant", "zh-TW", Some(1)),
            ("zh-Hans", "zh-SG", Some(1)),
            ("en", "en-GB", Some(2)),
            ("en-US", "en-GB", Some(3)),
            ("fr", "en", None),
            ("", "en", None),
            ("en", "", None),
        ] {
            assert_eq!(language_match(code, wanted), rank, "{code} / {wanted}");
        }
    }
}
