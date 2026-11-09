//! On-disk playback cache (Python `core/play_cache.py` +
//! the youtube_play_cache_* settings).
//!
//! When `youtube_play_via_download` is on, YouTube audio is
//! downloaded once into the cache and played from disk —
//! slower to start the first time, reliable afterwards, and
//! resumable by the player like any local file. Layout and
//! accounting match Python exactly (Audit 8 Task 22, M17):
//! the directory is `ytplay_cache` beside the data directory
//! unless configured, files are named by media ID
//! (`<id>.<ext>`, the `%(id)s.%(ext)s` template), and partial
//! sidecars (`.part`, `.ytdl`, `.tmp`) never count as cache
//! entries — a cache copied from the Python app is reused
//! offline, file for file.

use std::path::PathBuf;

/// Python `DEFAULT_DIRNAME`.
pub const DEFAULT_DIRNAME: &str = "ytplay_cache";

/// Suffixes of partial downloads (`_SKIP_SUFFIXES`): excluded
/// from playback, size, count and pruning accounting.
const SKIP_SUFFIXES: [&str; 3] = [".part", ".ytdl", ".tmp"];

fn is_partial(path: &std::path::Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    SKIP_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// The cache directory (Python `default_cache_dir` +
/// the `youtube_play_cache_dir` config key): the configured
/// one, else `<data_dir>/ytplay_cache`.
pub fn cache_dir(configured: Option<&std::path::Path>, data_dir: &std::path::Path) -> PathBuf {
    match configured {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => data_dir.join(DEFAULT_DIRNAME),
    }
}

struct CacheFile {
    path: PathBuf,
    size: u64,
    mtime: std::time::SystemTime,
}

fn list_files(dir: &std::path::Path, completed_only: bool) -> Vec<CacheFile> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if completed_only && is_partial(&path) {
                continue;
            }
            if let Ok(md) = entry.metadata() {
                out.push(CacheFile {
                    path,
                    size: md.len(),
                    mtime: md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                });
            }
        }
    }
    out
}

/// Completed cache files (`_cache_files`).
fn cache_files(dir: &std::path::Path) -> Vec<CacheFile> {
    list_files(dir, true)
}

/// Total size of the completed cache files.
pub fn size_bytes(dir: &std::path::Path) -> u64 {
    cache_files(dir).iter().map(|f| f.size).sum()
}

/// Number of completed cache files.
pub fn file_count(dir: &std::path::Path) -> usize {
    cache_files(dir).len()
}

/// Python `human_size`: bytes as an integer, larger units at
/// one decimal.
pub fn human_size(value: u64) -> String {
    let mut v = value as f64;
    for unit in ["B", "KB", "MB", "GB", "TB", "PB"] {
        if v < 1024.0 || unit == "PB" {
            if unit == "B" {
                return format!("{value} B");
            }
            return format!("{v:.1} {unit}");
        }
        v /= 1024.0;
    }
    unreachable!()
}

/// Delete the oldest completed files until the cache is
/// within `max_bytes` (Python `prune_cache` with a byte cap).
/// Returns bytes freed.
pub fn prune_once(dir: &std::path::Path, max_bytes: u64) -> u64 {
    let mut files = cache_files(dir);
    let mut total: u64 = files.iter().map(|f| f.size).sum();
    if total <= max_bytes {
        return 0;
    }
    // Newest first, drop from the tail (oldest) like Python.
    files.sort_by_key(|f| std::cmp::Reverse(f.mtime));
    let mut freed = 0;
    while total > max_bytes {
        let Some(f) = files.pop() else { break };
        if std::fs::remove_file(&f.path).is_ok() {
            total = total.saturating_sub(f.size);
            freed += f.size;
        }
    }
    freed
}

/// `prune_once` with Python's MB-shaped setting
/// (`youtube_play_cache_max_mb`; 0 or negative = unlimited).
pub fn prune(dir: &std::path::Path, max_mb: i64) -> u64 {
    if max_mb <= 0 {
        return 0;
    }
    prune_once(dir, (max_mb as u64) * 1024 * 1024)
}

/// Remove EVERY file in the cache, partials included
/// (Python `clear_cache`); returns (files removed, bytes freed).
pub fn clear_cache(dir: &std::path::Path) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    for f in list_files(dir, false) {
        if std::fs::remove_file(&f.path).is_ok() {
            removed += 1;
            freed += f.size;
        }
    }
    (removed, freed)
}

/// The YouTube media ID for a URL, when it has one.
pub fn youtube_media_id(url: &str) -> Option<String> {
    crate::sponsorblock::youtube_video_id(url)
}

/// The completed cache file for a YouTube URL, if a previous
/// download (by this app or by Python — the layout is shared)
/// already landed it. Partial sidecars are never a hit.
pub fn fetch_cached_audio(dir: &std::path::Path, url: &str) -> Option<PathBuf> {
    let id = youtube_media_id(url)?;
    cache_files(dir)
        .into_iter()
        .find(|f| f.path.file_stem().and_then(|s| s.to_str()) == Some(id.as_str()))
        .map(|f| f.path)
}

