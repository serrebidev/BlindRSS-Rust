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
    // Python's shared attachment rule (Audit 8 B05): cookies go
    // out together with the User-Agent they were issued to — the
    // host-pinned UA when one applies, the global UA only beside
    // clearance — and the client hints follow that same identity.
    // URLs with no applicable UA keep the app default untouched.
    let mut identity: Option<String> = None;
    if let Some(path) = path {
        let (cookie, ua) = crate::cookies::site_headers_for(&path, url);
        if !cookie.is_empty() {
            request = request.header("Cookie", cookie);
        }
        identity = ua;
    }
    match &identity {
        Some(ua) => {
            request = request.header("User-Agent", ua.clone());
            for (key, value) in crate::user_agents::client_hints(ua) {
                request = request.header(key, value);
            }
        }
        None => {
            for (key, value) in crate::user_agents::client_hints(&crate::http::user_agent()) {
                request = request.header(key, value);
            }
        }
    }
    request
}

type CachedFeed = (Option<String>, Option<String>, Vec<u8>);

/// One conditional request for a feed document: validators from
/// the cached copy unless `force_full`, with the Python issue #42
/// SSL fallback on certificate errors.
fn fetch_response(
    url: &str,
    force_full: bool,
    cached: &Option<CachedFeed>,
) -> Result<ureq::http::Response<ureq::Body>, String> {
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
    match fetch(crate::http::agent()) {
        Err(error)
            if crate::http::ssl_fallback_allowed()
                && crate::http::looks_like_ssl_cert_error(&error) =>
        {
            fetch(crate::http::insecure_agent())
        }
        result => result,
    }
}

fn response_validators(
    response: &ureq::http::Response<ureq::Body>,
) -> (Option<String>, Option<String>) {
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    (header("etag"), header("last-modified"))
}

fn read_body(response: &mut ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, String> {
    response
        .body_mut()
        .with_config()
        .limit(10 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))
}

