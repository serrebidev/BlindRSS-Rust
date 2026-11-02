//! Public feed directories used by Python's Find Feeds dialog.

use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProvider {
    ApplePodcasts,
    GPodder,
    Fyyd,
    Podverse,
    Feedly,
    NewsBlur,
    Feedsearch,
    Reddit,
    Lemmy,
    Mastodon,
    Bluesky,
    PieFed,
    Mixcloud,
    Feedspot,
    GroupsIo,
    Website,
    GoogleNews,
    BingNews,
    Awesome,
    Fediverse,
    YouTube,
    SoundCloud,
    NewsApis,
}

pub const SEARCH_PROVIDERS: &[(SearchProvider, &str)] = &[
    (SearchProvider::NewsBlur, "NewsBlur"),
    (SearchProvider::ApplePodcasts, "Apple Podcasts"),
    (SearchProvider::GPodder, "gPodder"),
    (SearchProvider::Fyyd, "fyyd"),
    (SearchProvider::Podverse, "Podverse"),
    (SearchProvider::Feedly, "Feedly"),
    (SearchProvider::Feedsearch, "Feedsearch"),
    (SearchProvider::Feedspot, "Feedspot"),
    (SearchProvider::GoogleNews, "Google News"),
    (SearchProvider::BingNews, "Bing News"),
    (SearchProvider::Mixcloud, "Mixcloud"),
    (SearchProvider::Awesome, "Publisher Catalogue (Awesome RSS)"),
    (SearchProvider::Reddit, "Reddit"),
    (SearchProvider::GroupsIo, "Groups.io"),
    (SearchProvider::Fediverse, "Fediverse (all)"),
    (SearchProvider::Mastodon, "Mastodon"),
    (SearchProvider::Bluesky, "Bluesky"),
    (SearchProvider::PieFed, "PieFed"),
    (SearchProvider::Lemmy, "Lemmy/Kbin"),
    (SearchProvider::Website, "Website scan (URL or site name)"),
    (SearchProvider::YouTube, "YouTube"),
    (SearchProvider::SoundCloud, "SoundCloud"),
    (
        SearchProvider::NewsApis,
        "News APIs (configured API key required)",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedResult {
    pub title: String,
    pub url: String,
    pub description: String,
}

/// Build the same public API request as the Python directory implementation.
pub fn request_url(provider: SearchProvider, query: &str, limit: usize) -> Result<String, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Enter a search term or site URL".to_string());
    }
    let (endpoint, key) = match provider {
        SearchProvider::ApplePodcasts => ("https://itunes.apple.com/search", "term"),
        SearchProvider::GPodder => ("https://gpodder.net/search.json", "q"),
        SearchProvider::Fyyd => ("https://api.fyyd.de/0.2/search/podcast", "term"),
        SearchProvider::Podverse => ("https://api.podverse.fm/api/v1/podcast", "searchTitle"),
        SearchProvider::Feedly => ("https://cloud.feedly.com/v3/search/feeds", "q"),
        SearchProvider::NewsBlur => ("https://www.newsblur.com/discover/autocomplete", "term"),
        SearchProvider::Feedsearch => ("https://feedsearch.dev/api/v1/search", "url"),
        SearchProvider::Reddit => ("https://www.reddit.com/subreddits/search.json", "q"),
        SearchProvider::Lemmy => ("https://lemmy.world/api/v3/search", "q"),
        SearchProvider::Mastodon => ("https://mastodon.social/api/v2/search", "q"),
        SearchProvider::Bluesky => (
            "https://public.api.bsky.app/xrpc/app.bsky.actor.searchActorsTypeahead",
            "q",
        ),
        SearchProvider::PieFed => ("https://piefed.social/api/alpha/search", "q"),
        SearchProvider::Mixcloud => ("https://api.mixcloud.com/search/", "q"),
        SearchProvider::GroupsIo => ("https://groups.io/search", "q"),
        SearchProvider::BingNews => ("https://www.bing.com/news/search", "q"),
        SearchProvider::Feedspot => {
            let slug = regex::Regex::new("[^a-z0-9]+")
                .unwrap()
                .replace_all(&query.to_ascii_lowercase(), "_")
                .trim_matches('_')
                .to_string();
            if slug.is_empty() {
                return Err("Enter a topic containing letters or numbers".into());
            }
            return Ok(format!("https://rss.feedspot.com/{slug}_rss_feeds/"));
        }
        SearchProvider::Website => {
            return site_target(query)
                .ok_or_else(|| "Enter a site URL or single site name".to_string());
        }
        _ => return Err("This source builds multiple requests".to_string()),
    };
    let mut url = url::Url::parse(endpoint).map_err(|e| e.to_string())?;
    let site = if provider == SearchProvider::Feedsearch
        && !query.contains(['.', ' '])
        && !query.contains("://")
    {
        format!(
            "{}.com",
            query
                .to_ascii_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
        )
    } else {
        query.to_string()
    };
    url.query_pairs_mut().append_pair(key, &site);
    let limit = limit.clamp(1, 100).to_string();
    match provider {
        SearchProvider::ApplePodcasts => {
            url.query_pairs_mut()
                .append_pair("media", "podcast")
                .append_pair("limit", &limit);
        }
        SearchProvider::Fyyd => {
            url.query_pairs_mut().append_pair("count", &limit);
        }
        SearchProvider::NewsBlur => {
            url.query_pairs_mut().append_pair("v", "2");
        }
        SearchProvider::Lemmy | SearchProvider::PieFed => {
            url.query_pairs_mut()
                .append_pair("type_", "Communities")
                .append_pair("limit", &limit);
            if provider == SearchProvider::Lemmy {
                url.query_pairs_mut().append_pair("sort", "TopAll");
            }
        }
        SearchProvider::Mastodon => {
            url.query_pairs_mut().append_pair("limit", &limit);
            if query.contains('@') && query.contains('.') {
                url.query_pairs_mut().append_pair("resolve", "true");
            }
        }
        SearchProvider::Bluesky => {
            url.query_pairs_mut().append_pair(
                "limit",
                &limit.parse::<usize>().unwrap_or(10).min(10).to_string(),
            );
        }
        SearchProvider::Reddit => {
            url.query_pairs_mut().append_pair("limit", &limit);
        }
        SearchProvider::Mixcloud => {
            url.query_pairs_mut()
                .append_pair("type", "user")
                .append_pair("limit", &limit);
        }
        SearchProvider::BingNews => {
            url.query_pairs_mut().append_pair("format", "rss");
        }
        _ => {}
    }
    Ok(url.into())
}

fn text<'a>(item: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|key| {
            item.get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
}

