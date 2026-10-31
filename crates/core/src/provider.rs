//! The active-provider architecture (Python `core/factory.py`
//! + `providers/base.py`).
//!
//! Python runs the whole UI against ONE provider chosen by the
//! `active_provider` config key: Local, Miniflux, Inoreader,
//! BazQux or The Old Reader. This module is the Rust factory
//! and the app-facing slice of that interface.
//!
//! Hosted providers are cache-backed: their sync pulls remote
//! content into the local database (the same store Local
//! reads), reads serve from that cache, and writes go to the
//! cache AND the server (read state, favorites, subscriptions)
//! so the two never silently diverge. Deeper remote surface
//! (categories sync, server-side full text) builds on this
//! seam.

use crate::config::Config;
use crate::db::{Article, Db, Feed};
use crate::providers::{GReaderClient, GReaderService, MinifluxClient};
use std::path::PathBuf;

/// Provider names in UI order; "local" is the default.
pub const PROVIDER_NAMES: [&str; 5] = ["local", "theoldreader", "bazqux", "inoreader", "miniflux"];

/// Display labels for PROVIDER_NAMES.
pub const PROVIDER_LABELS: [&str; 5] =
    ["Local", "The Old Reader", "BazQux", "Inoreader", "Miniflux"];

/// The app-facing provider interface.
pub trait Provider {
    /// Config name ("local", "miniflux", ...).
    fn name(&self) -> &str;
    /// True for hosted backends (writes also go to a server).
    fn is_remote(&self) -> bool {
        false
    }
    /// The feed list the tree shows.
    fn feeds(&self) -> Result<Vec<Feed>, String>;
    /// Articles for one feed (None = all), newest first.
    fn articles(&self, feed_id: Option<&str>, limit: usize) -> Result<Vec<Article>, String>;
    /// Set an article's read state (cache + server when hosted).
    fn mark_read(&self, article: &Article, read: bool) -> Result<(), String>;
    /// Set an article's favorite state (cache + server).
    fn set_favorite(&self, article: &Article, favorite: bool) -> Result<(), String>;
    /// Subscribe a feed URL. Returns a human summary.
    fn add_feed(&self, url: &str) -> Result<String, String>;
    /// Unsubscribe/remove a feed (cache + server when hosted).
    fn remove_feed(&self, feed: &Feed) -> Result<(), String>;
    /// Pull the provider's latest content into the cache.
    /// Returns a human summary.
    fn refresh(&self) -> Result<String, String>;
}

/// The local backend: everything goes to the on-disk database.
pub struct LocalProvider {
    db_path: PathBuf,
}

impl LocalProvider {
    fn db(&self) -> Result<Db, String> {
        Db::open(&self.db_path).map_err(|e| format!("{e}"))
    }
}

impl Provider for LocalProvider {
    fn name(&self) -> &str {
        "local"
    }
    fn feeds(&self) -> Result<Vec<Feed>, String> {
        self.db()?.feeds().map_err(|e| format!("{e}"))
    }
    fn articles(&self, feed_id: Option<&str>, limit: usize) -> Result<Vec<Article>, String> {
        self.db()?
            .articles(feed_id, limit)
            .map_err(|e| format!("{e}"))
    }
    fn mark_read(&self, article: &Article, read: bool) -> Result<(), String> {
        self.db()?
            .mark_read(&article.id, read)
            .map_err(|e| format!("{e}"))
    }
    fn set_favorite(&self, article: &Article, favorite: bool) -> Result<(), String> {
        self.db()?
            .set_favorite(&article.id, favorite)
            .map_err(|e| format!("{e}"))
    }
    fn add_feed(&self, url: &str) -> Result<String, String> {
        // Local subscribe resolves + stores like Add Feed does;
        // the app keeps its richer async flow for "local" and
        // uses this for hosted backends. Still functional:
        let (feed_url, parsed) = crate::discovery::resolve_subscribe_url(url)?;
        let db = self.db()?;
        let feed = db
            .add_feed(&feed_url, &parsed.title, None)
            .map_err(|e| format!("{e}"))?;
        crate::refresh::store_parsed(&db, &feed, &parsed)?;
        Ok(format!("Added {}", feed.title))
    }
    fn remove_feed(&self, feed: &Feed) -> Result<(), String> {
        self.db()?.remove_feed(&feed.id).map_err(|e| format!("{e}"))
    }
    fn refresh(&self) -> Result<String, String> {
        // The app runs local refresh through its worker pool
        // (progress + cancel); the trait path does the same
        // work synchronously for non-UI callers.
        let db = self.db()?;
        let results = crate::refresh::refresh_all(&db);
        let new: usize = results.iter().filter_map(|(_, r)| r.as_ref().ok()).sum();
        Ok(format!("{new} new article(s)"))
    }
}