/// Digest of a feed body that ignores view/rating counters
/// (Python `_feed_body_digest`): YouTube's feeds/videos.xml sends
/// no ETag/Last-Modified and its only moving parts between fetches
/// are the `<media:statistics>` / `<media:starRating>` tags, which
/// BlindRSS never stores. SHA-1 hex, byte-identical rule: strip
/// each volatile opening tag through its closing `>`.
pub fn feed_body_digest(bytes: &[u8]) -> String {
    use sha1::Digest;
    let mut stripped = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        let tag_at = ["<media:statistics", "<media:starRating"]
            .iter()
            .filter_map(|tag| {
                let at = find_subslice(rest, tag.as_bytes())?;
                // Python's `\b`: the tag name must not continue.
                match rest.get(at + tag.len()) {
                    Some(b) if b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-' => None,
                    _ => Some(at),
                }
            })
            .min();
        let Some(at) = tag_at else {
            stripped.extend_from_slice(rest);
            break;
        };
        stripped.extend_from_slice(&rest[..at]);
        let after = &rest[at..];
        match after.iter().position(|b| *b == b'>') {
            Some(end) => rest = &after[end + 1..],
            None => break,
        }
    }
    let mut hasher = sha1::Sha1::new();
    hasher.update(&stripped);
    format!("{:x}", hasher.finalize())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The outcome of one worker-side feed-document fetch (Audit 8
/// D16, Python's v2.5.5+ unchanged-body fast path).
#[derive(Debug, Clone, PartialEq)]
pub enum FeedFetch {
    /// The publisher's body is unchanged since a store of it
    /// already committed (a 304, or a 200 whose digest matches the
    /// recorded stored digest): nothing to parse or store.
    NotModified,
    /// A body that must be stored. `body_digest` identifies it:
    /// the caller records the digest only after the store commits,
    /// so a failed store is retried by the next refresh even when
    /// the publisher then answers 304.
    Fetched {
        parsed: ParsedFeed,
        body_digest: String,
    },
}

/// Fetch one feed document for the worker refresh path.
/// `stored_digest` is the digest of the last body whose store
/// committed for this feed (see `FeedFetch::Fetched`). Forced
/// fetches never take the fast path. Validators from an unchanged
/// 200 are still persisted, so the next refresh can earn a real
/// 304 (Python does the same).
pub fn fetch_feed_document(
    db: &crate::Db,
    url: &str,
    force_full: bool,
    stored_digest: Option<&str>,
) -> Result<FeedFetch, String> {
    fetch_feed_document_cancellable(db, url, force_full, stored_digest, None, "")
}

/// `fetch_feed_document` with the refresh worker's
/// cancellation flag and the feed's configured proxy
/// (`feed_settings["proxy"]`), for the browser recovery
/// stage (Audit 8 B01, Python
/// `providers/local.py::_try_browser_feed_fallback`):
/// when the plain fetch fails with a qualifying error, or
/// the fetched body is not a feed and qualifies (gate
/// status, Cloudflare challenge shape, challenge page),
/// the document is fetched once through the warm browser
/// — only when the fallback is enabled in Settings and
/// the refresh has not been cancelled. A browser-recovered
/// body REPLACES the response exactly as Python's does:
/// its validators are cleared and its digest is never
/// recorded, so the next refresh fetches in full again.
pub fn fetch_feed_document_cancellable(
    db: &crate::Db,
    url: &str,
    force_full: bool,
    stored_digest: Option<&str>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    proxy: &str,
) -> Result<FeedFetch, String> {
    if let Some(result) = crate::forum_sources::fetch_feed(url) {
        // Forum documents have no HTTP body to digest; they never
        // take the fast path (empty digest = never recorded).
        return Ok(FeedFetch::Fetched {
            parsed: result?,
            body_digest: String::new(),
        });
    }
    let cached = db
        .cached_feed(url)
        .map_err(|e| format!("feed cache: {e}"))?;
    let mut response = match fetch_response(url, force_full, &cached) {
        Ok(response) => response,
        Err(error) => {
            if crate::browser::refresh_error_qualifies(&error)
                && let Some(recovered) = browser_recover_feed(db, url, proxy, cancel)
            {
                return recovered;
            }
            return Err(error);
        }
    };
    if response.status().as_u16() == 304 {
        let (_, _, body) =
            cached.ok_or_else(|| "Feed returned 304 without cached content".to_string())?;
        let digest = feed_body_digest(&body);
        if !force_full && stored_digest == Some(digest.as_str()) {
            return Ok(FeedFetch::NotModified);
        }
        return Ok(FeedFetch::Fetched {
            parsed: parse_feed(&body)?,
            body_digest: digest,
        });
    }
    let status = response.status().as_u16();
    let cf_mitigated = response
        .headers()
        .get("cf-mitigated")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (etag, modified) = response_validators(&response);
    let body = read_body(&mut response)?;
    let digest = feed_body_digest(&body);
    if !force_full && stored_digest == Some(digest.as_str()) {
        db.cache_feed(url, etag.as_deref(), modified.as_deref(), &body)
            .map_err(|e| format!("feed cache: {e}"))?;
        return Ok(FeedFetch::NotModified);
    }
    // Parse before caching: a body that fails to parse must not
    // advance the validators, or the next conditional request
    // would 304 against content that was never stored (Python
    // persists validators only inside its store/unchanged paths).
    let parsed = match parse_feed(&body) {
        Ok(parsed) => parsed,
        Err(parse_error) => {
            let body_text = String::from_utf8_lossy(&body);
            if crate::browser::refresh_response_qualifies(
                status,
                cf_mitigated.as_deref(),
                &body_text,
            ) && let Some(recovered) = browser_recover_feed(db, url, proxy, cancel)
            {
                return recovered;
            }
            return Err(parse_error);
        }
    };
    db.cache_feed(url, etag.as_deref(), modified.as_deref(), &body)
        .map_err(|e| format!("feed cache: {e}"))?;
    Ok(FeedFetch::Fetched {
        parsed,
        body_digest: digest,
    })
}

/// The browser recovery attempt behind
/// `fetch_feed_document_cancellable`: None when the
/// fallback is disabled in Settings, the fetch was
/// cancelled, the browser produced no validated feed, or
/// the recovered document still does not parse — the
/// caller then reports its original failure, exactly as
/// Python falls back to the plain-fetch outcome.
fn browser_recover_feed(
    db: &crate::Db,
    url: &str,
    proxy: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Option<Result<FeedFetch, String>> {
    if !crate::browser::fallback_enabled()
        || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    {
        return None;
    }
    let timeout = crate::browser::fallback_timeout_seconds();
    let response = crate::browser::fetch_feed(url, timeout, proxy, cancel).ok()?;
    let parsed = parse_feed(response.text.as_bytes()).ok()?;
    // The recovered body replaces the response: cache it
    // with its validators cleared (Python sets etag /
    // last_modified to None and body_digest to None), so
    // no later conditional request can 304 against the
    // gated body it replaces. A cache write failure does
    // not undo the recovery; the refresh's own store path
    // reports database trouble itself.
    let _ = db.cache_feed(url, None, None, response.text.as_bytes());
    Some(Ok(FeedFetch::Fetched {
        parsed,
        body_digest: String::new(),
    }))
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
    let mut response = fetch_response(url, force_full, &cached)?;
    if response.status().as_u16() == 304 {
        return cached
            .map(|(_, _, body)| body)
            .ok_or_else(|| "Feed returned 304 without cached content".into())
            .and_then(|body| parse_feed(&body));
    }
    let (etag, modified) = response_validators(&response);
    let body = read_body(&mut response)?;
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
    /// The episode's duration as the feed document states it
    /// (the raw `itunes:duration` text — Python's feedparser
    /// value, kept un-normalized because archive identity keys
    /// embed it verbatim; Audit 8 Task 27, F16). None when the
    /// document carries no usable duration.
    pub duration: Option<String>,
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
        // feed-rs parses itunes:duration into a Duration,
        // losing the document's raw text; the raw scan in
        // parse_feed overwrites this fallback (formatted total
        // seconds) whenever it can see the element itself.
        duration: media_duration_fallback(e),
    }
}

/// feed-rs's parsed duration (the media object's, else the
/// first media content's) as a total-seconds string — the
/// fallback for documents whose duration element the raw scan
/// could not pair with its entry.
fn media_duration_fallback(e: &Entry) -> Option<String> {
    let duration = e.media.iter().find_map(|m| {
        m.duration
            .or_else(|| m.content.iter().find_map(|c| c.duration))
    })?;
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    if millis == 0 {
        Some(secs.to_string())
    } else {
        Some(
            format!("{secs}.{:03}", millis)
                .trim_end_matches('0')
                .to_string(),
        )
    }
}

/// Raw per-entry `itunes:duration` texts, in document order
/// (feedparser's `itunes_duration`: the element text exactly
/// as written, whitespace-trimmed — NOT normalized to seconds,
/// because Python's archive identity keys embed it verbatim
/// and the episode keys must hash identically in both apps).
/// The scan pairs durations with entries by order; entries
/// whose element it cannot see keep their index as None and
/// fall back to feed-rs's parsed value.
fn raw_entry_durations(bytes: &[u8]) -> Vec<Option<String>> {
    use quick_xml::events::Event;
    const ITUNES_NS: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
    let text = String::from_utf8_lossy(bytes);
    let mut reader = quick_xml::Reader::from_str(&text);
    reader.config_mut().check_end_names = false;
    let mut durations: Vec<Option<String>> = Vec::new();
    // Element stack: (local name, xmlns declarations added).
    let mut stack: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut scopes: Vec<(String, String)> = Vec::new();
    let mut in_entry = false;
    let mut capturing = false;
    let mut captured = String::new();
    let mut buf = Vec::new();
    let lookup_ns = |scopes: &[(String, String)], prefix: &str| -> Option<String> {
        scopes
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.clone())
    };
    loop {
        let event = reader.read_event_into(&mut buf);
        let is_empty = matches!(event, Ok(Event::Empty(_)));
        match event {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let qname = e.name().as_ref().to_string();
                let (prefix, local) = match qname.split_once(':') {
                    Some((p, l)) => (p.to_string(), l.to_lowercase()),
                    None => (String::new(), qname.to_lowercase()),
                };
                let mut decls: Vec<(String, String)> = Vec::new();
                for a in e.attributes().filter_map(|a| a.ok()) {
                    let key = a.key.as_ref().to_string();
                    if let Some(p) = key.strip_prefix("xmlns:") {
                        decls.push((
                            p.to_string(),
                            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .unwrap_or_default()
                                .into_owned(),
                        ));
                    } else if key == "xmlns" {
                        decls.push((
                            String::new(),
                            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                                .unwrap_or_default()
                                .into_owned(),
                        ));
                    }
                }
                for d in &decls {
                    scopes.push(d.clone());
                }
                if local == "item" || local == "entry" {
                    if !in_entry {
                        durations.push(None);
                        if !is_empty {
                            in_entry = true;
                        }
                    }
                } else if in_entry
                    && local == "duration"
                    && durations.last().is_some_and(|d| d.is_none())
                    && !capturing
                    && !is_empty
                {
                    let ns_ok = prefix == "itunes"
                        || prefix.is_empty()
                        || lookup_ns(&scopes, &prefix).as_deref() == Some(ITUNES_NS);
                    if ns_ok {
                        capturing = true;
                        captured.clear();
                    }
                }
                if is_empty {
                    for _ in &decls {
                        scopes.pop();
                    }
                } else {
                    stack.push((local, decls));
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name().as_ref().to_string();
                let local = qname
                    .split_once(':')
                    .map(|(_, l)| l.to_lowercase())
                    .unwrap_or_else(|| qname.to_lowercase());
                if capturing && local == "duration" {
                    capturing = false;
                    if let Some(slot) = durations.last_mut() {
                        let value = captured.trim().to_string();
                        if !value.is_empty() {
                            *slot = Some(value);
                        }
                    }
                }
                if (local == "item" || local == "entry") && in_entry {
                    in_entry = false;
                    capturing = false;
                }
                if let Some(pos) = stack.iter().rposition(|(name, _)| *name == local) {
                    let removed: Vec<_> = stack.drain(pos..).collect();
                    for (_, decls) in &removed {
                        for _ in decls {
                            scopes.pop();
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if capturing {
                    captured.push_str(
                        &quick_xml::escape::unescape(&e.xml10_content()).unwrap_or_default(),
                    );
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    durations
}

/// Parse a feed document (RSS, Atom, or JSON Feed) from raw bytes.
pub fn parse_feed(bytes: &[u8]) -> Result<ParsedFeed, String> {
    let feed = parser::parse(std::io::Cursor::new(bytes)).map_err(|e| format!("{e:?}"))?;
    let mut entries: Vec<ParsedEntry> = feed.entries.iter().map(entry_from).collect();
    // The raw itunes:duration text (feedparser's value) wins
    // over feed-rs's parsed Duration wherever the scan could
    // pair it with its entry (Audit 8 Task 27, F16).
    for (entry, raw) in entries.iter_mut().zip(raw_entry_durations(bytes)) {
        if raw.is_some() {
            entry.duration = raw;
        }
    }
    Ok(ParsedFeed {
        title: feed
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default(),
        entries,
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