/// Normalize directory JSON, ignoring entries without a subscribable HTTP URL.
pub fn parse_results(
    provider: SearchProvider,
    json: &str,
    limit: usize,
) -> Result<Vec<FeedResult>, String> {
    let data: Value =
        serde_json::from_str(json).map_err(|e| format!("Invalid feed search response: {e}"))?;
    if matches!(
        provider,
        SearchProvider::Reddit
            | SearchProvider::Lemmy
            | SearchProvider::Mastodon
            | SearchProvider::Bluesky
            | SearchProvider::PieFed
            | SearchProvider::Mixcloud
    ) {
        return parse_social(provider, &data, limit);
    }
    let items = match provider {
        SearchProvider::ApplePodcasts | SearchProvider::Feedly => data.get("results"),
        SearchProvider::Fyyd => data.get("data"),
        SearchProvider::Podverse => data.get(0),
        SearchProvider::NewsBlur if data.is_object() => data.get("feeds"),
        _ => Some(&data),
    }
    .and_then(Value::as_array)
    .ok_or("Feed directory returned an unexpected response")?;
    // Python's NewsBlur autocomplete search caps at 20 results.
    let limit = if provider == SearchProvider::NewsBlur {
        limit.min(20)
    } else {
        limit
    };
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    for item in items {
        if results.len() >= limit.min(100) {
            break;
        }
        let (title, address, mut description) = match provider {
            SearchProvider::ApplePodcasts => (
                text(item, &["collectionName"]),
                text(item, &["feedUrl"]),
                text(item, &["artistName"]).to_string(),
            ),
            SearchProvider::GPodder => (
                text(item, &["title"]),
                text(item, &["url"]),
                text(item, &["author"]).to_string(),
            ),
            SearchProvider::Fyyd => (
                text(item, &["title"]),
                text(item, &["xmlURL"]),
                text(item, &["author", "subtitle"]).to_string(),
            ),
            SearchProvider::Podverse => {
                let address = item
                    .get("feedUrls")
                    .and_then(Value::as_array)
                    .and_then(|urls| {
                        urls.iter()
                            .map(|v| text(v, &["url"]))
                            .find(|s| !s.is_empty())
                    })
                    .unwrap_or("");
                (
                    text(item, &["title"]),
                    address,
                    text(item, &["subtitle", "description"]).to_string(),
                )
            }
            SearchProvider::Feedly => (
                text(item, &["title"]),
                text(item, &["feedId"]).strip_prefix("feed/").unwrap_or(""),
                text(item, &["description"]).to_string(),
            ),
            SearchProvider::NewsBlur => {
                let tagline = text(item, &["tagline"]);
                let description = match item.get("num_subscribers").and_then(Value::as_u64) {
                    Some(n) if n > 0 && !tagline.is_empty() => {
                        format!("{tagline} ({n} subscribers)")
                    }
                    Some(n) if n > 0 => format!("{n} subscribers"),
                    _ => tagline.to_string(),
                };
                (
                    text(item, &["label"]),
                    text(item, &["value", "address"]),
                    description,
                )
            }
            _ => (
                text(item, &["title"]),
                text(item, &["url"]),
                text(item, &["site_name"]).to_string(),
            ),
        };
        let Ok(url) = url::Url::parse(address) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !seen.insert(url.to_string())
        {
            continue;
        }
        if description.is_empty() {
            description = SEARCH_PROVIDERS
                .iter()
                .find(|(p, _)| *p == provider)
                .map(|(_, label)| *label)
                .unwrap_or("")
                .to_string();
        }
        results.push(FeedResult {
            title: if title.is_empty() {
                address.to_string()
            } else {
                title.to_string()
            },
            url: address.to_string(),
            description,
        });
    }
    Ok(results)
}

pub fn search(
    provider: SearchProvider,
    query: &str,
    limit: usize,
) -> Result<Vec<FeedResult>, String> {
    search_configured(provider, query, limit, &crate::config::Config::default())
}

fn fetch(address: &str) -> Result<String, String> {
    // Each worker owns its agent; user configuration cannot remove the directory deadline.
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .user_agent(crate::http::user_agent())
            .timeout_global(Some(std::time::Duration::from_secs(15)))
            .build(),
    );
    let mut response = agent
        .get(address)
        .call()
        .map_err(|_| "Directory request failed or timed out".to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_to_string()
        .map_err(|_| "Could not read directory response".to_string())
}

pub fn search_configured(
    provider: SearchProvider,
    query: &str,
    limit: usize,
    config: &crate::config::Config,
) -> Result<Vec<FeedResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Enter a search term or site URL".into());
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = limit.min(100);
    match provider {
        SearchProvider::Fediverse => {
            return search_group_providers(
                &[
                    SearchProvider::Mastodon,
                    SearchProvider::Bluesky,
                    SearchProvider::PieFed,
                    SearchProvider::Lemmy,
                ],
                query,
                limit,
                config,
            )
            .map(|r| r.results);
        }
        SearchProvider::GoogleNews => return google_news_results(query, limit, config),
        SearchProvider::BingNews => {
            return Ok(vec![FeedResult {
                title: format!("Bing News: {query}"),
                url: request_url(provider, query, limit)?,
                description: "News search feed".into(),
            }]);
        }
        SearchProvider::Website => {
            let mut results =
                crate::discovery::discover_feed_urls(&request_url(provider, query, limit)?)?;
            results.truncate(limit);
            // Python `_search_blindrss`: fetch the feed title for
            // the first few candidates so results read as names,
            // not raw URLs (capped at five extra fetches).
            let mut results: Vec<FeedResult> = results
                .into_iter()
                .map(|url| FeedResult {
                    title: url.clone(),
                    url,
                    description: "Website scan".into(),
                })
                .collect();
            for result in results.iter_mut().take(5) {
                if let Ok(body) = fetch(&result.url)
                    && let Ok(parsed) = crate::feeds::parse_feed(body.as_bytes())
                    && !parsed.title.trim().is_empty()
                {
                    result.title = parsed.title.trim().to_string();
                }
            }
            return Ok(results);
        }
        SearchProvider::Awesome => return awesome_search(query, limit),
        SearchProvider::YouTube => return youtube_search(query, limit),
        SearchProvider::SoundCloud => return soundcloud_search(query, limit),
        SearchProvider::NewsApis => return news_api_search(query, limit, config),
        _ => {}
    }
    let address = request_url(provider, query, limit)?;
    let body = fetch(&address)?;
    if matches!(
        provider,
        SearchProvider::Feedspot | SearchProvider::GroupsIo
    ) {
        return parse_html_results(provider, &body, limit);
    }
    let mut results = parse_results(provider, &body, limit)?;
    if provider == SearchProvider::PieFed && results.len() < limit {
        let address = address.replace("type_=Communities", "type_=Users");
        results.extend(parse_results(
            provider,
            &fetch(&address)?,
            limit - results.len(),
        )?);
    }
    if provider == SearchProvider::Bluesky && results.len() < limit {
        // Python `_bluesky_search_response_to_feeds`: a tag-like
        // query also earns an OpenRSS hashtag result.
        if let Some(tag) = bluesky_tag_candidate(query)
            && !results.iter().any(|r| r.title == format!("#{tag}"))
        {
            results.push(FeedResult {
                title: format!("#{tag}"),
                url: format!("https://openrss.org/https://bsky.app/search?q=%23{tag}"),
                description: "Bluesky tag (OpenRSS, third-party)".into(),
            });
        }
    }
    if provider == SearchProvider::Mixcloud {
        // Python `search_mixcloud_feeds`: a few of each user's
        // playlists follow the user himself (Mixcloud has no global
        // playlist search).
        let mut expanded = Vec::new();
        for user in results {
            if expanded.len() >= limit {
                break;
            }
            let playlists = mixcloud_playlists(&user);
            expanded.push(user);
            for playlist in playlists {
                if expanded.len() >= limit {
                    break;
                }
                if !expanded.iter().any(|r: &FeedResult| r.url == playlist.url) {
                    expanded.push(playlist);
                }
            }
        }
        results = expanded;
    }
    Ok(results)
}

