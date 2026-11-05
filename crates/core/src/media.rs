//! Media support (slice 7): the yt-dlp managed-executable wrapper.
//!
//! yt-dlp stays an external executable (Brandon's standing rule);
//! Python BlindRSS drives it the same way. This module locates it,
//! fetches info JSON (title/duration/chapters), and resolves a
//! direct stream URL for the player.

use crate::db::Chapter;
use serde::Deserialize;
use std::path::PathBuf;

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

/// Locate the yt-dlp executable through the shared resolution
/// policy (Audit 8 Task 23, O14): user-configured path, explicit
/// override env, managed runtime, PATH, bundled and known
/// package locations — see `dependency::find_tool`.
pub fn find_ytdlp() -> Option<PathBuf> {
    crate::dependency::find_tool("yt-dlp")
}

/// Extra yt-dlp arguments from the app's environment, set at
/// startup from config: BLINDRSS_YTDLP_COOKIES
/// (`ytdlp_cookies_file`) and the shared fetch identity
/// (BLINDRSS_USER_AGENT, from core::http::user_agent()).
pub(crate) fn ytdlp_env_args() -> Vec<String> {
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
    // Owned subprocess (Audit 8 F20 policy): finite timeout,
    // capped output, killed and reaped on timeout.
    let run = crate::dependency::run_tool(&exe, &full, std::time::Duration::from_secs(600))
        .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    if !run.success {
        let first = run.stderr.lines().last().unwrap_or("unknown error");
        return Err(format!("yt-dlp: {first}"));
    }
    Ok(run.stdout)
}

/// Fetch info (title/duration/chapters) for a page URL.
pub fn info(url: &str) -> Result<MediaInfo, String> {
    let json = run_ytdlp(&["-J", "--no-warnings", "--no-playlist", url])?;
    parse_info(&json)
}

/// Full YouTube article metadata includes subtitle tracks
/// and public discussion. This IS the one shared pipeline
/// (Audit 8 Task 16, R10): `ytfull::extract_video_info` —
/// the same invocation, comment-completeness retry and
/// union every reader path uses. Callers that only need
/// chapters read them from the returned info as before.
pub fn youtube_article_info(url: &str) -> Result<serde_json::Value, String> {
    crate::ytfull::extract_video_info(url)
}

/// Resolve a direct stream URL through Python's shared,
/// bounded yt-dlp recovery policy (Audit 8 Task 22, M16 —
/// `ytdlp_recovery`): VLC-friendly audio selection, the
/// primary and fallback YouTube client pools, cookie attempts
/// with the futile-retry rules, and impersonated recovery-URL
/// phases. The cookie file comes from the app's environment
/// (BLINDRSS_YTDLP_COOKIES, set at startup from config).
pub fn stream_url(url: &str) -> Result<String, String> {
    let exe = find_ytdlp().ok_or("yt-dlp not found (install it or set BLINDRSS_YTDLP)")?;
    let cookie_file = std::env::var("BLINDRSS_YTDLP_COOKIES")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    stream_url_with(&exe, url, cookie_file.as_deref())
}

/// `stream_url` against an explicit executable and cookie file
/// (the seam the recovery regression tests drive).
pub fn stream_url_with(
    exe: &std::path::Path,
    url: &str,
    cookie_file: Option<&str>,
) -> Result<String, String> {
    use crate::ytdlp_recovery::{AttemptOutcome, CookieAttempt};
    crate::ytdlp_recovery::run_recovery(url, cookie_file, |phase, attempt| {
        let mut args: Vec<String> = vec![
            "-g".into(),
            "--no-playlist".into(),
            "--format".into(),
            crate::ytdlp_recovery::vlc_audio_format(&phase.target_url).to_string(),
            "--extractor-args".into(),
            crate::ytdlp_recovery::youtube_player_client_arg(phase.clients, None),
            "--quiet".into(),
            "--no-warnings".into(),
            "--no-progress".into(),
            "--geo-bypass".into(),
            "--color".into(),
            "never".into(),
            "--user-agent".into(),
            crate::http::user_agent(),
            "--referer".into(),
            phase.target_url.clone(),
        ];
        if phase.impersonate {
            args.push("--impersonate".into());
            args.push("chrome".into());
        }
        match attempt {
            CookieAttempt::Base => {}
            other => args.extend(other.args()),
        }
        args.push(phase.target_url.clone());
        let arg_refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
        match crate::dependency::run_tool(exe, &arg_refs, std::time::Duration::from_secs(30)) {
            Ok(run) if run.success => {
                match run.stdout.lines().map(|s| s.trim()).find(|s| !s.is_empty()) {
                    Some(u) => AttemptOutcome::Success(u.to_string()),
                    None => AttemptOutcome::Failed {
                        error: "yt-dlp returned no stream URL".to_string(),
                        fatal: false,
                    },
                }
            }
            Ok(run) => AttemptOutcome::Failed {
                error: run.stderr.trim().to_string(),
                fatal: false,
            },
            Err(e) => {
                let fatal = e.contains("timed out");
                AttemptOutcome::Failed { error: e, fatal }
            }
        }
    })
}