/// Which hosted service a HostedProvider talks to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum HostedKind {
    GReader(GReaderService),
    Miniflux,
}

/// A hosted backend (cache + server write-through).
pub struct HostedProvider {
    name: &'static str,
    kind: HostedKind,
    db_path: PathBuf,
    cfg: Config,
}

impl HostedProvider {
    fn db(&self) -> Result<Db, String> {
        Db::open(&self.db_path).map_err(|e| format!("{e}"))
    }

    fn section(&self) -> Option<serde_json::Value> {
        self.cfg.raw().get("providers")?.get(self.name).cloned()
    }

    fn cred(&self, key: &str) -> String {
        self.section()
            .as_ref()
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    fn greader(&self) -> Result<GReaderClient, String> {
        match self.kind {
            HostedKind::GReader(service) => {
                if service == GReaderService::Inoreader {
                    let token = self.cred("token");
                    if token.is_empty() {
                        return Err("Inoreader is not connected".to_string());
                    }
                    let meta_ttl = self
                        .cred("metadata_cache_ttl_seconds")
                        .parse::<i64>()
                        .unwrap_or(3600);
                    let article_ttl = self
                        .cred("article_cache_ttl_seconds")
                        .parse::<i64>()
                        .unwrap_or(90);
                    Ok(GReaderClient::with_oauth(service, &token)
                        .with_cache_ttls(meta_ttl, article_ttl))
                } else {
                    let (email, password) = (self.cred("email"), self.cred("password"));
                    if email.is_empty() || password.is_empty() {
                        return Err(format!("{} credentials are not set", self.name));
                    }
                    Ok(GReaderClient::new(service, &email, &password))
                }
            }
            HostedKind::Miniflux => Err("not a GReader provider".to_string()),
        }
    }

    fn miniflux(&self) -> Result<MinifluxClient, String> {
        let (url, key) = (self.cred("url"), self.cred("api_key"));
        if url.is_empty() || key.is_empty() {
            return Err("Miniflux URL or API key is not set".to_string());
        }
        Ok(MinifluxClient::new(&url, &key))
    }
}

impl Provider for HostedProvider {
    fn name(&self) -> &str {
        self.name
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn feeds(&self) -> Result<Vec<Feed>, String> {
        // The cache holds this provider's subscriptions under
        // its id scheme (GReader "feed/..." stream ids, Miniflux
        // "miniflux:<id>"); local feeds stay visible too when
        // mixed, matching the merged tree the sync model built.
        self.db()?.feeds().map_err(|e| format!("{e}"))
    }
    fn articles(&self, feed_id: Option<&str>, limit: usize) -> Result<Vec<Article>, String> {
        self.db()?
            .articles(feed_id, limit)
            .map_err(|e| format!("{e}"))
    }
    fn mark_read(&self, article: &Article, read: bool) -> Result<(), String> {
        self.db()?
            .mark_read(&article.id, read)
            .map_err(|e| format!("{e}"))?;
        match self.kind {
            HostedKind::GReader(_) => {
                if article.feed_id.starts_with("feed/") {
                    self.greader()?.set_read_remote(&article.id, read)?;
                }
            }
            HostedKind::Miniflux => {
                if let Some(entry) = article
                    .id
                    .strip_prefix("miniflux-entry:")
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    self.miniflux()?
                        .set_status_remote(&[entry], if read { "read" } else { "unread" })?;
                }
            }
        }
        Ok(())
    }
    fn set_favorite(&self, article: &Article, favorite: bool) -> Result<(), String> {
        self.db()?
            .set_favorite(&article.id, favorite)
            .map_err(|e| format!("{e}"))?;
        match self.kind {
            HostedKind::GReader(_) => {
                if article.feed_id.starts_with("feed/") {
                    self.greader()?.set_starred_remote(&article.id, favorite)?;
                }
            }
            HostedKind::Miniflux => {
                // Miniflux bookmarks toggle; only call when the
                // desired state differs (cache said not favorite
                // before this call in the toggle flow).
                if let Some(entry) = article
                    .id
                    .strip_prefix("miniflux-entry:")
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    let _ = favorite; // the app toggles from the old state
                    self.miniflux()?.toggle_bookmark_remote(entry)?;
                }
            }
        }
        Ok(())
    }
    fn add_feed(&self, url: &str) -> Result<String, String> {
        match self.kind {
            HostedKind::GReader(_) => {
                self.greader()?.subscribe_remote(url)?;
            }
            HostedKind::Miniflux => {
                self.miniflux()?.create_feed_remote(url)?;
            }
        }
        // Pull the new subscription into the cache right away.
        let summary = self.refresh()?;
        Ok(format!("Subscribed on {} ({summary})", self.name))
    }
    fn remove_feed(&self, feed: &Feed) -> Result<(), String> {
        match self.kind {
            HostedKind::GReader(_) => {
                if feed.id.starts_with("feed/") {
                    self.greader()?.unsubscribe_remote(&feed.id)?;
                }
            }
            HostedKind::Miniflux => {
                if let Some(id) = feed
                    .id
                    .strip_prefix("miniflux:")
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    self.miniflux()?.delete_feed_remote(id)?;
                }
            }
        }
        self.db()?.remove_feed(&feed.id).map_err(|e| format!("{e}"))
    }
    fn refresh(&self) -> Result<String, String> {
        let db = self.db()?;
        let (feeds, new) = match self.kind {
            HostedKind::GReader(_) => {
                let client = self.greader()?;
                // A manual refresh bypasses the Inoreader
                // caches (Python's invalidation rule).
                client.invalidate_caches();
                client.sync(&db, 100)?
            }
            HostedKind::Miniflux => self.miniflux()?.sync(&db, 100)?,
        };
        Ok(format!("{feeds} feeds, {new} new article(s)"))
    }
}

