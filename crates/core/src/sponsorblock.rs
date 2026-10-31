//! SponsorBlock client (Python core/sponsorblock.py port):
//! fetch the community skip segments for a YouTube video from
//! the public sponsor.ajayyy.com API. The player applies them.

/// One skip segment, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
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

/// Parse the API's JSON array into segments, keeping only the
/// requested categories (the API filters too; this is a guard).
pub fn parse_segments(json_text: &str, categories: &[String]) -> Vec<Segment> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_text) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let category = item
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !categories.is_empty() && !categories.iter().any(|c| c == &category) {
            continue;
        }
        let Some(pair) = item.get("segment").and_then(|v| v.as_array()) else {
            continue;
        };
        if pair.len() == 2
            && let (Some(start), Some(end)) = (pair[0].as_f64(), pair[1].as_f64())
            && end > start
        {
            out.push(Segment {
                start,
                end,
                category,
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

/// Fetch segments for a video. A 404 from the API means "no
/// segments" and yields an empty list, not an error.
pub fn fetch_segments(video_id: &str, categories: &[String]) -> Result<Vec<Segment>, String> {
    let cats_json = serde_json::to_string(categories).unwrap_or_else(|_| "[]".to_string());
    let encoded = cats_json
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace('"', "%22")
        .replace(',', "%2C");
    let url = format!(
        "https://sponsor.ajayyy.com/api/skipSegments?videoID={video_id}&categories={encoded}"
    );
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .new_agent();
    let mut resp = match agent
        .get(&url)
        .header("User-Agent", "BlindRSS-Rust/0.1")
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(Vec::new()),
        Err(e) => return Err(format!("SponsorBlock: {e}")),
    };
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("SponsorBlock body: {e}"))?;
    Ok(parse_segments(&text, categories))
}
