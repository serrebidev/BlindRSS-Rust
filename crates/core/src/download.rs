//! Downloads (port of the Python download paths + download_formats).
//!
//! Direct media downloads stream to disk; page URLs go through
//! yt-dlp with a small preset set mirroring Python's format choices.

use std::path::{Path, PathBuf};

fn download_index_keys(cfg: &crate::config::Config, article: &crate::Article) -> Vec<String> {
    use sha2::{Digest, Sha256};
    let provider = cfg.get_str("active_provider", "local");
    let mut values = Vec::new();
    if !article.id.is_empty() {
        values.push(("article", format!("{provider}|{}", article.id)));
        values.push((
            "article-id",
            format!("{provider}|{}|{}", article.feed_id, article.id),
        ));
    }
    if let Some(media) = article
        .media_url
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        values.push(("media", media.trim().to_string()));
    }
    if !article.url.trim().is_empty() {
        values.push(("url", article.url.trim().to_string()));
    }
    values
        .into_iter()
        .map(|(kind, value)| {
            let digest: String = Sha256::digest(value.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("{kind}:{digest}")
        })
        .collect()
}

/// Existing local media only; HTTP URLs and incomplete downloads are excluded.
pub fn existing_local_media_path(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let path = if value.to_ascii_lowercase().starts_with("file:") {
        url::Url::parse(value).ok()?.to_file_path().ok()?
    } else {
        let path = PathBuf::from(value);
        if value.contains("://") {
            return None;
        }
        path
    };
    if !path.is_file()
        || path
            .extension()
            .is_some_and(|s| s == "part" || s == "ytdl" || s == "tmp")
    {
        return None;
    }
    std::path::absolute(path).ok()
}

/// Read Python's `downloaded_media` SHA-256 index, including legacy string entries.
pub fn downloaded_media_path(
    cfg: &crate::config::Config,
    article: &crate::Article,
) -> Option<PathBuf> {
    if let Some(path) = article
        .media_url
        .as_deref()
        .and_then(existing_local_media_path)
    {
        return Some(path);
    }
    let index = cfg.raw().get("downloaded_media")?;
    for key in download_index_keys(cfg, article) {
        let Some(entry) = index.get(&key) else {
            continue;
        };
        if let Some(path) = entry
            .as_str()
            .or_else(|| entry.get("path").and_then(|v| v.as_str()))
            .and_then(existing_local_media_path)
        {
            return Some(path);
        }
    }
    None
}

/// Record a completed file using Python-compatible keys. The caller saves config.
pub fn record_article_download(
    cfg: &mut crate::config::Config,
    article: &crate::Article,
    path: &Path,
) -> Result<(), String> {
    let path = existing_local_media_path(&path.to_string_lossy())
        .ok_or("Downloaded file was not found")?;
    let entry = serde_json::json!({
        "path": path, "title": article.title, "feed_id": article.feed_id,
        "article_id": article.id, "media_url": article.media_url.as_deref().unwrap_or(""),
        "url": article.url,
        "updated_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
    });
    let keys = download_index_keys(cfg, article);
    let raw = cfg.raw_mut();
    if !raw.get("downloaded_media").is_some_and(|v| v.is_object()) {
        raw["downloaded_media"] = serde_json::json!({});
    }
    for key in keys {
        raw["downloaded_media"][key] = entry.clone();
    }
    Ok(())
}

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

/// Python `_safe_name` (gui/mainframe.py): runs of
/// `[\\/:*?"<>|]+` collapse to one underscore, whitespace is
/// stripped, trailing dots are stripped, the result is capped
/// at 120 chars, and empty becomes "untitled". Used for
/// download folder names and download file base names —
/// deliberately NOT the same rules as `sanitize_filename`.
pub fn safe_download_name(text: &str) -> String {
    if text.is_empty() {
        return "untitled".to_string();
    }
    let mut cleaned = String::with_capacity(text.len());
    let mut in_run = false;
    for c in text.chars() {
        if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            if !in_run {
                cleaned.push('_');
                in_run = true;
            }
        } else {
            in_run = false;
            cleaned.push(c);
        }
    }
    let cleaned = cleaned.trim().trim_end_matches('.');
    let out: String = cleaned.chars().take(120).collect();
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// Python `_download_dir_for_article`: the download folder
/// for an article — the root plus the safe-named destination
/// override (pasted-link items) when present, else the root
/// plus the safe-named feed title ("Feed" when unknown).
pub fn download_dir_for(
    root: &std::path::Path,
    feed_title: Option<&str>,
    folder_override: Option<&str>,
) -> PathBuf {
    if let Some(over) = folder_override.map(str::trim).filter(|s| !s.is_empty()) {
        return root.join(safe_download_name(over));
    }
    root.join(safe_download_name(feed_title.unwrap_or("Feed")))
}

