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
    let name = sanitize_filename(title);
    let mut path = dir.join(format!("{name}.{ext}"));
    if path.exists() {
        path = dir.join(format!("{name}-{}.{ext}", uuid::Uuid::new_v4()));
    }
    let mut resp = crate::http::agent()
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
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
        // Publish without replacing an existing download, including a concurrent writer's file.
        if let Err(error) = std::fs::hard_link(&partial, &path) {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                return Err("A download with this name already exists".into());
            }
            // FAT/exFAT destinations have no hard links; reserve the output before copying.
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("finish download: {e}"))?;
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
        }
        Ok::<(), String>(())
    })();
    let _ = std::fs::remove_file(&partial);
    result?;
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
/// format from config `download_format`. Returns the completed output file.
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
    let mut args: Vec<&str> = vec![
        "--no-warnings",
        "--no-playlist",
        "--no-progress",
        "--print",
        "after_move:filepath",
        "-o",
        &template,
    ];
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
    downloaded_output_path(&String::from_utf8_lossy(&out.stdout))
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
    youtube_full_text_in_language(url, "en")
}

/// Full YouTube article in Python's reading order, with language-ranked subtitles.
pub fn youtube_full_text_in_language(url: &str, ui_language: &str) -> Result<String, String> {
    let raw = crate::media::youtube_article_info(url)?;
    let info = crate::media::parse_info(&raw.to_string())?;
    let mut transcript = None;
    for (language, url, ext) in subtitle_candidates(&raw, ui_language).into_iter().take(12) {
        let Ok(mut response) = crate::http::agent().get(&url).call() else {
            continue;
        };
        let Ok(body) = response
            .body_mut()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_to_string()
        else {
            continue;
        };
        let text = if ext == "json3" {
            serde_json::from_str(&body)
                .map(|v| json3_to_text(&v))
                .unwrap_or_default()
        } else {
            clean_subtitle_text(&vtt_to_text(&body))
        };
        if !text.is_empty() {
            transcript = Some((language, text));
            break;
        }
    }
    if info.description.trim().is_empty()
        && info.chapters.is_empty()
        && transcript.is_none()
        && raw
            .get("comments")
            .and_then(|v| v.as_array())
            .is_none_or(|v| v.is_empty())
    {
        return Err("no description or transcript available".to_string());
    }
    let mut sections = vec![
        "Description".to_string(),
        if info.description.trim().is_empty() {
            "No video description was provided.".to_string()
        } else {
            info.description.trim().to_string()
        },
    ];
    let chapters = compose_youtube_text("", &info.chapters, "");
    if chapters.is_empty() {
        sections.extend(["Chapters".into(), "No chapters were provided.".into()]);
    } else {
        sections.push(chapters);
    }
    if let Some((language, text)) = transcript {
        sections.extend([format!("Subtitles ({language})"), text]);
    } else {
        sections.extend(["Subtitles".into(), "No subtitles were available.".into()]);
    }
    sections.push("Comments".into());
    let comments = youtube_comments_text(raw.get("comments").unwrap_or(&serde_json::Value::Null));
    sections.push(if comments.is_empty() {
        "No public comments were available.".into()
    } else {
        comments
    });
    Ok(sections.join("\n\n"))
}

fn language_match(code: &str, wanted: &str) -> Option<u8> {
    let code = code.to_ascii_lowercase().replace('_', "-");
    let code = code.strip_suffix("-orig").unwrap_or(&code);
    let wanted = wanted.to_ascii_lowercase().replace('_', "-");
    if code.is_empty() || wanted.is_empty() {
        return None;
    }
    if code == wanted {
        return Some(0);
    }
    let script = match wanted.as_str() {
        "zh-cn" | "zh-sg" => "zh-hans",
        "zh-tw" | "zh-hk" | "zh-mo" => "zh-hant",
        _ => "",
    };
    if code == script {
        return Some(1);
    }
    let base = wanted.split('-').next().unwrap_or("");
    if code == base {
        Some(2)
    } else if code.split('-').next() == Some(base) {
        Some(3)
    } else {
        None
    }
}

