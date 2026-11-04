//! Chapter discovery and storage (Audit 8 M12): the port of
//! Python's `core/utils.py` chapter pipeline and the local
//! provider's `get_article_chapters`.
//!
//! Chapters for an article come from, in Python's order: rows
//! already stored; the Podcasting 2.0 chapter file named by the
//! feed (`chapter_url`), conditionally fetched and revalidated
//! against the `chapter_sources` record with a 15-minute
//! freshness window; YouTube's structured chapter metadata; and
//! chapters embedded in the media file itself (ID3 CHAP frames
//! for MP3 — local files directly, remote files by bounded
//! prefix). Fetched chapters are stored back so every surface
//! (article rows, the player, the queue) sees one list.
//!
//! The network half sits behind [`ChapterTransport`] so the
//! pipeline's branching is testable offline; the production
//! transport rides the shared HTTP agent with Python's finite
//! timeouts and the chapter document's size and shape limits.

use crate::Db;
use crate::db::Chapter;

/// Python `_CHAPTER_REFRESH_SECONDS`: a stored chapter file is
/// trusted for fifteen minutes before it is revalidated.
pub const CHAPTER_REFRESH_SECONDS: f64 = 900.0;

/// Python `_MAX_CHAPTER_COUNT`: input cap applied before any
/// filtering, so a hostile document cannot make normalization
/// scan unbounded entries.
pub const MAX_CHAPTER_COUNT: usize = 10_000;

/// Python `_MAX_CHAPTER_JSON_BYTES`.
pub const MAX_CHAPTER_JSON_BYTES: usize = 2_000_000;

