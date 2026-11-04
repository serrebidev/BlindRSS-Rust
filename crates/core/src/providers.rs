//! Hosted providers (Stage 2, slice 5).
//!
//! Google Reader protocol family — one client covers The Old Reader,
//! BazQux, and Inoreader (Inoreader authenticates with an OAuth bearer
//! token instead of ClientLogin). Miniflux has its own REST API.
//! Endpoints and auth mirror the Python providers. Sync imports remote
//! subscriptions as local feed rows (keyed by remote id) and stores
//! stream items as articles, honouring local tombstones.

use crate::db::{Article, Db, Feed};
use serde::Deserialize;
use std::cell::RefCell;

// ---------------------------------------------------------------------------
// Google Reader family
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GReaderService {
    TheOldReader,
    BazQux,
    Inoreader,
}

impl GReaderService {
    pub fn api_base(&self) -> &'static str {
        match self {
            GReaderService::TheOldReader => "https://theoldreader.com/reader/api/0",
            GReaderService::BazQux => "https://www.bazqux.com/reader/api/0",
            GReaderService::Inoreader => "https://www.inoreader.com/reader/api/0",
        }
    }
    pub fn login_url(&self) -> &'static str {
        match self {
            GReaderService::TheOldReader => "https://theoldreader.com/accounts/ClientLogin",
            GReaderService::BazQux => "https://www.bazqux.com/accounts/ClientLogin",
            GReaderService::Inoreader => "https://www.inoreader.com/accounts/ClientLogin",
        }
    }
    pub fn config_key(&self) -> &'static str {
        match self {
            GReaderService::TheOldReader => "theoldreader",
            GReaderService::BazQux => "bazqux",
            GReaderService::Inoreader => "inoreader",
        }
    }
}

pub struct GReaderClient {
    pub service: GReaderService,
    /// Overridable for tests.
    pub api_base: String,
    pub login_url: String,
    pub email: String,
    password: String,
    /// OAuth bearer token (Inoreader). When set, ClientLogin is skipped.
    pub oauth_token: Option<String>,
    /// Inoreader app credentials, sent as AppId/AppKey headers
    /// on every request (Python `providers/inoreader.py`
    /// `_headers`; Audit 8 D08). Never sent for other services.
    app_id: String,
    app_key: String,
    token: RefCell<Option<String>>,
    /// Inoreader quota caches (providers/inoreader.py):
    /// subscription metadata and per-stream article lists are
    /// cached process-wide for their TTLs because the app
    /// quota is small (100 req/day on the free tier).
    meta_ttl: std::time::Duration,
    article_ttl: std::time::Duration,
    /// Inoreader stream page size (article_request_page_size).
    article_page_size: Option<usize>,
}

/// Process-wide Inoreader caches (see GReaderClient's TTLs).
static INOREADER_META_CACHE: std::sync::Mutex<
    Option<(String, std::time::Instant, Vec<RemoteFeed>)>,
> = std::sync::Mutex::new(None);
static INOREADER_LABEL_CACHE: std::sync::Mutex<Option<(String, std::time::Instant, Vec<String>)>> =
    std::sync::Mutex::new(None);
type ArticleCacheMap = std::collections::HashMap<String, (std::time::Instant, Vec<Article>)>;

static INOREADER_ARTICLE_CACHE: std::sync::OnceLock<std::sync::Mutex<ArticleCacheMap>> =
    std::sync::OnceLock::new();

fn article_cache() -> std::sync::MutexGuard<'static, ArticleCacheMap> {
    INOREADER_ARTICLE_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteFeed {
    /// Stream id, e.g. "feed/https://example.com/rss".
    pub stream_id: String,
    pub title: String,
    pub feed_url: String,
    pub category: Option<String>,
}

#[derive(Deserialize)]
struct SubList {
    subscriptions: Vec<Sub>,
}

#[derive(Deserialize)]
struct Sub {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    categories: Vec<SubCategory>,
}

#[derive(Deserialize)]
struct SubCategory {
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct StreamContents {
    #[serde(default)]
    items: Vec<StreamItem>,
    #[serde(default)]
    continuation: Option<String>,
}

#[derive(Deserialize)]
struct StreamItem {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    published: i64,
    #[serde(default)]
    author: String,
    #[serde(default)]
    alternate: Vec<LinkHref>,
    #[serde(default)]
    summary: Option<ContentBody>,
    #[serde(default)]
    content: Option<ContentBody>,
    origin: Option<Origin>,
    #[serde(default)]
    enclosure: Vec<LinkHref>,
    #[serde(default)]
    categories: Vec<String>,
}

#[derive(Deserialize)]
struct LinkHref {
    #[serde(default)]
    href: String,
    #[serde(rename = "type", default)]
    link_type: Option<String>,
}

#[derive(Deserialize)]
struct ContentBody {
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct Origin {
    #[serde(rename = "streamId", default)]
    stream_id: String,
}

/// Pick the enclosure to store as an article's media: the
/// first audio/video one, else the first enclosure at all.
/// Returns (url, mime type).
fn pick_enclosure(enclosures: &[LinkHref]) -> Option<(String, Option<String>)> {
    let chosen = enclosures
        .iter()
        .find(|e| {
            e.link_type
                .as_deref()
                .map(|t| t.starts_with("audio/") || t.starts_with("video/"))
                .unwrap_or(false)
        })
        .or_else(|| enclosures.first())?;
    Some((chosen.href.clone(), chosen.link_type.clone()))
}

/// The configured Inoreader page size: top-level config key
/// `article_request_page_size` (number or numeric string),
/// default 100, clamped 20..=1000 (Python's _articles_page_n).
pub fn page_size_from_config(cfg: &crate::config::Config) -> usize {
    let raw = cfg.raw().get("article_request_page_size");
    let n: i64 = match raw {
        Some(v) => v
            .as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .unwrap_or(100),
        None => 100,
    };
    n.clamp(20, 1000) as usize
}

impl GReaderClient {
    pub fn new(service: GReaderService, email: &str, password: &str) -> Self {
        Self {
            service,
            api_base: service.api_base().to_string(),
            login_url: service.login_url().to_string(),
            email: email.to_string(),
            password: password.to_string(),
            oauth_token: None,
            app_id: String::new(),
            app_key: String::new(),
            token: RefCell::new(None),
            meta_ttl: std::time::Duration::from_secs(3600),
            article_ttl: std::time::Duration::from_secs(90),
            article_page_size: None,
        }
    }

    pub fn with_oauth(service: GReaderService, token: &str) -> Self {
        let mut c = Self::new(service, "", "");
        c.oauth_token = Some(token.to_string());
        c
    }

    /// Set the Inoreader app credentials (providers.inoreader
    /// `app_id` / `app_key`). Stored on every GReader client but
    /// only ever sent for Inoreader (see `with_app_headers`).
    pub fn with_app_credentials(mut self, app_id: &str, app_key: &str) -> Self {
        self.app_id = app_id.trim().to_string();
        self.app_key = app_key.trim().to_string();
        self
    }

