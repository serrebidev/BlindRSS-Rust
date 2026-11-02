//! Feed parsing (RSS / Atom / JSON Feed) via feed-rs, normalized into
//! the shape the data layer stores. Stage 2, slice 1: local-feed parsing.
//! Hosted providers (Miniflux, Inoreader, ...) are later slices.

use feed_rs::model::Entry;
use feed_rs::parser;

static COOKIE_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

pub fn configure_cookies(config_path: &std::path::Path) {
    *COOKIE_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(config_path.to_owned());
}

fn request(agent: ureq::Agent, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    let mut request = agent.get(url);
    let path = COOKIE_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(path) = path {
        let jar = crate::cookies::load_jar_beside(&path);
        let cookie = crate::cookies::cookie_header_for(&jar, url);
        if !cookie.is_empty() {
            request = request.header("Cookie", cookie);
        }
    }
    for (key, value) in crate::user_agents::client_hints(&crate::http::user_agent()) {
        request = request.header(key, value);
    }
    request
}

/// Conditional refresh keeps the validated response body, so a failed later
/// store can retry safely even if the publisher next returns 304.
pub fn fetch_with_cache(db: &crate::Db, url: &str, force_full: bool) -> Result<ParsedFeed, String> {
    if let Some(result) = crate::forum_sources::fetch_feed(url) {
        return result;
    }
    let cached = db
        .cached_feed(url)
        .map_err(|e| format!("feed cache: {e}"))?;
    let fetch = |agent| {
        let mut request = request(agent, url);
        if !force_full && let Some((etag, modified, _)) = &cached {
            if let Some(value) = etag {
                request = request.header("If-None-Match", value);
            }
            if let Some(value) = modified {
                request = request.header("If-Modified-Since", value);
            }
        }
        request.call().map_err(|e| format!("fetch failed: {e}"))
    };
    let mut response = match fetch(crate::http::agent()) {
        Err(error)
            if crate::http::ssl_fallback_allowed()
                && crate::http::looks_like_ssl_cert_error(&error) =>
        {
            fetch(crate::http::insecure_agent())?
        }
        result => result?,
    };
    if response.status().as_u16() == 304 {
        return cached
            .map(|(_, _, body)| body)
            .ok_or_else(|| "Feed returned 304 without cached content".into())
            .and_then(|body| parse_feed(&body));
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = response
        .body_mut()
        .with_config()
        .limit(10 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))?;
    let feed = parse_feed(&body)?;
    db.cache_feed(url, etag.as_deref(), modified.as_deref(), &body)
        .map_err(|e| format!("feed cache: {e}"))?;
    Ok(feed)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub id: String,
    pub title: String,
    pub url: String,
    pub content_text: String,
    pub summary: String,
    pub date: Option<String>,
    pub author: Option<String>,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFeed {
    pub title: String,
    pub entries: Vec<ParsedEntry>,
}

fn entry_text(e: &Entry) -> (String, String) {
    let content = e
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .unwrap_or_default();
    let summary = e
        .summary
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_default();
    (content, summary)
}

fn entry_from(e: &Entry) -> ParsedEntry {
    let (content, summary) = entry_text(e);
    let webpage = |link: &&feed_rs::model::Link| {
        !link.href.trim().is_empty()
            && !matches!(
                link.rel.as_deref().unwrap_or(""),
                "enclosure" | "self" | "hub" | "replies" | "edit" | "icon" | "license" | "payment"
            )
    };
    let url = e
        .links
        .iter()
        .filter(webpage)
        .find(|link| link.rel.as_deref() == Some("alternate"))
        .or_else(|| e.links.iter().find(webpage))
        .map(|link| link.href.trim().to_string())
        .unwrap_or_else(|| e.id.clone());
    // Enclosures and media:content alike land in e.media. Podcast
    // posts can carry an image enclosure ahead of the episode audio
    // (Python's Sky lesson), so prefer the first audio/video item;
    // fall back to the first media item of any kind.
    let mut fallback: Option<(String, Option<String>)> = None;
    let mut chosen: Option<(String, Option<String>)> = None;
    'scan: for m in &e.media {
        for c in &m.content {
            let Some(u) = &c.url else { continue };
            let url = u.to_string();
            let ctype = c.content_type.as_ref().map(|t| t.to_string());
            if fallback.is_none() {
                fallback = Some((url.clone(), ctype.clone()));
            }
            // One playable test for the feed chooser and the
            // resolver: audio/video MIME, stream manifests, or a
            // media extension on the URL.
            let playable = crate::media::is_playable_media(&url, ctype.as_deref());
            if playable {
                chosen = Some((url, ctype));
                break 'scan;
            }
        }
    }
    let (media_url, media_type) = match chosen.or(fallback) {
        Some((u, t)) => (Some(u), t),
        None => (None, None),
    };
    ParsedEntry {
        id: e.id.clone(),
        title: e
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default(),
        url,
        content_text: content,
        summary,
        date: e.published.or(e.updated).map(|d| d.to_rfc3339()),
        author: e.authors.first().and_then(|p| p.name.clone()),
        media_url,
        media_type,
        tags: crate::enrich::merge_tags(
            Vec::new(),
            &e.categories
                .iter()
                .map(|c| c.term.clone())
                .collect::<Vec<_>>(),
        ),
    }
}

/// Parse a feed document (RSS, Atom, or JSON Feed) from raw bytes.
pub fn parse_feed(bytes: &[u8]) -> Result<ParsedFeed, String> {
    let feed = parser::parse(std::io::Cursor::new(bytes)).map_err(|e| format!("{e:?}"))?;
    Ok(ParsedFeed {
        title: feed
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default(),
        entries: feed.entries.iter().map(entry_from).collect(),
    })
}

/// Fetch a feed over HTTP and parse it. Used by the refresh path.
///
/// Python issue #42 semantics: when the fetch fails certificate
/// validation and `ignore_feed_ssl_errors` is on (the default),
/// retry once with verification disabled — public feeds behind
/// broken certificates (self-signed, missing intermediates,
/// expired) still load. Feed retrieval only.
pub fn fetch_and_parse(url: &str) -> Result<ParsedFeed, String> {
    if let Some(result) = crate::forum_sources::fetch_feed(url) {
        return result;
    }
    fn fetch(agent: ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
        let mut resp = request(agent, url)
            .call()
            .map_err(|e| format!("fetch failed: {e}"))?;
        resp.body_mut()
            .with_config()
            .limit(10 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| format!("read failed: {e}"))
    }
    let bytes = match fetch(crate::http::agent(), url) {
        Ok(bytes) => bytes,
        Err(e) => {
            if crate::http::ssl_fallback_allowed() && crate::http::looks_like_ssl_cert_error(&e) {
                eprintln!(
                    "blindrss: SSL certificate problem for {url} ({e}); retrying without certificate verification"
                );
                fetch(crate::http::insecure_agent(), url)?
            } else {
                return Err(e);
            }
        }
    };
    parse_feed(&bytes)
}