/// Python `_guess_extension`: the URL path's extension when
/// it is short (<= 5 chars including the dot), else the
/// response media type mapped through Python's table (exact
/// canonical match, then prefix), else ".mp3" — never ".bin".
pub fn extension_for_download(url: &str, content_type: Option<&str>) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    if let Some(idx) = last.rfind('.')
        && idx > 0
    {
        let ext = &last[idx..];
        if ext.len() <= 5 {
            return ext.to_string();
        }
    }
    const MIME_EXTENSIONS: [(&str, &str); 12] = [
        ("audio/mpeg", ".mp3"),
        ("audio/mp3", ".mp3"),
        ("audio/mp4", ".m4a"),
        ("audio/aac", ".aac"),
        ("audio/ogg", ".ogg"),
        ("audio/opus", ".opus"),
        ("audio/x-wav", ".wav"),
        ("audio/wav", ".wav"),
        ("audio/flac", ".flac"),
        ("audio/x-flac", ".flac"),
        ("application/flac", ".flac"),
        ("application/x-flac", ".flac"),
    ];
    if let Some(ct) = content_type {
        // utils.canonical_media_type: parameters stripped,
        // lowercased, FLAC aliases fold to audio/flac (which is
        // itself in the table, so the fold changes nothing here
        // beyond the prefix scan below).
        let canonical = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .trim_end_matches('/')
            .to_string();
        for (mime, ext) in MIME_EXTENSIONS {
            if canonical == mime {
                return ext.to_string();
            }
        }
        for (mime, ext) in MIME_EXTENSIONS {
            if canonical.starts_with(mime) {
                return ext.to_string();
            }
        }
    }
    ".mp3".to_string()
}

/// Python `_find_downloaded_file`: files in `dir` whose name
/// starts with `base_name`, excluding temp/partial sidecars,
/// newest by mtime.
pub fn find_downloaded_file(dir: &std::path::Path, base_name: &str) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(base_name)
            || name.to_ascii_lowercase().contains(".temp.")
            || [".part", ".ytdl", ".tmp"].iter().any(|s| name.ends_with(s))
        {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(_, t)| mtime > *t) {
            best = Some((path, mtime));
        }
    }
    best.map(|(p, _)| p)
}

/// Python `_find_legacy_downloaded_media_path`: locate a
/// download written before the `downloaded_media` index
/// tracked it. Python looks in the article's own download
/// folder; core does not know the feed title, so the scan
/// covers the root and every first-level subfolder (the feed
/// folders and "Media Links") — a superset that finds the
/// file in whichever folder it landed.
pub fn legacy_downloaded_media_path(
    root: &std::path::Path,
    article: &crate::Article,
) -> Option<PathBuf> {
    let title = article.title.trim();
    let base = safe_download_name(if title.is_empty() { "episode" } else { title });
    if let Some(path) = find_downloaded_file(root, &base) {
        return Some(path);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        if dir.is_dir()
            && let Some(path) = find_downloaded_file(&dir, &base)
        {
            return Some(path);
        }
    }
    None
}

/// `downloaded_media_path` plus the legacy folder scan
/// (Audit 8 Task 22, M19): the index wins; the scan runs only
/// when the index has nothing.
pub fn downloaded_media_path_or_legacy(
    cfg: &crate::config::Config,
    article: &crate::Article,
    root: &std::path::Path,
) -> Option<PathBuf> {
    downloaded_media_path(cfg, article).or_else(|| legacy_downloaded_media_path(root, article))
}

fn sweep_dir_retention(dir: &std::path::Path, cutoff: std::time::SystemTime) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_file()
                && entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t < cutoff)
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Python `_apply_download_retention`: delete files in THIS
/// folder older than the retention window (None = unlimited).
/// Runs after each successful download, scoped to the folder
/// the download landed in.
pub fn apply_download_retention(dir: &std::path::Path, retention_seconds: Option<i64>) {
    let Some(seconds) = retention_seconds else {
        return;
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(seconds.max(0) as u64);
    sweep_dir_retention(dir, cutoff);
}

/// The refresh-time retention cleanup: Python applies
/// retention per download folder; since downloads now land in
/// feed subfolders (M19), the sweep covers the root and its
/// first-level subfolders.
pub fn sweep_download_retention(root: &std::path::Path, retention_seconds: Option<i64>) {
    let Some(seconds) = retention_seconds else {
        return;
    };
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(seconds.max(0) as u64);
    sweep_dir_retention(root, cutoff);
    if let Ok(rd) = std::fs::read_dir(root) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                sweep_dir_retention(&path, cutoff);
            }
        }
    }
}

