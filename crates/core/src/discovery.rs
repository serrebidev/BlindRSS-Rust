//! Feed discovery + subscribe shortcuts (discovery pass).
//!
//! Ports Python `core/discovery.py::get_social_feed_url` — the
//! URL-to-feed transforms that need no network — plus HTML
//! alternate-link discovery. The synthetic forum subscriptions
//! (Google Groups, Groups.io filtered, Discourse forums) need
//! provider-side enumeration and are tracked in docs/PARITY.md.

/// Split a URL into (scheme, host, path, query) without a URL crate.
/// Sufficient for the validated shapes handled here.
fn split_url(url: &str) -> Option<(String, String, String, String)> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (host, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = host.to_lowercase();
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p.to_string(), q.split('#').next().unwrap_or("").to_string()),
        None => (
            tail.split('#').next().unwrap_or("").to_string(),
            String::new(),
        ),
    };
    Some((scheme, host, path, query))
}

fn valid_name(s: &str, extra: &str, max: usize) -> bool {
    !s.is_empty()
        && s.chars().count() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// Convert known social profile/tag/community URLs to RSS feed
/// URLs (Python `get_social_feed_url` chain, network-free part).
pub fn social_feed_url(url: &str) -> Option<String> {
    if let Some(feed) = crate::forum_sources::subscription_url(url)
        .or_else(|| crate::forum_sources::native_feed_url(url))
    {
        return Some(feed);
    }
    let (scheme, host, path, query) = split_url(url)?;
    let path_t = path.trim_end_matches('/');
    let parts: Vec<&str> = path_t.split('/').filter(|p| !p.is_empty()).collect();

    // Reddit: /r/<name> -> https://www.reddit.com/r/<name>/.rss
    if host == "reddit.com" || host.ends_with(".reddit.com") {
        if parts.len() == 2
            && parts[0].eq_ignore_ascii_case("r")
            && valid_name(parts[1], "_", 21)
            && parts[1].chars().count() >= 2
        {
            return Some(format!("https://www.reddit.com/r/{}/.rss", parts[1]));
        }
        return None;
    }

    // Bluesky: /profile/<id> -> /profile/<id>/rss
    if host == "bsky.app" || host.ends_with(".bsky.app") {
        if parts.len() >= 2 && parts[0] == "profile" && !parts[1].is_empty() {
            if parts.len() >= 3 && parts[2] == "rss" {
                return Some(url.trim().to_string());
            }
            return Some(format!("{scheme}://{host}/profile/{}/rss", parts[1]));
        }
        return None;
    }

    // Mastodon: /@user, /users/<name>, /tags/<tag> -> + ".rss"
    if path_t.to_lowercase().ends_with(".rss") {
        return Some(url.trim().to_string());
    }
    if (parts.len() == 1 && parts[0].starts_with('@'))
        || (parts.len() == 2 && (parts[0] == "users" || parts[0] == "tags"))
    {
        return Some(format!("{scheme}://{host}{path_t}.rss"));
    }

    // PieFed on piefed.social: /u/<name> -> /u/<name>/feed,
    // /c/<name> and /community/<name> -> /community/<name>/feed
    if host == "piefed.social" || host.ends_with(".piefed.social") {
        if parts.len() == 2 && parts[0] == "u" && valid_name(parts[1], "@._-", 64) {
            return Some(format!("{scheme}://{host}/u/{}/feed", parts[1]));
        }
        if parts.len() == 2
            && (parts[0] == "c" || parts[0] == "community")
            && valid_name(parts[1], "@._-", 64)
        {
            return Some(format!("{scheme}://{host}/community/{}/feed", parts[1]));
        }
        return None;
    }

    // Lemmy: /c/<name> -> /feeds/c/<name>.xml, /u/<name> -> /feeds/u/<name>.xml
    if parts.len() == 2 && parts[0] == "c" && valid_name(parts[1], "@._-", 64) {
        return Some(format!("{scheme}://{host}/feeds/c/{}.xml", parts[1]));
    }
    if parts.len() == 2 && parts[0] == "u" && valid_name(parts[1], "@._-", 64) {
        return Some(format!("{scheme}://{host}/feeds/u/{}.xml", parts[1]));
    }

    // Kbin/Mbin: /m/<name> -> /m/<name>/rss
    if parts.len() == 2 && parts[0] == "m" && valid_name(parts[1], "@._-", 64) {
        return Some(format!("{scheme}://{host}{path_t}/rss"));
    }

    // Source-feed family (Batch 35, Python discovery/local rules):
    // Google Groups group pages and addresses become the canonical
    // synthetic subscription; Groups.io group pages become the
    // group's native RSS (search URLs stay synthetic — they are a
    // listing source, handled by core::sources at refresh).
    if let Some(u) = crate::sources::google_group_subscription_url(url) {
        return Some(u);
    }
    if host == "groups.io" || host.ends_with(".groups.io") {
        if crate::sources::groups_io_search(url).is_some() {
            return Some(url.trim().to_string());
        }
        if let Some(u) = crate::sources::groups_io_feed_url(url) {
            return Some(u);
        }
        return None;
    }
    // Rumble channel/search pages normalize to their listing form;
    // the stored URL stays the listing page (a sources::classify
    // source at refresh).
    if host.contains("rumble.com") {
        return Some(crate::sources::normalize_rumble_feed_url(url));
    }
    // Odysee pages subscribe as the listing URL with tracking
    // parameters stripped.
    if host.contains("odysee.com") || host.contains("lbry.tv") {
        return Some(crate::sources::normalize_odysee_url(url));
    }
    // YouTube search pages subscribe as-is (a listing source).
    if host.contains("youtube.com") && crate::sources::youtube_search_query(url).is_some() {
        return Some(url.trim().to_string());
    }

    // YouTube: /channel/<UC..> and /playlist?list=<id>
    if host == "youtube.com" || host.ends_with(".youtube.com") {
        if let Ok(page) = url::Url::parse(url)
            && let Some((_, id)) = page
                .query_pairs()
                .find(|(key, value)| key == "list" && !value.is_empty())
        {
            let mut feed = url::Url::parse("https://www.youtube.com/feeds/videos.xml").ok()?;
            feed.query_pairs_mut().append_pair("playlist_id", &id);
            return Some(feed.into());
        }
        if parts.len() == 2 && parts[0] == "channel" && parts[1].starts_with("UC") {
            return Some(format!(
                "https://www.youtube.com/feeds/videos.xml?channel_id={}",
                parts[1]
            ));
        }
        if parts.len() == 2 && parts[0] == "user" && valid_name(parts[1], "._-", 100) {
            return Some(format!(
                "https://www.youtube.com/feeds/videos.xml?user={}",
                parts[1]
            ));
        }
        if parts.len() == 1 && parts[0] == "playlist" {
            for pair in query.split('&') {
                if let Some(id) = pair.strip_prefix("list=")
                    && !id.is_empty()
                {
                    return Some(format!(
                        "https://www.youtube.com/feeds/videos.xml?playlist_id={id}"
                    ));
                }
            }
        }
        return None;
    }

    None
}

/// Feed MIME types Python accepts on `<link rel="alternate">`
/// (`_ALTERNATE_FEED_TYPES`).
const ALTERNATE_FEED_TYPES: &[&str] = &[
    "application/rss+xml",
    "application/atom+xml",
    "application/xml",
    "text/xml",
    "application/feed+json",
    "application/x-cdf",
];

/// The wider detect-page set (`_DETECT_PAGE_FEED_TYPES`):
/// plain JSON and RDF feeds count there.
const DETECT_PAGE_FEED_TYPES: &[&str] = &[
    "application/rss+xml",
    "application/atom+xml",
    "application/xml",
    "text/xml",
    "application/feed+json",
    "application/x-cdf",
    "application/json",
    "application/rdf+xml",
];

/// One `<link rel="alternate">` feed candidate in document
/// order, resolved against the page's effective URL.
struct AlternateLink {
    url: String,
    title: String,
    index: usize,
}

fn alternate_links(html: &str, page_url: &str, types: &[&str]) -> Vec<AlternateLink> {
    let Ok(base) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<AlternateLink> = Vec::new();
    let mut idx = 0;
    let mut index = 0usize;
    while let Some(pos) = lower[idx..].find("<link") {
        let start = idx + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..start + gt + 1];
        idx = start + gt + 1;
        if !tag
            .as_bytes()
            .get(5)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
        {
            continue;
        }
        if !attr(tag, "rel").is_some_and(|rel| {
            rel.split_whitespace()
                .any(|word| word.eq_ignore_ascii_case("alternate"))
        }) || !attr(tag, "type").is_some_and(|kind| {
            types.contains(
                &kind
                    .to_ascii_lowercase()
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim(),
            )
        }) {
            continue;
        }
        let Some(href) = attr(tag, "href") else {
            continue;
        };
        let Ok(mut resolved) = base.join(&href.replace("&amp;", "&").replace("&#38;", "&")) else {
            continue;
        };
        if !matches!(resolved.scheme(), "http" | "https") || resolved.host_str().is_none() {
            continue;
        }
        resolved.set_fragment(None);
        let resolved = resolved.to_string();
        if out.iter().any(|c| c.url == resolved) {
            continue;
        }
        out.push(AlternateLink {
            url: resolved,
            title: attr(tag, "title").unwrap_or_default(),
            index,
        });
        index += 1;
    }
    out
}

