//! Feed parsing (RSS / Atom / JSON Feed) via feed-rs, normalized into
//! the shape the data layer stores. Stage 2, slice 1: local-feed parsing.
//! Hosted providers (Miniflux, Inoreader, ...) are later slices.

use feed_rs::model::Entry;
use feed_rs::parser;

static COOKIE_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

pub fn configure_cookies(config_path: &std::path::Path) {
    *COOKIE_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(config_path.to_owned());
}

/// The site-jar attachment for one URL: the Cookie header
/// value plus the User-Agent identity the cookies were
/// issued to (None = the caller's default identity). An
/// empty cookie with no identity is an anonymous request.
type SiteAttachment = (String, Option<String>);

fn site_attachment(url: &str) -> SiteAttachment {
    let path = COOKIE_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match path {
        Some(path) => crate::cookies::site_headers_for(&path, url),
        None => (String::new(), None),
    }
}

/// Statuses that earn one anonymous (cookie-free) retry when
/// the cookie-carrying request fails with them: Python's
/// 413/431 (`_retry_feed_without_site_cookies`, issue #101 —
/// the jar outgrew what the server accepts) plus the
/// stale-session 5xx family. A stale logged-in session can
/// send the server's request handling into a timeout while
/// the anonymous request succeeds — GitHub answers 504
/// ("Unicorn!") to stale session cookies on .atom feeds
/// (Brandon's report, 2026-11-09; reproduced: anonymous
/// 6/6 success, cookie-carrying flaky 504).
const COOKIE_FREE_RETRY_STATUSES: &[u16] = &[413, 431, 500, 502, 503, 504];

fn should_retry_without_cookies(status: u16, cookies_sent: bool) -> bool {
    cookies_sent && COOKIE_FREE_RETRY_STATUSES.contains(&status)
}

/// Whether the anonymous retry's result replaces the
/// cookie-carrying attempt's: only a non-error response
/// does (`None` = the retry failed at the transport level).
/// When the retry also errors, the ORIGINAL error is kept —
/// it names the real failure (a genuinely broken server, or
/// a private feed whose cookies the anonymous request
/// correctly lacks).
fn adopt_cookie_free_retry(retry_status: Option<u16>) -> bool {
    matches!(retry_status, Some(status) if status < 400)
}

/// The status carried by a request outcome: the response
/// status, the failed status code, or None for a
/// transport-level failure.
fn outcome_status(outcome: &Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Option<u16> {
    match outcome {
        Ok(response) => Some(response.status().as_u16()),
        Err(ureq::Error::StatusCode(status)) => Some(*status),
        Err(_) => None,
    }
}

/// One feed request with the anonymous fallback: run
/// `attempt` with the jar attached; when it draws a
/// rejection status, run it once more with no cookies and
/// no pinned identity. The retry keeps everything else the
/// attempt carries (validators, Referer, custom headers).
fn with_cookie_free_retry(
    url: &str,
    site: &SiteAttachment,
    attempt: impl Fn(&SiteAttachment) -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let anonymous: SiteAttachment = (String::new(), None);
    match attempt(site) {
        Err(ureq::Error::StatusCode(status))
            if should_retry_without_cookies(status, !site.0.is_empty()) =>
        {
            let retry = attempt(&anonymous);
            if adopt_cookie_free_retry(outcome_status(&retry)) {
                eprintln!(
                    "blindrss: retried feed without imported site cookies after HTTP {status}: {url}"
                );
                retry
            } else {
                Err(ureq::Error::StatusCode(status))
            }
        }
        other => other,
    }
}

fn request_with_attachment(
    agent: ureq::Agent,
    url: &str,
    site: &SiteAttachment,
) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    let mut request = agent.get(url);
    // Python's shared attachment rule (Audit 8 B05): cookies go
    // out together with the User-Agent they were issued to — the
    // host-pinned UA when one applies, the global UA only beside
    // clearance — and the client hints follow that same identity.
    // URLs with no applicable UA keep the app default untouched.
    let (cookie, identity) = site;
    if !cookie.is_empty() {
        request = request.header("Cookie", cookie.as_str());
    }
    match identity {
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

/// Per-feed HTTP overrides for one feed fetch (Python
/// `providers/local.py` issue #29 handling, Audit 8 G08):
/// custom request headers, a timeout override, a proxy, and
/// the feed-document encoding override. Built from the feed's
/// `feed_settings`; an all-default value takes the shared
/// agent path exactly as before.
#[derive(Debug, Clone, Default)]
pub struct FeedFetchOverrides {
    pub headers: Vec<(String, String)>,
    pub timeout_secs: Option<u64>,
    pub proxy: String,
    pub encoding: String,
    /// Per-feed browser-impersonation policy (Python
    /// feed_settings "impersonate": auto/always/never,
    /// issue #29). "" reads as "auto" through
    /// `feed_settings::normalize_impersonate` at the stage
    /// gate, which is also the Default value's meaning.
    pub impersonate: &'static str,
}

impl FeedFetchOverrides {
    pub fn from_settings(settings: &serde_json::Value) -> Self {
        let headers = settings
            .get("custom_headers")
            .and_then(|v| v.as_object())
            .map(|map| {
                map.iter()
                    .map(|(k, v)| {
                        let value = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), value)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let timeout_secs = settings
            .get("timeout_seconds")
            .and_then(|v| v.as_i64())
            .filter(|t| *t > 0)
            .map(|t| t as u64);
        Self {
            headers,
            timeout_secs,
            proxy: crate::feed_settings::text_value(settings, "proxy")
                .trim()
                .to_string(),
            encoding: crate::feed_settings::text_value(settings, "encoding")
                .trim()
                .to_string(),
            impersonate: crate::feed_settings::impersonate_value(settings),
        }
    }

    fn is_plain(&self) -> bool {
        self.headers.is_empty() && self.timeout_secs.is_none() && self.proxy.is_empty()
    }
}

/// The site-root Referer for a URL (`scheme://host/`, Python
/// `utils.referer_for_url`): the refresh fetch sends the
/// feed's own site as Referer so the request looks like an
/// in-site navigation past anti-bot WAFs (issue #29).
pub fn referer_for_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return String::new();
    };
    if scheme.is_empty() {
        return String::new();
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return String::new();
    }
    format!("{scheme}://{host}/")
}

/// An agent for a fetch with per-feed overrides: the shared
/// agent's User-Agent, the override timeout (or the global
/// one), the override proxy, and — for the issue #42 retry —
/// optionally disabled certificate verification.
fn feed_agent(overrides: &FeedFetchOverrides, insecure: bool) -> Result<ureq::Agent, String> {
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(
            overrides
                .timeout_secs
                .unwrap_or_else(crate::http::timeout_secs)
                .max(1),
        )))
        .user_agent(crate::http::user_agent());
    if !overrides.proxy.is_empty() {
        let proxy =
            ureq::Proxy::new(&overrides.proxy).map_err(|e| format!("invalid proxy: {e}"))?;
        config = config.proxy(Some(proxy));
    }
    if insecure {
        let tls = ureq::tls::TlsConfig::builder()
            .disable_verification(true)
            .build();
        config = config.tls_config(tls);
    }
    Ok(ureq::Agent::new_with_config(config.build()))
}

