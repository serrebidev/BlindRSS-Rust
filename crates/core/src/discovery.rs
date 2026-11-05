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

/// Extract feed URLs advertised by a page's
/// `<link rel="alternate" type="application/rss+xml|atom+xml">` tags,
/// resolved against the page URL (pure; fixture-tested).
pub fn discover_in_html(html: &str, page_url: &str) -> Vec<String> {
    let Ok(base) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut idx = 0;
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
            matches!(
                kind.to_ascii_lowercase()
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim(),
                "application/rss+xml" | "application/atom+xml"
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

/// Page HTML for discovery, with Python's browser
/// escalation (`core/discovery.py::detect_page_feeds`,
/// Audit 8 B01): the plain fetch first; when it fails or
/// the body it returns is a challenge page, one browser
/// fetch — only when the fallback is enabled in Settings.
/// A failed plain fetch with a failed browser fetch
/// reports the plain error; a challenge shell the browser
/// could not clear is still scanned as a page (Python
/// proceeds with it and finds no feeds). The impersonated
/// discovery retry Python runs between the two is part of
/// the pending B16 transport (see PARITY.md).
fn discovery_page_html(url: &str) -> Result<String, String> {
    match crate::extract::fetch_html(url) {
        Ok(html) if !crate::browser::is_challenge_page(&html) => Ok(html),
        Ok(html) => Ok(browser_page_html(url).unwrap_or(html)),
        Err(error) => browser_page_html(url).map_err(|_| error),
    }
}

fn browser_page_html(url: &str) -> Result<String, String> {
    if !crate::browser::fallback_enabled() {
        return Err("browser fallback is disabled".to_string());
    }
    let timeout = crate::browser::fallback_timeout_seconds();
    crate::browser::fetch_page_with_options(url, timeout, false, "", None)
        .map(|response| response.html)
}

/// Find all advertised feeds, falling back to common paths when necessary.
pub fn discover_feed_urls(url: &str) -> Result<Vec<String>, String> {
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
    let html = discovery_page_html(url)?;
    let advertised = discover_in_html(&html, url);
    if !advertised.is_empty() {
        return Ok(advertised);
    }
    Ok(probe_common_feeds(url)
        .into_iter()
        .map(|(url, _)| url)
        .collect())
}

/// Resolve a user-entered URL to a subscribable feed: social
/// transform, then the URL itself as a feed, then HTML alternate
/// links. Returns (feed_url, parsed feed).
pub fn resolve_subscribe_url(url: &str) -> Result<(String, crate::feeds::ParsedFeed), String> {
    if let Some(feed_url) = crate::forum_sources::subscription_url(url) {
        let parsed = crate::forum_sources::fetch_feed(&feed_url)
            .ok_or("Unrecognized forum subscription")??;
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
    let html = discovery_page_html(url)
        .map_err(|e| format!("no feed at URL and page fetch failed: {e}"))?;
    for candidate in discover_in_html(&html, url) {
        if let Ok(parsed) = crate::feeds::fetch_and_parse(&candidate) {
            return Ok((candidate, parsed));
        }
    }
    if let Some((candidate, parsed)) = probe_common_feeds(url).into_iter().next() {
        return Ok((candidate, parsed));
    }
    Err("no feed found at that URL".to_string())
}
