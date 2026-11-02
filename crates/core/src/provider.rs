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
use std::path::{Path, PathBuf};

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
    /// Edit subscription metadata, committing the cache only after remote success.
    fn update_feed(
        &self,
        feed: &Feed,
        title: Option<&str>,
        url: Option<&str>,
        category: Option<&str>,
    ) -> Result<(), String>;
    fn mark_all_read(&self, feed_id: Option<&str>) -> Result<usize, String>;
    /// Pull the provider's latest content into the cache.
    /// Returns a human summary.
    fn refresh(&self) -> Result<String, String>;
    fn refresh_scheduled(&self) -> Result<String, String> {
        self.refresh()
    }
    /// Later updates are delivered on a worker, never on the UI thread.
    fn refresh_with_updates(
        &self,
        force: bool,
        _on_background: Box<dyn FnOnce(Result<String, String>) + Send>,
    ) -> Result<String, String> {
        if force {
            self.refresh()
        } else {
            self.refresh_scheduled()
        }
    }
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
    fn update_feed(
        &self,
        feed: &Feed,
        title: Option<&str>,
        url: Option<&str>,
        category: Option<&str>,
    ) -> Result<(), String> {
        let db = self.db()?;
        db.update_feed(&feed.id, title, url)
            .map_err(|e| e.to_string())?;
        if let Some(category) = category {
            db.set_feed_category(&feed.id, Some(category))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn mark_all_read(&self, feed_id: Option<&str>) -> Result<usize, String> {
        self.db()?.mark_all_read(feed_id).map_err(|e| e.to_string())
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
        Db::open_or_create(&self.db_path).map_err(|e| format!("{e}"))
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
                        .with_cache_ttls(meta_ttl, article_ttl)
                        .with_article_page_size(crate::providers::page_size_from_config(&self.cfg)))
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
        Ok(MinifluxClient::new(&url, &key).with_refresh_config(&self.cfg))
    }
    fn refresh_miniflux(
        &self,
        force: bool,
        on_background: Box<dyn FnOnce(Result<String, String>) + Send>,
    ) -> Result<String, String> {
        let client = self.miniflux()?;
        let pending = client.request_refresh(force);
        let synced = client.sync(&self.db()?, 100);
        let background = pending?;
        if let Some(pending) = background {
            // The original account-scoped path survives account switches; workers own connections.
            let path = self.db_path.clone();
            std::thread::spawn(move || {
                let requests = pending
                    .recv()
                    .map_err(|_| "Miniflux refresh worker stopped".to_string())
                    .and_then(|r| r);
                let sync = Db::open(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|db| client.sync(&db, 100));
                let result = requests
                    .and(sync)
                    .map(|(feeds, new)| format!("{feeds} feeds, {new} new article(s)"));
                on_background(result);
            });
        }
        synced.map(|(feeds, new)| format!("{feeds} feeds, {new} new article(s)"))
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
        let db = self.db()?;
        let mut articles = db.articles(feed_id, limit).map_err(|e| format!("{e}"))?;
        // Hosted NPR attach: program feeds carry no enclosure, so
        // resolve the audio client-side (cached in the Db).
        crate::sources::attach_hosted_npr_media(&db, self.name, &mut articles);
        Ok(articles)
    }
    fn mark_read(&self, article: &Article, read: bool) -> Result<(), String> {
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
        self.db()?
            .mark_read(&article.id, read)
            .map_err(|e| format!("{e}"))
    }
    fn set_favorite(&self, article: &Article, favorite: bool) -> Result<(), String> {
        match self.kind {
            HostedKind::GReader(_) => {
                if article.feed_id.starts_with("feed/") {
                    self.greader()?.set_starred_remote(&article.id, favorite)?;
                }
            }
            HostedKind::Miniflux => {
                // Miniflux bookmarks toggle; read the server state
                // before setting an explicit desired state.
                if let Some(entry) = article
                    .id
                    .strip_prefix("miniflux-entry:")
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    self.miniflux()?.set_bookmark_remote(entry, favorite)?;
                }
            }
        }
        self.db()?
            .set_favorite(&article.id, favorite)
            .map_err(|e| format!("{e}"))
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
    fn update_feed(
        &self,
        feed: &Feed,
        title: Option<&str>,
        url: Option<&str>,
        category: Option<&str>,
    ) -> Result<(), String> {
        match self.kind {
            HostedKind::Miniflux => {
                let id = feed
                    .id
                    .strip_prefix("miniflux:")
                    .and_then(|s| s.parse().ok())
                    .ok_or("Invalid Miniflux feed")?;
                self.miniflux()?
                    .update_feed_remote(id, title, url, category)?;
            }
            HostedKind::GReader(_) => self.greader()?.update_feed_remote(
                &feed.id,
                title,
                url,
                category,
                feed.category.as_deref(),
            )?,
        }
        let db = self.db()?;
        db.update_feed(&feed.id, title, url)
            .map_err(|e| e.to_string())?;
        if let Some(category) = category {
            db.set_feed_category(&feed.id, Some(category))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn mark_all_read(&self, feed_id: Option<&str>) -> Result<usize, String> {
        match self.kind {
            HostedKind::Miniflux => {
                let id = feed_id
                    .map(|id| {
                        id.strip_prefix("miniflux:")
                            .and_then(|s| s.parse().ok())
                            .ok_or("Invalid Miniflux feed")
                    })
                    .transpose()?;
                self.miniflux()?.mark_all_read_remote(id)?;
            }
            HostedKind::GReader(_) => self.greader()?.mark_all_read_remote(feed_id)?,
        }
        self.db()?.mark_all_read(feed_id).map_err(|e| e.to_string())
    }
    fn refresh(&self) -> Result<String, String> {
        self.refresh_with_updates(true, Box::new(|_| {}))
    }
    fn refresh_scheduled(&self) -> Result<String, String> {
        self.refresh_with_updates(false, Box::new(|_| {}))
    }
    fn refresh_with_updates(
        &self,
        force: bool,
        on_background: Box<dyn FnOnce(Result<String, String>) + Send>,
    ) -> Result<String, String> {
        match self.kind {
            HostedKind::Miniflux => self.refresh_miniflux(force, on_background),
            HostedKind::GReader(_) => {
                let client = self.greader()?;
                if force {
                    client.invalidate_caches();
                }
                let (feeds, new) = client.sync(&self.db()?, 100)?;
                Ok(format!("{feeds} feeds, {new} new article(s)"))
            }
        }
    }
}

/// Build the active provider from config (Python
/// `factory.get_provider`). Unknown names fall back to Local,
/// like Python's else branch.
pub fn get_provider(cfg: &Config, db_path: PathBuf) -> Box<dyn Provider + Send> {
    let name = cfg.get_str("active_provider", "local").to_string();
    let hosted_path = active_db_path(cfg, &db_path);
    let hosted = |name: &'static str, kind: HostedKind| -> Box<dyn Provider + Send> {
        Box::new(HostedProvider {
            name,
            kind,
            db_path: hosted_path.clone(),
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

/// Hosted accounts keep separate rebuildable caches; the existing local database is untouched.
/// No data is migrated: a previously used hosted account needs its first sync in this cache.
pub fn active_db_path(cfg: &Config, base: &Path) -> PathBuf {
    use sha2::Digest;
    let name = active_name(cfg);
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("rss");
    let scoped = stem.contains(".hosted-");
    let stem = stem.split(".hosted-").next().unwrap_or(stem);
    let extension = base.extension().and_then(|s| s.to_str()).unwrap_or("db");
    if name == "local" {
        return if scoped {
            base.with_file_name(format!("{stem}.{extension}"))
        } else {
            base.to_path_buf()
        };
    }
    let section = cfg.raw().get("providers").and_then(|p| p.get(&name));
    let cred = |key: &str| {
        section
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    let url = cred("url").trim_end_matches('/').trim_end_matches("/v1");
    let identity = serde_json::json!([
        name,
        url,
        cred("email").to_lowercase(),
        cred("api_key"),
        cred("token")
    ]);
    let hash: String = sha2::Sha256::digest(identity.to_string())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    base.with_file_name(format!("{stem}.hosted-{name}-{hash}.{extension}"))
}