fn subtitle_candidates(
    info: &serde_json::Value,
    ui_language: &str,
) -> Vec<(String, String, String)> {
    let original = info
        .get("automatic_captions")
        .and_then(|v| v.as_object())
        .and_then(|v| v.keys().find_map(|k| k.strip_suffix("-orig")))
        .unwrap_or("");
    let mut candidates = Vec::new();
    for (kind_rank, key) in ["subtitles", "automatic_captions"].iter().enumerate() {
        let Some(tracks) = info.get(key).and_then(|v| v.as_object()) else {
            continue;
        };
        for (language, formats) in tracks {
            let Some(formats) = formats.as_array() else {
                continue;
            };
            let track = formats
                .iter()
                .filter(|v| {
                    v.get("url").and_then(|v| v.as_str()).is_some()
                        && v.get("protocol")
                            .is_none_or(|v| v.is_null() || v.as_str() == Some(""))
                })
                .min_by_key(|v| match v.get("ext").and_then(|v| v.as_str()) {
                    Some("json3") => 0,
                    Some("vtt") => 1,
                    _ => 2,
                });
            let Some(track) = track else {
                continue;
            };
            let url = track["url"].as_str().unwrap_or("");
            let translation = url::Url::parse(url)
                .ok()
                .and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "tlang")
                        .map(|(_, v)| v.into_owned())
                })
                .unwrap_or_default();
            let rank = if !translation.is_empty() {
                match language_match(&translation, ui_language) {
                    Some(m) => (2, m),
                    None => continue,
                }
            } else if let Some(m) = language_match(language, ui_language) {
                (0, m)
            } else if let Some(m) = language_match(language, "en") {
                (1, m)
            } else {
                (
                    if language_match(language, original).is_some() {
                        3
                    } else {
                        4
                    },
                    0,
                )
            };
            candidates.push((
                rank,
                kind_rank,
                language.to_string(),
                url.to_string(),
                track["ext"].as_str().unwrap_or("vtt").to_string(),
            ));
        }
    }
    candidates.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    candidates
        .into_iter()
        .map(|(_, _, lang, url, ext)| {
            (
                lang.strip_suffix("-orig").unwrap_or(&lang).to_string(),
                url,
                ext,
            )
        })
        .collect()
}

fn clean_subtitle_text(text: &str) -> String {
    crate::extract::html_to_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches(['>', ' '])
        .to_string()
}

fn json3_to_text(payload: &serde_json::Value) -> String {
    let mut kept = Vec::new();
    for event in payload
        .get("events")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let text = event
            .get("segs")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.get("utf8").and_then(|v| v.as_str()))
            .collect::<String>();
        let text = clean_subtitle_text(&text);
        if !text.is_empty() && kept.last() != Some(&text) {
            kept.push(text);
        }
    }
    kept.join(" ")
}

fn youtube_comments_text(value: &serde_json::Value) -> String {
    let Some(comments) = value.as_array() else {
        return String::new();
    };
    let known: std::collections::HashSet<&str> =
        comments.iter().filter_map(|v| v["id"].as_str()).collect();
    let mut children: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    let mut roots = Vec::new();
    for (index, comment) in comments.iter().enumerate() {
        let parent = comment["parent"].as_str().unwrap_or("root");
        if parent == "root" || !known.contains(parent) {
            roots.push(index);
        } else {
            children.entry(parent).or_default().push(index);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    // Include orphaned/cyclic comments after the ordinary root threads.
    for root in roots.into_iter().chain(0..comments.len()) {
        let mut stack = vec![(root, 0usize)];
        while let Some((index, depth)) = stack.pop() {
            if !seen.insert(index) {
                continue;
            }
            let comment = &comments[index];
            let author = comment["author"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("Unknown author");
            let label = if depth == 0 {
                "Comment".to_string()
            } else {
                format!("Reply level {depth}")
            };
            lines.push(format!(
                "{label} by {author}:\n{}",
                comment["text"].as_str().unwrap_or("")
            ));
            if let Some(children) = comment["id"].as_str().and_then(|id| children.get(id)) {
                stack.extend(children.iter().rev().map(|i| (*i, depth + 1)));
            }
        }
    }
    lines.join("\n\n")
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subtitle_candidates_prefer_ui_then_english_and_exclude_other_translations() {
        let info = json!({
            "subtitles": {"es": [{"url":"https://x/es", "ext":"vtt"}], "en": [{"url":"https://x/en", "ext":"vtt"}]},
            "automatic_captions": {
                "fr": [{"url":"https://x/fr", "ext":"json3"}],
                "de": [{"url":"https://x/de?tlang=de", "ext":"vtt"}],
                "zh-orig": [{"url":"https://x/zh", "ext":"vtt"}],
                "live_chat": [{"url":"https://x/chat", "protocol":"youtube_live_chat"}]
            }
        });
        let candidates = subtitle_candidates(&info, "fr-CA");
        assert_eq!(
            candidates.iter().map(|v| v.0.as_str()).collect::<Vec<_>>(),
            ["fr", "en", "zh", "es"]
        );
    }

    #[test]
    fn json3_subtitles_decode_entities_and_remove_speaker_markers() {
        let body = json!({"events":[
            {"segs":[{"utf8":">> Hello &amp; welcome"}]},
            {"segs":[{"utf8":">> Hello &amp; welcome"}]},
            {"segs":[{"utf8":"x > 2"}]}
        ]});
        assert_eq!(json3_to_text(&body), "Hello & welcome x > 2");
    }

    #[test]
    fn youtube_comments_keep_reply_order_and_break_cycles() {
        let comments = json!([
            {"id":"reply", "parent":"root-post", "author":"Reply", "text":"Second"},
            {"id":"root-post", "parent":"root", "author":"Author", "text":"First"},
            {"id":"cycle", "parent":"cycle", "author":"Cycle", "text":"Only once"}
        ]);
        let text = youtube_comments_text(&comments);
        assert!(text.find("First").unwrap() < text.find("Second").unwrap());
        assert!(text.contains("Reply level 1 by Reply:"));
        assert_eq!(text.matches("Only once").count(), 1);
    }

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