/// Build the active provider from config (Python
/// `factory.get_provider`). Unknown names fall back to Local,
/// like Python's else branch.
pub fn get_provider(cfg: &Config, db_path: PathBuf) -> Box<dyn Provider + Send> {
    let name = cfg.get_str("active_provider", "local").to_string();
    let hosted = |name: &'static str, kind: HostedKind| -> Box<dyn Provider + Send> {
        Box::new(HostedProvider {
            name,
            kind,
            db_path: db_path.clone(),
            cfg: cfg.clone(),
        })
    };
    match name.as_str() {
        "theoldreader" => hosted(
            "theoldreader",
            HostedKind::GReader(GReaderService::TheOldReader),
        ),
        "bazqux" => hosted("bazqux", HostedKind::GReader(GReaderService::BazQux)),
        "inoreader" => hosted("inoreader", HostedKind::GReader(GReaderService::Inoreader)),
        "miniflux" => hosted("miniflux", HostedKind::Miniflux),
        _ => Box::new(LocalProvider { db_path }),
    }
}

/// The configured active provider's name, normalized.
pub fn active_name(cfg: &Config) -> String {
    let name = cfg.get_str("active_provider", "local");
    if PROVIDER_NAMES.contains(&name) {
        name.to_string()
    } else {
        "local".to_string()
    }
}
