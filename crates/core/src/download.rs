//! Downloads (port of the Python download paths + download_formats).
//!
//! Direct media downloads stream to disk; page URLs go through
//! yt-dlp with a small preset set mirroring Python's format choices.

use std::path::PathBuf;

/// Strip characters that are illegal or hostile in file names.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim().to_string();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

fn extension_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    let (_, ext) = last.rsplit_once('.')?;
    if ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(ext.to_lowercase())
    } else {
        None
    }
}

/// Download a direct URL to `dir`, naming the file after `title`
/// (keeping the URL's extension). Returns the written path.
pub fn download_direct(url: &str, dir: &std::path::Path, title: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("download dir: {e}"))?;
    let ext = extension_from_url(url).unwrap_or_else(|| "bin".to_string());
    let path = dir.join(format!("{}.{ext}", sanitize_filename(title)));
    let mut resp = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let mut file = std::fs::File::create(&path).map_err(|e| format!("create file: {e}"))?;
    let mut reader = resp.body_mut().as_reader();
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("write failed: {e}"))?;
    Ok(path)
}

/// Named download formats (Python `core/download_formats.py`).
/// Config stores the stable identifier ("video_best",
/// "audio_mp3_192"); logic only ever sees identifiers.
pub const DOWNLOAD_FORMAT_IDS: [&str; 8] = [
    "video_best",
    "video_max",
    "video_720",
    "video_480",
    "audio_mp3_320",
    "audio_mp3_192",
    "audio_mp3_128",
    "audio_best",
];

pub const DOWNLOAD_FORMAT_DEFAULT: &str = "video_best";

/// Display label for a format identifier.
pub fn download_format_label(id: &str) -> &'static str {
    match normalize_download_format(id) {
        "video_max" => "Video (maximum quality)",
        "video_720" => "Video (up to 720p)",
        "video_480" => "Video (up to 480p)",
        "audio_mp3_320" => "Audio (MP3, 320 kbps)",
        "audio_mp3_192" => "Audio (MP3, 192 kbps)",
        "audio_mp3_128" => "Audio (MP3, 128 kbps)",
        "audio_best" => "Audio (best quality)",
        _ => "Video (best quality, MP4)",
    }
}

/// Normalize a stored value to a known format identifier.
pub fn normalize_download_format(value: &str) -> &'static str {
    match value.trim() {
        "video_max" => "video_max",
        "video_720" => "video_720",
        "video_480" => "video_480",
        "audio_mp3_320" => "audio_mp3_320",
        "audio_mp3_192" => "audio_mp3_192",
        "audio_mp3_128" => "audio_mp3_128",
        "audio_best" => "audio_best",
        _ => DOWNLOAD_FORMAT_DEFAULT,
    }
}

fn video_selector(max_height: Option<u32>) -> String {
    let cap = max_height
        .map(|h| format!("[height<=?{h}]"))
        .unwrap_or_default();
    let mut alternatives = vec![
        format!("bv*[vcodec^=avc1]{cap}+ba[acodec^=mp4a]"),
        format!("bv*{cap}+ba"),
        format!("b{cap}"),
        "b".to_string(),
    ];
    alternatives.dedup();
    alternatives.join("/")
}

/// (selector, mp3 bitrate) for an identifier — Python `_SPECS`.
fn format_spec(id: &str) -> (String, Option<u32>) {
    match normalize_download_format(id) {
        "video_max" => ("bv*+ba/b".to_string(), None),
        "video_720" => (video_selector(Some(720)), None),
        "video_480" => (video_selector(Some(480)), None),
        "audio_mp3_320" => ("ba/b".to_string(), Some(320)),
        "audio_mp3_192" => ("ba/b".to_string(), Some(192)),
        "audio_mp3_128" => ("ba/b".to_string(), Some(128)),
        "audio_best" => ("ba/b".to_string(), None),
        _ => (video_selector(None), None),
    }
}

fn is_audio_only(id: &str) -> bool {
    normalize_download_format(id).starts_with("audio_")
}

/// The yt-dlp argument list for a format identifier (Python
/// `ytdlp_args`): format selection + output kind.
pub fn ytdlp_format_args(id: &str) -> Vec<String> {
    let ident = normalize_download_format(id);
    let (selector, bitrate) = format_spec(ident);
    let mut args = vec!["-f".to_string(), selector];
    if is_audio_only(ident) {
        args.push("-x".to_string());
        match bitrate {
            None => {
                args.extend(["--audio-format".to_string(), "best".to_string()]);
            }
            Some(b) => {
                args.extend([
                    "--audio-format".to_string(),
                    "mp3".to_string(),
                    "--audio-quality".to_string(),
                    format!("{b}K"),
                ]);
            }
        }
    } else {
        args.extend(["--merge-output-format".to_string(), "mp4".to_string()]);
    }
    args
}

