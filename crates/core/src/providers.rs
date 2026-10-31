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
    token: RefCell<Option<String>>,
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
}

#[derive(Deserialize)]
struct LinkHref {
    #[serde(default)]
    href: String,
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

impl GReaderClient {
    pub fn new(service: GReaderService, email: &str, password: &str) -> Self {
        Self {
            service,
            api_base: service.api_base().to_string(),
            login_url: service.login_url().to_string(),
            email: email.to_string(),
            password: password.to_string(),
            oauth_token: None,
            token: RefCell::new(None),
        }
    }

    pub fn with_oauth(service: GReaderService, token: &str) -> Self {
        let mut c = Self::new(service, "", "");
        c.oauth_token = Some(token.to_string());
        c
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
        let mut resp = crate::http::agent()
            .get(&url)
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("GET {path} failed: {e}"))?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("read failed: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("bad JSON from {path}: {e}"))
    }

    pub fn subscriptions(&self) -> Result<Vec<RemoteFeed>, String> {
        let list: SubList = self.get_json("/subscription/list?output=json")?;
        Ok(list
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
            .collect())
    }

    /// Fetch a stream's items as articles (not yet stored).
    pub fn stream_articles(&self, stream_id: &str, count: usize) -> Result<Vec<Article>, String> {
        let encoded = stream_id
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace(':', "%3A")
            .replace('?', "%3F")
            .replace('&', "%26");
        let path = format!("/stream/contents/{encoded}?output=json&n={count}");
        let contents: StreamContents = self.get_json(&path)?;
        Ok(contents
            .items
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
                    is_read: false,
                    is_favorite: false,
                    media_url: None,
                    media_type: None,
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
        let mut token_resp = crate::http::agent()
            .get(&format!("{}/token", self.api_base))
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
        crate::http::agent()
            .post(&format!("{}/edit-tag", self.api_base))
            .header("Authorization", &self.auth_header()?)
            .send_form([("i", item_id), (verb, state), ("T", edit_token.trim())])
            .map_err(|e| format!("edit-tag failed: {e}"))?;
        Ok(())
    }

    /// Set one item's starred state on the server (the GReader
    /// starred state is the hosted favorite).
    pub fn set_starred_remote(&self, item_id: &str, starred: bool) -> Result<(), String> {
        let mut token_resp = crate::http::agent()
            .get(&format!("{}/token", self.api_base))
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
        crate::http::agent()
            .post(&format!("{}/edit-tag", self.api_base))
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
        let mut token_resp = crate::http::agent()
            .get(&format!("{}/token", self.api_base))
            .header("Authorization", &self.auth_header()?)
            .call()
            .map_err(|e| format!("token fetch failed: {e}"))?;
        let edit_token = token_resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("token read failed: {e}"))?;
        crate::http::agent()
            .post(&format!("{}/subscription/edit", self.api_base))
            .header("Authorization", &self.auth_header()?)
            .send_form([("s", stream), ("ac", action), ("T", edit_token.trim())])
            .map_err(|e| format!("subscription edit failed: {e}"))?;
        Ok(())
    }

    /// Full sync: import subscriptions as local feeds, then store the
    /// reading-list stream's articles. Returns (feeds, new articles).
    pub fn sync(&self, db: &Db, per_feed_items: usize) -> Result<(usize, usize), String> {
        let subs = self.subscriptions()?;
        for sub in &subs {
            db.insert_feed_if_missing(&Feed {
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
                    .insert_article_if_new(a)
                    .map_err(|e| format!("article store failed: {e}"))?
                {
                    new_articles += 1;
                }
            }
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

pub struct MinifluxClient {
    /// Base URL without trailing /v1.
    pub base_url: String,
    pub api_key: String,
}

#[derive(Deserialize)]
pub struct MfFeed {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    feed_url: String,
    #[serde(default)]
    category: Option<MfCategory>,
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
}

impl MinifluxClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url
                .trim_end_matches('/')
                .trim_end_matches("/v1")
                .to_string(),
            api_key: api_key.to_string(),
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
        let path = format!(
            "/v1/entries?feed_id={feed_id}&status=unread&order=published_at&direction=desc&limit={limit}"
        );
        let resp: MfEntries = self.get_json(&path)?;
        Ok(resp.entries)
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

    /// Toggle an entry's bookmark (Miniflux's favorite).
    pub fn toggle_bookmark_remote(&self, entry_id: i64) -> Result<(), String> {
        crate::http::agent()
            .put(&format!("{}/v1/entries/{entry_id}/bookmark", self.base_url))
            .header("X-Auth-Token", &self.api_key)
            .send_empty()
            .map_err(|e| format!("bookmark failed: {e}"))?;
        Ok(())
    }

    /// Sync feeds + unread entries into the local database.
    pub fn sync(&self, db: &Db, per_feed_items: usize) -> Result<(usize, usize), String> {
        let feeds = self.feeds()?;
        let tombstones = db.deleted_article_ids().unwrap_or_default();
        let mut new_articles = 0;
        for f in &feeds {
            let local_id = format!("miniflux:{}", f.id);
            db.insert_feed_if_missing(&Feed {
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
                    is_favorite: false,
                    media_url: None,
                    media_type: None,
                };
                if tombstones.contains(&article.id) {
                    continue;
                }
                if db
                    .insert_article_if_new(&article)
                    .map_err(|e| format!("article store failed: {e}"))?
                {
                    new_articles += 1;
                }
            }
        }
        Ok((feeds.len(), new_articles))
    }
}
