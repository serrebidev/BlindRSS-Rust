//! Python-compatible persistent archive state and hosted-account sidecar.
//!
//! The tables are Python's (`podcast_archive_state`,
//! `hosted_podcast_archive_state`, `hosted_podcast_archive_entries`)
//! and live in whichever database file the caller passes: the
//! local store for local feeds, the account's own cache file for
//! hosted feeds (Batch 39 isolation), with the Python provider
//! scope string scoping rows inside that file as well. Hosted
//! archive state must never be written into the local store or
//! another account's file/scope.

use crate::{Article, archive::ArchiveEpisode};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Wall-clock seconds for state timestamps (Python `time.time()`).
pub fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The `podcast_archive_*` config family (Python defaults from
/// `core/config.py`: enabled, 5000 max snapshots, 30 rescan days).
#[derive(Clone, Debug, PartialEq)]
pub struct ArchiveSettings {
    pub enabled: bool,
    pub max_snapshots: usize,
    pub rescan_days: f64,
    /// The global `delete_behavior` setting, carried here because
    /// archive recovery stores episodes through the same rule
    /// pipeline as a refresh (Audit 8 D12: rule deletes during a
    /// scan resolve through the configured behavior too).
    pub delete_behavior: String,
}

impl Default for ArchiveSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_snapshots: 5000,
            rescan_days: 30.0,
            delete_behavior: "deleted".to_string(),
        }
    }
}

impl ArchiveSettings {
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        // Python reads these through ConfigManager, whose defaults
        // are the config.py values; a stored 0 falls back to the
        // default exactly like Python's `or DEFAULT` reads.
        let days = cfg
            .raw()
            .get("podcast_archive_rescan_days")
            .and_then(|v| v.as_f64())
            .filter(|v| *v != 0.0)
            .unwrap_or(30.0);
        let max_snapshots = cfg
            .raw()
            .get("podcast_archive_max_snapshots")
            .and_then(|v| v.as_i64())
            .filter(|v| *v != 0)
            .unwrap_or(5000);
        Self {
            enabled: cfg.get_bool("podcast_archive_enabled", true),
            max_snapshots: max_snapshots.clamp(1, 5000) as usize,
            rescan_days: days,
            delete_behavior: cfg.get_str("delete_behavior", "deleted").to_string(),
        }
    }

    /// LocalProvider's interval: days clamped to at least one.
    pub fn local_rescan_seconds(&self) -> f64 {
        self.rescan_days.max(1.0) * 86400.0
    }

    /// The hosted mixin's interval: at least a day, no upper
    /// clamp on the configured days.
    pub fn hosted_rescan_seconds(&self) -> f64 {
        (self.rescan_days * 86400.0).max(86400.0)
    }
}

/// Python `HostedPodcastArchiveMixin._podcast_archive_scope`:
/// `{name.casefold()}:{sha256("{name}|{identity}")[:20]}`. The
/// name keeps its Python display case inside the digest
/// ("BazQux", "TheOldReader", "Inoreader"); the prefix is folded.
pub fn hosted_scope(provider_name: &str, account_identity: &str) -> String {
    use sha2::Digest;
    let digest: String = sha2::Sha256::digest(format!("{provider_name}|{account_identity}"))
        .iter()
        .take(10)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}:{digest}", provider_name.to_lowercase())
}

/// Miniflux overrides the scope shape entirely (Python
/// `MinifluxProvider._podcast_archive_scope`):
/// `miniflux:{sha256("{base_url}|{api_key}")[:20]}` with the base
/// URL lowercased and stripped of trailing slashes.
pub fn miniflux_scope(base_url: &str, api_key: &str) -> String {
    use sha2::Digest;
    let base = base_url.trim_end_matches('/').to_lowercase();
    let digest: String = sha2::Sha256::digest(format!("{base}|{api_key}"))
        .iter()
        .take(10)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("miniflux:{digest}")
}

