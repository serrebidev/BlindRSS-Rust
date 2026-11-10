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

    /// Subscribe into a category, Python's GReader-family
    /// `add_feed(url, category)` shape: the same
    /// subscription/edit request, with the category carried in
    /// the form's `t` field (sent empty when no category), as
    /// Python sends it (Audit 8 Task 27, F03).
    pub fn subscribe_remote_in(
        &self,
        feed_url: &str,
        category: Option<&str>,
    ) -> Result<(), String> {
        self.subscription_edit_with(
            &format!("feed/{feed_url}"),
            "subscribe",
            &[("t", category.unwrap_or(""))],
        )
    }

    /// Unsubscribe a stream ("feed/<url>" or a stream id).
    pub fn unsubscribe_remote(&self, stream_id: &str) -> Result<(), String> {
        self.subscription_edit(stream_id, "unsubscribe")
    }

    fn subscription_edit(&self, stream: &str, action: &str) -> Result<(), String> {
        self.subscription_edit_with(stream, action, &[])
    }

    fn subscription_edit_with(
        &self,
        stream: &str,
        action: &str,
        extra: &[(&str, &str)],
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
        let mut form: Vec<(&str, &str)> = vec![("s", stream), ("ac", action)];
        form.extend_from_slice(extra);
        form.push(("T", edit_token.trim()));
        self.with_app_headers(
            crate::http::agent().post(&format!("{}/subscription/edit", self.api_base)),
        )
        .header("Authorization", &self.auth_header()?)
        .send_form(form)
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

/// Why a Miniflux bulk OPML import failed: the HTTP status
/// when the server answered (Python checks for 404/405 to
/// decide the per-feed compatibility fallback), or a
/// transport-level message.
#[derive(Debug, Clone, PartialEq)]
pub enum OpmlImportError {
    Http(u16),
    Other(String),
}

impl std::fmt::Display for OpmlImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpmlImportError::Http(code) => write!(f, "OPML import failed: HTTP {code}"),
            OpmlImportError::Other(message) => write!(f, "{message}"),
        }
    }
}

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
    browser_fallback_available: Option<bool>,
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
    site_url: String,
    #[serde(default)]
    category: Option<MfCategory>,
    #[serde(default)]
    parsing_error_count: i64,
    #[serde(default)]
    parsing_error_message: String,
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
            let result = client.run_refresh_pool(ids);
            client.queue_browser_feed_recovery(&[]);
            let _ = tx.send(result);
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

    /// Every entry of a feed, newest first; with `changed_after` (Unix seconds)
    /// only those the server created or changed (read, starred, edited) since.
    pub fn entries(
        &self,
        feed_id: i64,
        limit: usize,
        changed_after: Option<i64>,
    ) -> Result<Vec<MfEntry>, String> {
        let limit = limit.clamp(1, 1000);
        let mut entries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut offset = 0;
        loop {
            let mut path = format!(
                "/v1/entries?feed_id={feed_id}&order=published_at&direction=desc&limit={limit}&offset={offset}"
            );
            if let Some(after) = changed_after {
                path.push_str(&format!("&changed_after={after}"));
            }
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

    /// `opml_import` with the HTTP status preserved: Python's
    /// Miniflux import falls back to per-feed adds when the
    /// bulk endpoint answers 404/405, so the caller needs the
    /// code, not just a message (Audit 8 Task 27, F03).
    pub fn opml_import_result(&self, opml_xml: &str) -> Result<(), OpmlImportError> {
        crate::http::agent()
            .post(&format!("{}/v1/import", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/xml; charset=utf-8")
            .send(opml_xml)
            .map_err(|e| match e {
                ureq::Error::StatusCode(code) => OpmlImportError::Http(code),
                other => OpmlImportError::Other(format!("OPML import failed: {other}")),
            })?;
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

    /// Create a feed filed under a category (Python's Miniflux
    /// `add_feed(url, category)`): the category is matched
    /// case-insensitively against the server inventory and
    /// created when missing; with no category the account's
    /// first category is used, as Miniflux requires one.
    /// An HTTP 400 for an already-subscribed URL counts as
    /// success with the existing feed's id, as in Python.
    pub fn create_feed_remote_in(
        &self,
        feed_url: &str,
        category: Option<&str>,
    ) -> Result<i64, String> {
        let cats = self.categories_remote()?;
        if cats.is_empty() {
            return Err("Miniflux account has no categories".to_string());
        }
        let category_id = match category.map(str::trim).filter(|c| !c.is_empty()) {
            None => cats[0].0,
            Some(title) => match cats.iter().find(|(_, t)| t.eq_ignore_ascii_case(title)) {
                Some((id, _)) => *id,
                None => self.create_category_remote(title)?,
            },
        };
        let body = serde_json::json!({"feed_url": feed_url, "category_id": category_id});
        let mut resp = match crate::http::agent()
            .post(&format!("{}/v1/feeds", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
        {
            Ok(resp) => resp,
            Err(ureq::Error::StatusCode(400)) => {
                // Already subscribed (Python treats the 400
                // as success): answer with the existing id.
                let feeds = self.feeds()?;
                return feeds
                    .iter()
                    .find(|f| f.feed_url == feed_url || f.site_url == feed_url)
                    .map(|f| f.id)
                    .ok_or_else(|| "create feed failed: HTTP 400".to_string());
            }
            Err(e) => {
                // Companion fallback (Python add_feed): the server may sit
                // behind a browser gate, so ask it to solve the challenge
                // from its own IP and create the feed through the API.
                if let Some(value) = self.browser_feed_fallback_request(
                    "add",
                    serde_json::json!({"feed_url": feed_url, "category_id": category_id}),
                    true,
                ) {
                    let fid = value.get("feed_id").and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
                    });
                    if let Some(fid) = fid {
                        return Ok(fid);
                    }
                }
                return Err(format!("create feed failed: {e}"));
            }
        };
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
        let synced = self.sync_feed_entries(db, mf_id)?;
        // Companion recovery (Python refresh_feed): a stored
        // challenge-looking error queues this feed for the
        // server-side browser gate solver.
        if let Ok(feeds) = self.feeds()
            && let Some(target) = feeds.iter().find(|f| f.id == mf_id)
            && target.parsing_error_count > 0
            && looks_like_browser_challenge_error(&target.parsing_error_message)
        {
            self.queue_browser_feed_recovery(&[mf_id]);
        }
        Ok(synced)
    }

    fn sync_feed_entries(&self, db: &Db, mf_id: i64) -> Result<usize, String> {
        let since = sync_since(db);
        let entries = self.entries(mf_id, 100, since)?;
        store_entries(db, vec![(format!("miniflux:{mf_id}"), entries)])
    }

    /// Sync feeds and their entries, including read history and bookmarks.
    ///
    /// The first sync of a cache downloads every entry. Later ones ask each feed
    /// only for entries changed since the last complete sync (`changed_after`,
    /// which Miniflux also bumps on read/star changes), fetch feeds in parallel,
    /// and write everything in one transaction once every feed has answered.
    /// `per_feed_items` is the page size.
    pub fn sync(&self, db: &Db, per_feed_items: usize) -> Result<(usize, usize), String> {
        let started = unix_now();
        let since = sync_since(db);
        let feeds = self.feeds()?;
        for f in &feeds {
            db.upsert_hosted_feed(&Feed {
                id: format!("miniflux:{}", f.id),
                url: f.feed_url.clone(),
                title: f.title.clone(),
                title_is_custom: false,
                category: f.category.as_ref().map(|c| c.title.clone()),
                icon_url: None,
            })
            .map_err(|e| format!("feed store failed: {e}"))?;
        }
        let next = std::sync::atomic::AtomicUsize::new(0);
        let fetched = std::sync::Mutex::new(Vec::with_capacity(feeds.len()));
        let failure = std::sync::Mutex::new(None::<String>);
        std::thread::scope(|scope| {
            for _ in 0..SYNC_WORKERS.min(feeds.len().max(1)) {
                scope.spawn(|| {
                    loop {
                        if failure.lock().unwrap_or_else(|p| p.into_inner()).is_some() {
                            return;
                        }
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(f) = feeds.get(i) else { return };
                        match self.entries(f.id, per_feed_items, since) {
                            Ok(entries) => fetched
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push((format!("miniflux:{}", f.id), entries)),
                            Err(e) => {
                                failure
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .get_or_insert(e);
                                return;
                            }
                        }
                    }
                });
            }
        });
        if let Some(e) = failure.into_inner().unwrap_or_else(|p| p.into_inner()) {
            return Err(e);
        }
        let new_articles =
            store_entries(db, fetched.into_inner().unwrap_or_else(|p| p.into_inner()))?;
        // Advance only after a complete sync; the margin absorbs clock skew between
        // this machine and the server and entries changed while we were fetching.
        db.set_hosted_sync_watermark("miniflux", started - SYNC_WATERMARK_MARGIN_S)
            .map_err(|e| format!("sync state store failed: {e}"))?;
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
/// Feeds fetched at once during a Miniflux sync.
const SYNC_WORKERS: usize = 8;
/// Seconds subtracted from a sync's start time before it becomes the next
/// `changed_after`, so skew and mid-sync changes are fetched again, not lost.
const SYNC_WATERMARK_MARGIN_S: i64 = 600;

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The `changed_after` for this sync: None (full download) for a fresh cache.
fn sync_since(db: &Db) -> Option<i64> {
    db.hosted_sync_watermark("miniflux").ok().flatten()
}

fn mf_article(e: MfEntry, feed_id: &str) -> Article {
    let media = e
        .enclosures
        .iter()
        .find(|enc| enc.mime_type.starts_with("audio/") || enc.mime_type.starts_with("video/"))
        .or_else(|| e.enclosures.first());
    let media_url = media.map(|enc| enc.url.clone());
    let media_type = media
        .map(|enc| enc.mime_type.clone())
        .filter(|t| !t.is_empty());
    Article {
        id: format!("miniflux-entry:{}", e.id),
        feed_id: feed_id.to_string(),
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
        media_url,
        media_type,
    }
}

/// Write fetched entries, skipping deleted ones, in one transaction; returns
/// how many were new to the cache.
fn store_entries(db: &Db, fetched: Vec<(String, Vec<MfEntry>)>) -> Result<usize, String> {
    let tombstones = db.deleted_article_ids().unwrap_or_default();
    let articles: Vec<Article> = fetched
        .into_iter()
        .flat_map(|(feed_id, entries)| entries.into_iter().map(move |e| mf_article(e, &feed_id)))
        .filter(|a| !tombstones.contains(&a.id))
        .collect();
    db.upsert_hosted_articles(&articles)
        .map_err(|e| format!("article store failed: {e}"))
}

/// Challenge markers (Python `_looks_like_browser_challenge_error`).
fn looks_like_browser_challenge_error(value: &str) -> bool {
    let text = value.to_lowercase();
    [
        "status code: 403",
        "status code 403",
        "http 403",
        "forbidden",
        "cloudflare",
        "browser verification",
        "bot challenge",
        "cf-mitigated",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

/// Companion endpoint (Python `_browser_feed_fallback_request`):
/// base + configured path + action, slashes normalized. None
/// when the base URL or the path is empty.
fn browser_fallback_endpoint(base_url: &str, path_conf: &str, action: &str) -> Option<String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    let mut path = path_conf.trim().to_string();
    if path.is_empty() {
        return None;
    }
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    let action = action.trim().trim_matches('/');
    let path = path.trim_end_matches('/');
    Some(format!("{base}{path}/{action}"))
}

/// Companion timeout (Python `_browser_feed_fallback_request`):
/// 12 s fire-and-forget, or clamp(config, 45..180) + 30 when
/// waiting on the clearance round-trip.
fn browser_fallback_timeout(wait: bool, cfg_seconds: f64) -> u64 {
    if !wait {
        return 12;
    }
    let cfg = if cfg_seconds.is_finite() {
        cfg_seconds
    } else {
        90.0
    };
    (cfg.clamp(45.0, 180.0) as u64) + 30
}

impl MinifluxClient {
    fn browser_fallback_path(&self) -> String {
        self.refresh_config
            .raw()
            .get("providers")
            .and_then(|p| p.get("miniflux"))
            .and_then(|m| m.get("browser_feed_fallback_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("/blindrss-browser-feed/v1")
            .to_string()
    }

    /// Call the optional same-origin companion that solves
    /// browser-verification challenges from the Miniflux host's
    /// own IP (Python `_browser_feed_fallback_request`). The API
    /// token is forwarded only to the configured origin; a 404
    /// disables probing for the rest of the process, every other
    /// failure returns None, and nothing is ever sent when the
    /// `browser_feed_fallback_enabled` gate is off.
    pub fn browser_feed_fallback_request(
        &self,
        action: &str,
        payload: serde_json::Value,
        wait: bool,
    ) -> Option<serde_json::Value> {
        if !self
            .refresh_config
            .get_bool("browser_feed_fallback_enabled", true)
        {
            return None;
        }
        {
            let state = self.refresh_state.lock().unwrap_or_else(|p| p.into_inner());
            if state.browser_fallback_available == Some(false) {
                return None;
            }
        }
        let endpoint =
            browser_fallback_endpoint(&self.base_url, &self.browser_fallback_path(), action)?;
        let timeout_s = browser_fallback_timeout(
            wait,
            self.refresh_config
                .get_i64("browser_feed_fallback_timeout_seconds", 90) as f64,
        );
        let config = ureq::Agent::config_builder()
            .user_agent(crate::http::user_agent())
            .timeout_connect(Some(std::time::Duration::from_secs(3)))
            .timeout_global(Some(std::time::Duration::from_secs(timeout_s)))
            .http_status_as_error(false)
            .build();
        let mut resp = ureq::Agent::new_with_config(config)
            .post(&endpoint)
            .header("X-Auth-Token", &self.api_key)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .send(payload.to_string())
            .ok()?;
        let status = resp.status().as_u16();
        if status == 404 {
            self.refresh_state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .browser_fallback_available = Some(false);
            return None;
        }
        if !(200..=202).contains(&status) {
            return None;
        }
        self.refresh_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .browser_fallback_available = Some(true);
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if value.is_object() {
            Some(value)
        } else {
            Some(serde_json::json!({}))
        }
    }

    /// Fire a "recover" companion request (Python
    /// `_queue_browser_feed_recovery`). An empty id list asks
    /// the server to filter by its own latest error text.
    pub fn queue_browser_feed_recovery(&self, feed_ids: &[i64]) -> bool {
        let ids: Vec<String> = feed_ids.iter().map(|id| id.to_string()).collect();
        self.browser_feed_fallback_request("recover", serde_json::json!({"feed_ids": ids}), false)
            .is_some()
    }
}

#[cfg(test)]
mod miniflux_fallback_tests {
    use super::*;
    use crate::config::Config;
    use std::io::{Read, Write};

    #[test]
    fn challenge_markers_match_python() {
        for hit in [
            "Error, status code: 403",
            "HTTP 403 Forbidden",
            "forbidden by policy",
            "Cloudflare challenge",
            "browser verification required",
            "bot challenge issued",
            "cf-mitigated: challenge",
        ] {
            assert!(looks_like_browser_challenge_error(hit), "missed: {hit}");
        }
        for miss in ["HTTP 500", "connection timed out", "", "not found"] {
            assert!(
                !looks_like_browser_challenge_error(miss),
                "false hit: {miss}"
            );
        }
    }

    #[test]
    fn fallback_endpoint_shape_matches_python() {
        assert_eq!(
            browser_fallback_endpoint("https://mf.example", "/blindrss-browser-feed/v1", "recover"),
            Some("https://mf.example/blindrss-browser-feed/v1/recover".to_string())
        );
        // Missing leading slash and trailing slashes are normalized.
        assert_eq!(
            browser_fallback_endpoint("https://mf.example/", "companion/", "/add/"),
            Some("https://mf.example/companion/add".to_string())
        );
        assert_eq!(browser_fallback_endpoint("", "/x", "recover"), None);
        assert_eq!(
            browser_fallback_endpoint("https://mf.example", "   ", "recover"),
            None
        );
    }

    #[test]
    fn fallback_timeout_matches_python_clamp() {
        assert_eq!(browser_fallback_timeout(false, 90.0), 12);
        // wait: clamp(config, 45..180) as int, plus 30.
        assert_eq!(browser_fallback_timeout(true, 90.0), 120);
        assert_eq!(browser_fallback_timeout(true, 1.0), 75);
        assert_eq!(browser_fallback_timeout(true, 999.0), 210);
    }

    /// Serve scripted statuses over loopback; returns the base
    /// URL and a hit counter.
    fn serve_statuses(statuses: Vec<u16>) -> (String, std::sync::Arc<std::sync::Mutex<usize>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let hits2 = hits.clone();
        std::thread::spawn(move || {
            for status in statuses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                *hits2.lock().unwrap() += 1;
                let mut req = Vec::new();
                let mut chunk = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&chunk[..n]);
                }
                // Drain the declared body too: closing with an
                // unread request body resets the connection and
                // the client loses the response body.
                let head_end = req
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|i| i + 4)
                    .unwrap_or(req.len());
                let want: usize = String::from_utf8_lossy(&req[..head_end])
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("Content-Length:")
                            .or_else(|| l.strip_prefix("content-length:"))
                    })
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while req.len() < head_end + want {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&chunk[..n]);
                }
                let body = if status == 200 {
                    r#"{"ok":true}"#
                } else {
                    "{}"
                };
                let head = format!(
                    "HTTP/1.1 {status} x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), hits)
    }

    #[test]
    fn fallback_404_disables_further_probing() {
        let (base, hits) = serve_statuses(vec![404]);
        let client = MinifluxClient::new(&base, "token").with_refresh_config(&Config::default());
        assert_eq!(
            client.browser_feed_fallback_request("recover", serde_json::json!({}), false),
            None
        );
        assert_eq!(*hits.lock().unwrap(), 1);
        // Second call skips the network entirely.
        assert_eq!(
            client.browser_feed_fallback_request("recover", serde_json::json!({}), false),
            None
        );
        assert_eq!(*hits.lock().unwrap(), 1);
    }

    #[test]
    fn fallback_200_returns_object_and_stays_enabled() {
        let (base, hits) = serve_statuses(vec![200, 200]);
        let client = MinifluxClient::new(&base, "token").with_refresh_config(&Config::default());
        let value =
            client.browser_feed_fallback_request("add", serde_json::json!({"feed_url": "x"}), true);
        assert_eq!(value, Some(serde_json::json!({"ok": true})));
        assert_eq!(*hits.lock().unwrap(), 1);
        let _ = client.browser_feed_fallback_request("recover", serde_json::json!({}), false);
        assert_eq!(*hits.lock().unwrap(), 2);
    }

    #[test]
    fn fallback_respects_the_settings_gate() {
        let (base, hits) = serve_statuses(vec![200]);
        let mut cfg = Config::default();
        cfg.set_path("browser_feed_fallback_enabled", serde_json::json!(false));
        let client = MinifluxClient::new(&base, "token").with_refresh_config(&cfg);
        assert_eq!(
            client.browser_feed_fallback_request("recover", serde_json::json!({}), false),
            None
        );
        assert_eq!(*hits.lock().unwrap(), 0);
    }
}