/// Python `_SEEKABLE_EXTENSIONS` (gui/player.py).
const SEEKABLE_EXTENSIONS: [&str; 13] = [
    ".mp3", ".m4a", ".m4b", ".aac", ".ogg", ".opus", ".wav", ".flac", ".mp4", ".m4v", ".webm",
    ".mkv", ".mov",
];

/// Python `_PODCAST_TRACKING_HOSTS`: redirector/tracker hosts
/// whose media-extension URLs still get redirect-resolved.
const PODCAST_TRACKING_HOSTS: [&str; 11] = [
    "op3.dev",
    "pdst.fm",
    "pscrb.fm",
    "chrt.fm",
    "chtbl.com",
    "podtrac.com",
    "mgln.ai",
    "blubrry.com",
    "podcasts.apple.com",
    "anchor.fm",
    "spotify.com",
];

fn is_podcast_tracking_url(url: &str) -> bool {
    let host = crate::ytdlp_recovery::url_host(url);
    !host.is_empty()
        && PODCAST_TRACKING_HOSTS
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// Python `_should_resolve_direct_media_url`: resolve
/// page-like URLs and podcast trackers, but skip ordinary
/// files — a URL whose path ends in a seekable extension is
/// resolved only when it sits on a tracking host; anything
/// else is resolved.
pub fn should_resolve_direct_media_url(url: &str, _media_type: &str) -> bool {
    let path = crate::ytdlp_recovery::url_path(url).to_ascii_lowercase();
    if SEEKABLE_EXTENSIONS.iter().any(|ext| path.ends_with(ext)) {
        return is_podcast_tracking_url(url);
    }
    true
}

/// The range-cache context the player carries out of
/// resolution (Audit 8 Task 22, M20; Python
/// `_resolve_media_worker` + `_maybe_range_cache_url`): when a
/// non-yt-dlp load was redirect-resolved to a DIFFERENT final
/// URL, the gate must register the ORIGINAL URL as the cache
/// identity — the proxy re-follows its redirects for a fresh
/// signed link when the resolved one expires, and the cached
/// segments are reused. Returns `(url_is_resolved,
/// original_url)` for `rangecache::maybe_range_cache_url`.
pub fn range_cache_context(
    input_url: &str,
    final_url: &str,
    via_ytdlp: bool,
    playback_enabled: bool,
) -> (bool, Option<String>) {
    if via_ytdlp || !playback_enabled {
        return (false, None);
    }
    if !input_url.is_empty() && input_url != final_url {
        return (true, Some(input_url.to_string()));
    }
    (false, None)
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
/// The stream manifests (HLS/DASH/MPEG-TS) belong here too: plenty
/// of podcast and video hosts publish the episode as a playlist
/// (`episode.m3u8`) instead of a plain audio file, and an enclosure
/// the resolver does not recognise gets replaced by whatever other
/// media item the feed carries (an episode image, usually).
pub const MEDIA_EXTENSIONS: [&str; 16] = [
    ".mp3", ".m4a", ".m4b", ".aac", ".ogg", ".opus", ".wav", ".flac", ".mp4", ".m4v", ".webm",
    ".mkv", ".mov", ".m3u8", ".mpd", ".ts",
];

/// Stream MIME types that are playable without a media-file
/// extension (HLS and DASH manifests, raw MPEG-TS).
fn is_stream_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/vnd.apple.mpegurl"
            | "application/x-mpegurl"
            | "audio/mpegurl"
            | "audio/x-mpegurl"
            | "application/dash+xml"
            | "application/x-mpegts"
            | "video/mp2t"
    )
}

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
    } else if path.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else if path.ends_with(".mpd") {
        "application/dash+xml"
    } else if path.ends_with(".ts") {
        "video/mp2t"
    } else {
        return None;
    };
    Some(ty.to_string())
}

/// Python's `canonical_media_type` (core/utils.py): strip any
/// parameters, lowercase, drop a trailing slash, and fold the
/// FLAC MIME aliases onto audio/flac.
pub fn canonical_media_type(media_type: Option<&str>) -> String {
    let mt = media_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mt = mt.trim_end_matches('/');
    if [
        "audio/flac",
        "audio/x-flac",
        "application/flac",
        "application/x-flac",
    ]
    .contains(&mt)
    {
        return "audio/flac".to_string();
    }
    mt.to_string()
}

/// Python's `media_type_is_audio_video_or_podcast`.
pub fn media_type_is_audio_video_or_podcast(media_type: Option<&str>) -> bool {
    let mt = canonical_media_type(media_type);
    !mt.is_empty()
        && (mt.starts_with("audio/") || mt.starts_with("video/") || mt.contains("podcast"))
}