/// Decode a feed body with the feed's encoding override
/// (Python local.py applies `feed_settings["encoding"]` when
/// decoding the document): the decoded text re-encoded as
/// UTF-8 for the parser. Without a usable override the body
/// passes through untouched (the parser reads the XML
/// declaration itself). The cached body and the stored-body
/// digest always stay the original bytes.
pub fn decode_feed_body<'a>(body: &'a [u8], encoding: &str) -> std::borrow::Cow<'a, [u8]> {
    let label = encoding.trim();
    if label.is_empty() {
        return std::borrow::Cow::Borrowed(body);
    }
    let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) else {
        return std::borrow::Cow::Borrowed(body);
    };
    let (text, _, _) = enc.decode(body);
    std::borrow::Cow::Owned(text.into_owned().into_bytes())
}

/// One conditional request for a feed document: validators from
/// the cached copy unless `force_full`, with one anonymous
/// retry when the cookie-carrying request draws a rejection
/// status, and the Python issue #42 SSL fallback on
/// certificate errors.
fn fetch_response(
    url: &str,
    force_full: bool,
    cached: &Option<CachedFeed>,
    overrides: &FeedFetchOverrides,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let site = site_attachment(url);
    let fetch = |agent: ureq::Agent, site: &SiteAttachment| {
        let mut request = request_with_attachment(agent, url, site);
        if !force_full && let Some((etag, modified, _)) = &cached {
            if let Some(value) = etag {
                request = request.header("If-None-Match", value);
            }
            if let Some(value) = modified {
                request = request.header("If-Modified-Since", value);
            }
        }
        // Per-feed overrides (issue #29): a site-root Referer
        // unless the feed's own headers carry one, then the
        // feed's custom headers.
        if !overrides
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("referer"))
        {
            let referer = referer_for_url(url);
            if !referer.is_empty() {
                request = request.header("Referer", &referer);
            }
        }
        for (name, value) in &overrides.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        request.call()
    };
    let (agent, insecure_agent) = if overrides.is_plain() {
        (crate::http::agent(), crate::http::insecure_agent())
    } else {
        (feed_agent(overrides, false)?, feed_agent(overrides, true)?)
    };
    let response = with_cookie_free_retry(url, &site, |site| fetch(agent.clone(), site));
    match response.map_err(|e| format!("fetch failed: {e}")) {
        Err(error)
            if crate::http::ssl_fallback_allowed()
                && crate::http::looks_like_ssl_cert_error(&error) =>
        {
            fetch(insecure_agent, &site).map_err(|e| format!("fetch failed: {e}"))
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
    fetch_feed_document_cancellable(
        db,
        url,
        force_full,
        stored_digest,
        None,
        "",
        &FeedFetchOverrides::default(),
    )
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
#[allow(clippy::too_many_arguments)]
pub fn fetch_feed_document_cancellable(
    db: &crate::Db,
    url: &str,
    force_full: bool,
    stored_digest: Option<&str>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    proxy: &str,
    overrides: &FeedFetchOverrides,
) -> Result<FeedFetch, String> {
    if let Some(result) = crate::forum_sources::fetch_feed(url) {
        // Forum documents have no HTTP body to digest; they never
        // take the fast path (empty digest = never recorded).
        return Ok(FeedFetch::Fetched {
            parsed: result?,
            body_digest: String::new(),
        });
    }
    // NPR feeds never take the fast paths (Python
    // providers/local.py: `use_conditional` excludes NPR feeds
    // and logs "Skipping conditional headers for NPR feed"):
    // no validators are sent, and the body-digest shortcut does
    // not apply, because NPR audio is discovered per session —
    // a story's audio can post after the story with the feed
    // body unchanged, and only a full parse + store re-resolves
    // it (Brandon's 2026-11-07 NPR report).
    let npr_feed = crate::sources::is_npr_url(url);
    let cached = db
        .cached_feed(url)
        .map_err(|e| format!("feed cache: {e}"))?;
    // Per-feed impersonation policy (issue #29, B16's
    // refresh half): "always" tries the impersonated fetch
    // first; "auto" keeps it as the reserved attempt when
    // the plain fetch fails or answers with a block;
    // "never" disables the stage. A recovered body follows
    // the browser stage's contract (validators cleared,
    // digest never recorded).
    let impersonate_mode = crate::feed_settings::normalize_impersonate(overrides.impersonate);
    if impersonate_mode == "always"
        && let Some(recovered) = impersonate_recover_feed(db, url, overrides)
    {
        return recovered;
    }
    let mut response = match fetch_response(url, force_full || npr_feed, &cached, overrides) {
        Ok(response) => response,
        Err(error) => {
            if impersonate_mode != "never"
                && let Some(recovered) = impersonate_recover_feed(db, url, overrides)
            {
                return recovered;
            }
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
        if !force_full && !npr_feed && stored_digest == Some(digest.as_str()) {
            return Ok(FeedFetch::NotModified);
        }
        return Ok(FeedFetch::Fetched {
            parsed: parse_feed(&decode_feed_body(&body, &overrides.encoding))?,
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
    if !force_full && !npr_feed && stored_digest == Some(digest.as_str()) {
        db.cache_feed(url, etag.as_deref(), modified.as_deref(), &body)
            .map_err(|e| format!("feed cache: {e}"))?;
        return Ok(FeedFetch::NotModified);
    }
    // Parse before caching: a body that fails to parse must not
    // advance the validators, or the next conditional request
    // would 304 against content that was never stored (Python
    // persists validators only inside its store/unchanged paths).
    // The encoding override re-decodes the body for parsing
    // only; the cache and the digest keep the original bytes.
    let parsed = match parse_feed(&decode_feed_body(&body, &overrides.encoding)) {
        Ok(parsed) => parsed,
        Err(parse_error) => {
            let body_text = String::from_utf8_lossy(&body);
            let qualifies = crate::browser::refresh_response_qualifies(
                status,
                cf_mitigated.as_deref(),
                &body_text,
            );
            // Python escalates the reserved attempt on
            // `_response_looks_blocked`: a challenge shape,
            // or a 200/403/429 HTML answer that is not a
            // feed. The status half is checked here; the
            // challenge half is the browser qualification.
            if impersonate_mode != "never"
                && (qualifies || matches!(status, 200 | 403 | 429))
                && let Some(recovered) = impersonate_recover_feed(db, url, overrides)
            {
                return recovered;
            }
            if qualifies && let Some(recovered) = browser_recover_feed(db, url, proxy, cancel) {
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

/// The impersonation recovery attempt behind
/// `fetch_feed_document_cancellable` (B16's refresh half;
/// Python providers/local.py's reserved impersonated
/// attempt, issue #29): one GET on a real-browser TLS
/// fingerprint — the site's pinned session target when one
/// exists, else Python's "chrome" default — carrying the
/// site's stored cookies exactly as the plain request
/// builder attaches them. None when the fetch fails or
/// the body does not parse as a feed; the caller then
/// continues to the browser stage or its original
/// failure. The recovered body is cached with validators
/// cleared and its digest is never recorded, the browser
/// stage's contract.
fn impersonate_recover_feed(
    db: &crate::Db,
    url: &str,
    overrides: &FeedFetchOverrides,
) -> Option<Result<FeedFetch, String>> {
    let pinned = crate::extract::site_impersonation_target(url);
    let target = if pinned.is_empty() { "chrome" } else { &pinned };
    let cookie = COOKIE_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .map(|path| crate::cookies::site_headers_for(&path, url).0)
        .unwrap_or_default();
    let bytes = crate::impersonate::fetch_bytes(
        url,
        target,
        &cookie,
        std::time::Duration::from_secs(crate::http::timeout_secs()),
    )?;
    let parsed = parse_feed(&decode_feed_body(&bytes, &overrides.encoding)).ok()?;
    let _ = db.cache_feed(url, None, None, &bytes);
    Some(Ok(FeedFetch::Fetched {
        parsed,
        body_digest: String::new(),
    }))
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
    let mut response = fetch_response(url, force_full, &cached, &FeedFetchOverrides::default())?;
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

/// Python local provider's titleless-post fallback: an 83-character
/// plain-text preview, including the trailing three dots when truncated.
pub fn article_title(title: &str, body: &str) -> String {
    if !title.trim().is_empty() && !title.trim().eq_ignore_ascii_case("No Title") {
        return title.to_string();
    }
    let text = if body.contains('<') && body.contains('>') {
        dom_smoothie::Readability::new(body, None, None)
            .map(|page| {
                page.doc.select("script, style").remove();
                page.doc
                    .root()
                    .descendants_it()
                    .filter(|node| node.is_text())
                    .map(|node| node.text().to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|_| body.to_string())
    } else {
        body.to_string()
    };
    let preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.is_empty() {
        return "No Title".to_string();
    }
    if preview.chars().count() > 83 {
        let prefix: String = preview.chars().take(80).collect();
        format!("{}...", prefix.trim_end())
    } else {
        preview
    }
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
/// One anonymous retry precedes the Python issue #42
/// semantics: when the fetch fails certificate validation
/// and `ignore_feed_ssl_errors` is on (the default), retry
/// once with verification disabled — public feeds behind
/// broken certificates (self-signed, missing intermediates,
/// expired) still load. Feed retrieval only.
pub fn fetch_and_parse(url: &str) -> Result<ParsedFeed, String> {
    if let Some(result) = crate::forum_sources::fetch_feed(url) {
        return result;
    }
    let site = site_attachment(url);
    let fetch = |agent: ureq::Agent, site: &SiteAttachment| {
        request_with_attachment(agent, url, site).call()
    };
    let response = with_cookie_free_retry(url, &site, |site| fetch(crate::http::agent(), site));
    let mut response = match response {
        Ok(response) => response,
        Err(error) => {
            let message = format!("fetch failed: {error}");
            if crate::http::ssl_fallback_allowed()
                && crate::http::looks_like_ssl_cert_error(&message)
            {
                eprintln!(
                    "blindrss: SSL certificate problem for {url} ({message}); retrying without certificate verification"
                );
                fetch(crate::http::insecure_agent(), &site)
                    .map_err(|e| format!("fetch failed: {e}"))?
            } else {
                return Err(message);
            }
        }
    };
    let bytes = response
        .body_mut()
        .with_config()
        .limit(10 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))?;
    parse_feed(&bytes)
}

#[cfg(test)]
mod cookie_free_retry_tests {
    use super::{adopt_cookie_free_retry, should_retry_without_cookies};

    #[test]
    fn rejection_statuses_with_cookies_retry_anonymously() {
        // Python's 413/431 (issue #101) plus the stale-session
        // 5xx family (GitHub .atom 504, 2026-11-09).
        for status in [413, 431, 500, 502, 503, 504] {
            assert!(
                should_retry_without_cookies(status, true),
                "status {status} with cookies should retry"
            );
        }
    }

    #[test]
    fn other_statuses_or_no_cookies_never_retry() {
        for status in [200, 301, 304, 400, 401, 403, 404, 406, 408, 429] {
            assert!(
                !should_retry_without_cookies(status, true),
                "status {status} with cookies should not retry"
            );
        }
        for status in [413, 431, 500, 502, 503, 504] {
            assert!(
                !should_retry_without_cookies(status, false),
                "status {status} without cookies should not retry"
            );
        }
    }

    #[test]
    fn only_a_healthy_retry_replaces_the_original_error() {
        assert!(adopt_cookie_free_retry(Some(200)));
        assert!(adopt_cookie_free_retry(Some(304)));
        assert!(!adopt_cookie_free_retry(Some(401)));
        assert!(!adopt_cookie_free_retry(Some(404)));
        assert!(!adopt_cookie_free_retry(Some(504)));
        assert!(!adopt_cookie_free_retry(None));
    }
}