/// The archive scope for the configured active provider, or the
/// empty (local) scope when the local provider is active.
/// Account identities follow Python's per-provider overrides:
/// GReader services use `{base_url}|{email}`; Inoreader uses its
/// refresh/access token (falling back to the app id key); when a
/// credential Python would use is not in config, the mixin's
/// default identity (the provider name itself) applies.
pub fn scope_for_config(cfg: &crate::config::Config) -> String {
    let name = crate::provider::active_name(cfg);
    let section = cfg.raw().get("providers").and_then(|p| p.get(&name));
    let cred = |key: &str| {
        section
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    match name.as_str() {
        "local" => String::new(),
        "miniflux" => {
            let url = cred("url");
            let url = if url.is_empty() {
                "https://localhost".to_string()
            } else {
                url
            };
            miniflux_scope(&url, &cred("api_key"))
        }
        "bazqux" => hosted_scope(
            "BazQux",
            &format!(
                "https://www.bazqux.com/reader/api/0|{}",
                cred("email").to_lowercase()
            ),
        ),
        "theoldreader" => hosted_scope(
            "TheOldReader",
            &format!(
                "https://theoldreader.com/reader/api/0|{}",
                cred("email").to_lowercase()
            ),
        ),
        "inoreader" => {
            let token = ["refresh_token", "token", "app_id"]
                .iter()
                .map(|key| cred(key))
                .find(|value| !value.is_empty())
                .unwrap_or_else(|| "Inoreader".to_string());
            hosted_scope(
                "Inoreader",
                &format!("https://www.inoreader.com/reader/api/0|{token}"),
            )
        }
        _ => hosted_scope(&name, &name),
    }
}

/// Python `podcast_source_url`: a GReader `feed/<url>` id encodes
/// its source URL; anything else falls back to the advertised URL.
pub fn podcast_source_url(feed_id: &str, advertised_url: &str) -> String {
    if let Some(encoded) = feed_id.trim().strip_prefix("feed/") {
        let encoded = encoded.trim();
        if encoded.to_lowercase().starts_with("http://")
            || encoded.to_lowercase().starts_with("https://")
        {
            return encoded.to_string();
        }
    }
    advertised_url.trim().to_string()
}

/// The due subset of `feeds` ((feed_id, feed_url) pairs) under
/// the claim rules, from one states read — the planning half of
/// Python's `_queue_podcast_archive_scans`. Feeds with no URL, or
/// whose state is still fresh / scanning / in error backoff, are
/// skipped. The queued job re-checks through the atomic claim,
/// so a stale plan can never double-run a scan.
pub fn due_archive_feeds(
    states: &[ArchiveState],
    feeds: &[(String, String)],
    rescan_seconds: f64,
    now: f64,
) -> Vec<(String, String)> {
    feeds
        .iter()
        .filter(|(feed_id, feed_url)| {
            if feed_id.is_empty() || feed_url.is_empty() {
                return false;
            }
            let state = states
                .iter()
                .find(|state| &state.feed_id == feed_id)
                .cloned()
                .unwrap_or_else(|| ArchiveState::never(feed_id));
            state.is_due(rescan_seconds, now)
        })
        .cloned()
        .collect()
}

/// In-process already-queued guard (Python's
/// `_podcast_archive_queued_ids`), keyed by database file +
/// scope + feed so two accounts never block each other.
fn queued_ids() -> &'static std::sync::Mutex<std::collections::HashSet<(String, String, String)>> {
    static QUEUED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashSet<(String, String, String)>>,
    > = std::sync::OnceLock::new();
    QUEUED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Queue automatic hosted scans for every due feed (Python
/// `_queue_podcast_archive_scans`): each job claims atomically
/// inside the job, then runs the hosted orchestration on the
/// serial archive worker. Returns how many jobs were queued.
pub fn queue_hosted_scans(
    path: &Path,
    scope: &str,
    settings: &ArchiveSettings,
    feeds: &[(String, String)],
) -> usize {
    if !settings.enabled {
        return 0;
    }
    let Ok(all_states) = states(path, scope) else {
        return 0;
    };
    let due = due_archive_feeds(
        &all_states,
        feeds,
        settings.hosted_rescan_seconds(),
        now_seconds(),
    );
    let mut queued = 0;
    for (feed_id, feed_url) in due {
        let key = (
            path.to_string_lossy().into_owned(),
            scope.to_string(),
            feed_id.clone(),
        );
        {
            let mut guard = queued_ids().lock().unwrap_or_else(|p| p.into_inner());
            if !guard.insert(key.clone()) {
                continue;
            }
        }
        queued += 1;
        let path = path.to_path_buf();
        let scope = scope.to_string();
        let settings = settings.clone();
        crate::archive::enqueue_archive_job(move || {
            let claimed = claim(
                &path,
                &scope,
                &feed_id,
                &feed_url,
                settings.hosted_rescan_seconds(),
                false,
                now_seconds(),
            )
            .unwrap_or(false);
            if claimed {
                crate::archive::run_hosted_scan(
                    &path,
                    &scope,
                    &feed_id,
                    &feed_url,
                    &settings,
                    &AtomicBool::new(false),
                    |_| {},
                );
            }
            queued_ids()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&key);
        });
    }
    queued
}

/// Queue one automatic local scan (Python's deferred archive
/// job): the claim is taken first, exactly as LocalProvider
/// claims after the refresh commit, and the job then runs the
/// scan already holding it. The refresh's just-downloaded
/// document travels with the job when the caller has it
/// (Python's does); `None` lets the scan fetch the document
/// itself, which is what the app passes — its parsed refresh
/// result carries no raw bytes. Returns false when the feed
/// was not due (or already queued).
pub fn queue_local_scan(
    path: &Path,
    feed: &crate::db::Feed,
    settings: &ArchiveSettings,
    current_document: Option<Vec<u8>>,
) -> bool {
    if !settings.enabled {
        return false;
    }
    let key = (
        path.to_string_lossy().into_owned(),
        String::new(),
        feed.id.clone(),
    );
    {
        let guard = queued_ids().lock().unwrap_or_else(|p| p.into_inner());
        if guard.contains(&key) {
            return false;
        }
    }
    let claimed = match claim(
        path,
        "",
        &feed.id,
        &feed.url,
        settings.local_rescan_seconds(),
        false,
        now_seconds(),
    ) {
        Ok(claimed) => claimed,
        Err(_) => return false,
    };
    if !claimed {
        return false;
    }
    queued_ids()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone());
    let path = path.to_path_buf();
    let feed = feed.clone();
    let settings = settings.clone();
    crate::archive::enqueue_archive_job(move || {
        let _ = crate::archive::perform_local_scan_with_fetch(
            &path,
            &feed,
            &settings,
            crate::archive::Claim::AlreadyHeld,
            current_document,
            &AtomicBool::new(false),
            |_| {},
            crate::archive::http_fetch,
        );
        queued_ids()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key);
    });
    true
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS podcast_archive_state (
 feed_id TEXT PRIMARY KEY, feed_url TEXT, status TEXT NOT NULL DEFAULT 'never',
 last_attempt REAL, last_success REAL, snapshot_count INTEGER DEFAULT 0,
 episode_count INTEGER DEFAULT 0, error TEXT,
 FOREIGN KEY(feed_id) REFERENCES feeds(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS hosted_podcast_archive_state (
 provider_scope TEXT NOT NULL, feed_id TEXT NOT NULL, feed_url TEXT,
 status TEXT NOT NULL DEFAULT 'never', last_attempt REAL, last_success REAL,
 snapshot_count INTEGER DEFAULT 0, episode_count INTEGER DEFAULT 0, error TEXT,
 PRIMARY KEY(provider_scope, feed_id));
CREATE TABLE IF NOT EXISTS hosted_podcast_archive_entries (
 provider_scope TEXT NOT NULL, feed_id TEXT NOT NULL, episode_key TEXT NOT NULL,
 title TEXT, url TEXT, content TEXT, date TEXT, author TEXT, media_url TEXT,
 media_type TEXT, PRIMARY KEY(provider_scope, feed_id, episode_key));
CREATE INDEX IF NOT EXISTS idx_hosted_podcast_archive_feed_date
 ON hosted_podcast_archive_entries(provider_scope, feed_id, date DESC);";

#[derive(Clone, Debug, PartialEq)]
pub struct ArchiveState {
    pub feed_id: String,
    pub feed_url: String,
    pub status: String,
    pub last_attempt: Option<f64>,
    pub last_success: Option<f64>,
    pub snapshot_count: usize,
    pub episode_count: usize,
    pub error: String,
}

impl ArchiveState {
    fn never(feed_id: &str) -> Self {
        Self {
            feed_id: feed_id.into(),
            feed_url: String::new(),
            status: "never".into(),
            last_attempt: None,
            last_success: None,
            snapshot_count: 0,
            episode_count: 0,
            error: String::new(),
        }
    }

    pub fn is_due(&self, rescan_seconds: f64, now: f64) -> bool {
        let rescan = rescan_seconds.max(0.0);
        if self.last_success.is_some_and(|at| now - at < rescan) {
            return false;
        }
        if self.status == "scanning" && self.last_attempt.is_some_and(|at| now - at < 6.0 * 3600.0)
        {
            return false;
        }
        if self.status == "error"
            && self
                .last_attempt
                .is_some_and(|at| now - at < rescan.clamp(3600.0, 86400.0))
        {
            return false;
        }
        true
    }
}

fn open(path: &Path) -> Result<Connection, String> {
    if !path.is_file() {
        return Err("Archive database does not exist".into());
    }
    let connection = Connection::open(path).map_err(|e| e.to_string())?;
    // Same connection setup as rss.db (Db::configure): archive
    // scans are claimed and queued from several threads while
    // refreshes run, so WAL + a 60-second busy timeout apply
    // here too — 5 seconds surfaced as locked-database errors.
    crate::db::Db::configure(&connection).map_err(|e| e.to_string())?;
    connection
        .execute_batch(SCHEMA)
        .map_err(|e| e.to_string())?;
    Ok(connection)
}

fn row_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArchiveState> {
    Ok(ArchiveState {
        feed_id: row.get(0)?,
        feed_url: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        status: row
            .get::<_, Option<String>>(2)?
            .unwrap_or_else(|| "never".into()),
        last_attempt: row.get(3)?,
        last_success: row.get(4)?,
        snapshot_count: row.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as usize,
        episode_count: row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as usize,
        error: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
    })
}

fn read_state(connection: &Connection, scope: &str, feed_id: &str) -> Result<ArchiveState, String> {
    let columns =
        "feed_id,feed_url,status,last_attempt,last_success,snapshot_count,episode_count,error";
    let state = if scope.is_empty() {
        connection.query_row(&format!("SELECT {columns} FROM podcast_archive_state WHERE feed_id=?1"), [feed_id], row_state)
    } else {
        connection.query_row(&format!("SELECT {columns} FROM hosted_podcast_archive_state WHERE provider_scope=?1 AND feed_id=?2"), params![scope,feed_id], row_state)
    }.optional().map_err(|e| e.to_string())?;
    Ok(state.unwrap_or_else(|| ArchiveState::never(feed_id)))
}

pub fn state(path: &Path, scope: &str, feed_id: &str) -> Result<ArchiveState, String> {
    read_state(&open(path)?, scope, feed_id)
}

pub fn states(path: &Path, scope: &str) -> Result<Vec<ArchiveState>, String> {
    let connection = open(path)?;
    let columns =
        "feed_id,feed_url,status,last_attempt,last_success,snapshot_count,episode_count,error";
    let sql = if scope.is_empty() {
        format!("SELECT {columns} FROM podcast_archive_state ORDER BY feed_id")
    } else {
        format!(
            "SELECT {columns} FROM hosted_podcast_archive_state WHERE provider_scope=?1 ORDER BY feed_id"
        )
    };
    let mut statement = connection.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = if scope.is_empty() {
        statement.query_map([], row_state)
    } else {
        statement.query_map([scope], row_state)
    }
    .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn claim(
    path: &Path,
    scope: &str,
    feed_id: &str,
    feed_url: &str,
    rescan_seconds: f64,
    force: bool,
    now: f64,
) -> Result<bool, String> {
    if !now.is_finite() || !rescan_seconds.is_finite() || feed_id.is_empty() {
        return Err("Invalid podcast archive claim".into());
    }
    let mut connection = open(path)?;
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if !force && !read_state(&transaction, scope, feed_id)?.is_due(rescan_seconds, now) {
        return Ok(false);
    }
    if scope.is_empty() {
        transaction.execute("INSERT INTO podcast_archive_state(feed_id,feed_url,status,last_attempt,error) VALUES(?1,?2,'scanning',?3,NULL) \
            ON CONFLICT(feed_id) DO UPDATE SET feed_url=excluded.feed_url,status='scanning',last_attempt=excluded.last_attempt,error=NULL", params![feed_id,feed_url,now])
    } else {
        transaction.execute("INSERT INTO hosted_podcast_archive_state(provider_scope,feed_id,feed_url,status,last_attempt,error) VALUES(?1,?2,?3,'scanning',?4,NULL) \
            ON CONFLICT(provider_scope,feed_id) DO UPDATE SET feed_url=excluded.feed_url,status='scanning',last_attempt=excluded.last_attempt,error=NULL", params![scope,feed_id,feed_url,now])
    }.map_err(|e| e.to_string())?;
    transaction.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub fn finish(
    path: &Path,
    scope: &str,
    feed_id: &str,
    snapshots: usize,
    episodes: usize,
    error: &str,
    now: f64,
) -> Result<(), String> {
    let connection = open(path)?;
    let table = if scope.is_empty() {
        "podcast_archive_state"
    } else {
        "hosted_podcast_archive_state"
    };
    let selector = if scope.is_empty() {
        "feed_id=?1"
    } else {
        "feed_id=?1 AND provider_scope=?2"
    };
    let sql = if error.trim().is_empty() {
        format!(
            "UPDATE {table} SET status='complete',last_success=?3,snapshot_count=?4,episode_count=?5,error=NULL WHERE {selector}"
        )
    } else {
        format!("UPDATE {table} SET status='error',error=?6 WHERE {selector}")
    };
    // Named binding permits the same update shape for local and hosted rows.
    let mut statement = connection.prepare(&sql).map_err(|e| e.to_string())?;
    statement
        .raw_bind_parameter(1, feed_id)
        .map_err(|e| e.to_string())?;
    if !scope.is_empty() {
        statement
            .raw_bind_parameter(2, scope)
            .map_err(|e| e.to_string())?;
    }
    if error.trim().is_empty() {
        statement
            .raw_bind_parameter(3, now)
            .map_err(|e| e.to_string())?;
        statement
            .raw_bind_parameter(4, snapshots as i64)
            .map_err(|e| e.to_string())?;
        statement
            .raw_bind_parameter(5, episodes as i64)
            .map_err(|e| e.to_string())?;
    } else {
        statement
            .raw_bind_parameter(6, error.trim())
            .map_err(|e| e.to_string())?;
    }
    statement.raw_execute().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn reset(path: &Path, scope: &str, feed_id: &str) -> Result<(), String> {
    let connection = open(path)?;
    if scope.is_empty() {
        connection.execute(
            "DELETE FROM podcast_archive_state WHERE feed_id=?1",
            [feed_id],
        )
    } else {
        connection.execute(
            "DELETE FROM hosted_podcast_archive_state WHERE provider_scope=?1 AND feed_id=?2",
            params![scope, feed_id],
        )
    }
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn replace_entries(
    path: &Path,
    scope: &str,
    feed_id: &str,
    episodes: &[ArchiveEpisode],
) -> Result<(), String> {
    let mut connection = open(path)?;
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    transaction
        .execute(
            "DELETE FROM hosted_podcast_archive_entries WHERE provider_scope=?1 AND feed_id=?2",
            params![scope, feed_id],
        )
        .map_err(|e| e.to_string())?;
    for episode in episodes
        .iter()
        .filter(|e| e.entry.media_url.as_deref().is_some_and(|m| !m.is_empty()))
    {
        let entry = &episode.entry;
        transaction.execute("INSERT INTO hosted_podcast_archive_entries(provider_scope,feed_id,episode_key,title,url,content,date,author,media_url,media_type) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![scope,feed_id,episode.key(),entry.title,entry.url,entry.content_text,entry.date,entry.author,entry.media_url,entry.media_type]).map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())
}

pub fn entries(path: &Path, scope: &str, feed_id: &str) -> Result<Vec<Article>, String> {
    let connection = open(path)?;
    // Python's order: date DESC, title ascending case-insensitively.
    let mut statement = connection.prepare("SELECT episode_key,title,url,content,date,author,media_url,media_type FROM hosted_podcast_archive_entries \
        WHERE provider_scope=?1 AND feed_id=?2 ORDER BY date DESC, title COLLATE NOCASE").map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(params![scope, feed_id], |row| {
            Ok(Article {
                id: format!("podcast-archive:{}", row.get::<_, String>(0)?),
                feed_id: feed_id.into(),
                title: row
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| "Untitled episode".into()),
                url: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                content: row.get(3)?,
                description: row.get(3)?,
                date: row.get(4)?,
                author: row.get(5)?,
                media_url: row.get(6)?,
                media_type: row.get(7)?,
                is_read: true,
                is_favorite: false,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

/// Whether any recovered episodes are stored for (scope,
/// feed) — the cheap existence check behind the main window's
/// merged feed view (Audit 8 Task 27, F02), so ordinary views
/// never pay for a merge that cannot add rows.
pub fn has_entries(path: &Path, scope: &str, feed_id: &str) -> Result<bool, String> {
    let connection = open(path)?;
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM hosted_podcast_archive_entries WHERE provider_scope=?1 AND feed_id=?2",
            params![scope, feed_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(count > 0)
}

pub fn merge_articles(
    path: &Path,
    scope: &str,
    view_id: &str,
    mut articles: Vec<Article>,
) -> Result<Vec<Article>, String> {
    if scope.is_empty()
        || view_id.is_empty()
        || view_id == "all"
        || view_id.starts_with("category:")
        || ["unread:", "favorites:", "fav:", "starred:"]
            .iter()
            .any(|prefix| view_id.starts_with(prefix))
    {
        return Ok(articles);
    }
    let feed_id = view_id.strip_prefix("read:").unwrap_or(view_id);
    let mut live = std::collections::HashSet::new();
    for article in &articles {
        live.extend(crate::archive::article_identity_keys(article));
    }
    for article in entries(path, scope, feed_id)? {
        let keys = crate::archive::article_identity_keys(&article);
        if keys.is_disjoint(&live) {
            articles.push(article);
            live.extend(keys);
        }
    }
    articles.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| b.title.to_lowercase().cmp(&a.title.to_lowercase()))
    });
    Ok(articles)
}