    /// Add the Inoreader AppId/AppKey headers to a request
    /// (Python `_headers`: both, each only when configured).
    /// A no-op for the other services and when unconfigured.
    fn with_app_headers<B>(&self, mut req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        if self.service == GReaderService::Inoreader {
            if !self.app_id.is_empty() {
                req = req.header("AppId", &self.app_id);
            }
            if !self.app_key.is_empty() {
                req = req.header("AppKey", &self.app_key);
            }
        }
        req
    }

    /// Set the Inoreader cache TTLs from config
    /// (providers.inoreader.metadata_cache_ttl_seconds,
    /// default 3600, clamped 60..=86400;
    /// article_cache_ttl_seconds, default 90, clamped
    /// 5..=900 — Python's clamps).
    pub fn with_cache_ttls(mut self, meta_secs: i64, article_secs: i64) -> Self {
        self.meta_ttl = std::time::Duration::from_secs(meta_secs.clamp(60, 86_400) as u64);
        self.article_ttl = std::time::Duration::from_secs(article_secs.clamp(5, 900) as u64);
        self
    }

    /// Set the Inoreader stream page size from the top-level
    /// `article_request_page_size` config key (Python's
    /// _articles_page_n: default 100, clamped 20..=1000). Only
    /// the Inoreader flavor consults it.
    pub fn with_article_page_size(mut self, n: usize) -> Self {
        self.article_page_size = Some(n.clamp(20, 1000));
        self
    }

    /// The page size a stream request should use: for Inoreader
    /// the configured size wins over the caller's count.
    fn effective_count(&self, requested: usize) -> usize {
        if self.service == GReaderService::Inoreader {
            self.article_page_size.unwrap_or(100).clamp(20, 1000)
        } else {
            requested
        }
    }

    /// Drop the Inoreader caches (a manual refresh bypasses
    /// them, like Python's cache invalidation).
    pub fn invalidate_caches(&self) {
        if self.service != GReaderService::Inoreader {
            return;
        }
        let scope = self.cache_scope();
        let mut metadata = INOREADER_META_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if metadata.as_ref().is_some_and(|(key, _, _)| key == &scope) {
            *metadata = None;
        }
        drop(metadata);
        let mut labels = INOREADER_LABEL_CACHE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if labels.as_ref().is_some_and(|(key, _, _)| key == &scope) {
            *labels = None;
        }
        article_cache().retain(|key, _| !key.starts_with(&format!("{scope}|")));
    }

