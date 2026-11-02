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
            return crate::discovery::discover_feed_urls(&request_url(provider, query, limit)?)
                .map(|items| {
                    items
                        .into_iter()
                        .take(limit)
                        .map(|url| FeedResult {
                            title: url.clone(),
                            url,
                            description: "Website scan".into(),
                        })
                        .collect()
                });
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
    Ok(results)
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
                    add(
                        &format!("r/{name}"),
                        format!("https://www.reddit.com/r/{name}/.rss"),
                        text(item, &["public_description", "title"]),
                    );
                }
            }
            SearchProvider::Lemmy | SearchProvider::PieFed => {
                let item = item
                    .get("community")
                    .or_else(|| item.get("person"))
                    .unwrap_or(&Value::Null);
                let actor = text(item, &["actor_id"]);
                let Some(feed) = federated_feed(actor, provider == SearchProvider::PieFed) else {
                    continue;
                };
                add(
                    text(item, &["title", "name", "user_name"]),
                    feed,
                    provider_label(provider),
                );
            }
            SearchProvider::Mastodon => {
                if let Some(feed) = crate::discovery::social_feed_url(text(item, &["url"])) {
                    add(text(item, &["display_name", "acct"]), feed, "Mastodon user");
                }
            }
            SearchProvider::Bluesky => {
                let handle = text(item, &["handle"]);
                if valid_identifier(handle) {
                    add(
                        text(item, &["displayName", "handle"]),
                        format!("https://bsky.app/profile/{handle}/rss"),
                        "Bluesky user",
                    );
                }
            }
            SearchProvider::Mixcloud => add(
                text(item, &["name", "username"]),
                text(item, &["url"]).to_string(),
                "Mixcloud user",
            ),
            _ => {}
        }
    }
    if provider == SearchProvider::Mastodon
        && let Some(tags) = data.get("hashtags").and_then(Value::as_array)
    {
        for tag in tags {
            let name = text(tag, &["name"]);
            if let Some(feed) = crate::discovery::social_feed_url(text(tag, &["url"])) {
                add(&format!("#{name}"), feed, "Mastodon tag");
            } else if valid_identifier(name) {
                add(
                    &format!("#{name}"),
                    format!("https://mastodon.social/tags/{name}.rss"),
                    "Mastodon tag",
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
        results.push(make(
            &format!("/rss/headlines/section/topic/{topic}"),
            None,
            format!("Google News: {query} headlines"),
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
            rows.push(serde_json::json!({"title": if provider == SearchProvider::Feedspot && !title.is_empty() { std::mem::take(&mut title) } else { label }, "url": address, "site_name": provider_label(provider)}));
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
            rows.push(serde_json::json!({"title": text(item, &["username", "title", "permalink"]), "url": text(item, &["permalink_url"]), "site_name": if kind == "users" {"SoundCloud user"} else {"SoundCloud playlist"}}));
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

pub fn parse_youtube_results(
    json: &str,
    playlists: bool,
    limit: usize,
) -> Result<Vec<FeedResult>, String> {
    let data: Value = serde_json::from_str(json).map_err(|_| "Invalid YouTube search response")?;
    let items = data
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("Unexpected YouTube search response")?;
    let mut items: Vec<_> = items.iter().collect();
    items.sort_by_key(|item| {
        std::cmp::Reverse(
            item.get("view_count")
                .or_else(|| item.get("play_count"))
                .and_then(Value::as_i64)
                .unwrap_or(-1),
        )
    });
    let mut rows = Vec::new();
    for item in items {
        let entry_url = text(item, &["url", "webpage_url"]);
        let id_from_url = url::Url::parse(entry_url).ok().and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "list")
                .map(|(_, value)| value.into_owned())
        });
        let id = if playlists {
            text(item, &["playlist_id"])
        } else {
            text(item, &["channel_id"])
        };
        let id = if id.is_empty() && playlists {
            id_from_url
                .as_deref()
                .unwrap_or_else(|| text(item, &["id"]))
        } else {
            id
        };
        if !valid_identifier(id) {
            continue;
        }
        let key = if playlists {
            "playlist_id"
        } else {
            "channel_id"
        };
        let mut address = url::Url::parse("https://www.youtube.com/feeds/videos.xml").unwrap();
        address.query_pairs_mut().append_pair(key, id);
        rows.push(serde_json::json!({"url": address.as_str(), "title": if playlists { text(item, &["title", "id"]) } else { text(item, &["channel", "uploader", "title", "channel_id"]) }, "site_name": if playlists { "YouTube playlist" } else { "YouTube channel" }}));
    }
    parse_results(
        SearchProvider::Feedsearch,
        &Value::Array(rows).to_string(),
        limit,
    )
}

fn youtube_search(query: &str, limit: usize) -> Result<Vec<FeedResult>, String> {
    let channels = (limit * 3 / 4).max(1);
    let channel_query = format!("ytsearch{}:{query}", (channels * 3).max(12));
    let mut playlist_query = url::Url::parse("https://www.youtube.com/results").unwrap();
    playlist_query
        .query_pairs_mut()
        .append_pair("search_query", query)
        .append_pair("sp", "EgIQAw%3D%3D");
    let (channels, playlists) = std::thread::scope(|scope| {
        let channels = scope.spawn(|| {
            youtube_dump(&channel_query, (channels * 3).max(12))
                .and_then(|json| parse_youtube_results(&json, false, channels))
        });
        let playlists = scope.spawn(|| {
            youtube_dump(playlist_query.as_str(), limit)
                .and_then(|json| parse_youtube_results(&json, true, limit))
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
    match (channels, playlists) {
        (Ok(mut channels), Ok(playlists)) => {
            channels.extend(playlists);
            channels.truncate(limit);
            Ok(channels)
        }
        (Ok(rows), Err(_)) | (Err(_), Ok(rows)) => Ok(rows),
        (Err(error), Err(_)) => Err(error),
    }
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
