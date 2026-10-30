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
    })
}

/// Locate the yt-dlp executable: explicit override env, then PATH.
pub fn find_ytdlp() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BLINDRSS_YTDLP")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    let names = if cfg!(windows) {
        ["yt-dlp.exe", "yt-dlp"]
    } else {
        ["yt-dlp", "yt-dlp"]
    };
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn run_ytdlp(args: &[&str]) -> Result<String, String> {
    let exe = find_ytdlp().ok_or("yt-dlp not found (install it or set BLINDRSS_YTDLP)")?;
    let out = Command::new(&exe)
        .args(args)
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