    fn cache_scope(&self) -> String {
        use sha2::Digest;
        // Never retain raw credentials in process-wide cache keys.
        let identity =
            serde_json::json!([self.api_base, self.email, self.oauth_token, self.password]);
        sha2::Sha256::digest(identity.to_string().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn login(&self) -> Result<String, String> {
        if let Some(t) = self.oauth_token.clone() {
            return Ok(t);
        }
        if let Some(t) = self.token.borrow().clone() {
            return Ok(t);
        }
        let form = [
            ("client", "BlindRSS"),
            ("accountType", "HOSTED_OR_GOOGLE"),
            ("service", "reader"),
            ("Email", self.email.as_str()),
            ("Passwd", self.password.as_str()),
            ("output", "json"),
        ];
        let mut resp = crate::http::agent()
            .post(&self.login_url)
            .send_form(form)
            .map_err(|e| format!("login failed: {e}"))?;
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("login read failed: {e}"))?;
        for line in body.lines() {
            if let Some(t) = line.strip_prefix("Auth=") {
                let t = t.trim().to_string();
                *self.token.borrow_mut() = Some(t.clone());
                return Ok(t);
            }
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
            && let Some(t) = v.get("Auth").and_then(|a| a.as_str())
        {
            let t = t.to_string();
            *self.token.borrow_mut() = Some(t.clone());
            return Ok(t);
        }
        Err("login failed: no Auth token in response".to_string())
    }

    fn auth_header(&self) -> Result<String, String> {
        let t = self.login()?;
        if self.oauth_token.is_some() {
            Ok(format!("Bearer {t}"))
        } else {
            Ok(format!("GoogleLogin auth={t}"))
        }
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let url = format!("{}{}", self.api_base, path);
        let auth = self.auth_header()?;
        // Status-aware agent + one Retry-After retry: Inoreader
        // quotas answer 429 (Python backs off the same way).
        let (status, text) = crate::http::with_rate_limit_backoff(|| {
            let mut resp = self
                .with_app_headers(crate::http::status_agent().get(&url))
                .header("Authorization", &auth)
                .call()
                .map_err(|e| format!("GET {path} failed: {e}"))?;
            let status = resp.status().as_u16();
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let text = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("read failed: {e}"))?;
            Ok((status, retry, (status, text)))
        })?;
        if !(200..300).contains(&status) {
            return Err(format!("GET {path} returned HTTP {status}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("bad JSON from {path}: {e}"))
    }

    pub fn subscriptions(&self) -> Result<Vec<RemoteFeed>, String> {
        if self.service == GReaderService::Inoreader {
            let cache = INOREADER_META_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some((scope, at, feeds)) = &*cache
                && scope == &self.cache_scope()
                && at.elapsed() < self.meta_ttl
            {
                return Ok(feeds.clone());
            }
            drop(cache);
        }
        let list: SubList = self.get_json("/subscription/list?output=json")?;
        let feeds: Vec<RemoteFeed> = list
            .subscriptions
            .into_iter()
            .map(|s| RemoteFeed {
                stream_id: s.id,
                title: s.title,
                feed_url: s.url,
                category: s
                    .categories
                    .into_iter()
                    .next()
                    .map(|c| c.label)
                    .filter(|l| !l.is_empty()),
            })
            .collect();
        if self.service == GReaderService::Inoreader {
            *INOREADER_META_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner()) =
                Some((self.cache_scope(), std::time::Instant::now(), feeds.clone()));
        }
        Ok(feeds)
    }

    /// The account's full label (category) inventory,
    /// including labels no subscription carries (Python
    /// `providers/inoreader.py::get_categories` reads the same
    /// `/tag/list`; Audit 8 D10). Sorted case-insensitively,
    /// like Python.
    pub fn labels(&self) -> Result<Vec<String>, String> {
        // Inoreader's inventory rides the metadata cache like
        // the subscription list (Python caches categories
        // with the feeds): /tag/list on every sync would burn
        // the small app quota.
        if self.service == GReaderService::Inoreader {
            let cache = INOREADER_LABEL_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some((scope, at, labels)) = &*cache
                && scope == &self.cache_scope()
                && at.elapsed() < self.meta_ttl
            {
                return Ok(labels.clone());
            }
            drop(cache);
        }
        #[derive(Deserialize)]
        struct TagList {
            #[serde(default)]
            tags: Vec<Tag>,
        }
        #[derive(Deserialize)]
        struct Tag {
            #[serde(default)]
            id: String,
        }
        let list: TagList = self.get_json("/tag/list?output=json")?;
        let mut labels: Vec<String> = list
            .tags
            .into_iter()
            .filter_map(|t| {
                t.id.split_once("/label/")
                    .map(|(_, label)| label.to_string())
            })
            .filter(|l| !l.is_empty())
            .collect();
        labels.sort_by_key(|l| l.to_lowercase());
        labels.dedup();
        if self.service == GReaderService::Inoreader {
            *INOREADER_LABEL_CACHE
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some((
                self.cache_scope(),
                std::time::Instant::now(),
                labels.clone(),
            ));
        }
        Ok(labels)
    }

    /// Fetch a stream's items as articles (not yet stored).
    /// Inoreader results ride the short-lived article cache.
    pub fn stream_articles(&self, stream_id: &str, count: usize) -> Result<Vec<Article>, String> {
        let count = self.effective_count(count);
        if self.service == GReaderService::Inoreader {
            let key = format!("{}|{stream_id}|{count}", self.cache_scope());
            {
                let cache = article_cache();
                if let Some((at, articles)) = cache.get(&key)
                    && at.elapsed() < self.article_ttl
                {
                    return Ok(articles.clone());
                }
            }
            let articles = self.stream_articles_fresh(stream_id, count)?;
            article_cache().insert(key, (std::time::Instant::now(), articles.clone()));
            return Ok(articles);
        }
        self.stream_articles_fresh(stream_id, count)
    }

    fn stream_articles_fresh(&self, stream_id: &str, count: usize) -> Result<Vec<Article>, String> {
        let encoded = stream_id
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace(':', "%3A")
            .replace('?', "%3F")
            .replace('&', "%26");
        let path = format!("/stream/contents/{encoded}?output=json&n={count}");
        let mut items = Vec::new();
        let mut continuation: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page_path = match &continuation {
                Some(token) => format!(
                    "{path}&c={}",
                    url::form_urlencoded::byte_serialize(token.as_bytes()).collect::<String>()
                ),
                None => path.clone(),
            };
            let contents: StreamContents = self.get_json(&page_path)?;
            let empty = contents.items.is_empty();
            items.extend(contents.items);
            let Some(token) = contents.continuation.filter(|t| !t.is_empty()) else {
                break;
            };
            if empty || !seen.insert(token.clone()) {
                break;
            }
            continuation = Some(token);
        }
        Ok(items
            .into_iter()
            .map(|item| {
                let body = item
                    .content
                    .map(|c| c.content)
                    .filter(|c| !c.trim().is_empty())
                    .or_else(|| item.summary.map(|s| s.content))
                    .unwrap_or_default();
                Article {
                    id: item.id.clone(),
                    feed_id: item
                        .origin
                        .map(|o| o.stream_id)
                        .unwrap_or_else(|| stream_id.to_string()),
                    title: if item.title.trim().is_empty() {
                        "No Title".to_string()
                    } else {
                        item.title
                    },
                    url: item
                        .alternate
                        .into_iter()
                        .next()
                        .map(|l| l.href)
                        .unwrap_or_default(),
                    content: Some(body.clone()),
                    description: Some(body),
                    date: if item.published > 0 {
                        Some(chrono_rfc3339(item.published))
                    } else {
                        None
                    },
                    author: if item.author.is_empty() {
                        None
                    } else {
                        Some(item.author)
                    },
                    is_read: item
                        .categories
                        .iter()
                        .any(|s| s.ends_with("/state/com.google/read")),
                    is_favorite: item
                        .categories
                        .iter()
                        .any(|s| s.ends_with("/state/com.google/starred")),
                    // Hosted podcast media (the hosted
                    // article-media mixin): prefer the first
                    // audio/video enclosure.
                    media_url: pick_enclosure(&item.enclosure).map(|e| e.0),
                    media_type: pick_enclosure(&item.enclosure).and_then(|e| e.1),
                }
            })
            .collect())
    }

    /// Mark one item read on the server (edit-tag with an action token).
    pub fn mark_read_remote(&self, item_id: &str) -> Result<(), String> {
        self.set_read_remote(item_id, true)
    }

    /// Set one item's read state on the server: edit-tag adds the
    /// read state to mark read, removes it to mark unread.
    pub fn set_read_remote(&self, item_id: &str, read: bool) -> Result<(), String> {
        let mut token_resp = self
            .with_app_headers(crate::http::agent().get(&format!("{}/token", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let edit_token = token_resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("token read failed: {e}"))?;
        let (verb, state) = if read {
            ("a", "user/-/state/com.google/read")
        } else {
            ("r", "user/-/state/com.google/read")
        };
        self.with_app_headers(crate::http::agent().post(&format!("{}/edit-tag", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .send_form([("i", item_id), (verb, state), ("T", edit_token.trim())])
            .map_err(|e| format!("edit-tag failed: {e}"))?;
        Ok(())
    }

    /// Set one item's starred state on the server (the GReader
    /// starred state is the hosted favorite).
    pub fn set_starred_remote(&self, item_id: &str, starred: bool) -> Result<(), String> {
        let mut token_resp = self
            .with_app_headers(crate::http::agent().get(&format!("{}/token", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let edit_token = token_resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("token read failed: {e}"))?;
        let (verb, state) = if starred {
            ("a", "user/-/state/com.google/starred")
        } else {
            ("r", "user/-/state/com.google/starred")
        };
        self.with_app_headers(crate::http::agent().post(&format!("{}/edit-tag", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .send_form([("i", item_id), (verb, state), ("T", edit_token.trim())])
            .map_err(|e| format!("edit-tag failed: {e}"))?;
        Ok(())
    }

    /// Subscribe to a feed URL on the server
    /// (POST /subscription/edit, ac=subscribe).
    pub fn subscribe_remote(&self, feed_url: &str) -> Result<(), String> {
        self.subscription_edit(&format!("feed/{feed_url}"), "subscribe")
    }

    /// Unsubscribe a stream ("feed/<url>" or a stream id).
    pub fn unsubscribe_remote(&self, stream_id: &str) -> Result<(), String> {
        self.subscription_edit(stream_id, "unsubscribe")
    }

    fn subscription_edit(&self, stream: &str, action: &str) -> Result<(), String> {
        let mut token_resp = self
            .with_app_headers(crate::http::agent().get(&format!("{}/token", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let edit_token = token_resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("token read failed: {e}"))?;
        self.with_app_headers(
            crate::http::agent().post(&format!("{}/subscription/edit", self.api_base)),
        )
        .header("Authorization", &self.auth_header()?)
        .send_form([("s", stream), ("ac", action), ("T", edit_token.trim())])
        .map_err(|e| format!("subscription edit failed: {e}"))?;
        Ok(())
    }

    /// Refresh ONE subscription into the database (per-feed
    /// refresh for hosted feeds): fetch its stream and store
    /// new articles. Returns how many were new.
    pub fn refresh_feed(&self, db: &Db, stream_id: &str) -> Result<usize, String> {
        // Make sure the feed row exists with its remote title.
        if let Ok(subs) = self.subscriptions()
            && let Some(sub) = subs.iter().find(|s| s.stream_id == stream_id)
        {
            db.upsert_hosted_feed(&Feed {
                id: sub.stream_id.clone(),
                url: sub.feed_url.clone(),
                title: sub.title.clone(),
                title_is_custom: false,
                category: sub.category.clone(),
                icon_url: None,
            })
            .map_err(|e| format!("feed store failed: {e}"))?;
        }
        let tombstones = db.deleted_article_ids().unwrap_or_default();
        let mut new = 0;
        for a in &self.stream_articles(stream_id, 100)? {
            if tombstones.contains(&a.id) {
                continue;
            }
            if db
                .upsert_hosted_article(a)
                .map_err(|e| format!("article store failed: {e}"))?
            {
                new += 1;
            }
        }
        Ok(new)
    }

    /// Move a subscription into a category (label) on the
    /// server: subscription/edit with the label add verb.
    pub fn set_category_remote(
        &self,
        stream_id: &str,
        category: Option<&str>,
    ) -> Result<(), String> {
        let mut token_resp = self
            .with_app_headers(crate::http::agent().get(&format!("{}/token", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let edit_token = token_resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("token read failed: {e}"))?;
        let mut form: Vec<(&str, &str)> =
            vec![("s", stream_id), ("ac", "edit"), ("T", edit_token.trim())];
        let label;
        if let Some(cat) = category.filter(|c| !c.is_empty()) {
            label = format!("user/-/label/{cat}");
            form.push(("a", &label));
        }
        self.with_app_headers(
            crate::http::agent().post(&format!("{}/subscription/edit", self.api_base)),
        )
        .header("Authorization", &self.auth_header()?)
        .send_form(form)
        .map_err(|e| format!("subscription edit failed: {e}"))?;
        Ok(())
    }

    pub fn update_feed_remote(
        &self,
        stream_id: &str,
        title: Option<&str>,
        url: Option<&str>,
        category: Option<&str>,
        previous_category: Option<&str>,
    ) -> Result<(), String> {
        if url.is_some() {
            return Err("This provider does not support changing subscription URLs".to_string());
        }
        let mut token = self
            .with_app_headers(crate::http::agent().get(&format!("{}/token", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let token = token
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        let mut form = vec![("s", stream_id), ("ac", "edit"), ("T", token.trim())];
        if let Some(title) = title {
            form.push(("t", title));
        }
        let old = previous_category.map(|c| format!("user/-/label/{c}"));
        let new = category
            .filter(|c| !c.is_empty())
            .map(|c| format!("user/-/label/{c}"));
        if category.is_some() {
            if let Some(old) = &old {
                form.push(("r", old));
            }
            if let Some(new) = &new {
                form.push(("a", new));
            }
        }
        self.with_app_headers(
            crate::http::agent().post(&format!("{}/subscription/edit", self.api_base)),
        )
        .header("Authorization", &self.auth_header()?)
        .send_form(form)
        .map_err(|e| format!("subscription edit failed: {e}"))?;
        self.invalidate_caches();
        Ok(())
    }

    /// Rename a label (category) on the server
    /// (POST /rename-tag; Python's GReader-family
    /// rename_category). Labels are flat: the full local
    /// category path is the label title.
    pub fn rename_label_remote(&self, old_title: &str, new_title: &str) -> Result<(), String> {
        let source = format!("user/-/label/{old_title}");
        let dest = format!("user/-/label/{new_title}");
        self.with_app_headers(crate::http::agent().post(&format!("{}/rename-tag", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .send_form([("s", source.as_str()), ("dest", dest.as_str())])
            .map_err(|e| format!("rename-tag failed: {e}"))?;
        self.invalidate_caches();
        Ok(())
    }

    /// Remove a label (category) from every subscription on
    /// the server (POST /disable-tag; Python's GReader-family
    /// delete_category). The subscriptions themselves stay.
    pub fn disable_label_remote(&self, title: &str) -> Result<(), String> {
        let tag = format!("user/-/label/{title}");
        self.with_app_headers(crate::http::agent().post(&format!("{}/disable-tag", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .send_form([("s", tag.as_str())])
            .map_err(|e| format!("disable-tag failed: {e}"))?;
        self.invalidate_caches();
        Ok(())
    }

    pub fn mark_all_read_remote(&self, feed_id: Option<&str>) -> Result<(), String> {
        let mut token = self
            .with_app_headers(crate::http::agent().get(&format!("{}/token", self.api_base)))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let token = token
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        self.with_app_headers(
            crate::http::agent().post(&format!("{}/mark-all-as-read", self.api_base)),
        )
        .header("Authorization", &self.auth_header()?)
        .send_form([
            (
                "s",
                feed_id.unwrap_or("user/-/state/com.google/reading-list"),
            ),
            ("T", token.trim()),
        ])
        .map_err(|e| format!("mark all read failed: {e}"))?;
        self.invalidate_caches();
        Ok(())
    }

    /// Mark many items read with one action token (the
    /// mark-all-read path for hosted feeds).
    pub fn mark_read_batch(&self, item_ids: &[String]) -> Result<(), String> {
        for id in item_ids {
            self.set_read_remote(id, true)?;
        }
        Ok(())
    }

    /// Mark a whole category read through the label stream
    /// (Python's GReader-family `mark_all_read` resolves a
    /// category view to `user/-/label/<category>`; Audit 8
    /// D23). Server-side label membership covers articles
    /// labeled into the category from feeds outside it, which
    /// a per-feed fan-out cannot see.
    pub fn mark_category_read_remote(&self, category: &str) -> Result<(), String> {
        self.mark_all_read_remote(Some(&format!("user/-/label/{category}")))
    }

    /// Full sync: import subscriptions as local feeds, then store the
    /// reading-list stream's articles. Returns (feeds, new articles).
    pub fn sync(&self, db: &Db, per_feed_items: usize) -> Result<(usize, usize), String> {
        let subs = self.subscriptions()?;
        for sub in &subs {
            db.upsert_hosted_feed(&Feed {
                id: sub.stream_id.clone(),
                url: sub.feed_url.clone(),
                title: sub.title.clone(),
                title_is_custom: false,
                category: sub.category.clone(),
                icon_url: None,
            })
            .map_err(|e| format!("feed store failed: {e}"))?;
        }
        let tombstones = db.deleted_article_ids().unwrap_or_default();
        let mut new_articles = 0;
        for sub in &subs {
            let articles = self.stream_articles(&sub.stream_id, per_feed_items)?;
            for a in &articles {
                if tombstones.contains(&a.id) {
                    continue;
                }
                if db
                    .upsert_hosted_article(a)
                    .map_err(|e| format!("article store failed: {e}"))?
                {
                    new_articles += 1;
                }
            }
        }
        // Reconcile only now, with the complete inventory in
        // hand (Audit 8 D09): every fetch above either
        // succeeded or returned early, so a failed or partial
        // sync never reaches this point and cannot delete a
        // feed. Python's providers read through to the server,
        // so a subscription removed there simply stops
        // appearing; the cache has to drop it explicitly. Only
        // this provider's own id shape is eligible — local
        // feeds in the same file are not the server's business.
        let remote: std::collections::HashSet<&str> =
            subs.iter().map(|s| s.stream_id.as_str()).collect();
        let stale: Vec<String> = db
            .feeds()
            .map_err(|e| format!("feed list failed: {e}"))?
            .into_iter()
            .map(|f| f.id)
            .filter(|id| id.starts_with("feed/") && !remote.contains(id.as_str()))
            .collect();
        if !stale.is_empty() {
            let refs: Vec<&str> = stale.iter().map(|s| s.as_str()).collect();
            db.remove_hosted_feeds(&refs)
                .map_err(|e| format!("feed reconcile failed: {e}"))?;
        }
        // Category inventory (Audit 8 D10): labels with no
        // subscription exist remotely too. When /tag/list
        // cannot be read the category table is left exactly
        // as it was — pruning from a partial inventory would
        // delete real categories.
        if let Ok(labels) = self.labels() {
            let mut inventory: Vec<String> = labels;
            for sub in &subs {
                if let Some(cat) = &sub.category
                    && !inventory.contains(cat)
                {
                    inventory.push(cat.clone());
                }
            }
            db.reconcile_hosted_categories(&inventory)
                .map_err(|e| format!("category reconcile failed: {e}"))?;
        }
        Ok((subs.len(), new_articles))
    }
}

/// Seconds-since-epoch to an RFC 3339 string (no chrono dependency;
/// civil-date conversion is small and well-defined).
fn chrono_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let yr = if mo <= 2 { y + 1 } else { y };
    format!("{yr:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// ---------------------------------------------------------------------------
// Miniflux
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct MinifluxClient {
    /// Base URL without trailing /v1.
    pub base_url: String,
    pub api_key: String,
    refresh_config: crate::config::Config,
    refresh_state: std::sync::Arc<std::sync::Mutex<MinifluxRefreshState>>,
}

#[derive(Default)]
struct MinifluxRefreshState {
    failures: std::collections::HashMap<i64, (u32, std::time::Instant)>,
    cooldown_until: Option<std::time::Instant>,
    in_flight: std::collections::HashSet<i64>,
}

type MinifluxRefreshStates =
    std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<MinifluxRefreshState>>>;
static MINIFLUX_REFRESH_STATES: std::sync::OnceLock<std::sync::Mutex<MinifluxRefreshStates>> =
    std::sync::OnceLock::new();

/// Completion of requests still running after the manual-refresh soft deadline.
pub type MinifluxRefreshPending = std::sync::mpsc::Receiver<Result<(), String>>;

#[derive(Deserialize)]
pub struct MfFeed {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    feed_url: String,
    #[serde(default)]
    category: Option<MfCategory>,
    #[serde(default)]
    parsing_error_count: i64,
    #[serde(default)]
    checked_at: Option<String>,
}

#[derive(Deserialize)]
pub struct MfCategory {
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct MfEntries {
    #[serde(default)]
    entries: Vec<MfEntry>,
    #[serde(default)]
    total: Option<usize>,
}

#[derive(Deserialize)]
pub struct MfEntry {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    published_at: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    starred: bool,
    #[serde(default)]
    enclosures: Vec<MfEnclosure>,
}

#[derive(Deserialize)]
pub struct MfEnclosure {
    #[serde(default)]
    url: String,
    #[serde(default)]
    mime_type: String,
}

impl MinifluxClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        use sha2::Digest;
        let base_url = base_url
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .to_string();
        let identity = sha2::Sha256::digest(format!("{base_url}|{api_key}"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let refresh_state = MINIFLUX_REFRESH_STATES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(identity)
            .or_default()
            .clone();
        Self {
            base_url,
            api_key: api_key.to_string(),
            refresh_config: Default::default(),
            refresh_state,
        }
    }

    pub fn with_refresh_config(mut self, cfg: &crate::config::Config) -> Self {
        self.refresh_config = cfg.clone();
        self
    }

    fn refresh_workers(&self, count: usize) -> usize {
        (self
            .refresh_config
            .get_i64("miniflux_targeted_refresh_workers", 8)
            .clamp(1, 32) as usize)
            .min(count.max(1))
    }

    fn refresh_seconds(&self, key: &str, default: f64) -> f64 {
        self.refresh_config
            .raw()
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|v| v.is_finite())
            .unwrap_or(default)
            .clamp(0.0, 86400.0)
    }

    /// Manual refresh fetches each healthy feed synchronously on the server;
    /// scheduled refresh only recovers up to 15 feeds overdue by the client interval.
    pub fn request_refresh(&self, force: bool) -> Result<Option<MinifluxRefreshPending>, String> {
        let feeds = self.feeds()?;
        let skip = self
            .refresh_config
            .get_i64("miniflux_force_skip_error_count", 3)
            .max(0);
        // SQLite already owns the cache's timestamp normalization, including UTC offsets.
        let clock = rusqlite::Connection::open_in_memory()
            .map_err(|e| format!("refresh clock failed: {e}"))?;
        let interval = self
            .refresh_config
            .get_i64("refresh_interval", 300)
            .max(300);
        let mut ids = Vec::new();
        for feed in feeds {
            if skip > 0 && feed.parsing_error_count >= skip {
                continue;
            }
            let due = force
                || clock
                    .query_row(
                        "SELECT COALESCE(unixepoch(?1) <= unixepoch() - ?2, 1)",
                        rusqlite::params![feed.checked_at, interval],
                        |r| r.get::<_, bool>(0),
                    )
                    .map_err(|e| format!("refresh clock failed: {e}"))?;
            if due {
                ids.push(feed.id);
            }
            if !force && ids.len() == 15 {
                break;
            }
        }
        let ids = self.claim_refresh_ids(&ids, force);
        if ids.is_empty() {
            return Ok(None);
        }
        let deadline = if force && ids.len() >= 3 && self.refresh_workers(ids.len()) > 1 {
            self.refresh_seconds("miniflux_refresh_soft_deadline_s", 5.0)
        } else {
            0.0
        };
        let client = self.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(client.run_refresh_pool(ids));
        });
        if deadline == 0.0 {
            rx.recv()
                .map_err(|_| "Miniflux refresh worker stopped".to_string())??;
            return Ok(None);
        }
        match rx.recv_timeout(std::time::Duration::from_secs_f64(deadline)) {
            Ok(result) => result.map(|()| None),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(Some(rx)),
            Err(_) => Err("Miniflux refresh worker stopped".to_string()),
        }
    }

    fn claim_refresh_ids(&self, ids: &[i64], force: bool) -> Vec<i64> {
        let now = std::time::Instant::now();
        let mut state = self.refresh_state.lock().unwrap_or_else(|p| p.into_inner());
        if state.cooldown_until.is_some_and(|until| until > now) {
            return Vec::new();
        }
        ids.iter()
            .copied()
            .filter(|id| {
                let backoff = state
                    .failures
                    .get(id)
                    .is_some_and(|(count, until)| *until > now && (!force || *count >= 2));
                *id > 0 && !backoff && state.in_flight.insert(*id)
            })
            .collect()
    }

    /// Trigger explicit feed refreshes without fetching or changing cached articles.
    pub fn request_feed_refreshes(&self, ids: &[i64], force: bool) -> Result<(), String> {
        self.run_refresh_pool(self.claim_refresh_ids(ids, force))
    }

    fn run_refresh_pool(&self, ids: Vec<i64>) -> Result<(), String> {
        let workers = self.refresh_workers(ids.len());
        let limit = self
            .refresh_config
            .get_i64("miniflux_targeted_refresh_5xx_limit", 3)
            .max(0) as usize;
        let cooldown = self.refresh_seconds("miniflux_targeted_refresh_cooldown_s", 300.0);
        let base_timeout = self
            .refresh_config
            .get_i64("feed_timeout_seconds", 15)
            .clamp(5, 120);
        let floor = self
            .refresh_config
            .get_i64("miniflux_per_feed_refresh_timeout_s", 18);
        let timeout = base_timeout.max(if floor == 0 { 18 } else { floor }) as u64;
        let mut errors = Vec::new();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut queue = ids.iter().copied();
            let mut active = 0;
            let mut probing = limit > 0 && workers > 1;
            let mut consecutive = 0;
            let mut parked = false;
            loop {
                let window = if probing { workers.min(limit) } else { workers };
                let fill = !probing || active == 0;
                while fill && !parked && active < window {
                    let Some(id) = queue.next() else { break };
                    let tx = tx.clone();
                    scope.spawn(move || {
                        // Every worker owns its HTTP agent; no shared session crosses threads.
                        let config = ureq::Agent::config_builder()
                            .user_agent(crate::http::user_agent())
                            .timeout_global(Some(std::time::Duration::from_secs(timeout)))
                            .build();
                        let result = ureq::Agent::new_with_config(config)
                            .put(&format!("{}/v1/feeds/{id}/refresh", self.base_url))
                            .header("X-Auth-Token", &self.api_key)
                            .send_empty();
                        let status = match &result {
                            Ok(r) => Some(r.status().as_u16()),
                            Err(ureq::Error::StatusCode(s)) => Some(*s),
                            Err(_) => None,
                        };
                        let _ = tx.send((id, result.is_ok(), status));
                    });
                    active += 1;
                }
                if active == 0 {
                    break;
                }
                let Ok((id, ok, status)) = rx.recv() else {
                    break;
                };
                active -= 1;
                let mut state = self.refresh_state.lock().unwrap_or_else(|p| p.into_inner());
                if ok {
                    state.failures.remove(&id);
                } else {
                    let count = state
                        .failures
                        .get(&id)
                        .map_or(1, |(n, _)| n.saturating_add(1));
                    let seconds =
                        (60_u64.saturating_mul(1_u64 << count.saturating_sub(1).min(5))).min(1800);
                    state.failures.insert(
                        id,
                        (
                            count,
                            std::time::Instant::now() + std::time::Duration::from_secs(seconds),
                        ),
                    );
                    errors.push(match status {
                        Some(s) => format!("feed {id}: HTTP {s}"),
                        None => format!("feed {id}: request failed or timed out"),
                    });
                }
                consecutive = if status.is_some_and(|s| s >= 500) {
                    consecutive + 1
                } else {
                    0
                };
                if limit > 0 && consecutive >= limit && cooldown > 0.0 {
                    state.cooldown_until = Some(
                        std::time::Instant::now() + std::time::Duration::from_secs_f64(cooldown),
                    );
                    parked = true;
                }
                // Let every initial probe settle before admitting more requests.
                if probing && active > 0 {
                    continue;
                }
                probing = false;
            }
        });
        let mut state = self.refresh_state.lock().unwrap_or_else(|p| p.into_inner());
        for id in ids {
            state.in_flight.remove(&id);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("Miniflux refresh failed: {}", errors.join("; ")))
        }
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut resp = crate::http::agent()
            .get(&url)
            .header("X-Auth-Token", &self.api_key)
            .call()
            .map_err(|e| format!("GET {path} failed: {e}"))?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read failed: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("bad JSON from {path}: {e}"))
    }

    pub fn feeds(&self) -> Result<Vec<MfFeed>, String> {
        self.get_json("/v1/feeds")
    }

    pub fn entries(&self, feed_id: i64, limit: usize) -> Result<Vec<MfEntry>, String> {
        let limit = limit.clamp(1, 1000);
        let mut entries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut offset = 0;
        loop {
            let path = format!(
                "/v1/entries?feed_id={feed_id}&order=published_at&direction=desc&limit={limit}&offset={offset}"
            );
            let resp: MfEntries = self.get_json(&path)?;
            let count = resp.entries.len();
            if count == 0 {
                break;
            }
            let previous = entries.len();
            entries.extend(resp.entries.into_iter().filter(|e| seen.insert(e.id)));
            if entries.len() == previous {
                return Err("Miniflux repeated an entries page; sync was stopped".to_string());
            }
            offset += count;
            if resp.total.is_some_and(|total| offset >= total)
                || (resp.total.is_none() && count < limit)
            {
                break;
            }
        }
        Ok(entries)
    }

    pub fn mark_read_remote(&self, entry_ids: &[i64]) -> Result<(), String> {
        self.set_status_remote(entry_ids, "read")
    }

    pub fn set_status_remote(&self, entry_ids: &[i64], status: &str) -> Result<(), String> {
        let body = serde_json::json!({"entry_ids": entry_ids, "status": status});
        crate::http::agent()
            .put(&format!("{}/v1/entries", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| format!("mark read failed: {e}"))?;
        Ok(())
    }

    /// Use the server account/feed endpoint rather than paging cached article IDs.
    pub fn mark_all_read_remote(&self, feed_id: Option<i64>) -> Result<(), String> {
        let path = if let Some(id) = feed_id {
            if id <= 0 {
                return Err("Invalid Miniflux feed".to_string());
            }
            format!("/v1/feeds/{id}/mark-all-as-read")
        } else {
            #[derive(Deserialize)]
            struct Me {
                id: i64,
            }
            let me: Me = self.get_json("/v1/me")?;
            if me.id <= 0 {
                return Err("Invalid Miniflux account response".to_string());
            }
            format!("/v1/users/{}/mark-all-as-read", me.id)
        };
        crate::http::agent()
            .put(&format!("{}{path}", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .send_empty()
            .map_err(|e| format!("mark all read failed: {e}"))?;
        Ok(())
    }

    /// Mark a whole category read through the server category
    /// endpoint (Python `providers/miniflux.py::mark_all_read`
    /// resolves a category view to its id and PUTs
    /// /v1/categories/{id}/mark-all-as-read; Audit 8 D23).
    /// Server-side membership covers every entry in the
    /// category, not just the feeds the local tree groups
    /// under it.
    pub fn mark_category_read_remote(&self, category: &str) -> Result<(), String> {
        let Some(id) = self.category_id(category)? else {
            return Err(format!("Miniflux category not found: {category}"));
        };
        crate::http::agent()
            .put(&format!(
                "{}/v1/categories/{id}/mark-all-as-read",
                self.base_url
            ))
            .header("X-Auth-Token", &self.api_key)
            .send_empty()
            .map_err(|e| format!("mark category read failed: {e}"))?;
        Ok(())
    }

    /// Test the connection (GET /v1/me): returns the account's
    /// username. Powers the Settings "Test" button.
    pub fn test_connection(&self) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Me {
            #[serde(default)]
            username: String,
        }
        let me: Me = self.get_json("/v1/me")?;
        Ok(me.username)
    }

    /// Export the account's subscriptions as OPML
    /// (GET /v1/export).
    pub fn opml_export(&self) -> Result<String, String> {
        let mut resp = crate::http::agent()
            .get(&format!("{}/v1/export", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .call()
            .map_err(|e| format!("OPML export failed: {e}"))?;
        resp.body_mut()
            .read_to_string()
            .map_err(|e| format!("read failed: {e}"))
    }

    /// Import OPML into the account (POST /v1/import,
    /// multipart form with the file part, built by hand —
    /// ureq's multipart feature is not enabled).
    pub fn opml_import(&self, opml_xml: &str) -> Result<(), String> {
        crate::http::agent()
            .post(&format!("{}/v1/import", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/xml; charset=utf-8")
            .send(opml_xml)
            .map_err(|e| format!("OPML import failed: {e}"))?;
        Ok(())
    }

    /// Fetch an entry's full content from the server
    /// (GET /v1/entries/{id}/fetch-content): Miniflux's own
    /// scraper result, the provider side of full-text
    /// arbitration.
    pub fn fetch_content_remote(&self, entry_id: i64) -> Result<String, String> {
        #[derive(serde::Deserialize)]
        struct FetchContent {
            content: String,
        }
        let resp: FetchContent = self.get_json(&format!("/v1/entries/{entry_id}/fetch-content"))?;
        Ok(resp.content)
    }

    /// Create a feed on the server (POST /v1/feeds); returns
    /// the new feed's id.
    pub fn create_feed_remote(&self, feed_url: &str) -> Result<i64, String> {
        let body = serde_json::json!({"feed_url": feed_url});
        let mut resp = crate::http::agent()
            .post(&format!("{}/v1/feeds", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| format!("create feed failed: {e}"))?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read failed: {e}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("bad JSON: {e}"))?;
        value
            .get("feed_id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "no feed_id in create response".to_string())
    }

    /// Delete a feed on the server (DELETE /v1/feeds/{id}).
    pub fn delete_feed_remote(&self, feed_id: i64) -> Result<(), String> {
        crate::http::agent()
            .delete(&format!("{}/v1/feeds/{feed_id}", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .call()
            .map_err(|e| format!("delete feed failed: {e}"))?;
        Ok(())
    }

    /// Server categories as (id, title).
    pub fn categories_remote(&self) -> Result<Vec<(i64, String)>, String> {
        let cats: Vec<serde_json::Value> = self.get_json("/v1/categories")?;
        Ok(cats
            .into_iter()
            .filter_map(|c| {
                Some((
                    c.get("id")?.as_i64()?,
                    c.get("title")?.as_str()?.to_string(),
                ))
            })
            .collect())
    }

    fn category_id(&self, title: &str) -> Result<Option<i64>, String> {
        Ok(self
            .categories_remote()?
            .into_iter()
            .find(|(_, t)| t == title)
            .map(|(id, _)| id))
    }

    /// Create a server category; Miniflux categories are flat.
    pub fn create_category_remote(&self, title: &str) -> Result<i64, String> {
        let mut response = crate::http::agent()
            .post(&format!("{}/v1/categories", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .send(serde_json::json!({"title":title}).to_string())
            .map_err(|e| format!("create category failed: {e}"))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read failed: {e}"))?;
        let category: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("bad JSON: {e}"))?;
        category
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "no category id in create response".to_string())
    }

    /// Edit a server feed, resolving or creating the requested category title.
    pub fn update_feed_remote(
        &self,
        feed_id: i64,
        title: Option<&str>,
        url: Option<&str>,
        category: Option<&str>,
    ) -> Result<(), String> {
        let mut body = serde_json::Map::new();
        if let Some(title) = title {
            body.insert("title".to_string(), serde_json::json!(title));
        }
        if let Some(url) = url {
            body.insert("feed_url".to_string(), serde_json::json!(url));
        }
        if let Some(category) = category {
            let existing = self
                .categories_remote()?
                .into_iter()
                .find(|(_, title)| title.eq_ignore_ascii_case(category));
            let id = match existing {
                Some((id, _)) => Some(id),
                None if !category.is_empty() => Some(self.create_category_remote(category)?),
                None => None,
            };
            if let Some(id) = id {
                body.insert("category_id".to_string(), serde_json::json!(id));
            }
        }
        if body.is_empty() {
            return Ok(());
        }
        crate::http::agent()
            .put(&format!("{}/v1/feeds/{feed_id}", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .send(serde_json::Value::Object(body).to_string())
            .map_err(|e| format!("update feed failed: {e}"))?;
        Ok(())
    }

    /// Rename a category on the server (Python's
    /// MinifluxProvider.rename_category). False when the title
    /// is not a server category.
    pub fn rename_category_remote(&self, old_title: &str, new_title: &str) -> Result<bool, String> {
        let Some(id) = self.category_id(old_title)? else {
            return Ok(false);
        };
        let body = serde_json::json!({"title": new_title});
        crate::http::agent()
            .put(&format!("{}/v1/categories/{id}", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| format!("rename category failed: {e}"))?;
        Ok(true)
    }

    /// Delete a category on the server (Python's
    /// MinifluxProvider.delete_category). False when the title
    /// is unknown or the server refuses (Miniflux refuses a
    /// category that still has feeds with a 400).
    pub fn delete_category_remote(&self, title: &str) -> Result<bool, String> {
        let Some(id) = self.category_id(title)? else {
            return Ok(false);
        };
        match crate::http::agent()
            .delete(&format!("{}/v1/categories/{id}", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .call()
        {
            Ok(_) => Ok(true),
            Err(ureq::Error::StatusCode(_)) => Ok(false),
            Err(e) => Err(format!("delete category failed: {e}")),
        }
    }

    /// Toggle an entry's bookmark (Miniflux's favorite).
    pub fn toggle_bookmark_remote(&self, entry_id: i64) -> Result<(), String> {
        crate::http::agent()
            .put(&format!("{}/v1/entries/{entry_id}/bookmark", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .send_empty()
            .map_err(|e| format!("bookmark failed: {e}"))?;
        Ok(())
    }

    /// Miniflux only exposes a toggle, so read the authoritative state first.
    pub fn set_bookmark_remote(&self, entry_id: i64, favorite: bool) -> Result<(), String> {
        let entry: MfEntry = self.get_json(&format!("/v1/entries/{entry_id}"))?;
        if entry.starred != favorite {
            self.toggle_bookmark_remote(entry_id)?;
        }
        Ok(())
    }

    /// Refresh ONE feed's entries into the database (per-feed
    /// refresh for hosted feeds). Returns how many were new.
    pub fn refresh_feed(&self, db: &Db, mf_id: i64) -> Result<usize, String> {
        self.request_feed_refreshes(&[mf_id], true)?;
        self.sync_feed_entries(db, mf_id)
    }

    fn sync_feed_entries(&self, db: &Db, mf_id: i64) -> Result<usize, String> {
        let local_id = format!("miniflux:{mf_id}");
        let tombstones = db.deleted_article_ids().unwrap_or_default();
        let mut new = 0;
        for e in self.entries(mf_id, 100)? {
            let article = Article {
                id: format!("miniflux-entry:{}", e.id),
                feed_id: local_id.clone(),
                title: if e.title.trim().is_empty() {
                    "No Title".to_string()
                } else {
                    e.title
                },
                url: e.url,
                content: Some(e.content.clone()),
                description: Some(e.content),
                date: if e.published_at.is_empty() {
                    None
                } else {
                    Some(e.published_at)
                },
                author: if e.author.is_empty() {
                    None
                } else {
                    Some(e.author)
                },
                is_read: e.status == "read",
                is_favorite: e.starred,
                media_url: e
                    .enclosures
                    .iter()
                    .find(|enc| {
                        enc.mime_type.starts_with("audio/") || enc.mime_type.starts_with("video/")
                    })
                    .or_else(|| e.enclosures.first())
                    .map(|enc| enc.url.clone()),
                media_type: e
                    .enclosures
                    .iter()
                    .find(|enc| {
                        enc.mime_type.starts_with("audio/") || enc.mime_type.starts_with("video/")
                    })
                    .or_else(|| e.enclosures.first())
                    .map(|enc| enc.mime_type.clone())
                    .filter(|t| !t.is_empty()),
            };
            if tombstones.contains(&article.id) {
                continue;
            }
            if db
                .upsert_hosted_article(&article)
                .map_err(|e| format!("article store failed: {e}"))?
            {
                new += 1;
            }
        }
        Ok(new)
    }

    /// Sync feeds and all paginated entries, including read history and bookmarks.
    pub fn sync(&self, db: &Db, per_feed_items: usize) -> Result<(usize, usize), String> {
        let feeds = self.feeds()?;
        let tombstones = db.deleted_article_ids().unwrap_or_default();
        let mut new_articles = 0;
        for f in &feeds {
            let local_id = format!("miniflux:{}", f.id);
            db.upsert_hosted_feed(&Feed {
                id: local_id.clone(),
                url: f.feed_url.clone(),
                title: f.title.clone(),
                title_is_custom: false,
                category: f.category.as_ref().map(|c| c.title.clone()),
                icon_url: None,
            })
            .map_err(|e| format!("feed store failed: {e}"))?;
            for e in self.entries(f.id, per_feed_items)? {
                let article = Article {
                    id: format!("miniflux-entry:{}", e.id),
                    feed_id: local_id.clone(),
                    title: if e.title.trim().is_empty() {
                        "No Title".to_string()
                    } else {
                        e.title
                    },
                    url: e.url,
                    content: Some(e.content.clone()),
                    description: Some(e.content),
                    date: if e.published_at.is_empty() {
                        None
                    } else {
                        Some(e.published_at)
                    },
                    author: if e.author.is_empty() {
                        None
                    } else {
                        Some(e.author)
                    },
                    is_read: e.status == "read",
                    is_favorite: e.starred,
                    media_url: e
                        .enclosures
                        .iter()
                        .find(|enc| {
                            enc.mime_type.starts_with("audio/")
                                || enc.mime_type.starts_with("video/")
                        })
                        .or_else(|| e.enclosures.first())
                        .map(|enc| enc.url.clone()),
                    media_type: e
                        .enclosures
                        .iter()
                        .find(|enc| {
                            enc.mime_type.starts_with("audio/")
                                || enc.mime_type.starts_with("video/")
                        })
                        .or_else(|| e.enclosures.first())
                        .map(|enc| enc.mime_type.clone())
                        .filter(|t| !t.is_empty()),
                };
                if tombstones.contains(&article.id) {
                    continue;
                }
                if db
                    .upsert_hosted_article(&article)
                    .map_err(|e| format!("article store failed: {e}"))?
                {
                    new_articles += 1;
                }
            }
        }
        // Reconcile only after every feed and entry page
        // succeeded (Audit 8 D09): a feed the server no
        // longer carries leaves the cache, with its entries;
        // any failure above returned early and changed
        // nothing beyond upserts.
        let remote: std::collections::HashSet<String> =
            feeds.iter().map(|f| format!("miniflux:{}", f.id)).collect();
        let stale: Vec<String> = db
            .feeds()
            .map_err(|e| format!("feed list failed: {e}"))?
            .into_iter()
            .map(|f| f.id)
            .filter(|id| id.starts_with("miniflux:") && !remote.contains(id))
            .collect();
        if !stale.is_empty() {
            let refs: Vec<&str> = stale.iter().map(|s| s.as_str()).collect();
            db.remove_hosted_feeds(&refs)
                .map_err(|e| format!("feed reconcile failed: {e}"))?;
        }
        // Category inventory (Audit 8 D10): an empty remote
        // category syncs like any other, and one removed
        // remotely is pruned when unreferenced. A failed
        // inventory read skips this entirely.
        if let Ok(cats) = self.categories_remote() {
            let mut inventory: Vec<String> = cats.into_iter().map(|(_, title)| title).collect();
            for f in &feeds {
                if let Some(cat) = &f.category
                    && !inventory.contains(&cat.title)
                {
                    inventory.push(cat.title.clone());
                }
            }
            db.reconcile_hosted_categories(&inventory)
                .map_err(|e| format!("category reconcile failed: {e}"))?;
        }
        Ok((feeds.len(), new_articles))
    }
}
