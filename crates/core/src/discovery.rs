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
    if host.contains("bsky.app") {
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
    if host.ends_with("piefed.social") {
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

    // YouTube: /channel/<UC..> and /playlist?list=<id>
    if host.contains("youtube.com") {
        if parts.len() == 2 && parts[0] == "channel" && parts[1].starts_with("UC") {
            return Some(format!(
                "https://www.youtube.com/feeds/videos.xml?channel_id={}",
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
    let (scheme, host, path, _) = match split_url(page_url) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let base_dir = match path.rfind('/') {
        Some(i) => path[..=i].to_string(),
        None => "/".to_string(),
    };
    let lower = html.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("<link") {
        let start = idx + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..start + gt + 1];
        let tag_low = &lower[start..start + gt + 1];
        idx = start + gt + 1;
        if !tag_low.contains("alternate")
            || !(tag_low.contains("application/rss+xml")
                || tag_low.contains("application/atom+xml"))
        {
            continue;
        }
        let Some(href) = attr(tag, "href") else {
            continue;
        };
        let resolved = if href.starts_with("http://") || href.starts_with("https://") {
            href
        } else if let Some(rest) = href.strip_prefix("//") {
            format!("{scheme}://{rest}")
        } else if href.starts_with('/') {
            format!("{scheme}://{host}{href}")
        } else {
            format!("{scheme}://{host}{base_dir}{href}")
        };
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let low = tag.to_lowercase();
    let pos = low.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[pos..];
    let mut chars = rest.chars();
    match chars.next()? {
        '"' | '\'' => {
            let q = rest.chars().next()?;
            rest[1..].split(q).next().map(|s| s.to_string())
        }
        _ => rest
            .split(|c: char| c.is_whitespace() || c == '>')
            .next()
            .map(|s| s.to_string()),
    }
}

/// Resolve a user-entered URL to a subscribable feed: social
/// transform, then the URL itself as a feed, then HTML alternate
/// links. Returns (feed_url, parsed feed).
pub fn resolve_subscribe_url(url: &str) -> Result<(String, crate::feeds::ParsedFeed), String> {
    if let Some(feed_url) = social_feed_url(url) {
        let parsed = crate::feeds::fetch_and_parse(&feed_url)?;
        return Ok((feed_url, parsed));
    }
    if let Ok(parsed) = crate::feeds::fetch_and_parse(url) {
        return Ok((url.trim().to_string(), parsed));
    }
    let html = crate::extract::fetch_html(url)
        .map_err(|e| format!("no feed at URL and page fetch failed: {e}"))?;
    for candidate in discover_in_html(&html, url) {
        if let Ok(parsed) = crate::feeds::fetch_and_parse(&candidate) {
            return Ok((candidate, parsed));
        }
    }
    Err("no feed found at that URL".to_string())
}