/// Python `_normalize_tag_candidate`: a single hashtag-like token.
pub fn bluesky_tag_candidate(query: &str) -> Option<String> {
    let query = query
        .trim()
        .strip_prefix('#')
        .unwrap_or(query.trim())
        .trim();
    if query.is_empty()
        || query.len() > 64
        || !query
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    {
        return None;
    }
    Some(query.to_string())
}

/// Parse one Mixcloud user's playlist list (Python
/// `search_mixcloud_feeds`' per-user playlist surfacing).
pub fn parse_mixcloud_playlists(json: &str, user_name: &str, limit: usize) -> Vec<FeedResult> {
    let Ok(data) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(items) = data.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let address = text(item, &["url"]);
        if address.is_empty() {
            continue;
        }
        let name = text(item, &["name"]);
        out.push(FeedResult {
            title: format!(
                "{} ({})",
                if name.is_empty() { address } else { name },
                user_name
            ),
            url: address.to_string(),
            description: "Mixcloud playlist".into(),
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn mixcloud_playlists(user: &FeedResult) -> Vec<FeedResult> {
    let Ok(parsed) = url::Url::parse(&user.url) else {
        return Vec::new();
    };
    let path = parsed.path().trim_end_matches('/');
    if path.is_empty() {
        return Vec::new();
    }
    let address = format!("https://api.mixcloud.com{path}/playlists/?limit=3");
    match fetch(&address) {
        Ok(body) => parse_mixcloud_playlists(&body, &user.title, 3),
        Err(_) => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SearchGroup {
    All,
    Podcasts,
    Rss,
}

pub fn site_target(query: &str) -> Option<String> {
    let query = query.trim();
    if query.contains("://") {
        return Some(query.to_string());
    }
    if query.contains(char::is_whitespace) || query.is_empty() {
        return None;
    }
    if query.contains('.') {
        return Some(format!("https://{query}"));
    }
    let slug: String = query
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!slug.is_empty()).then(|| format!("https://{slug}.com"))
}

pub fn selected_providers(group: SearchGroup, query: &str) -> Vec<SearchProvider> {
    SEARCH_PROVIDERS
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| {
            let podcast = matches!(
                p,
                SearchProvider::ApplePodcasts
                    | SearchProvider::GPodder
                    | SearchProvider::Fyyd
                    | SearchProvider::Podverse
                    | SearchProvider::Mixcloud
                    | SearchProvider::SoundCloud
            );
            let explicit = matches!(
                p,
                SearchProvider::Feedly
                    | SearchProvider::Mastodon
                    | SearchProvider::Bluesky
                    | SearchProvider::PieFed
                    | SearchProvider::Lemmy
                    | SearchProvider::NewsApis
            );
            !explicit
                && match group {
                    SearchGroup::All => true,
                    SearchGroup::Podcasts => podcast,
                    SearchGroup::Rss => {
                        !podcast
                            || matches!(p, SearchProvider::Mixcloud | SearchProvider::SoundCloud)
                    }
                }
                && (*p != SearchProvider::Feedsearch
                    || query.contains('.')
                    || query.contains("://"))
                && (*p != SearchProvider::Website || site_target(query).is_some())
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct SearchReport {
    pub results: Vec<FeedResult>,
    pub errors: Vec<String>,
}

pub fn search_all(query: &str, limit: usize) -> Result<Vec<FeedResult>, String> {
    search_group(
        SearchGroup::All,
        query,
        limit,
        &crate::config::Config::default(),
    )
    .map(|r| r.results)
}

pub fn search_group(
    group: SearchGroup,
    query: &str,
    limit: usize,
    config: &crate::config::Config,
) -> Result<SearchReport, String> {
    search_group_providers(&selected_providers(group, query), query, limit, config)
}

pub fn search_group_providers(
    providers: &[SearchProvider],
    query: &str,
    limit: usize,
    config: &crate::config::Config,
) -> Result<SearchReport, String> {
    if query.trim().is_empty() {
        return Err("Enter a search term or site URL".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    for provider in providers.iter().copied() {
        let (tx, query, config) = (tx.clone(), query.to_string(), config.clone());
        std::thread::spawn(move || {
            let _ = tx.send((
                provider,
                search_configured(provider, &query, limit, &config),
            ));
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut report = SearchReport::default();
    let mut completed = 0;
    let mut successes = 0;
    let mut responses = Vec::new();
    while completed < providers.len() {
        match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok((provider, result)) => {
                completed += 1;
                responses.push((provider, result));
            }
            Err(_) => {
                report
                    .errors
                    .push("Some directories exceeded the search deadline".into());
                break;
            }
        }
    }
    responses.sort_by_key(|(p, _)| {
        providers
            .iter()
            .position(|provider| provider == p)
            .unwrap_or(usize::MAX)
    });
    let mut seen = HashSet::new();
    for (provider, response) in responses {
        match response {
            Ok(items) => {
                successes += 1;
                for item in items {
                    if seen.insert(item.url.clone()) {
                        report.results.push(item);
                    }
                }
            }
            Err(error) => report
                .errors
                .push(format!("{}: {error}", provider_label(provider))),
        }
    }
    report.results.sort_by_key(|r| {
        if r.url
            .starts_with("https://www.youtube.com/feeds/videos.xml")
        {
            0
        } else if r.url.starts_with("https://news.google.com/rss") {
            2
        } else {
            1
        }
    });
    report.results.truncate(limit.min(100));
    if successes == 0 {
        Err(report.errors.join("; "))
    } else {
        Ok(report)
    }
}

fn provider_label(provider: SearchProvider) -> &'static str {
    SEARCH_PROVIDERS
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, label)| *label)
        .unwrap_or("Directory")
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:@".contains(c))
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn count_field(item: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        item.get(*key)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
    })
}

fn parse_social(
    provider: SearchProvider,
    data: &Value,
    limit: usize,
) -> Result<Vec<FeedResult>, String> {
    let mut normalized = Vec::new();
    let mut add = |title: &str, address: String, description: &str| {
        normalized
            .push(serde_json::json!({"title": title, "url": address, "site_name": description}))
    };
    let items = match provider {
        SearchProvider::Reddit => data.get("data").and_then(|d| d.get("children")),
        SearchProvider::Lemmy | SearchProvider::PieFed => data
            .get("communities")
            .filter(|rows| rows.as_array().is_some_and(|rows| !rows.is_empty()))
            .or_else(|| data.get("users"))
            .or_else(|| data.get("communities")),
        SearchProvider::Mastodon => data.get("accounts"),
        SearchProvider::Bluesky => data.get("actors"),
        SearchProvider::Mixcloud => data.get("data"),
        _ => None,
    }
    .and_then(Value::as_array)
    .ok_or("Feed directory returned an unexpected response")?;
    for item in items {
        match provider {
            SearchProvider::Reddit => {
                let item = &item["data"];
                let name = text(item, &["display_name"]);
                if valid_identifier(name) && !name.contains([':', '@', '.']) {
                    // Python `_search_reddit`: description falls back
                    // to "r/<name>", with a subscriber suffix.
                    let mut detail = text(item, &["public_description", "title"]).to_string();
                    if detail.is_empty() {
                        detail = format!("r/{name}");
                    }
                    if let Some(subs) = count_field(item, &["subscribers"]).filter(|n| *n != 0) {
                        detail = format!("{detail} ({subs} subs)");
                    }
                    add(
                        &format!("r/{name}"),
                        format!("https://www.reddit.com/r/{name}/.rss"),
                        &detail,
                    );
                }
            }
            SearchProvider::Lemmy | SearchProvider::PieFed => {
                let wrapper = item;
                let item = item
                    .get("community")
                    .or_else(|| item.get("person"))
                    .unwrap_or(&Value::Null);
                let actor = text(item, &["actor_id"]);
                let Some(feed) = federated_feed(actor, provider == SearchProvider::PieFed) else {
                    continue;
                };
                let counts = wrapper.get("counts").unwrap_or(&Value::Null);
                let detail = if provider == SearchProvider::PieFed {
                    // Python `_piefed_search_response_to_feeds`.
                    if wrapper.get("community").is_some() {
                        let mut detail = "PieFed community".to_string();
                        let name = text(item, &["name"]);
                        if !name.is_empty() {
                            detail = format!("{detail} ({name})");
                        }
                        let domain = text(item, &["ap_domain"]);
                        if !domain.is_empty() {
                            detail = format!("{detail} - {domain}");
                        }
                        if let Some(subs) = count_field(
                            counts,
                            &["subscriptions_count", "total_subscriptions_count"],
                        ) {
                            detail = format!("{detail} ({subs} subs)");
                        }
                        detail
                    } else {
                        let mut detail = "Fediverse user (via PieFed)".to_string();
                        let user_name = text(item, &["user_name"]);
                        if !user_name.is_empty() {
                            detail = format!("{detail} (@{user_name})");
                        }
                        let posts = count_field(counts, &["post_count"]);
                        let comments = count_field(counts, &["comment_count"]);
                        if posts.is_some() || comments.is_some() {
                            detail = format!(
                                "{detail} ({} posts, {} comments)",
                                posts.unwrap_or(0),
                                comments.unwrap_or(0)
                            );
                        }
                        detail
                    }
                } else {
                    // Python `_search_lemmy_items`: "Lemmy - name
                    // (N subs)" / "Kbin - name (N subs)".
                    let label = if actor.contains("/m/") {
                        "Kbin"
                    } else {
                        "Lemmy"
                    };
                    let name = text(item, &["name"]);
                    let mut detail = format!("{label} - {name}");
                    if let Some(subs) = count_field(counts, &["subscribers"]).filter(|n| *n != 0) {
                        detail = format!("{detail} ({subs} subs)");
                    }
                    detail
                };
                add(text(item, &["title", "name", "user_name"]), feed, &detail);
            }
            SearchProvider::Mastodon => {
                if let Some(feed) = crate::discovery::social_feed_url(text(item, &["url"])) {
                    // Python `_mastodon_search_response_to_feeds`.
                    let acct = text(item, &["acct"]);
                    let mut detail = "Mastodon user".to_string();
                    if !acct.is_empty() {
                        detail = format!("{detail} (@{acct})");
                    }
                    if let Some(followers) = count_field(item, &["followers_count"]) {
                        detail = format!("{detail} ({followers} followers)");
                    }
                    let title = text(item, &["display_name"]);
                    let fallback = format!("@{acct}");
                    add(
                        if title.is_empty() && !acct.is_empty() {
                            &fallback
                        } else {
                            title
                        },
                        feed,
                        &detail,
                    );
                }
            }
            SearchProvider::Bluesky => {
                let handle = text(item, &["handle"]);
                if valid_identifier(handle) {
                    // Python `_bluesky_search_response_to_feeds`.
                    let mut detail = format!("Bluesky user (@{handle})");
                    let did = text(item, &["did"]);
                    if !did.is_empty() {
                        detail = format!("{detail} ({did})");
                    }
                    let title = text(item, &["displayName"]);
                    let fallback = format!("@{handle}");
                    add(
                        if title.is_empty() { &fallback } else { title },
                        format!("https://bsky.app/profile/{handle}/rss"),
                        &detail,
                    );
                }
            }
            SearchProvider::Mixcloud => {
                // Python `search_mixcloud_feeds`: follower counts in
                // the detail; the user's playlists are surfaced by
                // the search path (see search_configured).
                let mut detail = "Mixcloud user".to_string();
                if let Some(followers) = count_field(item, &["follower_count"]).filter(|n| *n > 0) {
                    detail = format!("{detail} • {} followers", thousands(followers as u64));
                }
                add(
                    text(item, &["name", "username"]),
                    text(item, &["url"]).to_string(),
                    &detail,
                );
            }
            _ => {}
        }
    }
    if provider == SearchProvider::Mastodon
        && let Some(tags) = data.get("hashtags").and_then(Value::as_array)
    {
        for tag in tags {
            let name = text(tag, &["name"]);
            let mut detail = "Mastodon tag".to_string();
            if let Some(uses) = tag
                .get("history")
                .and_then(Value::as_array)
                .and_then(|history| history.first())
                .and_then(|day| count_field(day, &["uses"]))
            {
                detail = format!("{detail} ({uses} recent uses)");
            }
            if let Some(feed) = crate::discovery::social_feed_url(text(tag, &["url"])) {
                add(&format!("#{name}"), feed, &detail);
            } else if valid_identifier(name) {
                add(
                    &format!("#{name}"),
                    format!("https://mastodon.social/tags/{name}.rss"),
                    &detail,
                );
            }
        }
    }
    parse_results(
        SearchProvider::Feedsearch,
        &Value::Array(normalized).to_string(),
        limit,
    )
}

fn federated_feed(actor: &str, piefed: bool) -> Option<String> {
    let mut url = url::Url::parse(actor).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    let path = url.path().trim_end_matches('/').to_string();
    let path = if piefed && url.host_str() == Some("piefed.social") && path.starts_with("/c/") {
        format!("/community/{}/feed", path.strip_prefix("/c/")?)
    } else if piefed && url.host_str() == Some("piefed.social") && path.starts_with("/u/") {
        format!("{path}/feed")
    } else if let Some(name) = path.strip_prefix("/c/") {
        format!("/feeds/c/{name}.xml")
    } else if let Some(name) = path.strip_prefix("/u/") {
        format!("/feeds/u/{name}.xml")
    } else if path.starts_with("/m/") {
        format!("{path}/rss")
    } else {
        return crate::discovery::social_feed_url(actor);
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

pub fn google_news_results(
    query: &str,
    limit: usize,
    config: &crate::config::Config,
) -> Result<Vec<FeedResult>, String> {
    let country = config.get_str("discovery_country", "CA").to_uppercase();
    let language = config.get_str("discovery_language", "en").to_lowercase();
    let make = |path: &str,
                term: Option<&str>,
                title: String,
                description: &str|
     -> Result<FeedResult, String> {
        let mut url = url::Url::parse(&format!("https://news.google.com{path}"))
            .map_err(|e| e.to_string())?;
        if let Some(term) = term {
            url.query_pairs_mut().append_pair("q", term);
        }
        url.query_pairs_mut()
            .append_pair("hl", &format!("{language}-{country}"))
            .append_pair("gl", &country)
            .append_pair("ceid", &format!("{country}:{language}"));
        Ok(FeedResult {
            title,
            url: url.into(),
            description: description.into(),
        })
    };
    let mut results = vec![make(
        "/rss/search",
        Some(query),
        format!("Google News: {query}"),
        "News search feed (not a single publisher)",
    )?];
    // Python's GOOGLE_NEWS_TOPIC_IDS (discovery.py, used by
    // search_google_news_localized).
    let topic = match query.to_ascii_lowercase().as_str() {
        "world" => "WORLD",
        "politics" => "NATION",
        "business" => "BUSINESS",
        "technology" | "gaming" => "TECHNOLOGY",
        "science" => "SCIENCE",
        "health" => "HEALTH",
        "entertainment" => "ENTERTAINMENT",
        "sports" => "SPORTS",
        _ => "",
    };
    if !topic.is_empty() {
        // Python titles the topic result with the capitalized term.
        let mut capitalized = query.to_ascii_lowercase();
        if let Some(first) = capitalized.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        results.push(make(
            &format!("/rss/headlines/section/topic/{topic}"),
            None,
            format!("Google News: {capitalized} headlines"),
            "News topic feed",
        )?);
    }
    let place = [
        config.get_str("discovery_city", ""),
        config.get_str("discovery_region", ""),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    if !place.is_empty() {
        results.push(make(
            "/rss/search",
            Some(&place),
            format!("Google News: {place} local news"),
            "Local news search feed (not a single publisher)",
        )?);
    }
    results.push(make(
        "/rss",
        None,
        format!("Google News: {country} headlines"),
        "National headlines feed",
    )?);
    results.truncate(limit.min(100));
    Ok(results)
}

pub fn parse_html_results(
    provider: SearchProvider,
    html: &str,
    limit: usize,
) -> Result<Vec<FeedResult>, String> {
    let anchor = regex::Regex::new(r#"(?is)<a\b([^>]*)>(.*?)</a>"#).unwrap();
    let attr = |tag: &str, name: &str| -> String {
        let pattern = format!(r#"(?is)\b{}\s*=\s*["']([^"']*)["']"#, regex::escape(name));
        regex::Regex::new(&pattern)
            .unwrap()
            .captures(tag)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().replace("&amp;", "&"))
            .unwrap_or_default()
    };
    let plain = |html: &str| crate::extract::html_to_text(html);
    if provider == SearchProvider::GroupsIo {
        // Python `search_groups_io_feeds`: one result per table
        // row, detail = the row's text with the title prefix
        // removed, falling back to "Groups.io group".
        let base = url::Url::parse("https://groups.io/search").unwrap();
        let row_re = regex::Regex::new(r"(?is)<tr\b[^>]*>(.*?)</tr>").unwrap();
        let mut rows = Vec::new();
        for row in row_re.captures_iter(html) {
            let row_html = &row[1];
            let Some(anchor) = anchor.captures(row_html) else {
                continue;
            };
            let href = attr(&anchor[1], "href");
            if !href.contains("/g/") {
                continue;
            }
            let Some(address) = base
                .join(&href)
                .ok()
                .and_then(|url| crate::sources::groups_io_feed_url(url.as_str()))
            else {
                continue;
            };
            let label = plain(&anchor[2]);
            let title = if label.is_empty() {
                address
                    .trim_end_matches("/rss")
                    .rsplit('/')
                    .next()
                    .unwrap_or("Groups.io")
                    .to_string()
            } else {
                label
            };
            let detail = plain(row_html);
            let detail = if detail.to_lowercase().starts_with(&title.to_lowercase()) {
                detail[title.len()..]
                    .trim_start_matches([' ', '-', '–', '—'])
                    .to_string()
            } else {
                detail
            };
            rows.push(serde_json::json!({"title": title, "url": address, "site_name": if detail.is_empty() { "Groups.io group".to_string() } else { detail }}));
            if rows.len() >= limit.min(100) {
                break;
            }
        }
        return parse_results(
            SearchProvider::Feedsearch,
            &Value::Array(rows).to_string(),
            limit,
        );
    }
    let mut rows = Vec::new();
    let mut title = String::new();
    let base = url::Url::parse("https://groups.io/search").unwrap();
    for captures in anchor.captures_iter(html) {
        let tag = &captures[1];
        let href = attr(tag, "href");
        let label = plain(&captures[2]);
        let address = match provider {
            SearchProvider::GroupsIo => base
                .join(&href)
                .ok()
                .and_then(|url| crate::sources::groups_io_feed_url(url.as_str())),
            SearchProvider::Feedspot => {
                let classes = attr(tag, "class");
                if !classes.split_whitespace().any(|c| c == "ext") {
                    if !label.is_empty() {
                        title = label;
                    }
                    continue;
                }
                if classes.split_whitespace().any(|c| c == "extdomain") {
                    continue;
                }
                url::Url::parse(&href)
                    .ok()
                    .filter(|u| {
                        !u.host_str()
                            .is_some_and(|h| h == "feedspot.com" || h.ends_with(".feedspot.com"))
                    })
                    .map(String::from)
            }
            _ => return Err("Expected an HTML feed directory".into()),
        };
        if let Some(address) = address {
            // Python `_search_feedspot`: the detail is the feed's host.
            let detail = if provider == SearchProvider::Feedspot {
                url::Url::parse(&address)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_else(|| provider_label(provider).to_string())
            } else {
                provider_label(provider).to_string()
            };
            rows.push(serde_json::json!({"title": if provider == SearchProvider::Feedspot && !title.is_empty() { std::mem::take(&mut title) } else { label }, "url": address, "site_name": detail}));
        }
    }
    parse_results(
        SearchProvider::Feedsearch,
        &Value::Array(rows).to_string(),
        limit,
    )
}

fn awesome_search(query: &str, limit: usize) -> Result<Vec<FeedResult>, String> {
    let countries = [
        "Canada",
        "United States",
        "United Kingdom",
        "Australia",
        "France",
        "Germany",
        "India",
        "Ireland",
        "Italy",
        "Japan",
        "Mexico",
        "Poland",
        "Spain",
        "Ukraine",
        "Brazil",
        "South Africa",
    ];
    let topics = [
        "Android",
        "Apple",
        "Business & Economy",
        "Cyber security",
        "Environment",
        "Food",
        "Football",
        "Gaming",
        "History",
        "Movies",
        "Music",
    ];
    let query = query.to_lowercase();
    let (tx, rx) = std::sync::mpsc::channel();
    for (folder, names) in [
        ("countries/with_category", countries.as_slice()),
        ("recommended/without_category", topics.as_slice()),
    ] {
        for name in names {
            let (tx, name) = (tx.clone(), name.to_string());
            std::thread::spawn(move || {
                let mut address = url::Url::parse(
                    "https://raw.githubusercontent.com/plenaryapp/awesome-rss-feeds/master/",
                )
                .unwrap();
                address.set_path(&format!(
                    "/plenaryapp/awesome-rss-feeds/master/{folder}/{name}.opml"
                ));
                let result = fetch(address.as_str()).and_then(|body| parse_catalogue(&body, &name));
                let _ = tx.send(result);
            });
        }
    }
    drop(tx);
    let mut matches = Vec::new();
    let mut successes = 0;
    for entries in rx.into_iter().flatten() {
        successes += 1;
        for entry in entries {
            let haystack =
                format!("{} {} {}", entry.title, entry.description, entry.url).to_lowercase();
            if haystack.contains(&query)
                || query
                    .split_whitespace()
                    .any(|word| word.len() > 3 && haystack.contains(word))
            {
                matches.push(entry);
            }
        }
    }
    if successes == 0 {
        return Err("Publisher catalogue could not be downloaded".into());
    }
    matches.sort_by(|a, b| a.url.cmp(&b.url));
    matches.dedup_by(|a, b| a.url == b.url);
    matches.truncate(limit);
    Ok(matches)
}

pub fn parse_catalogue(xml: &str, category: &str) -> Result<Vec<FeedResult>, String> {
    let doc = opml::OPML::from_str(xml).map_err(|_| "Invalid publisher catalogue".to_string())?;
    fn walk(outlines: &[opml::Outline], category: &str, results: &mut Vec<Value>) {
        for outline in outlines {
            if let Some(url) = &outline.xml_url {
                results.push(
                    serde_json::json!({"url": url, "title": outline.text, "site_name": category}),
                );
            }
            let child = format!("{category} {}", outline.text);
            walk(&outline.outlines, &child, results);
        }
    }
    let mut rows = Vec::new();
    walk(&doc.body.outlines, category, &mut rows);
    let mut results = Vec::new();
    for chunk in rows.chunks(100) {
        results.extend(parse_results(
            SearchProvider::Feedsearch,
            &serde_json::to_string(chunk).map_err(|_| "Invalid publisher catalogue")?,
            100,
        )?);
    }
    Ok(results)
}

fn soundcloud_search(query: &str, limit: usize) -> Result<Vec<FeedResult>, String> {
    let client_id = crate::sources::soundcloud_client_id()
        .ok_or("SoundCloud public client could not be resolved")?;
    let mut rows = Vec::new();
    for kind in ["users", "playlists"] {
        let mut address =
            url::Url::parse(&format!("https://api-v2.soundcloud.com/search/{kind}")).unwrap();
        address
            .query_pairs_mut()
            .append_pair("q", query)
            .append_pair("limit", &(limit / 2).max(3).to_string())
            .append_pair("client_id", &client_id);
        let data: Value = serde_json::from_str(&fetch(address.as_str())?)
            .map_err(|_| "Invalid SoundCloud response")?;
        let collection = data
            .get("collection")
            .and_then(Value::as_array)
            .ok_or("Unexpected SoundCloud response")?;
        for item in collection {
            // Python `search_soundcloud_feeds`: follower counts on
            // users; playlists titled "Name (Owner)" with owner and
            // track count in the detail.
            if kind == "users" {
                let mut detail = "SoundCloud user".to_string();
                if let Some(followers) = count_field(item, &["followers_count"]).filter(|n| *n > 0)
                {
                    detail = format!("{detail} • {} followers", thousands(followers as u64));
                }
                rows.push(serde_json::json!({"title": text(item, &["username", "permalink"]), "url": text(item, &["permalink_url"]), "site_name": detail}));
            } else {
                let owner = item
                    .get("user")
                    .map(|user| text(user, &["username"]))
                    .unwrap_or("");
                let name = text(item, &["title"]);
                let mut detail = "SoundCloud playlist".to_string();
                if !owner.is_empty() {
                    detail = format!("{detail} • {owner}");
                }
                if let Some(count) = count_field(item, &["track_count"]).filter(|n| *n > 0) {
                    detail = format!("{detail} • {count} tracks");
                }
                rows.push(serde_json::json!({"title": if owner.is_empty() { name.to_string() } else { format!("{name} ({owner})") }, "url": text(item, &["permalink_url"]), "site_name": detail}));
            }
        }
    }
    parse_results(
        SearchProvider::Feedsearch,
        &Value::Array(rows).to_string(),
        limit,
    )
}

fn youtube_dump(target: &str, limit: usize) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let executable = crate::media::find_ytdlp().ok_or("YouTube search requires yt-dlp")?;
    let mut command = Command::new(executable);
    command
        .args([
            "--dump-single-json",
            "--flat-playlist",
            "--socket-timeout",
            "10",
            "--retries",
            "0",
            "--playlist-end",
            &limit.to_string(),
            "--user-agent",
            &crate::http::user_agent(),
            "--",
            target,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|_| "Could not start yt-dlp")?;
    let stdout = child.stdout.take().ok_or("Could not read yt-dlp output")?;
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        stdout
            .take(4 * 1024 * 1024 + 1)
            .read_to_string(&mut output)
            .map(|_| output)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader
                    .join()
                    .map_err(|_| "YouTube search reader stopped")?
                    .map_err(|_| "Could not read YouTube search")?;
                if !status.success() {
                    return Err("YouTube search failed".into());
                }
                if output.len() > 4 * 1024 * 1024 {
                    return Err("YouTube search response exceeded the size limit".into());
                }
                return Ok(output);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("YouTube search timed out".into());
            }
        }
    }
}

/// Python `_normalize_youtube_search_text`: lowercase, non-alphanumeric
/// runs become single spaces.
fn normalize_youtube_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_space = true;
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

/// Python `_youtube_query_match_score`: +100 when the whole query is a
/// substring of the title, +6 per query token (3+ chars) in the title.
pub fn youtube_match_score(title: &str, query: &str) -> i64 {
    let tnorm = normalize_youtube_text(title);
    let qnorm = normalize_youtube_text(query);
    if tnorm.is_empty() || qnorm.is_empty() {
        return 0;
    }
    let mut score = 0;
    if tnorm.contains(&qnorm) {
        score += 100;
    }
    let tokens: Vec<&str> = qnorm.split(' ').filter(|t| t.len() >= 3).collect();
    if !tokens.is_empty() {
        let title_tokens: HashSet<&str> = tnorm.split(' ').collect();
        score += 6 * tokens.iter().filter(|t| title_tokens.contains(**t)).count() as i64;
    }
    score
}

/// Python `_youtube_search_query_variants`: the query itself, then
/// one-token-dropped variants (from the tail) for sparse or
/// misspelled searches, at most six.
pub fn youtube_query_variants(term: &str) -> Vec<String> {
    let query = term.split_whitespace().collect::<Vec<_>>().join(" ");
    if query.is_empty() {
        return Vec::new();
    }
    let mut variants = vec![query.clone()];
    let mut seen: HashSet<String> = HashSet::from([query.to_lowercase()]);
    static TOKEN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[0-9A-Za-z@#']+").unwrap());
    let tokens: Vec<String> = TOKEN
        .find_iter(&query)
        .map(|m| m.as_str().trim().trim_matches('\'').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.len() < 2 {
        return variants;
    }
    for drop_idx in (0..tokens.len()).rev() {
        let candidate: Vec<&str> = tokens
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != drop_idx)
            .map(|(_, t)| t.as_str())
            .collect();
        if candidate.len() < 2 {
            continue;
        }
        let candidate = candidate.join(" ");
        if !seen.insert(candidate.to_lowercase()) {
            continue;
        }
        variants.push(candidate);
        if variants.len() >= 6 {
            break;
        }
    }
    variants
}

/// Python `_extract_ytdlp_search_entry_play_count`.
fn youtube_play_count(item: &Value) -> Option<i64> {
    for key in ["view_count", "play_count"] {
        if let Some(raw) = item.get(key) {
            let value = raw.as_i64().or_else(|| {
                raw.as_f64().map(|f| f as i64).or_else(|| {
                    raw.as_str()
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .map(|f| f as i64)
                })
            });
            if let Some(value) = value.filter(|v| *v >= 0) {
                return Some(value);
            }
        }
    }
    None
}

/// Python `_youtube_owner_label`: "Name / @handle" (either half may
/// stand alone; a name equal to the handle collapses to the handle).
fn youtube_owner_label(item: &Value) -> String {
    let name = text(
        item,
        &[
            "channel",
            "uploader",
            "playlist_uploader",
            "artist",
            "creator",
        ],
    );
    let handle = [
        text(item, &["uploader_id"]),
        text(item, &["channel_handle"]),
    ]
    .into_iter()
    .find_map(|candidate| {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            None
        } else if candidate.starts_with('@') {
            Some(candidate.to_string())
        } else if candidate.to_uppercase().starts_with("UC") {
            None
        } else if candidate.len() >= 2
            && candidate.len() <= 40
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            Some(format!("@{candidate}"))
        } else {
            None
        }
    })
    .unwrap_or_default();
    if !name.is_empty() && !handle.is_empty() {
        if name
            .trim_start_matches('@')
            .eq_ignore_ascii_case(handle.trim_start_matches('@'))
        {
            return handle;
        }
        return format!("{name} / {handle}");
    }
    if !name.is_empty() {
        return name.to_string();
    }
    handle
}

struct YtRow {
    result: FeedResult,
    play_count: Option<i64>,
    playlist: bool,
}

/// Convert one yt-dlp search dump into ranked feed rows (Python
/// `_youtube_search_entries_to_channel_feeds` /
/// `_youtube_search_entries_to_playlist_feeds`): channels sort by
/// play count, playlists by query match then play count; each id
/// appears once.
fn youtube_rows(
    json: &str,
    playlists: bool,
    query: &str,
    limit: usize,
) -> Result<Vec<YtRow>, String> {
    let data: Value = serde_json::from_str(json).map_err(|_| "Invalid YouTube search response")?;
    let items = data
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("Unexpected YouTube search response")?;
    let mut ranked: Vec<(String, usize, i64, i64, YtRow)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let entry_url = text(item, &["url", "webpage_url"]);
        let id = if playlists {
            let from_url = url::Url::parse(entry_url)
                .ok()
                .and_then(|url| {
                    url.query_pairs()
                        .find(|(key, _)| key == "list")
                        .map(|(_, value)| value.into_owned())
                })
                .unwrap_or_default();
            let id = text(item, &["playlist_id"]);
            if !id.is_empty() {
                id.to_string()
            } else if !from_url.is_empty() {
                from_url
            } else {
                let entry_id = text(item, &["id"]);
                if !entry_id.is_empty() && !entry_id.starts_with("UC") {
                    entry_id.to_string()
                } else {
                    String::new()
                }
            }
        } else {
            text(item, &["channel_id"]).to_string()
        };
        if !valid_identifier(&id) {
            continue;
        }
        let key = if playlists {
            "playlist_id"
        } else {
            "channel_id"
        };
        let mut address = url::Url::parse("https://www.youtube.com/feeds/videos.xml").unwrap();
        address.query_pairs_mut().append_pair(key, &id);
        let title = if playlists {
            let title = text(item, &["title"]);
            if title.is_empty() {
                id.clone()
            } else {
                title.to_string()
            }
        } else {
            let title = text(item, &["channel", "uploader", "title"]);
            if title.is_empty() {
                id.clone()
            } else {
                title.to_string()
            }
        };
        let play_count = youtube_play_count(item);
        let mut detail = if playlists {
            "YouTube playlist".to_string()
        } else {
            "YouTube channel".to_string()
        };
        let owner = youtube_owner_label(item);
        if !owner.is_empty() {
            detail = format!("{detail} ({owner})");
        }
        if let Some(count) = play_count {
            detail = format!("{detail} | {} plays", thousands(count as u64));
        }
        let match_score = if playlists {
            youtube_match_score(&title, query)
        } else {
            0
        };
        ranked.push((
            id,
            idx,
            match_score,
            play_count.unwrap_or(-1),
            YtRow {
                result: FeedResult {
                    title,
                    url: address.to_string(),
                    description: detail,
                },
                play_count,
                playlist: playlists,
            },
        ));
    }
    if playlists {
        ranked.sort_by_key(|row| (-row.2, -row.3, row.1));
    } else {
        ranked.sort_by_key(|row| (-row.3, row.1));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (id, _, _, _, row) in ranked {
        if seen.insert(id) {
            out.push(row);
            if out.len() >= limit.max(1) {
                break;
            }
        }
    }
    Ok(out)
}

pub fn parse_youtube_results(
    json: &str,
    playlists: bool,
    limit: usize,
) -> Result<Vec<FeedResult>, String> {
    Ok(youtube_rows(json, playlists, "", limit)?
        .into_iter()
        .map(|row| row.result)
        .collect())
}

fn youtube_search(query: &str, limit: usize) -> Result<Vec<FeedResult>, String> {
    // Python `search_youtube_feeds`: channels get three quarters of
    // the budget; query variants fill in when the exact query is
    // sparse; the merged list ranks playlists first, then by play
    // count and query match.
    let limit = limit.clamp(1, 200);
    let channel_limit = ((limit as f64 * 0.75).round() as usize).clamp(1, limit);
    let mut out: Vec<FeedResult> = Vec::new();
    let mut seen_urls = HashSet::new();
    let mut first_error: Option<String> = None;
    for variant in youtube_query_variants(query) {
        let fetch_count = (channel_limit * 3).max(12);
        let channel_query = format!("ytsearch{fetch_count}:{variant}");
        let mut playlist_query = url::Url::parse("https://www.youtube.com/results").unwrap();
        playlist_query
            .query_pairs_mut()
            .append_pair("search_query", &variant)
            .append_pair("sp", "EgIQAw%3D%3D");
        let (channels, playlists) = std::thread::scope(|scope| {
            let channels = scope.spawn(|| {
                youtube_dump(&channel_query, fetch_count)
                    .and_then(|json| youtube_rows(&json, false, &variant, channel_limit))
            });
            let playlists = scope.spawn(|| {
                youtube_dump(playlist_query.as_str(), limit)
                    .and_then(|json| youtube_rows(&json, true, &variant, limit))
            });
            (
                channels
                    .join()
                    .unwrap_or_else(|_| Err("YouTube channel search stopped".into())),
                playlists
                    .join()
                    .unwrap_or_else(|_| Err("YouTube playlist search stopped".into())),
            )
        });
        let mut merged: Vec<(usize, YtRow)> = Vec::new();
        for result in [channels, playlists] {
            match result {
                Ok(rows) => {
                    let base = merged.len();
                    merged.extend(rows.into_iter().enumerate().map(|(i, row)| (base + i, row)));
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        let mut ranked: Vec<(i64, i64, i64, i64, usize, YtRow)> = merged
            .into_iter()
            .map(|(idx, row)| {
                (
                    if row.playlist { 0 } else { 1 },
                    if row.play_count.is_some() { 0 } else { 1 },
                    -row.play_count.unwrap_or(0),
                    -youtube_match_score(&row.result.title, &variant),
                    idx,
                    row,
                )
            })
            .collect();
        ranked.sort_by_key(|row| (row.0, row.1, row.2, row.3, row.4));
        for (_, _, _, _, _, row) in ranked {
            if seen_urls.insert(row.result.url.clone()) {
                out.push(row.result);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    if out.is_empty()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    Ok(out)
}

fn news_api_search(
    query: &str,
    limit: usize,
    config: &crate::config::Config,
) -> Result<Vec<FeedResult>, String> {
    let mut publishers = Vec::new();
    let mut configured = false;
    let mut failures = Vec::new();
    let mut successes = 0;
    for (key, endpoint, parameter, field) in [
        (
            "newsapi_api_key",
            "https://newsapi.org/v2/top-headlines/sources",
            "apiKey",
            "sources",
        ),
        (
            "thenewsapi_api_key",
            "https://api.thenewsapi.com/v1/news/sources",
            "api_token",
            "data",
        ),
    ] {
        let secret = config.get_str(key, "");
        if secret.trim().is_empty() {
            continue;
        }
        configured = true;
        let mut address = url::Url::parse(endpoint).unwrap();
        address
            .query_pairs_mut()
            .append_pair(parameter, secret.trim());
        if field == "data" {
            address
                .query_pairs_mut()
                .append_pair("search", query)
                .append_pair("limit", "10");
        }
        // fetch deliberately omits the URL from diagnostics: it contains the API key.
        let body = match fetch(address.as_str()) {
            Ok(body) => body,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        let data: Value = match serde_json::from_str(&body) {
            Ok(data) => data,
            Err(_) => {
                failures.push("Invalid publisher directory response".into());
                continue;
            }
        };
        let Some(sources) = data.get(field).and_then(Value::as_array) else {
            failures.push("Unexpected publisher directory response".into());
            continue;
        };
        successes += 1;
        for source in sources {
            let title = text(source, &["name"]);
            let description = text(source, &["description"]);
            if field == "sources"
                && !format!("{title} {description}")
                    .to_lowercase()
                    .contains(&query.to_lowercase())
            {
                continue;
            }
            let site = text(source, &["url", "domain"]);
            if !site.is_empty() {
                publishers.push((
                    title.to_string(),
                    description.to_string(),
                    if site.contains("://") {
                        site.to_string()
                    } else {
                        format!("https://{site}")
                    },
                ));
            }
        }
    }
    if !configured {
        return Err("Configure a NewsAPI.org or The News API key to search this source".into());
    }
    if successes == 0 {
        return Err(failures.join("; "));
    }
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    // Bound website discovery work even when a directory returns many publishers.
    for (title, description, site) in publishers.into_iter().take(10) {
        if let Ok(feeds) = crate::discovery::discover_feed_urls(&site) {
            for url in feeds {
                if seen.insert(url.clone()) {
                    results.push(FeedResult {
                        title: title.clone(),
                        description: description.clone(),
                        url,
                    });
                }
            }
        }
        if results.len() >= limit {
            break;
        }
    }
    results.truncate(limit);
    Ok(results)
}
