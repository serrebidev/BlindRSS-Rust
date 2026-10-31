//! On-disk playback cache (Python `core/play_cache.py` +
//! the youtube_play_cache_* settings).
//!
//! When `youtube_play_via_download` is on, YouTube audio is
//! downloaded once into the cache and played from disk —
//! slower to start the first time, reliable afterwards, and
//! resumable by the player like any local file. The cache
//! prunes oldest-first past `youtube_play_cache_max_mb`.

use std::path::PathBuf;

/// The cache directory: the configured one, else
/// <data_dir>/cache/play (the caller supplies the data dir).
pub fn cache_dir(configured: &str, data_dir: &std::path::Path) -> PathBuf {
    let configured = configured.trim();
    if configured.is_empty() {
        data_dir.join("cache").join("play")
    } else {
        PathBuf::from(configured)
    }
}

fn key_for(url: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:x}")
}

/// Files currently in the cache with their total size.
fn entries(dir: &std::path::Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Ok(md) = entry.metadata()
            {
                out.push((
                    path,
                    md.len(),
                    md.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                ));
            }
        }
    }
    out
}

/// Delete oldest files until the cache is within `max_mb`
/// (0 or negative = unlimited). Returns bytes freed.
pub fn prune(dir: &std::path::Path, max_mb: i64) -> u64 {
    if max_mb <= 0 {
        return 0;
    }
    let cap = (max_mb as u64) * 1024 * 1024;
    let mut files = entries(dir);
    let mut total: u64 = files.iter().map(|(_, n, _)| n).sum();
    if total <= cap {
        return 0;
    }
    files.sort_by_key(|(_, _, t)| *t);
    let mut freed = 0;
    for (path, size, _) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
            freed += size;
        }
    }
    freed
}

/// Return the cached file for a YouTube URL, downloading the
/// audio (yt-dlp, audio_best format) on first use.
pub fn fetch_youtube_audio(
    url: &str,
    dir: &std::path::Path,
    max_mb: i64,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cache dir: {e}"))?;
    let key = key_for(url);
    // A completed download is named <key>.<ext>; a partial
    // download carries .part and is ignored.
    for (path, _, _) in entries(dir) {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && stem == key
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e != "part")
                .unwrap_or(false)
        {
            return Ok(path);
        }
    }
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let template = dir.join(format!("{key}.%(ext)s"));
    let template = template.to_string_lossy().to_string();
    let format_args = crate::download::ytdlp_format_args("audio_best");
    let mut args: Vec<&str> = vec!["--no-warnings", "--no-playlist", "-o", &template];
    args.extend(format_args.iter().map(|a| a.as_str()));
    args.push(url);
    let out = std::process::Command::new(&exe)
        .args(&args)
        .output()
        .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().last().unwrap_or("unknown error");
        return Err(format!("cache download: {first}"));
    }
    prune(dir, max_mb);
    entries(dir)
        .into_iter()
        .find(|(path, _, _)| path.file_stem().and_then(|s| s.to_str()) == Some(key.as_str()))
        .map(|(path, _, _)| path)
        .ok_or_else(|| "cache download produced no file".to_string())
}

/// Clear the whole cache; returns how many files went.
pub fn clear(dir: &std::path::Path) -> usize {
    let files = entries(dir);
    let n = files.len();
    for (path, _, _) in files {
        let _ = std::fs::remove_file(path);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_respects_cap() {
        let dir = std::env::temp_dir().join("blindrss_play_cache_test");
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
        let remaining: u64 = entries(&dir).iter().map(|(_, n, _)| n).sum();
        assert!(remaining <= 1024 * 1024);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
