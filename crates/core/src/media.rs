//! Media support (slice 7): the yt-dlp managed-executable wrapper.
//!
//! yt-dlp stays an external executable (Brandon's standing rule);
//! Python BlindRSS drives it the same way. This module locates it,
//! fetches info JSON (title/duration/chapters), and resolves a
//! direct stream URL for the player.

use crate::db::Chapter;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaInfo {
    pub title: String,
    pub duration_secs: Option<f64>,
    pub chapters: Vec<Chapter>,
    pub webpage_url: String,
    pub description: String,
}

#[derive(Deserialize)]
struct RawInfo {
    #[serde(default)]
    title: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    chapters: Vec<RawChapter>,
    #[serde(default)]
    webpage_url: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct RawChapter {
    #[serde(default)]
    start_time: f64,
    #[serde(default)]
    title: String,
}

/// Parse yt-dlp `-J` info JSON (pure; fixture-tested).
pub fn parse_info(json: &str) -> Result<MediaInfo, String> {
    let raw: RawInfo = serde_json::from_str(json).map_err(|e| format!("yt-dlp info: {e}"))?;
    Ok(MediaInfo {
        title: raw.title,
        duration_secs: raw.duration,
        chapters: raw
            .chapters
            .into_iter()
            .map(|c| Chapter {
                start: c.start_time,
                title: if c.title.is_empty() {
                    None
                } else {
                    Some(c.title)
                },
                href: None,
            })
            .collect(),
        webpage_url: raw.webpage_url,
        description: raw.description,
    })
}

/// Locate the yt-dlp executable: user-configured path, explicit
/// override env, then PATH.
pub fn find_ytdlp() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["yt-dlp.exe", "yt-dlp"]
    } else {
        &["yt-dlp"]
    };
    if let Some(p) = crate::dependency::user_tool_path("yt-dlp", names) {
        return Some(p);
    }
    if let Ok(p) = std::env::var("BLINDRSS_YTDLP")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    crate::dependency::find_on_path(names)
}

/// Extra yt-dlp arguments from the app's environment, set at
/// startup from config: BLINDRSS_YTDLP_COOKIES
/// (`ytdlp_cookies_file`) and the shared fetch identity
/// (BLINDRSS_USER_AGENT, from core::http::user_agent()).
fn ytdlp_env_args() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(c) = std::env::var("BLINDRSS_YTDLP_COOKIES")
        && !c.trim().is_empty()
    {
        out.push("--cookies".to_string());
        out.push(c);
    }
    if let Ok(ua) = std::env::var("BLINDRSS_USER_AGENT")
        && !ua.trim().is_empty()
    {
        out.push("--user-agent".to_string());
        out.push(ua);
    }
    out
}

fn run_ytdlp(args: &[&str]) -> Result<String, String> {
    let exe = find_ytdlp().ok_or("yt-dlp not found (install it or set BLINDRSS_YTDLP)")?;
    let env_args = ytdlp_env_args();
    let mut full: Vec<&str> = env_args.iter().map(|a| a.as_str()).collect();
    full.extend(args.iter().copied());
    let out = Command::new(&exe)
        .args(&full)
        .output()
        .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().last().unwrap_or("unknown error");
        return Err(format!("yt-dlp: {first}"));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("yt-dlp output: {e}"))
}

/// Fetch info (title/duration/chapters) for a page URL.
pub fn info(url: &str) -> Result<MediaInfo, String> {
    let json = run_ytdlp(&["-J", "--no-warnings", "--no-playlist", url])?;
    parse_info(&json)
}

/// Resolve a direct stream URL (best audio first — BlindRSS is an
/// audio-first reader; video falls back to best combined).
pub fn stream_url(url: &str) -> Result<String, String> {
    let out = run_ytdlp(&[
        "-g",
        "--no-warnings",
        "--no-playlist",
        "-f",
        "bestaudio/best",
        url,
    ])?;
    out.lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "yt-dlp returned no stream URL".to_string())
}

// ---------------------------------------------------------------------
// Playback resolution (Python parity: _playback_target_for_article,
// discovery.is_ytdlp_supported, discovery.detect_media).
//
// Python never sends an arbitrary article page to yt-dlp as the
// first step: an enclosure plays directly, yt-dlp is for sites it
// actually supports, and other pages are scanned for embedded
// media first, with yt-dlp's generic extractor as the last resort.
// ---------------------------------------------------------------------

