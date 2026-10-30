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
    let media_url = e
        .media
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|c| c.url.as_ref().map(|u| u.to_string()));
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
pub fn fetch_and_parse(url: &str) -> Result<ParsedFeed, String> {
    let mut resp = ureq::get(url)
        .header("User-Agent", "BlindRSS-Rust/0.1")
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))?;
    parse_feed(&bytes)
}
