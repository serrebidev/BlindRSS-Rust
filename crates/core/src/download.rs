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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DownloadPreset {
    /// Audio only, mp3 (Python "mp3" format).
    AudioMp3,
    /// Best single file (Python default).
    Best,
    /// Best video+audio merged to mp4, capped at 720p.
    VideoMp4,
}

/// Download a page URL with yt-dlp into `dir`. Returns the
/// directory (yt-dlp names the file itself from the title).
pub fn download_with_ytdlp(
    url: &str,
    dir: &std::path::Path,
    preset: DownloadPreset,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("download dir: {e}"))?;
    let exe = crate::media::find_ytdlp().ok_or("yt-dlp not found")?;
    let template = dir.join("%(title)s.%(ext)s");
    let template = template.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["--no-warnings", "--no-playlist", "-o", &template];
    match preset {
        DownloadPreset::AudioMp3 => {
            args.extend([
                "-f",
                "bestaudio/best",
                "--extract-audio",
                "--audio-format",
                "mp3",
            ]);
        }
        DownloadPreset::Best => {
            args.extend(["-f", "best"]);
        }
        DownloadPreset::VideoMp4 => {
            args.extend([
                "-f",
                "bestvideo[height<=720]+bestaudio/best[height<=720]",
                "--merge-output-format",
                "mp4",
            ]);
        }
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