/// File extensions BlindRSS treats as directly playable media.
pub const MEDIA_EXTENSIONS: [&str; 13] = [
    ".mp3", ".m4a", ".m4b", ".aac", ".ogg", ".opus", ".wav", ".flac", ".mp4", ".m4v", ".webm",
    ".mkv", ".mov",
];

/// True when the URL path ends in a media file extension.
pub fn is_media_file_url(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    MEDIA_EXTENSIONS.iter().any(|e| path.ends_with(e))
}

/// MIME type guessed from a media file URL's extension.
pub fn media_type_for_url(url: &str) -> Option<String> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let ty = if path.ends_with(".mp3") {
        "audio/mpeg"
    } else if path.ends_with(".m4a") || path.ends_with(".m4b") {
        "audio/mp4"
    } else if path.ends_with(".aac") {
        "audio/aac"
    } else if path.ends_with(".ogg") || path.ends_with(".opus") {
        "audio/ogg"
    } else if path.ends_with(".wav") {
        "audio/wav"
    } else if path.ends_with(".flac") {
        "audio/flac"
    } else if path.ends_with(".mp4") || path.ends_with(".m4v") {
        "video/mp4"
    } else if path.ends_with(".webm") {
        "video/webm"
    } else if path.ends_with(".mkv") {
        "video/x-matroska"
    } else if path.ends_with(".mov") {
        "video/quicktime"
    } else {
        return None;
    };
    Some(ty.to_string())
}

/// Python's direct-media test: an audio/video media type, or a
/// media file extension on the URL.
pub fn is_playable_media(url: &str, media_type: Option<&str>) -> bool {
    if let Some(t) = media_type {
        let t = t.trim().to_ascii_lowercase();
        if t.starts_with("audio/") || t.starts_with("video/") {
            return true;
        }
    }
    is_media_file_url(url)
}

/// Python's fast yt-dlp allowlist (discovery.is_ytdlp_supported):
/// hosts where yt-dlp has a real extractor. Python additionally
/// consults yt-dlp's full extractor table; the generic attempt at
/// the end of resolve_for_play covers much of that long tail.
pub fn is_ytdlp_supported(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    const KNOWN: [&str; 16] = [
        "youtube.com",
        "youtu.be",
        "vimeo.com",
        "twitch.tv",
        "dailymotion.com",
        "soundcloud.com",
        "facebook.com",
        "twitter.com",
        "x.com",
        "tiktok.com",
        "instagram.com",
        "rumble.com",
        "bilibili.com",
        "mixcloud.com",
        "odysee.com",
        "lbry.tv",
    ];
    KNOWN
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Resolve a possibly-relative media URL against a page URL.
fn resolve_relative(base: &str, candidate: &str) -> String {
    if candidate.starts_with("http://") || candidate.starts_with("https://") {
        return candidate.to_string();
    }
    let Some((scheme, rest)) = base.split_once("://") else {
        return candidate.to_string();
    };
    if let Some(stripped) = candidate.strip_prefix("//") {
        return format!("{scheme}://{stripped}");
    }
    let origin_end = rest
        .find('/')
        .map(|i| i + scheme.len() + 3)
        .unwrap_or(base.len());
    if candidate.starts_with('/') {
        return format!("{}{candidate}", &base[..origin_end]);
    }
    let dir_end = base.rfind('/').map(|i| i + 1).unwrap_or(base.len());
    format!("{}{candidate}", &base[..dir_end.max(origin_end)])
}

/// Scan page HTML for embedded media (Python's detect_media page
/// heuristics, minus its Sky/NPR specials): og:audio meta first,
/// then any src/href pointing at a media file, audio before video.
pub fn detect_media_in_page(html: &str, base_url: &str) -> Option<(String, Option<String>)> {
    let lower = html.to_ascii_lowercase();
    // og:audio / twitter:player:stream meta tags carry the media
    // URL in their content attribute.
    for marker in ["og:audio", "twitter:player:stream"] {
        if let Some(pos) = lower.find(marker)
            && let Some(cap) = attr_value(&html[pos..], "content")
            && is_media_file_url(&cap)
        {
            let url = resolve_relative(base_url, &cap);
            return Some((url.clone(), media_type_for_url(&url)));
        }
    }
    // Every src=/href= attribute value that names a media file, in
    // document order; audio wins over video regardless of order.
    let mut first_video: Option<String> = None;
    for value in attr_values(html) {
        if !is_media_file_url(&value) {
            continue;
        }
        let url = resolve_relative(base_url, &value);
        let ty = media_type_for_url(&url);
        if ty
            .as_deref()
            .map(|t| t.starts_with("audio/"))
            .unwrap_or(false)
        {
            return Some((url, ty));
        }
        if first_video.is_none() {
            first_video = Some(url);
        }
    }
    first_video.map(|url| {
        let ty = media_type_for_url(&url);
        (url, ty)
    })
}

/// All src=/href= attribute values in document order.
fn attr_values(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lower.len() && out.len() <= 512 {
        let src_at = lower[i..].find("src=").map(|p| i + p);
        let href_at = lower[i..].find("href=").map(|p| i + p);
        let (at, name) = match (src_at, href_at) {
            (Some(a), Some(b)) => {
                if a <= b {
                    (a, "src")
                } else {
                    (b, "href")
                }
            }
            (Some(a), None) => (a, "src"),
            (None, Some(b)) => (b, "href"),
            (None, None) => break,
        };
        if let Some(v) = attr_value(&html[at..], name) {
            out.push(v);
        }
        i = at + name.len() + 1;
    }
    out
}

/// Extract one attribute's value (quoted or bare) from a fragment.
fn attr_value(fragment: &str, name: &str) -> Option<String> {
    let lower = fragment.to_ascii_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find(&format!("{name}=")) {
        let at = search + pos + name.len() + 1;
        let rest = &fragment[at..];
        let rest = rest.trim_start_matches([' ', '\t']);
        if let Some(stripped) = rest.strip_prefix('"') {
            if let Some(end) = stripped.find('"') {
                return Some(stripped[..end].to_string());
            }
        } else if let Some(stripped) = rest.strip_prefix('\'') {
            if let Some(end) = stripped.find('\'') {
                return Some(stripped[..end].to_string());
            }
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '>' || c == '"' || c == '\'')
                .unwrap_or(rest.len());
            if end > 0 {
                return Some(rest[..end].to_string());
            }
        }
        search = at;
    }
    None
}