/// Python `_tokenize_feed_hint`: lowercase alphanumeric
/// tokens of length >= 3.
fn feed_hint_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else {
            if current.len() >= 3 {
                tokens.push(current.clone());
            }
            current.clear();
        }
    }
    if current.len() >= 3 {
        tokens.push(current);
    }
    tokens
}

/// Rank alternate candidates the way Python
/// `_alternate_feed_candidates` does (Audit 8 F08):
/// page-specific feeds first — path/title token overlap,
/// page-slug matches — with comment feeds penalized and
/// generic index feeds slightly demoted. Ties keep
/// document order.
fn rank_alternates(mut candidates: Vec<AlternateLink>, page_url: &str) -> Vec<AlternateLink> {
    let page_tokens: Vec<String> = url::Url::parse(page_url)
        .map(|u| feed_hint_tokens(&u.path().replace('/', " ")))
        .unwrap_or_default();
    let page_slug = page_tokens.join("-");
    candidates.sort_by(|a, b| {
        alternate_score(b, &page_tokens, &page_slug)
            .cmp(&alternate_score(a, &page_tokens, &page_slug))
            .then_with(|| a.index.cmp(&b.index))
    });
    candidates
}

fn alternate_score(cand: &AlternateLink, page_tokens: &[String], page_slug: &str) -> i64 {
    let cand_path = url::Url::parse(&cand.url)
        .map(|u| u.path().to_lowercase())
        .unwrap_or_default();
    let cand_tokens = feed_hint_tokens(&cand_path.replace('/', " "));
    let title_tokens = feed_hint_tokens(&cand.title);
    let mut score: i64 = 0;
    score += 8 * page_tokens
        .iter()
        .filter(|t| cand_tokens.contains(t))
        .count() as i64;
    score += 10
        * page_tokens
            .iter()
            .filter(|t| title_tokens.contains(t))
            .count() as i64;
    if !page_slug.is_empty() && cand_path.contains(page_slug) {
        score += 12;
    }
    if !page_slug.is_empty()
        && cand
            .title
            .to_lowercase()
            .replace(' ', "-")
            .contains(page_slug)
    {
        score += 12;
    }
    if cand_path.ends_with("/index.xml") || cand_path.ends_with("/rss.xml") {
        score -= 1;
    }
    if regex::Regex::new(r"(?:^|/)rss/(index\.xml)?$")
        .map(|re| re.is_match(&cand_path))
        .unwrap_or(false)
    {
        score -= 3;
    }
    let comment_path = regex::Regex::new(r"(?:^|/)(comments?|comment-feed)(?:/|$)")
        .map(|re| re.is_match(&cand_path))
        .unwrap_or(false);
    if comment_path || cand.title.to_lowercase().contains("comment") {
        score -= 12;
    }
    score
}

