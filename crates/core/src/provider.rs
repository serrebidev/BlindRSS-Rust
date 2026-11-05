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
    /// Import an OPML file through this provider (Python
    /// `providers/base.py::import_opml` and its overrides):
    /// the local provider imports into its database directly;
    /// hosted providers subscribe on the server (Miniflux
    /// through its bulk endpoint, with the per-feed fallback)
    /// and pull the result into the cache. A target category
    /// files every imported feed under it. Returns a human
    /// summary; Err when nothing could be imported.
    fn import_opml(&self, path: &Path, target_category: Option<&str>) -> Result<String, String>;
    /// The provider's category inventory (Python
    /// `get_categories`).
    fn categories(&self) -> Result<Vec<String>, String>;
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
    /// Mark a whole category view read. Python's providers
    /// resolve the view to server category membership (a
    /// GReader label stream, a Miniflux category endpoint),
    /// so articles labeled or moved into the category from a
    /// feed outside it are marked too — a per-feed fan-out
    /// over the category's own feeds cannot see them
    /// (Audit 8 D23). Local applies the same membership
    /// clause the category view displays. Returns the number
    /// of cache rows marked.
    fn mark_category_read(&self, category: &str) -> Result<usize, String>;
    /// Python `providers/base.py::uncategorized_is_real_category`:
    /// true when this provider's "Uncategorized" is an ordinary,
    /// deletable category rather than the synthetic "no category"
    /// bucket. Only Miniflux qualifies — it requires every feed
    /// to have a category, so "no category" is a real server
    /// category the user can rename or delete (issue #86). The
    /// system-folder guards consult this capability.
    fn uncategorized_is_real_category(&self) -> bool {
        false
    }
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
    /// The global `delete_behavior` setting captured at
    /// construction (per-feed overrides still win at apply time).
    delete_behavior: String,
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
        // Local subscribe goes through the one shared worker
        // subscribe path (resolve + row + first store, with
        // listing-source dispatch) — the same path Add Feed uses.
        let db = self.db()?;
        let feed = crate::refresh::subscribe_url(&db, url, None, &self.delete_behavior, None)?;
        Ok(format!("Added {}", feed.title))
    }
    fn import_opml(&self, path: &Path, target_category: Option<&str>) -> Result<String, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("Could not read OPML file: {e}"))?;
        let db = self.db()?;
        let added = crate::opml::import_opml_under(&db, &bytes, target_category)?;
        Ok(format!("Imported {added} new feed(s)"))
    }
    fn categories(&self) -> Result<Vec<String>, String> {
        Ok(self
            .db()?
            .categories()
            .map_err(|e| format!("{e}"))?
            .into_iter()
            .map(|c| c.title)
            .collect())
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
    fn mark_category_read(&self, category: &str) -> Result<usize, String> {
        self.db()?
            .mark_category_read(category)
            .map_err(|e| e.to_string())
    }
    fn refresh(&self) -> Result<String, String> {
        // The app runs local refresh through its worker pool
        // (progress + cancel); the trait path does the same
        // work synchronously for non-UI callers.
        let db = self.db()?;
        let results = crate::refresh::refresh_all(&db, &self.delete_behavior);
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
    /// Inoreader OAuth token endpoint (tests override it with
    /// a loopback URL, like GReaderClient's api_base).
    inoreader_token_url: String,
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

    /// Build the Inoreader client, running the OAuth refresh
    /// lifecycle first (Audit 8 D07; Python
    /// `providers/inoreader.py::_has_required_auth`): an
    /// expired access token is refreshed through the stored
    /// refresh token, rotated tokens are written back to the
    /// config file, a revoked grant clears them (the account
    /// then reads as not connected), and a transient refresh
    /// failure fails this call with the stored tokens — and
    /// the account cache — untouched.
    fn inoreader_client(&self, service: GReaderService) -> Result<GReaderClient, String> {
        let mut auth = crate::oauth::InoreaderAuth::from_config(&self.cfg);
        auth.token_url = self.inoreader_token_url.clone();
        if auth.access_token.is_empty() && auth.refresh_token.is_empty() {
            return Err("Inoreader is not connected".to_string());
        }
        if auth.has_app_credentials() {
            let attempted_refresh = auth.refresh_token.clone();
            match auth.ensure_fresh_now() {
                Ok(crate::oauth::TokenAction::Fresh) => {}
                Ok(action) => {
                    if let Some(adopted) =
                        self.persist_inoreader_auth(&auth, &attempted_refresh, action)
                    {
                        // The revocation was superseded: the
                        // config file already carries a newer
                        // grant from another flow. Continue
                        // with it instead of clearing anything.
                        auth = adopted;
                    } else if action == crate::oauth::TokenAction::Revoked {
                        return Err("Inoreader is not connected".to_string());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if auth.access_token.is_empty() {
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
        Ok(GReaderClient::with_oauth(service, &auth.access_token)
            .with_app_credentials(&auth.app_id, &auth.app_key)
            .with_cache_ttls(meta_ttl, article_ttl)
            .with_article_page_size(crate::providers::page_size_from_config(&self.cfg)))
    }

    /// Write Inoreader token state back to the file this
    /// provider's config was loaded from (Python's
    /// `_update_provider_config` writes through the shared
    /// config; best-effort — a config with no source file
    /// keeps the state in memory only). Returns the fresher
    /// on-disk credential set when a `Revoked` turns out to be
    /// superseded: the file already carries a different
    /// refresh token than the one just proved revoked, so
    /// another flow rotated first and nothing may be cleared.
    fn persist_inoreader_auth(
        &self,
        auth: &crate::oauth::InoreaderAuth,
        attempted_refresh: &str,
        action: crate::oauth::TokenAction,
    ) -> Option<crate::oauth::InoreaderAuth> {
        if action == crate::oauth::TokenAction::Revoked
            && let Some(path) = self.cfg.source_path()
            && let Ok(on_disk) = Config::load(path)
        {
            let disk_auth = crate::oauth::InoreaderAuth::from_config(&on_disk);
            if !disk_auth.refresh_token.is_empty() && disk_auth.refresh_token != attempted_refresh {
                return Some(disk_auth);
            }
        }
        let mut cfg = self.cfg.clone();
        auth.apply_to_config(&mut cfg);
        let _ = cfg.save_to_source();
        None
    }

    fn greader(&self) -> Result<GReaderClient, String> {
        match self.kind {
            HostedKind::GReader(service) => {
                if service == GReaderService::Inoreader {
                    self.inoreader_client(service)
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

    /// Drop hosted rows belonging to a different kind of
    /// account (a sync aimed at the wrong cache file before
    /// per-account paths were enforced). Hygiene only: a
    /// failure never blocks the sync.
    fn purge_foreign(&self) {
        if let Ok(db) = self.db() {
            let _ = match self.kind {
                HostedKind::GReader(_) => db.purge_hosted_rows_except("feed/"),
                HostedKind::Miniflux => db.purge_hosted_rows_except("miniflux:"),
            };
        }
    }

    fn miniflux(&self) -> Result<MinifluxClient, String> {
        let (url, key) = (self.cred("url"), self.cred("api_key"));
        if url.is_empty() || key.is_empty() {
            return Err("Miniflux URL or API key is not set".to_string());
        }
        Ok(MinifluxClient::new(&url, &key).with_refresh_config(&self.cfg))
    }
    /// Python `providers/miniflux.py::import_opml`: prepare
    /// the document strictly (a malformed file fails the
    /// import), wrap it in the target category's folder, and
    /// POST it to the bulk endpoint; on HTTP 404/405 fall
    /// back to the base per-feed import.
    fn import_opml_miniflux(&self, path: &Path, target: Option<&str>) -> Result<String, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("Could not read OPML file: {e}"))?;
        let text =
            String::from_utf8(bytes).map_err(|_| "OPML file is not valid UTF-8".to_string())?;
        let document = crate::opml::bulk_import_document(&text, target)?;
        let client = self.miniflux()?;
        match client.opml_import_result(&document) {
            Ok(()) => {
                let summary = self.refresh()?;
                Ok(format!("Imported OPML into Miniflux ({summary})"))
            }
            Err(crate::providers::OpmlImportError::Http(404))
            | Err(crate::providers::OpmlImportError::Http(405)) => {
                let triples = crate::opml::parse_opml(text.as_bytes())?;
                let mut count = 0;
                for (_title, url, category) in &triples {
                    let cat = target.unwrap_or(category.as_str());
                    if client.create_feed_remote_in(url, Some(cat)).is_ok() {
                        count += 1;
                    }
                }
                if count == 0 {
                    return Err("No subscriptions could be imported".to_string());
                }
                let summary = self.refresh()?;
                Ok(format!("Imported {count} subscription(s) ({summary})"))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn refresh_miniflux(
        &self,
        force: bool,
        on_background: Box<dyn FnOnce(Result<String, String>) + Send>,
    ) -> Result<String, String> {
        self.purge_foreign();
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
    fn uncategorized_is_real_category(&self) -> bool {
        // Miniflux only (Python providers/miniflux.py); the
        // GReader family uses the sentinel as "no folder".
        matches!(self.kind, HostedKind::Miniflux)
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
        // Recovered archive episodes merge into ordinary feed
        // reads (Python's hosted mixin,
        // `_merge_hosted_archive_articles`); the merge owns
        // the virtual-view exclusions (all/category/unread/
        // favorites views stay unmerged, `read:` resolves to
        // the real feed). Audit 8 Task 27, F02.
        if let Some(view) = feed_id {
            articles = crate::archive_store::merge_articles(
                &self.db_path,
                &crate::archive_store::scope_for_config(&self.cfg),
                view,
                articles,
            )?;
        }
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
    fn import_opml(&self, path: &Path, target_category: Option<&str>) -> Result<String, String> {
        let target = target_category.map(str::trim).filter(|t| !t.is_empty());
        match self.kind {
            HostedKind::Miniflux => self.import_opml_miniflux(path, target),
            HostedKind::GReader(_) => {
                // Python's base implementation: parse the
                // triples and add_feed each one with the
                // target category or its OPML category.
                let bytes =
                    std::fs::read(path).map_err(|e| format!("Could not read OPML file: {e}"))?;
                let triples = crate::opml::parse_opml(&bytes)?;
                let client = self.greader()?;
                let mut count = 0;
                for (_title, url, category) in &triples {
                    let cat = target.unwrap_or(category.as_str());
                    if client.subscribe_remote_in(url, Some(cat)).is_ok() {
                        count += 1;
                    }
                }
                if count == 0 {
                    return Err("No subscriptions could be imported".to_string());
                }
                let summary = self.refresh()?;
                Ok(format!("Imported {count} subscription(s) ({summary})"))
            }
        }
    }
    fn categories(&self) -> Result<Vec<String>, String> {
        match self.kind {
            HostedKind::Miniflux => Ok(self
                .miniflux()?
                .categories_remote()?
                .into_iter()
                .map(|(_, title)| title)
                .collect()),
            HostedKind::GReader(_) => {
                // The GReader family's inventory is the set of
                // labels its subscriptions carry.
                let subs = self.greader()?.subscriptions()?;
                let mut cats: Vec<String> = subs
                    .iter()
                    .filter_map(|s| s.category.clone())
                    .filter(|c| !c.trim().is_empty())
                    .collect();
                cats.sort();
                cats.dedup();
                Ok(cats)
            }
        }
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
    fn mark_category_read(&self, category: &str) -> Result<usize, String> {
        // Remote first (Python's providers mark on the server,
        // the cache follows): the server applies its own
        // category membership, then the cache applies the
        // displayed membership clause, so labeled-in articles
        // are marked on both sides (Audit 8 D23).
        match self.kind {
            HostedKind::Miniflux => self.miniflux()?.mark_category_read_remote(category)?,
            HostedKind::GReader(_) => self.greader()?.mark_category_read_remote(category)?,
        }
        self.db()?
            .mark_category_read(category)
            .map_err(|e| e.to_string())
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
                self.purge_foreign();
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
            inoreader_token_url: crate::oauth::TOKEN_URL.to_string(),
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
        _ => Box::new(LocalProvider {
            db_path,
            delete_behavior: cfg.get_str("delete_behavior", "deleted").to_string(),
        }),
    }
}

/// The active provider's sentinel capability (see
/// `Provider::uncategorized_is_real_category`), resolved from
/// config for call sites that hold no provider object.
pub fn uncategorized_is_real_category(cfg: &Config) -> bool {
    // Mirrors the HostedProvider override: Miniflux is the only
    // provider whose Uncategorized is a real category.
    active_name(cfg) == "miniflux"
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
    db_path_for_provider(cfg, base, &active_name(cfg))
}

/// The database file for one named provider ("local" = the
/// base file itself). Every hosted account gets its own
/// cache file, keyed by a hash of its credentials, so two
/// accounts' rows can never share a namespace; syncs and
/// views must open the file for the account they serve,
/// never another account's.
pub fn db_path_for_provider(cfg: &Config, base: &Path, name: &str) -> PathBuf {
    use sha2::Digest;
    let name = if PROVIDER_NAMES.contains(&name) {
        name
    } else {
        "local"
    };
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
    let section = cfg.raw().get("providers").and_then(|p| p.get(name));
    let cred = |key: &str| {
        section
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    let url = cred("url").trim_end_matches('/').trim_end_matches("/v1");
    // The stable account key: Inoreader's refresh token when
    // one is stored (Python's `_podcast_archive_account_identity`
    // prefers it for the same reason) — the access token
    // rotates on every refresh, so keying the cache file to it
    // would orphan the account's whole cache at the first
    // rotation once refresh became reachable (Audit 8 D07).
    // Sections without a refresh token keep the previous key.
    let token_key = if cred("refresh_token").is_empty() {
        cred("token")
    } else {
        cred("refresh_token")
    };
    let identity = serde_json::json!([
        name,
        url,
        cred("email").to_lowercase(),
        cred("api_key"),
        token_key
    ]);
    let hash: String = sha2::Sha256::digest(identity.to_string())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    base.with_file_name(format!("{stem}.hosted-{name}-{hash}.{extension}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// One-shot loopback token endpoint: answers the first
    /// request with the given status/body, records the raw
    /// request, then stops.
    fn token_server(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/oauth2/token", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut data = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let head_end = data
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4);
                if let Some(end) = head_end {
                    let head = String::from_utf8_lossy(&data[..end]).into_owned();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= end + len {
                        break;
                    }
                }
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => data.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            String::from_utf8_lossy(&data).into_owned()
        });
        (url, handle)
    }

    fn inoreader_setup(token_url: &str) -> (HostedProvider, PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("blindrss_provider_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.json");
        // An access token that expired long ago, a refresh
        // token, and the app credentials Python requires.
        std::fs::write(
            &config_path,
            r#"{"providers":{"inoreader":{"app_id":"app-id","app_key":"app-key","token":"old-access","refresh_token":"old-refresh","token_expires_at":1000}}}"#,
        )
        .unwrap();
        let cfg = Config::load(&config_path).unwrap();
        let provider = HostedProvider {
            name: "inoreader",
            kind: HostedKind::GReader(GReaderService::Inoreader),
            db_path: dir.join("rss.db"),
            cfg,
            inoreader_token_url: token_url.to_string(),
        };
        (provider, config_path, dir)
    }

    #[test]
    fn d07_expired_token_refreshes_and_persists_rotation() {
        let (url, server) = token_server(
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#,
        );
        let (provider, config_path, dir) = inoreader_setup(&url);
        let client = provider
            .inoreader_client(GReaderService::Inoreader)
            .unwrap();
        assert_eq!(client.oauth_token.as_deref(), Some("new-access"));
        let request = server.join().unwrap();
        assert!(request.contains("grant_type=refresh_token"), "{request}");
        assert!(request.contains("refresh_token=old-refresh"), "{request}");
        // The rotation reached the config file (Python's
        // _update_provider_config), tokens included.
        let saved = std::fs::read_to_string(&config_path).unwrap();
        assert!(saved.contains("new-access"), "{saved}");
        assert!(saved.contains("new-refresh"), "{saved}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn d07_revoked_grant_clears_tokens_and_disconnects() {
        let (url, server) = token_server(400, r#"{"error":"invalid_grant"}"#);
        let (provider, config_path, dir) = inoreader_setup(&url);
        let err = provider
            .inoreader_client(GReaderService::Inoreader)
            .err()
            .expect("revoked grant must fail the client build");
        assert_eq!(err, "Inoreader is not connected");
        let _ = server.join().unwrap();
        let saved = std::fs::read_to_string(&config_path).unwrap();
        assert!(!saved.contains("old-access"), "{saved}");
        assert!(!saved.contains("old-refresh"), "{saved}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn d07_transient_refresh_failure_keeps_stored_tokens() {
        let (url, server) = token_server(500, r#"{"error":"server_error"}"#);
        let (provider, config_path, dir) = inoreader_setup(&url);
        assert!(
            provider
                .inoreader_client(GReaderService::Inoreader)
                .is_err()
        );
        let _ = server.join().unwrap();
        let saved = std::fs::read_to_string(&config_path).unwrap();
        assert!(saved.contains("old-access"), "{saved}");
        assert!(saved.contains("old-refresh"), "{saved}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