/// Download a direct URL to `dir`, naming the file after
/// `title`. Python names the file from the response, not the
/// request (Audit 8 Task 22, M19): the request goes out first,
/// then the extension comes from the URL or the response's
/// media type (`extension_for_download`), the base name is
/// the Python-safe article title (or "episode"), and an
/// existing file advances to `name-1`, `name-2`, ... —
/// never a UUID suffix, never ".bin". The partial-file +
/// no-overwrite publish is unchanged.
pub fn download_direct(url: &str, dir: &std::path::Path, title: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("download dir: {e}"))?;
    let mut resp = crate::http::agent()
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let ext = extension_for_download(url, content_type.as_deref());
    let title = title.trim();
    let name = safe_download_name(if title.is_empty() { "episode" } else { title });
    let partial = dir.join(format!(".blindrss-{}.part", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|e| format!("create file: {e}"))?;
        let mut reader = resp.body_mut().as_reader();
        std::io::copy(&mut reader, &mut file).map_err(|e| format!("write failed: {e}"))?;
        file.sync_all().map_err(|e| format!("save download: {e}"))?;
        drop(file);
        // Publish without replacing an existing download
        // (Python _unique_path): name, name-1, name-2, ...
        let mut counter = 0u32;
        loop {
            let stem = if counter == 0 {
                name.clone()
            } else {
                format!("{name}-{counter}")
            };
            let path = dir.join(format!("{stem}{ext}"));
            match std::fs::hard_link(&partial, &path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    counter += 1;
                    continue;
                }
                Err(_) => {
                    // FAT/exFAT destinations have no hard links; reserve the output before copying.
                    let mut output = match std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                    {
                        Ok(f) => f,
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            counter += 1;
                            continue;
                        }
                        Err(e) => return Err(format!("finish download: {e}")),
                    };
                    let copied = (|| {
                        let mut source = std::fs::File::open(&partial)?;
                        std::io::copy(&mut source, &mut output)?;
                        output.sync_all()
                    })();
                    drop(output);
                    if let Err(error) = copied {
                        let _ = std::fs::remove_file(&path);
                        return Err(format!("finish download: {error}"));
                    }
                    return Ok(path);
                }
            }
        }
    })();
    let _ = std::fs::remove_file(&partial);
    result
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
/// `ytdlp_args`): format selection + output kind. The merge
/// format applies to video presets only (Python ignores it
/// for audio presets); it exists so a failed MP4 mux can be
/// retried as MKV (M18).
pub fn ytdlp_format_args_with_merge(id: &str, merge_output_format: &str) -> Vec<String> {
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
        args.extend([
            "--merge-output-format".to_string(),
            if merge_output_format.is_empty() {
                "mp4".to_string()
            } else {
                merge_output_format.to_string()
            },
        ]);
    }
    args
}

/// `ytdlp_format_args_with_merge` with Python's default
/// merge format (mp4).
pub fn ytdlp_format_args(id: &str) -> Vec<String> {
    ytdlp_format_args_with_merge(id, "mp4")
}

/// Download a page URL with yt-dlp into `dir`, using the named
/// format. Runs Python's shared recovery policy (Audit 8
/// Task 22, M16+M18; mainframe `_download_article_via_ytdlp`):
/// the client-pool/cookie/recovery-URL phases, the MP4 mux
/// failure retried once as MKV inside the same attempt, audio
/// presets never merge-retried, a timeout stopping all
/// phases, and the anonymous attempt's error reported. The
/// output template is Python's: the safe-named article title,
/// so `_find_downloaded_file` (and `find_downloaded_file`)
/// can locate the result by prefix.
pub fn download_with_ytdlp(
    url: &str,
    dir: &std::path::Path,
    format_id: &str,
    title: &str,
) -> Result<PathBuf, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let cookies = ytdlp_cookies_file();
    download_with_ytdlp_using(&exe, url, dir, format_id, title, cookies.as_deref())
}