/// Extract feed URLs advertised by a page's
/// `<link rel="alternate">` tags, resolved against the
/// page URL and ranked best-first (Python
/// `_alternate_feed_candidates`; pure, fixture-tested).
pub fn discover_in_html(html: &str, page_url: &str) -> Vec<String> {
    rank_alternates(
        alternate_links(html, page_url, ALTERNATE_FEED_TYPES),
        page_url,
    )
    .into_iter()
    .map(|c| c.url)
    .collect()
}

/// Anchor-href feed candidates (Python `discover_feeds`
/// step 2): hrefs ending in a feed extension or containing
/// "/feed" / "rss", resolved against the effective URL.
pub fn anchor_feed_urls(html: &str, page_url: &str) -> Vec<String> {
    let Ok(base) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("<a") {
        let start = idx + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..start + gt + 1];
        idx = start + gt + 1;
        if !tag
            .as_bytes()
            .get(2)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
        {
            continue;
        }
        let Some(href) = attr(tag, "href") else {
            continue;
        };
        let h = href.to_lowercase();
        if !(h.ends_with(".rss")
            || h.ends_with(".atom")
            || h.ends_with(".xml")
            || h.ends_with(".json")
            || h.contains("/feed")
            || h.contains("rss"))
        {
            continue;
        }
        let Ok(mut resolved) = base.join(&href.replace("&amp;", "&").replace("&#38;", "&")) else {
            continue;
        };
        if !matches!(resolved.scheme(), "http" | "https") || resolved.host_str().is_none() {
            continue;
        }
        resolved.set_fragment(None);
        let resolved = resolved.to_string();
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!(
        r#"(?i)(?:^|\s){}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#,
        regex::escape(name)
    );
    let re = regex::Regex::new(&pattern).ok()?;
    let captures = re.captures(tag)?;
    (1..=3).find_map(|i| captures.get(i).map(|m| m.as_str().to_string()))
}