/// Return the cached file for a YouTube URL, downloading the
/// audio on first use through the shared recovery policy
/// (M16) with Python's download-to-play command shape
/// (player.py `_ytdlp_download_command`): media-ID template,
/// VLC audio format, client pools, cookie attempts,
/// `--print after_move:filepath`, 900 s timeout.
pub fn fetch_youtube_audio(
    url: &str,
    dir: &std::path::Path,
    max_mb: i64,
) -> Result<PathBuf, String> {
    if let Some(hit) = fetch_cached_audio(dir, url) {
        return Ok(hit);
    }
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let cookie_file = std::env::var("BLINDRSS_YTDLP_COOKIES")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    fetch_youtube_audio_using(&exe, url, dir, max_mb, cookie_file.as_deref())
}

/// `fetch_youtube_audio` against an explicit executable and
/// cookie file (the seam the regression tests drive).
pub fn fetch_youtube_audio_using(
    exe: &std::path::Path,
    url: &str,
    dir: &std::path::Path,
    max_mb: i64,
    cookie_file: Option<&str>,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cache dir: {e}"))?;
    if let Some(hit) = fetch_cached_audio(dir, url) {
        return Ok(hit);
    }
    if youtube_media_id(url).is_none() {
        return Err("not a YouTube video URL".to_string());
    }
    let template = dir.join("%(id)s.%(ext)s").to_string_lossy().to_string();
    let ffmpeg = crate::dependency::find_tool("ffmpeg");
    let user_agent = std::env::var("BLINDRSS_USER_AGENT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let path = crate::ytdlp_recovery::run_recovery(url, cookie_file, |phase, attempt| {
        let mut args: Vec<String> = vec![
            "--no-playlist".into(),
            "--no-warnings".into(),
            "--no-progress".into(),
            "--no-color".into(),
            "--geo-bypass".into(),
            "--extractor-args".into(),
            crate::ytdlp_recovery::youtube_player_client_arg(
                phase.clients,
                phase.visitor_data.as_deref(),
            ),
            "-f".into(),
            crate::ytdlp_recovery::vlc_audio_format(&phase.target_url).to_string(),
        ];
        if let Some(ffmpeg) = &ffmpeg {
            args.push("--ffmpeg-location".into());
            args.push(ffmpeg.to_string_lossy().to_string());
        }
        args.push("--print".into());
        args.push("after_move:filepath".into());
        args.push("-o".into());
        args.push(template.clone());
        // The bootstrap phase carries the browser UA the
        // cookie jar was warmed with; every other phase keeps
        // the configured fetch identity.
        if let Some(ua) = phase.browser_user_agent.as_ref().or(user_agent.as_ref()) {
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
        match crate::dependency::run_tool(exe, &arg_refs, std::time::Duration::from_secs(900)) {
            Ok(run) if run.success => match resolve_printed_filepath(&run.stdout, dir) {
                Some(p) => crate::ytdlp_recovery::AttemptOutcome::Success(p),
                None => crate::ytdlp_recovery::AttemptOutcome::Failed {
                    error: "yt-dlp download produced no file".to_string(),
                    fatal: false,
                },
            },
            Ok(run) => crate::ytdlp_recovery::AttemptOutcome::Failed {
                error: run.stderr.trim().to_string(),
                fatal: false,
            },
            Err(e) => {
                let fatal = e.contains("timed out");
                crate::ytdlp_recovery::AttemptOutcome::Failed { error: e, fatal }
            }
        }
    })?;
    prune(dir, max_mb);
    Ok(path)
}

/// Python `_resolve_printed_filepath`: the last stdout line
/// that names an existing file; otherwise the newest completed
/// file in the directory.
fn resolve_printed_filepath(stdout: &str, dir: &std::path::Path) -> Option<PathBuf> {
    for line in stdout.lines().rev() {
        let candidate = std::path::Path::new(line.trim());
        if candidate.is_file() {
            return Some(candidate.to_path_buf());
        }
    }
    let mut files = cache_files(dir);
    files.sort_by_key(|f| f.mtime);
    files.pop().map(|f| f.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_respects_cap() {
        let dir =
            std::env::temp_dir().join(format!("blindrss_play_cache_test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Three 600 KB files, cap 1 MB -> the oldest go.
        // (Written in order, so mtimes ascend a < b < c.)
        for name in ["a.mp3", "b.mp3", "c.mp3"] {
            std::fs::write(dir.join(name), vec![0u8; 600_000]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let freed = prune(&dir, 1);
        assert!(freed >= 600_000, "freed {freed}");
        let remaining = size_bytes(&dir);
        assert!(remaining <= 1024 * 1024);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn human_size_matches_python() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