/// Would the player open this article (Python
/// `_should_play_in_player(article, include_downloads=False)`)?
/// This is the article row's "Contains audio" test (Audit 8
/// G11): a yt-dlp-supported article URL that is not plainly an
/// image, or a playable enclosure. Completed downloads are
/// deliberately not consulted — Python's label path excludes
/// them; the live-progress correction in the main window covers
/// rows whose media only becomes known once played.
pub fn article_has_playable_media(article: &crate::Article) -> bool {
    const IMAGE_EXTS: [&str; 6] = [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp"];
    const AUDIO_EXTS: [&str; 8] = [
        ".mp3", ".m4a", ".m4b", ".aac", ".ogg", ".opus", ".wav", ".flac",
    ];
    if !article.url.is_empty() && is_ytdlp_supported(&article.url) {
        let url_low = article.url.to_ascii_lowercase();
        if IMAGE_EXTS.iter().any(|ext| url_low.ends_with(ext)) {
            return false;
        }
        return true;
    }
    if let Some(media_url) = article.media_url.as_deref().filter(|m| !m.is_empty()) {
        let url = media_url.to_ascii_lowercase();
        if IMAGE_EXTS.iter().any(|ext| url.ends_with(ext)) && !is_ytdlp_supported(media_url) {
            return false;
        }
        if media_type_is_audio_video_or_podcast(article.media_type.as_deref()) {
            return true;
        }
        if canonical_media_type(article.media_type.as_deref()) == "video/youtube" {
            return true;
        }
        if AUDIO_EXTS.iter().any(|ext| url.ends_with(ext)) {
            return true;
        }
    }
    false
}

/// Python's direct-media test: an audio/video (or stream) media
/// type, a media file extension, or a stream manifest extension on
/// the URL.
pub fn is_playable_media(url: &str, media_type: Option<&str>) -> bool {
    if let Some(t) = media_type {
        let t = t.trim().to_ascii_lowercase();
        if t.starts_with("audio/") || t.starts_with("video/") || is_stream_mime(&t) {
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

/// Encode a local path for either native media backend.
pub fn local_media_url(path: &std::path::Path) -> Result<String, String> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| "Invalid local media path".into())
}

/// Prefer completed downloads, including files indexed by the Python app.
pub fn resolve_article_for_play(
    cfg: &crate::config::Config,
    article: &crate::Article,
) -> Result<ResolvedMedia, String> {
    if let Some(path) = crate::download::downloaded_media_path(cfg, article) {
        let url = local_media_url(&path)?;
        return Ok(ResolvedMedia {
            media_type: media_type_for_url(&url),
            url,
            detected: false,
            via_ytdlp: false,
        });
    }
    resolve_for_play(
        article.media_url.as_deref(),
        article.media_type.as_deref(),
        &article.url,
        article.content.as_deref(),
    )
}

/// The full play-resolution chain, in Python's order. Runs on a
/// worker thread: it may fetch the article page and run yt-dlp.
pub fn resolve_for_play(
    media_url: Option<&str>,
    media_type: Option<&str>,
    page_url: &str,
    stored_html: Option<&str>,
) -> Result<ResolvedMedia, String> {
    if let Some(path) = media_url.and_then(crate::download::existing_local_media_path) {
        let url = url::Url::from_file_path(&path)
            .map_err(|_| "Invalid local media path")?
            .to_string();
        return Ok(ResolvedMedia {
            media_type: media_type_for_url(&url),
            url,
            detected: false,
            via_ytdlp: false,
        });
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    // The yt-dlp resolution-precedence unit test that used to
    // live here covered the removed pure `resolve_ytdlp` helper.
    // Precedence is now `dependency::find_tool`'s and is covered
    // in crates/core/tests/dependency_parity.rs (Audit 8 Task 23).

    #[test]
    fn hls_and_dash_enclosures_are_playable() {
        // A podcast episode served as a playlist: no audio MIME, no
        // familiar audio extension — it must still be playable, or
        // the feed chooser falls back to the episode image.
        assert!(is_playable_media(
            "https://cdn.example.com/ep1.m3u8",
            Some("application/vnd.apple.mpegurl")
        ));
        assert!(is_playable_media(
            "https://cdn.example.com/ep1",
            Some("application/x-mpegURL")
        ));
        assert!(is_playable_media(
            "https://cdn.example.com/manifest.mpd",
            Some("application/dash+xml")
        ));
        assert!(is_playable_media("https://cdn.example.com/ep1.m3u8", None));
        assert!(is_playable_media("https://cdn.example.com/ep1.ts", None));
        // An image enclosure is still not playable.
        assert!(!is_playable_media(
            "https://cdn.example.com/cover.jpg",
            Some("image/jpeg")
        ));
    }

    #[test]
    fn stream_extensions_get_stream_types() {
        assert_eq!(
            media_type_for_url("https://cdn.example.com/ep1.m3u8").as_deref(),
            Some("application/vnd.apple.mpegurl")
        );
        assert_eq!(
            media_type_for_url("https://cdn.example.com/ep1.mpd?token=1").as_deref(),
            Some("application/dash+xml")
        );
    }

    #[test]
    fn audio_extension_still_wins_over_query() {
        assert!(is_media_file_url("https://cdn.example.com/a.mp3?x=1#t=5"));
        assert!(!is_media_file_url("https://cdn.example.com/a.mp3x"));
    }
}