/// Conventional feed paths for sites that advertise no alternate link.
pub fn common_feed_candidates(page_url: &str) -> Vec<String> {
    let Ok(mut page) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    if !matches!(page.scheme(), "http" | "https") || page.host_str().is_none() {
        return Vec::new();
    }
    page.set_query(None);
    page.set_fragment(None);
    let mut root = page.clone();
    root.set_path("/");
    let mut bases = vec![page.as_str().trim_end_matches('/').to_string()];
    if root != page {
        bases.push(root.as_str().trim_end_matches('/').to_string());
    }
    let paths = [
        "/feed",
        "/rss",
        "/feed.xml",
        "/rss.xml",
        "/atom.xml",
        "/index.xml",
        "/backend.xml",
        "/index.rss",
        "/index.atom",
        "/atom",
        "/feeds",
        "?feed=rss2",
        "?rss=1",
    ];
    let mut candidates = Vec::new();
    for base in bases {
        for path in paths {
            let candidate = format!("{base}{path}");
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

fn probe_common_feeds(page_url: &str) -> Vec<(String, crate::feeds::ParsedFeed)> {
    let candidates = common_feed_candidates(page_url);
    let results = std::sync::Mutex::new(Vec::new());
    let index = std::sync::atomic::AtomicUsize::new(0);
    // Python bounds discovery to six workers and a five-second probe timeout.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .user_agent(crate::http::user_agent())
        .build();
    let agent = ureq::Agent::new_with_config(config);
    std::thread::scope(|scope| {
        for _ in 0..6.min(candidates.len()) {
            let agent = agent.clone();
            let candidates = &candidates;
            let index = &index;
            let results = &results;
            scope.spawn(move || {
                loop {
                    let i = index.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(candidate) = candidates.get(i) else {
                        break;
                    };
                    let parsed = agent
                        .get(candidate)
                        .call()
                        .ok()
                        .and_then(|mut response| {
                            response
                                .body_mut()
                                .with_config()
                                .limit(8 * 1024 * 1024)
                                .read_to_vec()
                                .ok()
                        })
                        .and_then(|bytes| crate::feeds::parse_feed(&bytes).ok());
                    if let Some(parsed) = parsed {
                        results.lock().unwrap_or_else(|e| e.into_inner()).push((
                            i,
                            candidate.clone(),
                            parsed,
                        ));
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(i, _, _)| *i);
    results
        .into_iter()
        .map(|(_, url, parsed)| (url, parsed))
        .collect()
}

/// The subscription URL for a listing-source page (Rumble/Odysee
/// channel, YouTube search, SoundCloud/Mixcloud user or playlist):
/// the URL the subscription stores, normalized the way Python's
/// `_resolve_feed_url` normalizes it. `None` for ordinary feed
/// URLs and for the forum sources — Google Groups and Groups.io
/// search keep their independent, working forum dispatch
/// (Audit 8 F01).
pub fn source_listing_url(url: &str) -> Option<String> {
    let kind = crate::sources::classify(url)?;
    match kind {
        crate::sources::SourceKind::GoogleGroup | crate::sources::SourceKind::GroupsIoSearch => {
            None
        }
        crate::sources::SourceKind::Odysee => Some(crate::sources::normalize_odysee_url(url)),
        crate::sources::SourceKind::Rumble => Some(crate::sources::normalize_rumble_feed_url(url)),
        _ => Some(url.trim().to_string()),
    }
}

/// Resolve a listing-source page to its subscription document.
/// Python's `add_feed` derives a YouTube search subscription's
/// title from the query (`YouTube: {query}`) and fetches nothing —
/// refresh enumerates the items later, under ids shaped with the
/// real feed id — so that branch returns an empty entry list.
/// The other listing sources return their current items.
fn resolve_source_listing(listing_url: &str) -> Result<(String, crate::feeds::ParsedFeed), String> {
    let kind =
        crate::sources::classify(listing_url).ok_or_else(|| "not a listing source".to_string())?;
    if kind == crate::sources::SourceKind::YoutubeSearch {
        let query = crate::sources::youtube_search_query(listing_url)
            .ok_or_else(|| "not a YouTube search URL".to_string())?;
        return Ok((
            listing_url.to_string(),
            crate::feeds::ParsedFeed {
                title: format!("YouTube: {query}"),
                entries: Vec::new(),
            },
        ));
    }
    let (title, items) = crate::sources::fetch_listing(listing_url, kind, "", 0)?;
    Ok((
        listing_url.to_string(),
        crate::feeds::ParsedFeed {
            title,
            entries: crate::sources::items_to_entries(items),
        },
    ))
}

/// One plain page fetch with its status and EFFECTIVE
/// (post-redirect) URL — the base relative feed links must
/// resolve against (Audit 8 F09).
struct PageResponse {
    status: u16,
    body: String,
    effective_url: String,
    content_type: String,
}

fn fetch_page_response(url: &str) -> Result<PageResponse, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .user_agent(crate::http::user_agent())
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent
        .get(url)
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    use ureq::ResponseExt as _;
    let status = resp.status().as_u16();
    let effective_url = resp.get_uri().to_string();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(PageResponse {
        status,
        body: crate::extract::decode_bytes(&bytes, &content_type, ""),
        effective_url,
        content_type,
    })
}

/// The browser escalation's page: (html, effective URL).
fn browser_page(url: &str) -> Result<(String, String), String> {
    if !crate::browser::fallback_enabled() {
        return Err("browser fallback is disabled".to_string());
    }
    let timeout = crate::browser::fallback_timeout_seconds();
    crate::browser::fetch_page_with_options(url, timeout, false, "", None).map(|response| {
        let effective = if response.url.trim().is_empty() {
            url.to_string()
        } else {
            response.url.clone()
        };
        (response.html, effective)
    })
}

/// Page HTML + effective URL for discovery, with Python's
/// browser escalation (`core/discovery.py::detect_page_feeds`,
/// Audit 8 B01): the plain fetch first; when it fails, is an
/// error status, or the body it returns is a challenge page,
/// one browser fetch — only when the fallback is enabled in
/// Settings. A failed plain fetch with a failed browser fetch
/// reports the plain error; a challenge shell the browser
/// could not clear is still scanned as a page (Python
/// proceeds with it and finds no feeds). The impersonated
/// discovery retry Python runs between the two is the
/// pending B16 transport seam (see PARITY.md).
fn discovery_page(url: &str) -> Result<(String, String), String> {
    match fetch_page_response(url) {
        Ok(resp) if resp.status < 400 && !crate::browser::is_challenge_page(&resp.body) => {
            Ok((resp.body, resp.effective_url))
        }
        Ok(resp) if resp.status < 400 => {
            Ok(browser_page(url).unwrap_or((resp.body, resp.effective_url)))
        }
        Ok(resp) => browser_page(url).map_err(|_| format!("HTTP {}", resp.status)),
        Err(error) => browser_page(url).map_err(|_| error),
    }
}

/// Python `discover_feeds`: enumerate every candidate —
/// native/social transforms first, then ranked page
/// alternates, anchor hrefs and validated common-path
/// probes (all resolved against the EFFECTIVE page URL),
/// with OpenRSS as the final fallback (Audit 8 F08/F09).
pub fn discover_feed_urls(url: &str) -> Result<Vec<String>, String> {
    if looks_like_feed_url(url) {
        return Ok(vec![url.trim().to_string()]);
    }
    if let Some(feed_url) = ytdlp_feed_url(url) {
        return Ok(vec![feed_url]);
    }
    if let Some(feed_url) =
        crate::forum_sources::subscription_url(url).or_else(|| social_feed_url(url))
    {
        return Ok(vec![feed_url]);
    }
    if let Some(listing_url) = source_listing_url(url) {
        return Ok(vec![listing_url]);
    }
    if crate::feeds::fetch_and_parse(url).is_ok() {
        return Ok(vec![url.trim().to_string()]);
    }
    let mut feeds: Vec<String> = Vec::new();
    let add = |feeds: &mut Vec<String>, candidate: String| {
        if !candidate.is_empty() && !feeds.contains(&candidate) {
            feeds.push(candidate);
        }
    };
    if let Ok((html, effective)) = discovery_page(url) {
        if body_looks_like_feed(&html, "") {
            return Ok(vec![effective]);
        }
        for candidate in discover_in_html(&html, &effective) {
            add(&mut feeds, candidate);
        }
        for candidate in anchor_feed_urls(&html, &effective) {
            add(&mut feeds, candidate);
        }
        for (candidate, _) in probe_common_feeds(&effective) {
            add(&mut feeds, candidate);
        }
    }
    if feeds.is_empty()
        && let Some(openrss) = openrss_feed_url(url)
    {
        feeds.push(openrss);
    }
    Ok(feeds)
}

/// Resolve a user-entered URL to a subscribable feed:
/// Python `discover_feed`'s order — feed-shaped URL, the
/// yt-dlp native conversion, social transform, listing
/// sources, the URL itself as a feed, ranked HTML
/// alternates, validated common paths, and OpenRSS last.
/// Returns (feed_url, parsed feed).
pub fn resolve_subscribe_url(url: &str) -> Result<(String, crate::feeds::ParsedFeed), String> {
    if let Some(feed_url) = crate::forum_sources::subscription_url(url) {
        let parsed = crate::forum_sources::fetch_feed(&feed_url)
            .ok_or("Unrecognized forum subscription")??;
        return Ok((feed_url, parsed));
    }
    if let Some(feed_url) = ytdlp_feed_url(url) {
        let parsed = crate::feeds::fetch_and_parse(&feed_url)?;
        return Ok((feed_url, parsed));
    }
    if let Some(feed_url) = social_feed_url(url) {
        // Some social transforms return a listing-source page
        // itself (YouTube search, Rumble/Odysee channels): it has
        // no native feed document, so resolve it as a listing.
        if let Some(listing_url) = source_listing_url(&feed_url) {
            return resolve_source_listing(&listing_url);
        }
        let parsed = crate::feeds::fetch_and_parse(&feed_url)?;
        return Ok((feed_url, parsed));
    }
    // Listing sources have no native feed document at the page
    // URL; resolve them through their source fetcher instead of
    // assuming feed XML (Audit 8 F01).
    if let Some(listing_url) = source_listing_url(url) {
        return resolve_source_listing(&listing_url);
    }
    if let Ok(parsed) = crate::feeds::fetch_and_parse(url) {
        return Ok((url.trim().to_string(), parsed));
    }
    let (html, effective) =
        discovery_page(url).map_err(|e| format!("no feed at URL and page fetch failed: {e}"))?;
    if body_looks_like_feed(&html, "") {
        let parsed = crate::feeds::fetch_and_parse(&effective)?;
        return Ok((effective, parsed));
    }
    for candidate in discover_in_html(&html, &effective) {
        if let Ok(parsed) = crate::feeds::fetch_and_parse(&candidate) {
            return Ok((candidate, parsed));
        }
    }
    if let Some((candidate, parsed)) = probe_common_feeds(&effective).into_iter().next() {
        return Ok((candidate, parsed));
    }
    // Last resort: OpenRSS generates feeds for many sites
    // with no native RSS (validated before use).
    if let Some(openrss) = openrss_feed_url(url)
        && let Ok(parsed) = crate::feeds::fetch_and_parse(&openrss)
    {
        return Ok((openrss, parsed));
    }
    Err("no feed found at that URL".to_string())
}

// ---------------------------------------------------------------------------
// Python discovery predicates and the yt-dlp native conversion
// ---------------------------------------------------------------------------

/// Python `_host_matches`: exact host or a subdomain of
/// `domain`.
pub fn host_matches(host: &str, domain: &str) -> bool {
    let host = host.trim().to_lowercase();
    let host = host.trim_end_matches('.');
    let domain = domain.trim().to_lowercase();
    let domain = domain.trim_end_matches('.');
    !host.is_empty()
        && !domain.is_empty()
        && (host == domain || host.ends_with(&format!(".{domain}")))
}

fn is_youtube_host(host: &str) -> bool {
    host_matches(host, "youtube.com") || host_matches(host, "youtu.be")
}

/// Python `_looks_like_feed_url`: feed-shaped path or a
/// feed-selecting query parameter.
pub fn looks_like_feed_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let path_low = parsed.path().to_lowercase();
    if path_low.ends_with(".rss")
        || path_low.ends_with(".xml")
        || path_low.ends_with(".atom")
        || path_low.ends_with(".cdf")
    {
        return true;
    }
    if path_low.ends_with("/feed") || path_low.ends_with("/feeds") {
        return true;
    }
    if path_low.contains("/feed/") || path_low.contains("/feeds/") {
        return true;
    }
    for (key, value) in parsed.query_pairs() {
        let key_low = key.to_lowercase();
        let value_low = value.trim().to_lowercase();
        if (key_low == "feed" || key_low == "rss")
            && [
                "1", "true", "yes", "feed", "rss", "rss2", "atom", "rdf", "xml", "json",
                "jsonfeed", "cdf",
            ]
            .contains(&value_low.as_str())
        {
            return true;
        }
        if key_low == "format"
            && ["rss", "rss2", "atom", "rdf", "xml", "jsonfeed", "cdf"]
                .contains(&value_low.as_str())
        {
            return true;
        }
    }
    false
}

/// Python `_body_looks_like_feed`: a JSON Feed document, or
/// XML whose root is rss/channel-bearing, an Atom feed, an
/// RDF channel, or a bare channel with items.
pub fn body_looks_like_feed(body: &str, content_type: &str) -> bool {
    let text = body.trim_start();
    if text.is_empty() {
        return false;
    }
    if (text.starts_with('{')
        || text.starts_with('[')
        || content_type.to_lowercase().contains("json"))
        && let Ok(payload) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(obj) = payload.as_object()
    {
        let version = obj.get("version").and_then(|v| v.as_str()).unwrap_or("");
        let title_ok = obj
            .get("title")
            .and_then(|v| v.as_str())
            .is_some_and(|t| !t.trim().is_empty());
        let items_ok = obj.get("items").is_some_and(|v| v.is_array());
        if [
            "https://jsonfeed.org/version/1",
            "https://jsonfeed.org/version/1.1",
        ]
        .contains(&version)
            && title_ok
            && items_ok
        {
            return true;
        }
    }
    // XML root sniff: skip the prolog, comments and doctype,
    // then read the root element's local name.
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(stripped) = rest.strip_prefix("<?") {
            match stripped.find("?>") {
                Some(end) => rest = &stripped[end + 2..],
                None => return false,
            }
        } else if let Some(stripped) = rest.strip_prefix("<!--") {
            match stripped.find("-->") {
                Some(end) => rest = &stripped[end + 3..],
                None => return false,
            }
        } else if let Some(stripped) = rest.strip_prefix("<!") {
            match stripped.find('>') {
                Some(end) => rest = &stripped[end + 1..],
                None => return false,
            }
        } else {
            break;
        }
    }
    if !rest.starts_with('<') {
        return false;
    }
    let tag_end = rest
        .find(['>', ' ', '\t', '\r', '\n'])
        .unwrap_or(rest.len());
    let raw_name = &rest[1..tag_end];
    let local = raw_name
        .rsplit('}')
        .next()
        .unwrap_or(raw_name)
        .rsplit(':')
        .next()
        .unwrap_or(raw_name)
        .to_lowercase();
    let root_open_end = rest.find('>').unwrap_or(rest.len());
    let root_open = &rest[..root_open_end];
    let has_child = |name: &str| {
        rest.to_lowercase().contains(&format!("<{name}"))
            || rest.to_lowercase().contains(&format!(":{name}"))
    };
    match local.as_str() {
        "rss" => has_child("channel"),
        "feed" => {
            root_open.contains("http://www.w3.org/2005/Atom")
                || root_open.contains("http://purl.org/atom/ns#")
        }
        "rdf" => {
            root_open.contains("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                && has_child("channel")
        }
        "channel" => has_child("item"),
        _ => false,
    }
}

/// Python `is_ytdlp_supported`, offline part: the fast
/// domain allowlist plus the article-path rejection. The
/// final extractor-regex consultation Python does cannot
/// run through the yt-dlp CLI bridge (names only, no
/// patterns), so a non-allowlisted host is treated as
/// unsupported here; this only feeds the subscribe-target
/// clearing and the Add Feed hint.
pub fn ytdlp_supported_offline(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let scheme = parsed.scheme();
    if !matches!(scheme, "http" | "https" | "lbry") {
        return false;
    }
    let host = parsed.host_str().unwrap_or("");
    if matches!(scheme, "http" | "https") && host.is_empty() {
        return false;
    }
    for known in [
        "youtube.com",
        "youtu.be",
        "vimeo.com",
        "twitch.tv",
        "dailymotion.com",
        "soundcloud.com",
        "facebook.com",
        "twitter.com",
        "x.com",
        "tiktok.com",
        "instagram.com",
        "rumble.com",
        "bilibili.com",
        "mixcloud.com",
        "odysee.com",
        "lbry.tv",
    ] {
        if host_matches(host, known) {
            return true;
        }
    }
    let path_low = parsed.path().to_lowercase();
    let looks_like_media = [
        "/video/", "/videos/", "/watch", "/clip", "/player", "/av/", "/reel/", "/embed",
        "/podcast", "/audio", "/episode", "/track",
    ]
    .iter()
    .any(|hint| path_low.contains(hint));
    if !looks_like_media {
        let date_path = regex::Regex::new(r"/\d{4}/\d{2}/\d{2}/")
            .map(|re| re.is_match(&path_low))
            .unwrap_or(false);
        if date_path
            || ["/news/", "/article", "/story/"]
                .iter()
                .any(|h| path_low.contains(h))
        {
            return false;
        }
    }
    false
}

/// The channel id in one yt-dlp channel/playlist info
/// document (Python's acceptance rule in
/// `get_ytdlp_feed_url`: `channel_id` or `id`, and only
/// for playlist/channel documents).
fn youtube_channel_id_from_info(data: &serde_json::Value) -> Option<String> {
    let doc_type = data.get("_type").and_then(|v| v.as_str()).unwrap_or("");
    if doc_type != "playlist" && doc_type != "channel" {
        return None;
    }
    for key in ["channel_id", "id"] {
        if let Some(id) = data.get(key).and_then(|v| v.as_str())
            && !id.trim().is_empty()
        {
            return Some(id.trim().to_string());
        }
    }
    None
}

/// Resolve a YouTube handle/custom URL to its channel id
/// through yt-dlp metadata (Audit 8 F07): a flat dump with
/// NO video enumeration, tried anonymously first and then
/// with each configured browser cookie source — the cookie
/// retries are what recover channels hidden behind
/// consent interstitials and sign-in gates, exactly as in
/// Python's `get_ytdlp_feed_url`. Cookie values are never
/// logged or stored; yt-dlp reads them itself.
fn resolve_youtube_channel_id(url: &str) -> Option<String> {
    let exe = crate::media::find_ytdlp()?;
    for attempt in crate::ytdlp_recovery::cookie_attempts(None) {
        let mut args: Vec<String> = vec![
            "--ignore-config".to_string(),
            "--dump-single-json".to_string(),
            "--flat-playlist".to_string(),
            "--playlist-items".to_string(),
            "0".to_string(),
        ];
        args.extend(attempt.args());
        args.push(url.to_string());
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let Ok(out) =
            crate::dependency::run_tool(&exe, &arg_refs, std::time::Duration::from_secs(15))
        else {
            continue;
        };
        if !out.success {
            continue;
        }
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&out.stdout) else {
            continue;
        };
        if let Some(id) = youtube_channel_id_from_info(&data) {
            return Some(id);
        }
    }
    None
}

/// Python `get_ytdlp_feed_url`: the native RSS feed for a
/// yt-dlp-supported URL (YouTube today): the feed URL
/// itself, playlist and /channel/ and /user/ shapes
/// directly, and handle/custom channel URLs through the
/// yt-dlp metadata recovery above.
pub fn ytdlp_feed_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // Search-results URLs have no native RSS; they are
    // enumerated via yt-dlp on refresh.
    if crate::sources::youtube_search_query(url).is_some() {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if !is_youtube_host(host) {
        return None;
    }
    // Already the native feed.
    if parsed
        .path()
        .trim_end_matches('/')
        .eq_ignore_ascii_case("/feeds/videos.xml")
    {
        return Some(url.to_string());
    }
    if let Some((_, id)) = parsed
        .query_pairs()
        .find(|(key, value)| key == "list" && !value.is_empty())
    {
        return Some(format!(
            "https://www.youtube.com/feeds/videos.xml?playlist_id={id}"
        ));
    }
    if let Some(pos) = url.find("/channel/") {
        let id = url[pos + "/channel/".len()..]
            .split(['/', '?'])
            .next()
            .unwrap_or("");
        if !id.is_empty() {
            return Some(format!(
                "https://www.youtube.com/feeds/videos.xml?channel_id={id}"
            ));
        }
    }
    if let Some(pos) = url.find("/user/") {
        let user = url[pos + "/user/".len()..]
            .split(['/', '?'])
            .next()
            .unwrap_or("");
        if !user.is_empty() {
            return Some(format!(
                "https://www.youtube.com/feeds/videos.xml?user={user}"
            ));
        }
    }
    // Handle, custom and every other channel shape: yt-dlp
    // metadata recovery with cookie retries.
    resolve_youtube_channel_id(url)
        .map(|id| format!("https://www.youtube.com/feeds/videos.xml?channel_id={id}"))
}

/// Python `openrss_feed_url`: the OpenRSS proxy feed for a
/// page, returned only after a successful, feed-shaped
/// response validates it (Audit 8 F08's final fallback).
pub fn openrss_feed_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?;
    if host.is_empty() || host_matches(host, "openrss.org") {
        return None;
    }
    let netloc = parsed
        .port()
        .map(|port| format!("{host}:{port}"))
        .unwrap_or_else(|| host.to_string());
    let mut candidate = format!("https://openrss.org/feed/{netloc}{}", parsed.path());
    if let Some(query) = parsed.query()
        && !query.is_empty()
    {
        candidate.push('?');
        candidate.push_str(query);
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .user_agent(crate::http::user_agent())
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent.get(&candidate).call().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    let body = crate::extract::decode_bytes(&bytes, &content_type, "");
    if body_looks_like_feed(&body, &content_type) {
        Some(candidate)
    } else {
        None
    }
}

/// Python `site_cookies.looks_like_challenge_response`:
/// a bot-check interstitial behind a 401/403/503.
pub fn looks_like_challenge_response(status: u16, body: &str) -> bool {
    if ![401, 403, 503].contains(&status) {
        return false;
    }
    let text: String = body.chars().take(20_000).collect();
    [
        "challenges.cloudflare.com",
        "cf-mitigated",
        "Just a moment...",
        "_cf_chl_opt",
        "Performing security verification",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

// ---------------------------------------------------------------------------
// Detect Feeds on Page (Audit 8 F10) — Python `detect_page_feeds`
// ---------------------------------------------------------------------------

/// One detected feed on a page: the link title (often
/// empty; the Add Feed dialog reads the real title from
/// the feed) and the feed URL.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedFeed {
    pub title: String,
    pub url: String,
}

/// Python `PageFetchError`: the page for feed detection
/// could not be retrieved. `is_challenge` marks a
/// bot-verification interstitial, so callers can point the
/// user at Tools > Import Site Cookies.
#[derive(Debug, Clone, PartialEq)]
pub struct PageFetchError {
    pub message: String,
    pub is_challenge: bool,
}

impl std::fmt::Display for PageFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PageFetchError {}

/// Python `_native_channel_feed_for_page`: a YouTube
/// channel/playlist page resolves straight to its native
/// feed. The title is left empty on purpose: the Add Feed
/// dialog reads the real channel title from the feed.
fn native_channel_feed_for_page(page_url: &str) -> Option<DetectedFeed> {
    let parsed = url::Url::parse(page_url).ok()?;
    if !is_youtube_host(parsed.host_str()?) {
        return None;
    }
    if crate::sources::youtube_search_query(page_url).is_some() {
        return None;
    }
    let path = parsed.path();
    let low_path = path.to_lowercase();
    let is_playlist = parsed
        .query_pairs()
        .any(|(k, v)| k == "list" && !v.is_empty());
    let is_channel = path.starts_with("/@")
        || ["/channel/", "/c/", "/user/"]
            .iter()
            .any(|prefix| low_path.starts_with(prefix));
    if !is_playlist && !is_channel {
        return None;
    }
    let feed_url = ytdlp_feed_url(page_url)?;
    Some(DetectedFeed {
        title: String::new(),
        url: feed_url,
    })
}

/// Python `_vbulletin_feed_candidates`: vBulletin forums
/// advertise feeds as ordinary external.php anchors. Once
/// one same-host RSS anchor proves the page shape, derive
/// the all-forums feed plus one per visible /forums/<id>.
fn vbulletin_feed_candidates(html: &str, page_url: &str) -> Vec<DetectedFeed> {
    let Ok(page) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    if !matches!(page.scheme(), "http" | "https") || page.host_str().is_none() {
        return Vec::new();
    }
    let page_host = page.host_str().unwrap_or("").to_lowercase();
    // Collect (resolved URL, anchor text) pairs.
    let lower = html.to_ascii_lowercase();
    let mut anchors: Vec<(String, String)> = Vec::new();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("<a") {
        let start = idx + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        idx = start + gt + 1;
        let tag = &html[start..start + gt + 1];
        if !tag
            .as_bytes()
            .get(2)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
        {
            continue;
        }
        let Some(href) = attr(tag, "href") else {
            continue;
        };
        let Ok(resolved) = page.join(&href.replace("&amp;", "&").replace("&#38;", "&")) else {
            continue;
        };
        // Anchor text: up to the matching close tag.
        let text = match lower[idx..].find("</a>") {
            Some(end) => {
                let frag = &html[idx..idx + end];
                frag.split('<')
                    .enumerate()
                    .map(|(i, piece)| {
                        if i == 0 {
                            piece.to_string()
                        } else {
                            piece
                                .split_once('>')
                                .map(|(_, t)| t.to_string())
                                .unwrap_or_default()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            None => String::new(),
        };
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        anchors.push((resolved.to_string(), text));
    }
    let saw_external_rss = anchors.iter().any(|(resolved, _)| {
        let Ok(candidate) = url::Url::parse(resolved) else {
            return false;
        };
        candidate.host_str().unwrap_or("").to_lowercase() == page_host
            && candidate.path().to_lowercase().ends_with("/external.php")
            && candidate
                .query_pairs()
                .find(|(k, _)| k == "type")
                .is_some_and(|(_, v)| {
                    ["RSS", "RSS1", "RSS2", "XML"].contains(&v.to_uppercase().as_str())
                })
    });
    if !saw_external_rss {
        return Vec::new();
    }
    let origin = format!("{}://{}", page.scheme(), page.host_str().unwrap_or(""));
    let origin = match page.port() {
        Some(port) => format!("{origin}:{port}"),
        None => origin,
    };
    let mut results = vec![DetectedFeed {
        title: "All forums".to_string(),
        url: format!("{origin}/external.php?type=RSS2"),
    }];
    let forum_re = regex::Regex::new(r"^/forums/([0-9]+)(?:-|/|$)").unwrap();
    let mut seen_ids: Vec<String> = Vec::new();
    for (resolved, text) in &anchors {
        let Ok(candidate) = url::Url::parse(resolved) else {
            continue;
        };
        if candidate.host_str().unwrap_or("").to_lowercase() != page_host {
            continue;
        }
        let Some(caps) = forum_re.captures(candidate.path()) else {
            continue;
        };
        let forum_id = caps.get(1).unwrap().as_str().to_string();
        if seen_ids.contains(&forum_id) || text.is_empty() {
            continue;
        }
        seen_ids.push(forum_id.clone());
        results.push(DetectedFeed {
            title: text.clone(),
            url: format!("{origin}/external.php?type=RSS2&forumids={forum_id}"),
        });
    }
    results
}

/// Python `detect_page_feeds` (Audit 8 F10): the dedicated
/// page-detection chain — native YouTube channel first,
/// then the page fetch (plain, the B16 impersonation seam,
/// then Task 24's browser recovery), feed documents,
/// `<link rel="alternate">` in document order with the
/// wider detect type set, and vBulletin anchors. Fetch and
/// challenge failures raise `PageFetchError` so the caller
/// can give the Import Site Cookies guidance.
pub fn detect_page_feeds(url: &str) -> Result<Vec<DetectedFeed>, PageFetchError> {
    let url = url.trim();
    let url = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    if let Some(native) = native_channel_feed_for_page(&url) {
        return Ok(vec![native]);
    }
    let mut fetch_error: Option<String> = None;
    let mut resp: Option<PageResponse> = None;
    match fetch_page_response(&url) {
        Ok(r) => {
            if r.status >= 400 {
                fetch_error = Some(format!("HTTP {}", r.status));
            }
            resp = Some(r);
        }
        Err(e) => fetch_error = Some(e),
    }
    // The impersonated discovery retry Python runs here is
    // the pending B16 transport (see PARITY.md); the chain
    // continues with the browser fetch.
    let needs_browser = match &resp {
        None => true,
        Some(r) => [401, 403, 429, 503].contains(&r.status) || r.status >= 520,
    };
    if needs_browser && let Ok((html, effective)) = browser_page(&url) {
        resp = Some(PageResponse {
            status: 200,
            body: html,
            effective_url: effective,
            content_type: String::new(),
        });
    }
    let Some(resp) = resp else {
        return Err(PageFetchError {
            message: fetch_error.unwrap_or_else(|| "no response".to_string()),
            is_challenge: false,
        });
    };
    if !(200..400).contains(&resp.status) {
        if looks_like_challenge_response(resp.status, &resp.body) {
            return Err(PageFetchError {
                message: format!("HTTP {} (bot challenge)", resp.status),
                is_challenge: true,
            });
        }
        return Err(PageFetchError {
            message: format!("HTTP {}", resp.status),
            is_challenge: false,
        });
    }
    let mut body = resp.body;
    let mut effective = resp.effective_url;
    if resp.status == 200 && crate::browser::is_challenge_page(&body) {
        match browser_page(&url) {
            Ok((html, eff)) => {
                body = html;
                effective = eff;
            }
            Err(_) => {
                return Err(PageFetchError {
                    message: "bot challenge".to_string(),
                    is_challenge: true,
                });
            }
        }
    }
    if body_looks_like_feed(&body, &resp.content_type) {
        return Ok(vec![DetectedFeed {
            title: String::new(),
            url: effective,
        }]);
    }
    let mut out: Vec<DetectedFeed> = alternate_links(&body, &effective, DETECT_PAGE_FEED_TYPES)
        .into_iter()
        .map(|link| DetectedFeed {
            title: link.title,
            url: link.url,
        })
        .collect();
    for candidate in vbulletin_feed_candidates(&body, &effective) {
        if !out.iter().any(|f| f.url == candidate.url) {
            out.push(candidate);
        }
    }
    Ok(out)
}
