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

/// Run yt-dlp -J with comment extraction (Python
/// `_extract_info_via_cli` with include_comments=True).
fn extract_info(url: &str, timeout_s: u64) -> Result<Value, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let args: Vec<String> = vec![
        "-J".into(),
        "--ignore-config".into(),
        "--no-playlist".into(),
        "--skip-download".into(),
        "--socket-timeout".into(),
        "20".into(),
        "--retries".into(),
        "2".into(),
        "--extractor-retries".into(),
        "6".into(),
        "--extractor-args".into(),
        "youtube:player_client=default,android_vr;max_comments=all,all,all,all,all".into(),
        "--write-comments".into(),
        "--".into(),
        url.to_string(),
    ];
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&exe).args(&args).output();
        let _ = tx.send(out);
    });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(timeout_s.max(30))) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("yt-dlp: {e}")),
        Err(_) => return Err("yt-dlp did not finish in time".to_string()),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("yt-dlp failed")
            .trim()
            .to_string();
        return Err(last);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).map_err(|e| format!("yt-dlp output parse failed: {e}"))
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
    for pair in url.split(['?', '&']) {
        if let Some(v) = pair.strip_prefix("tlang=") {
            return v.to_string();
        }
    }
    String::new()
}

struct SubCandidate {
    rank: (i32, u8),
    kind_rank: i32,
    language: String,
    track: Value,
}

/// Subtitle tracks best-first (Python `_subtitle_candidates`): UI
/// language, English, machine translation into the UI language, the
/// video's original language, other authored tracks; authored
/// before automatic within each step.
fn subtitle_candidates(info: &Value) -> Vec<SubCandidate> {
    let ui = ui_language();
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
            let mut usable: Vec<&Value> = formats
                .iter()
                .filter(|f| !json_str(f, "url").is_empty() && f.get("protocol").is_none())
                .collect();
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
                match language_match(&translation, &ui) {
                    Some(m) => (2, m),
                    None => continue, // never offer other-language translations
                }
            } else if let Some(m) = language_match(language, &ui) {
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

fn fetch_text(url: &str) -> Result<String, String> {
    let (bytes, _final) = crate::extract::fetch_page_bytes(url, "")?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// (language, cue texts) for the best subtitle track (Python
/// `transcript_from_info`: at most 2 tracks tried).
fn transcript_from_info(info: &Value) -> (String, Vec<String>) {
    let mut first_language = String::new();
    let mut tried: HashMap<String, ()> = HashMap::new();
    for cand in subtitle_candidates(info) {
        let track_url = json_str(&cand.track, "url");
        if track_url.is_empty() || tried.contains_key(&track_url) {
            continue;
        }
        if tried.len() >= 2 {
            break;
        }
        tried.insert(track_url.clone(), ());
        if first_language.is_empty() {
            first_language = cand.language.clone();
        }
        let Ok(body) = fetch_text(&track_url) else {
            continue;
        };
        let entries = if json_str(&cand.track, "ext") == "json3" {
            match serde_json::from_str::<Value>(&body) {
                Ok(payload) => transcript_from_json3(&payload),
                Err(_) => Vec::new(),
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
    let (language, transcript) = transcript_from_info(info);
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
    if !is_youtube_video_url(url) {
        return None;
    }
    // With comments the CLI gets Python's generous budget.
    let info = extract_info(url, 300).ok()?;
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
