//! SponsorBlock client (Python core/sponsorblock.py port):
//! skip crowdsourced sponsor/intro/etc. segments in YouTube
//! videos. Lookups use the privacy-preserving hash-prefix
//! endpoint, so the service only learns the first four hex
//! digits of SHA-256(video id), never which video is playing.
//! Segment data is volunteer-submitted and the service is
//! sometimes down: every failure returns no segments and
//! playback carries on.

use sha2::Digest;
use std::collections::HashSet;

/// Base of the hash-prefix lookup endpoint; the four-hex
/// prefix is appended as the final path segment.
pub const API_BASE: &str = "https://sponsor.ajay.app/api/skipSegments";

/// Category id -> English label (msgid; the GUI translates),
/// in Python's CATEGORIES order.
pub const CATEGORIES: [(&str, &str); 8] = [
    ("sponsor", "Sponsor"),
    ("selfpromo", "Unpaid or self promotion"),
    ("interaction", "Interaction reminder (subscribe)"),
    ("intro", "Intermission or intro animation"),
    ("outro", "Endcards or credits"),
    ("preview", "Preview or recap"),
    ("filler", "Filler or tangent"),
    ("music_offtopic", "Non-music section of a music video"),
];

/// Skipped out of the box: the categories people almost
/// always want gone.
pub const DEFAULT_SKIP: [&str; 3] = ["sponsor", "selfpromo", "interaction"];

/// The English label for a category id, or the id itself
/// when unknown (Python `CATEGORIES.get(category, category)`).
pub fn category_label(category: &str) -> &str {
    CATEGORIES
        .iter()
        .find(|(id, _)| *id == category)
        .map(|(_, label)| *label)
        .unwrap_or(category)
}

/// One skip segment, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
    pub uuid: String,
}

/// Extract a YouTube video id from a watch / share / shorts /
/// embed / live URL. Returns None for anything else.
pub fn youtube_video_id(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let take_id = |s: &str| -> Option<String> {
        let id: String = s
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if id.len() >= 6 { Some(id) } else { None }
    };
    if host == "youtu.be" {
        return take_id(path);
    }
    if !host.ends_with("youtube.com") && host != "youtube-nocookie.com" {
        return None;
    }
    if let Some(q) = path.strip_prefix("watch?") {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("v=") {
                return take_id(v);
            }
        }
        return None;
    }
    for prefix in ["shorts/", "embed/", "live/", "v/"] {
        if let Some(tail) = path.strip_prefix(prefix) {
            return take_id(tail);
        }
    }
    None
}

/// Categories the user has chosen to skip; empty when
/// SponsorBlock is off (Python `skip_categories`). The
/// enabled default follows Python's config defaults
/// (`sponsorblock_enabled: True`). A stored value that is
/// not a list falls back to DEFAULT_SKIP; the result is in
/// CATEGORIES order with unknown ids dropped.
pub fn skip_categories(cfg: &crate::config::Config) -> Vec<String> {
    if !cfg.get_bool("sponsorblock_enabled", true) {
        return Vec::new();
    }
    let chosen: Vec<String> = match cfg.raw().get("sponsorblock_categories") {
        None => DEFAULT_SKIP.iter().map(|s| s.to_string()).collect(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(_) => DEFAULT_SKIP.iter().map(|s| s.to_string()).collect(),
    };
    CATEGORIES
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| chosen.iter().any(|c| c == id))
        .map(str::to_string)
        .collect()
}

/// The first four hex digits of SHA-256(video id) — all the
/// service ever learns about the video.
pub fn hash_prefix(video_id: &str) -> String {
    let digest = sha2::Sha256::digest(video_id.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..4].to_string()
}

/// Percent-encode a query value (everything outside the
/// RFC 3986 unreserved set), matching what Python's requests
/// does to the JSON params.
fn encode_query_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The full lookup URL for the public API, or None when no
/// request may be issued (empty video id or no categories).
pub fn lookup_url(video_id: &str, categories: &[String]) -> Option<String> {
    lookup_url_with_base(API_BASE, video_id, categories)
}