/// Download a page URL with yt-dlp into `dir`, using the named
/// format from config `download_format`. Returns the directory
/// (yt-dlp names the file itself from the title).
pub fn download_with_ytdlp(
    url: &str,
    dir: &std::path::Path,
    format_id: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("download dir: {e}"))?;
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let template = dir.join("%(title)s.%(ext)s");
    let template = template.to_string_lossy().to_string();
    let format_args = ytdlp_format_args(format_id);
    let cookies = ytdlp_cookies_file();
    let mut args: Vec<&str> = vec!["--no-warnings", "--no-playlist", "-o", &template];
    args.extend(format_args.iter().map(|a| a.as_str()));
    if let Some(cookies) = &cookies {
        args.extend(["--cookies", cookies.as_str()]);
    }
    args.push(url);
    let out = std::process::Command::new(&exe)
        .args(&args)
        .output()
        .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().last().unwrap_or("unknown error");
        return Err(format!("yt-dlp download: {first}"));
    }
    Ok(dir.to_path_buf())
}

/// The configured yt-dlp cookies file (config
/// `ytdlp_cookies_file`), when set and non-empty. Read through
/// the shared config-next-to-database convention: the app sets
/// it via the environment for worker processes — here it is
/// simply the BLINDRSS_YTDLP_COOKIES override or None; the app
/// passes the config value through `media` invocations too.
fn ytdlp_cookies_file() -> Option<String> {
    std::env::var("BLINDRSS_YTDLP_COOKIES")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Convert a WebVTT subtitle file to plain transcript text with
/// rolling-caption dedupe (the algorithm Python's YouTube full-text
/// path relies on): cue text repeats cumulatively, so a line that
/// extends the previous kept line replaces it.
pub fn vtt_to_text(vtt: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for raw in vtt.lines() {
        let line = raw.trim();
        if line.is_empty()
            || line.starts_with("WEBVTT")
            || line.contains("-->")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        // Strip inline tags (<c>, <00:00:01.000>, ...).
        let mut text = String::new();
        let mut in_tag = false;
        for c in line.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => text.push(c),
                _ => {}
            }
        }
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        match kept.last() {
            Some(prev) if prev == &text => {}
            Some(prev) if text.starts_with(prev.as_str()) => {
                *kept.last_mut().unwrap() = text;
            }
            _ => kept.push(text),
        }
    }
    kept.join(" ")
}

/// Compose a YouTube article's full text from its info (description)
/// plus a transcript, mirroring `core/youtube_fulltext.py`'s shape.
pub fn compose_youtube_text(
    description: &str,
    chapters: &[crate::db::Chapter],
    transcript: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !description.trim().is_empty() {
        parts.push(description.trim().to_string());
    }
    if !chapters.is_empty() {
        let lines: Vec<String> = chapters
            .iter()
            .map(|c| {
                let secs = c.start as i64;
                format!(
                    "{}:{:02} {}",
                    secs / 60,
                    secs % 60,
                    c.title.as_deref().unwrap_or("")
                )
            })
            .collect();
        parts.push(format!("Chapters:\n{}", lines.join("\n")));
    }
    if !transcript.trim().is_empty() {
        parts.push(format!("Transcript:\n{}", transcript.trim()));
    }
    parts.join("\n\n")
}

/// Fetch a YouTube page's full text: yt-dlp info for the
/// description/chapters, and the subtitle file for the transcript.
pub fn youtube_full_text(url: &str) -> Result<String, String> {
    let info = crate::media::info(url)?;
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let tmp = std::env::temp_dir().join(format!("blindrss_subs_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("temp dir: {e}"))?;
    let template = tmp.join("sub").to_string_lossy().to_string();
    let _ = std::process::Command::new(&exe)
        .args([
            "--no-warnings",
            "--no-playlist",
            "--skip-download",
            "--write-subs",
            "--write-auto-subs",
            "--sub-langs",
            "en.*",
            "--sub-format",
            "vtt",
            "-o",
            &template,
            url,
        ])
        .output();
    let mut transcript = String::new();
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "vtt")
                && let Ok(text) = std::fs::read_to_string(&p)
            {
                transcript = vtt_to_text(&text);
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let text = compose_youtube_text(&info.description, &info.chapters, &transcript);
    if text.trim().is_empty() {
        return Err("no description or transcript available".to_string());
    }
    Ok(text)
}