/// A resolved playable target for an article.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMedia {
    pub url: String,
    pub media_type: Option<String>,
    /// Found by page detection (the caller persists it, like
    /// Python's update_article_media after Detect Audio).
    pub detected: bool,
    /// Resolved through yt-dlp (info/chapters are worth fetching).
    pub via_ytdlp: bool,
}

/// The full play-resolution chain, in Python's order. Runs on a
/// worker thread: it may fetch the article page and run yt-dlp.
pub fn resolve_for_play(
    media_url: Option<&str>,
    media_type: Option<&str>,
    page_url: &str,
    stored_html: Option<&str>,
) -> Result<ResolvedMedia, String> {
    if let Some(m) = media_url.filter(|u| !u.is_empty())
        && is_playable_media(m, media_type)
    {
        return Ok(ResolvedMedia {
            url: m.to_string(),
            media_type: media_type.map(|s| s.to_string()),
            detected: false,
            via_ytdlp: false,
        });
    }
    if !page_url.is_empty() && is_ytdlp_supported(page_url) {
        let stream = stream_url(page_url)?;
        return Ok(ResolvedMedia {
            url: stream,
            media_type: None,
            detected: false,
            via_ytdlp: true,
        });
    }
    if let Some(html) = stored_html
        && let Some((url, ty)) = detect_media_in_page(html, page_url)
    {
        return Ok(ResolvedMedia {
            url,
            media_type: ty,
            detected: true,
            via_ytdlp: false,
        });
    }
    if !page_url.is_empty() {
        if let Ok(html) = crate::extract::fetch_html(page_url)
            && let Some((url, ty)) = detect_media_in_page(&html, page_url)
        {
            return Ok(ResolvedMedia {
                url,
                media_type: ty,
                detected: true,
                via_ytdlp: false,
            });
        }
        // Last resort, mirroring Python's player: let yt-dlp's
        // generic extractor try the page before giving up.
        if let Ok(stream) = stream_url(page_url) {
            return Ok(ResolvedMedia {
                url: stream,
                media_type: None,
                detected: true,
                via_ytdlp: true,
            });
        }
        return Err("No playable media found for this article".to_string());
    }
    Err("Article has no playable media".to_string())
}
