//! Feed parsing (RSS / Atom / JSON Feed) via feed-rs, normalized into
//! the shape the data layer stores. Stage 2, slice 1: local-feed parsing.
//! Hosted providers (Miniflux, Inoreader, ...) are later slices.

use feed_rs::model::Entry;
use feed_rs::parser;

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
    let url = e
        .links
        .first()
        .map(|l| l.href.clone())
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
    fn fetch(agent: ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
        let mut resp = agent
            .get(url)
            .call()
            .map_err(|e| format!("fetch failed: {e}"))?;
        resp.body_mut()
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