/// One scripted chapter-document response.
pub enum ChapterJsonFetch {
    /// HTTP 304: the validators still match. Carries the
    /// response's own validators when it sent any (Python falls
    /// back to the request's).
    NotModified {
        etag: Option<String>,
        last_modified: Option<String>,
    },
    /// A document body to validate and parse.
    Document {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// The network surface of the chapter pipeline.
pub trait ChapterTransport {
    /// Conditionally GET a chapter document. Implementations
    /// must enforce Python's limits (finite timeout, compatible
    /// content type, 2 MB cap) and report 304 as
    /// [`ChapterJsonFetch::NotModified`] rather than an error.
    fn get_chapter_json(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<ChapterJsonFetch, String>;

    /// Chapters from YouTube's structured metadata for a watch
    /// URL (Python `youtube_fulltext.extract_chapters`).
    fn youtube_chapters(&self, url: &str) -> Vec<Chapter>;
}

/// Production transport over the shared HTTP agent.
pub struct HttpChapterTransport;

impl ChapterTransport for HttpChapterTransport {
    fn get_chapter_json(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<ChapterJsonFetch, String> {
        fetch_chapter_json_http(url, etag, last_modified)
    }

    fn youtube_chapters(&self, url: &str) -> Vec<Chapter> {
        // Python tries the watch-next endpoint first and falls
        // back to the yt-dlp info dict; the Rust yt-dlp info
        // path (`chapters_from_info`: the info's own chapter
        // list, else the description timestamp grammar) is the
        // same source the Rust YouTube article reader uses.
        let info = crate::media::youtube_article_info(url).ok();
        let Some(info) = info else {
            return Vec::new();
        };
        crate::ytfull::chapters_from_info(&info, url)
            .into_iter()
            .map(|c| Chapter {
                start: c.start,
                title: Some(c.title),
                href: if c.href.is_empty() {
                    None
                } else {
                    Some(c.href)
                },
            })
            .collect()
    }
}

/// Inputs for one chapter fetch (Python `fetch_and_store_chapters`).
pub struct ChapterRequest<'a> {
    pub article_id: &'a str,
    pub media_url: Option<&'a str>,
    pub media_type: Option<&'a str>,
    pub chapter_url: Option<&'a str>,
    pub allow_id3: bool,
    pub cache_key: Option<&'a str>,
    pub force_refresh: bool,
}

/// Python `_chapter_source_key`: hosted rows key by their cache
/// key; local rows key by `local:{article_id}`.
pub fn chapter_source_key(article_key: Option<&str>, cache_key: Option<&str>) -> Option<String> {
    let storage = cache_key.or(article_key).unwrap_or("").trim();
    if storage.is_empty() {
        return None;
    }
    Some(if cache_key.is_some() {
        storage.to_string()
    } else {
        format!("local:{storage}")
    })
}

/// Python `_chapter_start_seconds`: a finite, non-negative
/// chapter timestamp from a number or an `MM:SS` / `H:MM:SS`
/// string. Booleans and nulls are not timestamps.
pub fn chapter_start_seconds(value: &serde_json::Value) -> Option<f64> {
    use serde_json::Value;
    match value {
        Value::Bool(_) | Value::Null => None,
        Value::Number(n) => {
            let result = n.as_f64()?;
            if result.is_finite() && result >= 0.0 {
                Some(result)
            } else {
                None
            }
        }
        other => {
            let text = match other {
                Value::String(s) => s.trim().to_string(),
                _ => other.to_string(),
            };
            if text.is_empty() {
                return None;
            }
            if !text.contains(':') {
                let result: f64 = text.parse().ok()?;
                return if result.is_finite() && result >= 0.0 {
                    Some(result)
                } else {
                    None
                };
            }
            let parts: Vec<&str> = text.split(':').collect();
            if parts.len() != 2 && parts.len() != 3 {
                return None;
            }
            let mut values = Vec::new();
            for part in &parts {
                let part = part.trim();
                if part.is_empty() {
                    return None;
                }
                let v: f64 = part.parse().ok()?;
                if !v.is_finite() || v < 0.0 {
                    return None;
                }
                values.push(v);
            }
            if values[1..].iter().any(|v| *v >= 60.0) {
                return None;
            }
            let mut result = 0.0f64;
            for (index, part) in values.iter().rev().enumerate() {
                result += part * 60f64.powi(index as i32);
            }
            Some(result)
        }
    }
}

fn chapter_text(value: Option<&serde_json::Value>, empty: Option<String>) -> Option<String> {
    match value {
        None | Some(serde_json::Value::Null) => empty,
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// The (start, title, href) triple normalization works on.
struct RawChapter {
    start: f64,
    title: String,
    href: Option<String>,
}

/// Python `_normalize_chapters` over already-extracted triples:
/// sort by start, then merge equal starts in source order — a
/// later duplicate may supply a missing title or href but never
/// replaces one already present.
fn normalize_raw(raw: Vec<RawChapter>) -> Vec<Chapter> {
    let mut raw = raw;
    raw.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out: Vec<Chapter> = Vec::new();
    for item in raw {
        if let Some(last) = out.last_mut()
            && last.start == item.start
        {
            if last.title.as_deref().unwrap_or("").is_empty() && !item.title.is_empty() {
                last.title = Some(item.title);
            }
            if last.href.as_deref().unwrap_or("").is_empty()
                && item.href.as_deref().is_some_and(|h| !h.is_empty())
            {
                last.href = item.href;
            }
            continue;
        }
        out.push(Chapter {
            start: item.start,
            title: Some(item.title),
            href: item.href,
        });
    }
    out
}

/// Python `_normalize_chapters` over raw chapter mappings (the
/// Podcasting 2.0 document shape and yt-dlp's chapter dicts).
pub fn normalize_chapters(chapters: &[serde_json::Value]) -> Vec<Chapter> {
    let mut raw = Vec::new();
    for chapter in chapters.iter().take(MAX_CHAPTER_COUNT) {
        let Some(obj) = chapter.as_object() else {
            continue;
        };
        if obj.get("toc") == Some(&serde_json::Value::Bool(false)) {
            continue;
        }
        // The first PRESENT start key wins, even when its value
        // is unusable — Python does not fall through to the next.
        let start_value = ["startTime", "start_time", "start"]
            .iter()
            .find_map(|key| obj.get(*key));
        let Some(start) = start_value.and_then(chapter_start_seconds) else {
            continue;
        };
        let title = chapter_text(obj.get("title"), Some(String::new())).unwrap_or_default();
        let mut href_value = obj.get("url");
        if href_value.is_none() || href_value == Some(&serde_json::Value::String(String::new())) {
            href_value = obj.get("link");
        }
        if href_value.is_none() || href_value == Some(&serde_json::Value::String(String::new())) {
            href_value = obj.get("href");
        }
        let href = chapter_text(href_value, None);
        raw.push(RawChapter { start, title, href });
    }
    normalize_raw(raw)
}

/// Normalize already-parsed (start, title, href) triples — the
/// ID3 and YouTube extractors' output shape.
pub fn normalize_triples(triples: Vec<(f64, String, Option<String>)>) -> Vec<Chapter> {
    normalize_raw(
        triples
            .into_iter()
            .filter(|(start, _, _)| start.is_finite() && *start >= 0.0)
            .map(|(start, title, href)| RawChapter { start, title, href })
            .collect(),
    )
}

/// Python `_validate_chapter_document` + parse: the body must
/// be a JSON object (a UTF-8 BOM is tolerated) with a non-empty
/// string `version` and a `chapters` list.
pub fn parse_chapter_document(bytes: &[u8]) -> Result<Vec<Chapter>, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let doc: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("chapter document is not JSON: {e}"))?;
    let obj = doc
        .as_object()
        .ok_or_else(|| "chapter document root is not an object".to_string())?;
    let version = obj.get("version").and_then(|v| v.as_str()).unwrap_or("");
    if version.is_empty() {
        return Err("chapter document has no version".to_string());
    }
    let chapters = obj
        .get("chapters")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "chapter document has no chapters list".to_string())?;
    Ok(normalize_chapters(chapters))
}

/// Python `_fetch_chapter_json` over the shared agent: the
/// chapter Accept header, conditional validators, Python's
/// (5 s connect, 10 s read) timeouts, the compatible
/// content-type set, and the 2 MB body cap. A 304 is a result,
/// not an error.
fn fetch_chapter_json_http(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<ChapterJsonFetch, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(5)))
        .timeout_recv_body(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();
    let mut request = agent
        .get(url)
        .header(
            "Accept",
            "application/json+chapters, application/json;q=0.9, application/*+json;q=0.8, text/json;q=0.7",
        )
        .header("User-Agent", crate::http::user_agent());
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(since) = last_modified {
        request = request.header("If-Modified-Since", since);
    }
    let mut response = match request.call() {
        Ok(resp) => resp,
        Err(ureq::Error::StatusCode(304)) => {
            return Ok(ChapterJsonFetch::NotModified {
                etag: etag.map(|s| s.to_string()),
                last_modified: last_modified.map(|s| s.to_string()),
            });
        }
        Err(e) => return Err(format!("chapter fetch failed: {e}")),
    };
    if response.status().as_u16() == 304 {
        return Ok(ChapterJsonFetch::NotModified {
            etag: response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .or_else(|| etag.map(|s| s.to_string())),
            last_modified: response
                .headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .or_else(|| last_modified.map(|s| s.to_string())),
        });
    }
    let resp_etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let resp_modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if let Some(mime) = response.headers().get("content-type") {
        let mime = mime.to_str().unwrap_or("");
        if !chapter_json_mime_is_compatible(mime) {
            return Err(format!("chapter document content type is not JSON: {mime}"));
        }
    }
    if let Some(len) = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        && len > MAX_CHAPTER_JSON_BYTES
    {
        return Err("chapter document is too large".to_string());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take((MAX_CHAPTER_JSON_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("chapter document read failed: {e}"))?;
    if bytes.len() > MAX_CHAPTER_JSON_BYTES {
        return Err("chapter document is too large".to_string());
    }
    Ok(ChapterJsonFetch::Document {
        bytes,
        etag: resp_etag,
        last_modified: resp_modified,
    })
}

/// Python `_chapter_json_mime_is_compatible`.
fn chapter_json_mime_is_compatible(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime.is_empty() {
        return true;
    }
    if [
        "application/json",
        "application/json+chapters",
        "application/octet-stream",
        "text/json",
        "text/plain",
    ]
    .contains(&mime.as_str())
    {
        return true;
    }
    mime.starts_with("application/") && mime.ends_with("+json")
}

/// Python's player-side chapter link validation
/// (`_validated_chapter_href`): a trimmed absolute http(s) URL
/// with a host, no userinfo, a parseable port, and no backslash,
/// whitespace or DEL characters.
pub fn validated_chapter_href(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.contains('\\')
        || value.chars().any(|c| c.is_whitespace() || c == '\u{7f}')
    {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // Python's urlparse requires a netloc: the WHATWG parser
    // instead skips an empty host segment ("https:///path"
    // parses with a host), so the authority is checked
    // textually as well.
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or("")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.is_empty() || url.host_str().is_none_or(|h| h.is_empty()) {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    // Url::parse has already rejected an unparseable port.
    Some(value.to_string())
}

/// Python's main-window variant (`_validated_chapter_web_url`):
/// the same rules plus rejection of C0/C1 control characters.
fn validated_chapter_web_url(value: &str) -> Option<String> {
    if value
        .chars()
        .any(|c| (c as u32) < 32 || (0x7f..=0x9f).contains(&(c as u32)))
    {
        return None;
    }
    validated_chapter_href(value)
}

/// One exposed chapter link: the chapter's index in the full
/// list, its menu label and the resolved URL.
#[derive(Debug, Clone, PartialEq)]
pub struct ChapterLink {
    pub index: usize,
    pub label: String,
    pub url: String,
}

/// Python `_format_chapter_timestamp` (via `_format_time`):
/// `H:MM:SS` at an hour or more, else `MM:SS`.
pub fn chapter_timestamp(start_secs: f64) -> String {
    crate::playback::format_time_ms((start_secs * 1000.0) as i64)
}

/// Python `_format_player_chapter_menu_label`.
pub fn chapter_menu_label(chapter: &Chapter) -> String {
    let ts = chapter_timestamp(chapter.start);
    let title = chapter.title.as_deref().unwrap_or("");
    if title.is_empty() {
        format!("{ts} Chapter at {ts}")
    } else {
        format!("{ts} {title}")
    }
}

/// Python `_chapter_announce_text`.
pub fn chapter_announce_text(chapter: &Chapter) -> String {
    let title = chapter.title.as_deref().unwrap_or("").trim();
    if title.is_empty() {
        format!("Chapter at {}", chapter_timestamp(chapter.start))
    } else {
        title.to_string()
    }
}

/// Python `_article_chapter_links`: every chapter link on the
/// article, resolved against the article URL and validated.
/// There is deliberately no count limit — the menu exposes all
/// of them (Audit 8 G19).
pub fn article_chapter_links(article_url: &str, chapters: &[Chapter]) -> Vec<ChapterLink> {
    let mut out = Vec::new();
    for (index, chapter) in chapters.iter().enumerate() {
        let Some(href) = chapter.href.as_deref() else {
            continue;
        };
        if href.is_empty()
            || href.contains('\\')
            || href.chars().any(|c| {
                c.is_whitespace() || (c as u32) < 32 || (0x7f..=0x9f).contains(&(c as u32))
            })
        {
            continue;
        }
        let resolved = if url::Url::parse(href).is_ok_and(|u| !u.scheme().is_empty()) {
            validated_chapter_web_url(href)
        } else {
            url::Url::parse(article_url)
                .ok()
                .and_then(|base| base.join(href).ok())
                .and_then(|joined| validated_chapter_web_url(joined.as_str()))
        };
        if let Some(url) = resolved {
            out.push(ChapterLink {
                index,
                label: chapter_menu_label(chapter),
                url,
            });
        }
    }
    out
}

/// Python `_embedded_media_chapters`: embedded chapters from
/// the media file. Local MP3s (and local files typed
/// audio/mpeg) read their ID3 tag directly; remote files are
/// only attempted for MP3 paths / MPEG types, through the same
/// bounded prefix read — remote MP4 chapter atoms can live at
/// the end of the file and are intentionally not downloaded.
/// Local MP4/M4A/M4B chapters are the one Python source not
/// ported: the Rust tree has no MP4 chapter-atom reader.
pub fn embedded_media_chapters(media_url: &str, media_type: Option<&str>) -> Vec<Chapter> {
    let media_type = crate::media::canonical_media_type(media_type);
    let path_lower = url::Url::parse(media_url)
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_else(|_| media_url.to_ascii_lowercase());
    let is_local = !media_url.contains("://") || media_url.starts_with("file://");
    if is_local {
        let local_path = media_url.strip_prefix("file://").unwrap_or(media_url);
        let suffix = std::path::Path::new(local_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()))
            .unwrap_or_default();
        if suffix != ".mp3" && media_type != "audio/mpeg" && media_type != "audio/mp3" {
            return Vec::new();
        }
        return crate::extract::id3_chapters(media_url);
    }
    if !path_lower.ends_with(".mp3") && media_type != "audio/mpeg" && media_type != "audio/mp3" {
        return Vec::new();
    }
    crate::extract::id3_chapters(media_url)
}

fn existing_chapters(db: &Db, req: &ChapterRequest) -> Vec<Chapter> {
    match req.cache_key {
        Some(key) if !key.trim().is_empty() => db.cached_chapters(key.trim()).unwrap_or_default(),
        _ if !req.article_id.trim().is_empty() => {
            db.chapters(req.article_id.trim()).unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn store_chapters(db: &Db, req: &ChapterRequest, chapters: &[Chapter], allow_empty: bool) {
    if chapters.is_empty() && !allow_empty {
        return;
    }
    match req.cache_key {
        Some(key) if !key.trim().is_empty() => {
            let _ = db.replace_cached_chapters(key.trim(), chapters);
        }
        _ if !req.article_id.trim().is_empty() => {
            let _ = db.replace_chapters(req.article_id.trim(), chapters);
        }
        _ => {}
    }
}

/// Python `fetch_and_store_chapters`: fetch, validate, store
/// and return an article's external or embedded chapters.
/// `now` is the caller's clock (unix seconds) so the freshness
/// window is testable.
pub fn fetch_and_store_chapters(
    db: &Db,
    transport: &dyn ChapterTransport,
    req: &ChapterRequest,
    now: f64,
) -> Vec<Chapter> {
    let existing = existing_chapters(db, req);

    // 1) The explicit Podcasting 2.0 chapter URL. Python wraps
    // the whole branch in one try: any failure returns the
    // existing rows when there are any and otherwise falls
    // through to the YouTube / embedded sources below — the
    // helper's None is that fall-through.
    if let Some(chapter_url) = req.chapter_url.filter(|u| !u.is_empty())
        && let Some(out) = chapter_url_branch(db, transport, req, chapter_url, &existing, now)
    {
        return out;
    }

    if !existing.is_empty() {
        return existing;
    }

    // YouTube chapters are structured player metadata.
    if let Some(media_url) = req.media_url.filter(|u| !u.is_empty())
        && crate::ytfull::is_youtube_video_url(media_url)
    {
        let parsed = normalize_triples(
            transport
                .youtube_chapters(media_url)
                .into_iter()
                .map(|c| {
                    (
                        c.start,
                        c.title.unwrap_or_default(),
                        c.href.filter(|h| !h.is_empty()),
                    )
                })
                .collect(),
        );
        if !parsed.is_empty() {
            store_chapters(db, req, &parsed, false);
        }
        return parsed;
    }

    if !req.allow_id3 {
        return Vec::new();
    }

    // 2) Chapters embedded in the media file itself.
    if let Some(media_url) = req.media_url.filter(|u| !u.is_empty()) {
        let parsed = embedded_media_chapters(media_url, req.media_type);
        if parsed.is_empty() {
            return Vec::new();
        }
        store_chapters(db, req, &parsed, false);
        return parsed;
    }
    Vec::new()
}

/// The Podcasting 2.0 branch of the pipeline. Some(result) is
/// final; None means the fetch failed with nothing stored and
/// the caller continues to the next source, exactly like
/// Python's exception handler falling through.
fn chapter_url_branch(
    db: &Db,
    transport: &dyn ChapterTransport,
    req: &ChapterRequest,
    chapter_url: &str,
    existing: &[Chapter],
    now: f64,
) -> Option<Vec<Chapter>> {
    let article_key = {
        let trimmed = req.article_id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    };
    let cache_key = req.cache_key.map(|k| k.trim()).filter(|k| !k.is_empty());
    let source_key = chapter_source_key(article_key, cache_key);
    let source = source_key
        .as_deref()
        .and_then(|key| db.chapter_source(key).ok().flatten());
    let source_matches = source.as_ref().is_some_and(|s| s.source_url == chapter_url);
    if !existing.is_empty()
        && source_matches
        && !req.force_refresh
        && (now - source.as_ref().map(|s| s.checked_at).unwrap_or(0.0)) < CHAPTER_REFRESH_SECONDS
    {
        return Some(existing.to_vec());
    }
    let fetched = transport.get_chapter_json(
        chapter_url,
        if source_matches {
            source.as_ref().and_then(|s| s.etag.as_deref())
        } else {
            None
        },
        if source_matches {
            source.as_ref().and_then(|s| s.last_modified.as_deref())
        } else {
            None
        },
    );
    match fetched {
        Ok(ChapterJsonFetch::NotModified {
            etag,
            last_modified,
        }) => {
            if existing.is_empty() {
                // A validator is only useful with a cached
                // representation; broken intermediaries can
                // answer 304 after the local rows were evicted,
                // so retry once with no conditional headers. A
                // second 304 is an error (Python raises).
                match transport.get_chapter_json(chapter_url, None, None) {
                    Ok(ChapterJsonFetch::Document {
                        bytes,
                        etag,
                        last_modified,
                    }) => match parse_chapter_document(&bytes) {
                        Ok(chapters) => {
                            store_chapters(db, req, &chapters, true);
                            save_source(
                                db,
                                source_key.as_deref(),
                                chapter_url,
                                etag.as_deref(),
                                last_modified.as_deref(),
                                now,
                                now,
                            );
                            Some(chapters)
                        }
                        Err(_) => None,
                    },
                    _ => None,
                }
            } else {
                save_source(
                    db,
                    source_key.as_deref(),
                    chapter_url,
                    etag.as_deref(),
                    last_modified.as_deref(),
                    now,
                    source
                        .as_ref()
                        .map(|s| s.fetched_at)
                        .filter(|f| *f != 0.0)
                        .unwrap_or(now),
                );
                Some(existing.to_vec())
            }
        }
        Ok(ChapterJsonFetch::Document {
            bytes,
            etag,
            last_modified,
        }) => match parse_chapter_document(&bytes) {
            Ok(chapters) => {
                store_chapters(db, req, &chapters, true);
                save_source(
                    db,
                    source_key.as_deref(),
                    chapter_url,
                    etag.as_deref(),
                    last_modified.as_deref(),
                    now,
                    now,
                );
                Some(chapters)
            }
            Err(_) => {
                if existing.is_empty() {
                    None
                } else {
                    Some(existing.to_vec())
                }
            }
        },
        Err(_) => {
            if existing.is_empty() {
                None
            } else {
                Some(existing.to_vec())
            }
        }
    }
}

fn save_source(
    db: &Db,
    source_key: Option<&str>,
    source_url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    checked_at: f64,
    fetched_at: f64,
) {
    let Some(key) = source_key else { return };
    if source_url.is_empty() {
        return;
    }
    let _ = db.save_chapter_source(
        key,
        &crate::db::ChapterSource {
            source_url: source_url.to_string(),
            etag: etag.map(|s| s.to_string()),
            last_modified: last_modified.map(|s| s.to_string()),
            checked_at,
            fetched_at,
        },
    );
}

/// Python local provider `get_article_chapters`: stored rows
/// first; otherwise the article's own media fields drive one
/// fetch through the pipeline.
pub fn get_article_chapters(
    db: &Db,
    transport: &dyn ChapterTransport,
    article_id: &str,
    now: f64,
) -> Vec<Chapter> {
    let stored = db.chapters(article_id).unwrap_or_default();
    if !stored.is_empty() {
        return stored;
    }
    let Some((media_url, media_type, chapter_url, article_url)) =
        db.article_media_fields(article_id).ok().flatten()
    else {
        return Vec::new();
    };
    let media_url = media_url
        .filter(|m| !m.is_empty())
        .or(Some(article_url))
        .filter(|m| !m.is_empty());
    fetch_and_store_chapters(
        db,
        transport,
        &ChapterRequest {
            article_id,
            media_url: media_url.as_deref(),
            media_type: media_type.as_deref(),
            chapter_url: chapter_url.as_deref(),
            allow_id3: true,
            cache_key: None,
            force_refresh: false,
        },
        now,
    )
}