/// `lookup_url` against an explicit endpoint base (the
/// local fixture server in tests).
pub fn lookup_url_with_base(base: &str, video_id: &str, categories: &[String]) -> Option<String> {
    if video_id.is_empty() || categories.is_empty() {
        return None;
    }
    let cats_json = serde_json::to_string(categories).unwrap_or_else(|_| "[]".to_string());
    let actions_json =
        serde_json::to_string(&["skip"]).unwrap_or_else(|_| "[\"skip\"]".to_string());
    Some(format!(
        "{base}/{}?categories={}&actionTypes={}",
        hash_prefix(video_id),
        encode_query_value(&cats_json),
        encode_query_value(&actions_json)
    ))
}

fn segment_number(value: Option<&serde_json::Value>) -> Option<f64> {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Parse the hash-prefix response into this video's segments
/// (Python `parse_segments`). The response lists every video
/// sharing the prefix, so only the entry whose `videoID`
/// matches is read (collision matching). Only "skip" actions
/// (a missing actionType defaults to "skip") lasting at
/// least 0.5 s survive. Sorted by start.
pub fn parse_segments(json_text: &str, video_id: &str) -> Vec<Segment> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_text) else {
        return Vec::new();
    };
    let Some(videos) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for video in videos {
        if video.get("videoID").and_then(|v| v.as_str()) != Some(video_id) {
            continue;
        }
        let Some(segments) = video.get("segments").and_then(|v| v.as_array()) else {
            continue;
        };
        for seg in segments {
            if seg
                .get("actionType")
                .and_then(|v| v.as_str())
                .unwrap_or("skip")
                != "skip"
            {
                continue;
            }
            let Some(pair) = seg.get("segment").and_then(|v| v.as_array()) else {
                continue;
            };
            if pair.len() != 2 {
                continue;
            }
            let (Some(start), Some(end)) =
                (segment_number(pair.first()), segment_number(pair.get(1)))
            else {
                continue;
            };
            if end - start < 0.5 {
                continue;
            }
            out.push(Segment {
                start,
                end,
                category: seg
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                uuid: seg
                    .get("UUID")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// The segment playback is inside (and has not already
/// skipped), if any (Python `segment_at`). The last half
/// second of a segment does not count: skipping there would
/// land past content the user already heard begin. `done`
/// holds the UUIDs already skipped.
pub fn segment_at<'a>(
    segments: &'a [Segment],
    pos_s: f64,
    done: &HashSet<String>,
) -> Option<&'a Segment> {
    for seg in segments {
        if seg.start > pos_s {
            break;
        }
        if pos_s < seg.end - 0.5 && !done.contains(&seg.uuid) {
            return Some(seg);
        }
    }
    None
}

/// Fetch segments for a video from the public API. Empty
/// video id or empty categories issue NO request. A 404
/// means "no segments for any video under this prefix";
/// every failure returns an empty list so playback
/// carries on (Python `fetch_segments`).
pub fn fetch_segments(video_id: &str, categories: &[String]) -> Vec<Segment> {
    fetch_segments_with_base(API_BASE, video_id, categories)
}

/// `fetch_segments` against an explicit endpoint base (the
/// local fixture server in tests).
pub fn fetch_segments_with_base(base: &str, video_id: &str, categories: &[String]) -> Vec<Segment> {
    let Some(url) = lookup_url_with_base(base, video_id, categories) else {
        return Vec::new();
    };
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();
    let mut resp = match agent
        .get(&url)
        .header("User-Agent", crate::http::user_agent())
        .call()
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let Ok(text) = resp.body_mut().read_to_string() else {
        return Vec::new();
    };
    parse_segments(&text, video_id)
}

/// Segments to skip for a media/page URL, or none for
/// non-YouTube media or when SponsorBlock is off (Python
/// `segments_for_url`).
pub fn segments_for_url(url: &str, cfg: &crate::config::Config) -> Vec<Segment> {
    let categories = skip_categories(cfg);
    if categories.is_empty() {
        return Vec::new();
    }
    match youtube_video_id(url) {
        Some(video_id) => fetch_segments(&video_id, &categories),
        None => Vec::new(),
    }
}