/// `download_with_ytdlp` against an explicit executable and
/// cookie file (the seam the regression tests drive).
pub fn download_with_ytdlp_using(
    exe: &std::path::Path,
    url: &str,
    dir: &std::path::Path,
    format_id: &str,
    title: &str,
    cookie_file: Option<&str>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("download dir: {e}"))?;
    let base_name = safe_download_name(title);
    let template = dir
        .join(format!("{base_name}.%(ext)s"))
        .to_string_lossy()
        .to_string();
    let audio_only = is_audio_only(format_id);
    let ffmpeg = crate::dependency::find_tool("ffmpeg");
    let user_agent = std::env::var("BLINDRSS_USER_AGENT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    crate::ytdlp_recovery::run_recovery(url, cookie_file, |phase, attempt| {
        // Python's per-attempt merge loop: video presets try
        // mp4, then mkv ONLY when the mp4 attempt's error says
        // the conversion failed.
        let merges: [&str; 2] = if audio_only { ["", ""] } else { ["mp4", "mkv"] };
        let merge_count = if audio_only { 1 } else { 2 };
        let mut attempt_err = String::new();
        for (i, merge) in merges.iter().take(merge_count).enumerate() {
            if i > 0
                && !attempt_err
                    .to_ascii_lowercase()
                    .contains("conversion failed")
            {
                break;
            }
            let mut args: Vec<String> = vec![
                "--no-playlist".into(),
                "--no-warnings".into(),
                "--no-progress".into(),
                "--no-color".into(),
                "--geo-bypass".into(),
                "--extractor-args".into(),
                crate::ytdlp_recovery::youtube_player_client_arg(phase.clients, None),
            ];
            args.extend(ytdlp_format_args_with_merge(format_id, merge));
            args.push("--print".into());
            args.push("after_move:filepath".into());
            args.push("-o".into());
            args.push(template.clone());
            if let Some(ffmpeg) = &ffmpeg {
                args.push("--ffmpeg-location".into());
                args.push(ffmpeg.to_string_lossy().to_string());
            }
            if let Some(ua) = &user_agent {
                args.push("--user-agent".into());
                args.push(ua.clone());
            }
            if phase.impersonate {
                args.push("--impersonate".into());
                args.push("chrome".into());
            }
            if !matches!(attempt, crate::ytdlp_recovery::CookieAttempt::Base) {
                args.extend(attempt.args());
            }
            args.push(phase.target_url.clone());
            let arg_refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
            match crate::dependency::run_tool(exe, &arg_refs, std::time::Duration::from_secs(1800))
            {
                Ok(run) if run.success => {
                    return match downloaded_output_path(&run.stdout) {
                        Ok(p) => crate::ytdlp_recovery::AttemptOutcome::Success(p),
                        Err(e) => crate::ytdlp_recovery::AttemptOutcome::Failed {
                            error: e,
                            fatal: false,
                        },
                    };
                }
                Ok(run) => {
                    attempt_err = run.stderr.trim().to_string();
                }
                Err(e) => {
                    let fatal = e.contains("timed out");
                    return crate::ytdlp_recovery::AttemptOutcome::Failed { error: e, fatal };
                }
            }
        }
        crate::ytdlp_recovery::AttemptOutcome::Failed {
            error: attempt_err,
            fatal: false,
        }
    })
}

fn downloaded_output_path(stdout: &str) -> Result<PathBuf, String> {
    stdout
        .lines()
        .rev()
        .find_map(existing_local_media_path)
        .ok_or_else(|| "yt-dlp finished without a completed media file".to_string())
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

/// Fetch a YouTube page's full text (yt-dlp info for the
/// description/chapters, and the subtitle file for the
/// transcript) — through the ONE shared pipeline (Audit 8
/// Task 16): `ytfull`'s extraction with the
/// comment-completeness retry, the shared subtitle
/// downloader, and the shared document builder, exactly
/// what the plain and rich readers show. This module used
/// to carry a second, divergent implementation (12 subtitle
/// attempts, no extracted headers, no 429 stop, no comment
/// retry); it is gone.
pub fn youtube_full_text(url: &str) -> Result<String, String> {
    youtube_full_text_in_language(url, "en")
}

/// Full YouTube article in Python's reading order, with
/// language-ranked subtitles.
pub fn youtube_full_text_in_language(url: &str, ui_language: &str) -> Result<String, String> {
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    youtube_full_text_with(&exe, url, ui_language)
}

/// `youtube_full_text_in_language` against an explicit
/// executable (the seam the regression tests drive with a
/// fake yt-dlp).
pub fn youtube_full_text_with(exe: &Path, url: &str, ui_language: &str) -> Result<String, String> {
    let info = crate::ytfull::extract_video_info_with(exe, url)?;
    // An empty info dict means there is no video document
    // (Python extract_article returns nothing for it); the
    // failure reads the same as the plain reader's.
    if info.as_object().is_some_and(|o| o.is_empty()) {
        return Err("Could not retrieve YouTube video details.".to_string());
    }
    let (_title, _author, text) =
        crate::ytfull::article_fields_from_info_in_language(&info, url, ui_language);
    Ok(text)
}

#[cfg(test)]
mod parity_tests {
    use super::*;

    #[test]
    fn ytdlp_reports_exact_completed_file_and_rejects_directory_or_partial() {
        let dir = std::env::temp_dir().join(format!("blindrss-output-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("done.mp3");
        let partial = dir.join("pending.mp3.part");
        std::fs::write(&file, b"audio").unwrap();
        std::fs::write(&partial, b"partial").unwrap();
        assert_eq!(
            downloaded_output_path(&format!("[download] done\n{}\n", file.display())).unwrap(),
            file
        );
        assert!(downloaded_output_path(&dir.to_string_lossy()).is_err());
        assert!(downloaded_output_path(&partial.to_string_lossy()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
