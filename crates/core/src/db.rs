//! Data layer over the existing BlindRSS `rss.db`.
//!
//! Schema is owned by Python `core/db.py`; this module only reads and
//! writes rows in that schema. No tables are created or altered here
//! (additive migrations, if ever needed, are a separate deliberate step).

use rusqlite::{Connection, params};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    pub id: String,
    pub url: String,
    pub title: String,
    pub title_is_custom: bool,
    pub category: Option<String>,
    pub icon_url: Option<String>,
}

/// One hosted-media cache row: (media_url, media_type,
/// checked_at epoch); NULL media is a recorded miss.
pub type HostedMediaCache = (Option<String>, Option<String>, i64);

/// One chapter mark (chapters table; start in seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub title: Option<String>,
    pub href: Option<String>,
}

/// Saved playback position (playback_state table).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlaybackState {
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub completed: bool,
    pub seek_supported: Option<bool>,
    pub title: Option<String>,
}

pub struct Category {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
}

/// Python `core/db.py` CATEGORY_PATH_SEP: a category's identity
/// is its full path (`Parent / Child`) in storage; only the leaf
/// is displayed. Two subcategories sharing a leaf name under
/// different parents must not collide (Python issue #27).
pub const CATEGORY_PATH_SEP: &str = " / ";

/// Join a parent category path and a leaf title into a full
/// category path (Python `make_category_path`).
pub fn make_category_path(parent: Option<&str>, leaf: &str) -> String {
    let leaf = leaf.trim();
    match parent.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("{p}{CATEGORY_PATH_SEP}{leaf}"),
        None => leaf.to_string(),
    }
}

/// The leaf (last path segment) of a category path, for display
/// (Python `category_display_leaf`).
pub fn category_display_leaf(path: &str) -> String {
    match path.rsplit_once(CATEGORY_PATH_SEP) {
        Some((_, leaf)) => leaf.to_string(),
        None => path.to_string(),
    }
}

/// The parent path of a category path, if any.
pub fn category_parent_path(path: &str) -> Option<String> {
    path.rsplit_once(CATEGORY_PATH_SEP)
        .map(|(parent, _)| parent.to_string())
}

/// Strip the path separator out of a user-entered leaf name so it
/// cannot corrupt the path encoding (Python
/// `sanitize_category_leaf`).
pub fn sanitize_category_leaf(leaf: &str) -> String {
    leaf.trim().replace(CATEGORY_PATH_SEP, " - ")
}

/// The (old, new) path mapping for a subtree rooted at `old`
/// when the root moves to `new`: the root itself plus every
/// descendant, tails preserved.
pub fn category_subtree_mapping(titles: &[String], old: &str, new: &str) -> Vec<(String, String)> {
    let prefix = format!("{old}{CATEGORY_PATH_SEP}");
    titles
        .iter()
        .filter(|t| t.as_str() == old || t.starts_with(&prefix))
        .map(|t| (t.clone(), format!("{new}{}", &t[old.len()..])))
        .collect()
}

/// Rewrite a `delete_behavior` config value (`category:<path>`)
/// through a category path mapping (Python
/// `LocalProvider._update_category_delete_behavior`). Returns
/// the new value when the target category was remapped.
pub fn remap_delete_behavior(behavior: &str, mapping: &[(String, String)]) -> Option<String> {
    let target = behavior.trim().strip_prefix("category:")?.trim();
    mapping
        .iter()
        .find(|(old, _)| old == target)
        .map(|(_, new)| format!("category:{new}"))
}

/// One recorded change-history version of an article
/// (Python `article_versions` row, newest first when listed).
#[derive(Debug, Clone, PartialEq)]
pub struct ArticleVersion {
    pub captured_at: f64,
    pub title: String,
    pub content: String,
}

/// Publisher-owned fields of a stored article (see
/// `Db::existing_article_meta`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExistingArticleMeta {
    pub date: Option<String>,
    pub description: Option<String>,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
    pub chapter_url: Option<String>,
    pub url: String,
}

/// One feed's recorded refresh failure (Python issue #32 shape).
#[derive(Debug, Clone, PartialEq)]
pub struct FeedError {
    pub feed_id: String,
    pub title: String,
    pub url: String,
    pub category: Option<String>,
    pub error: String,
    pub error_at: Option<f64>,
    pub success_at: Option<f64>,
    pub consecutive_failures: i64,
}

/// The Python schema for every table this crate uses, with the
/// later Python migration columns inline. A fresh install starts
/// here; an existing Python database already has all of it.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS feeds (id TEXT PRIMARY KEY, url TEXT, title TEXT,
    title_is_custom INTEGER DEFAULT 0, category TEXT, icon_url TEXT,
    last_error TEXT, last_error_at REAL, last_success_at REAL,
    consecutive_failures INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS rust_feed_http_cache (
    url TEXT PRIMARY KEY, etag TEXT, modified TEXT, body BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS articles (id TEXT PRIMARY KEY, feed_id TEXT,
    title TEXT, url TEXT, content TEXT, description TEXT, date TEXT,
    author TEXT, is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0,
    media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL,
    category_override TEXT);
CREATE TABLE IF NOT EXISTS categories (id TEXT PRIMARY KEY,
    title TEXT UNIQUE, parent_id TEXT);
CREATE TABLE IF NOT EXISTS deleted_articles (feed_id TEXT NOT NULL,
    article_id TEXT NOT NULL, url TEXT, deleted_at REAL NOT NULL,
    title TEXT, content TEXT, description TEXT, date TEXT, author TEXT,
    media_url TEXT, media_type TEXT, chapter_url TEXT, is_read INTEGER,
    is_favorite INTEGER, purged INTEGER DEFAULT 0,
    PRIMARY KEY (feed_id, article_id));
CREATE TABLE IF NOT EXISTS chapters (id TEXT PRIMARY KEY,
    article_id TEXT, start REAL, title TEXT, href TEXT);
CREATE TABLE IF NOT EXISTS playback_state (id TEXT PRIMARY KEY,
    position_ms INTEGER, duration_ms INTEGER, updated_at INTEGER,
    completed INTEGER, seek_supported INTEGER, title TEXT);
CREATE TABLE IF NOT EXISTS smart_folders (id TEXT PRIMARY KEY,
    name TEXT, rule_json TEXT, position INTEGER);
CREATE TABLE IF NOT EXISTS filter_rules (id TEXT PRIMARY KEY,
    name TEXT, rule_json TEXT, actions_json TEXT, enabled INTEGER,
    stop INTEGER, position INTEGER);
CREATE TABLE IF NOT EXISTS article_labels (article_id TEXT,
    category TEXT, PRIMARY KEY (article_id, category));
CREATE TABLE IF NOT EXISTS article_versions (id INTEGER PRIMARY KEY
    AUTOINCREMENT, article_id TEXT NOT NULL, captured_at REAL NOT NULL,
    content_hash TEXT NOT NULL, title TEXT, content TEXT);
CREATE INDEX IF NOT EXISTS idx_article_versions_article
    ON article_versions (article_id, captured_at);
CREATE TABLE IF NOT EXISTS youtube_recommendations (
    account TEXT NOT NULL, video_id TEXT NOT NULL,
    title TEXT NOT NULL, author TEXT NOT NULL, date TEXT NOT NULL,
    position INTEGER NOT NULL, current INTEGER NOT NULL DEFAULT 1,
    is_read INTEGER NOT NULL DEFAULT 0,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account, video_id));
CREATE TABLE IF NOT EXISTS youtube_history_state (
    feed_id TEXT PRIMARY KEY, done INTEGER NOT NULL DEFAULT 0,
    last_attempt REAL,
    FOREIGN KEY(feed_id) REFERENCES feeds(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS hosted_article_media (
    cache_key TEXT PRIMARY KEY, media_url TEXT, media_type TEXT,
    checked_at INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS idx_articles_feed_id ON articles (feed_id);
CREATE INDEX IF NOT EXISTS idx_articles_is_read ON articles (is_read);
CREATE INDEX IF NOT EXISTS idx_articles_date ON articles (date);
CREATE INDEX IF NOT EXISTS idx_articles_is_read_feed_id
    ON articles (is_read, feed_id);
CREATE INDEX IF NOT EXISTS idx_articles_date_id ON articles (date, id);
CREATE INDEX IF NOT EXISTS idx_articles_feed_id_date_id
    ON articles (feed_id, date, id);
CREATE INDEX IF NOT EXISTS idx_deleted_articles_feed_id
    ON deleted_articles (feed_id);
CREATE INDEX IF NOT EXISTS idx_deleted_articles_feed_url
    ON deleted_articles (feed_id, url);
CREATE INDEX IF NOT EXISTS idx_chapters_article_id_start
    ON chapters (article_id, start);
";

/// Columns Python added to existing tables by migration; older
/// databases get them on open.
const MIGRATION_COLUMNS: [(&str, &str, &str); 13] = [
    ("feeds", "last_error", "TEXT"),
    ("feeds", "last_error_at", "REAL"),
    ("feeds", "last_success_at", "REAL"),
    ("feeds", "consecutive_failures", "INTEGER DEFAULT 0"),
    ("categories", "parent_id", "TEXT"),
    ("articles", "category_override", "TEXT"),
    ("feeds", "delete_behavior", "TEXT"),
    ("deleted_articles", "purged", "INTEGER DEFAULT 0"),
    ("deleted_articles", "is_read", "INTEGER"),
    ("deleted_articles", "is_favorite", "INTEGER"),
    ("articles", "tags", "TEXT"),
    ("feeds", "feed_settings", "TEXT"),
    ("feeds", "show_images", "INTEGER"),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Article {
    pub id: String,
    pub feed_id: String,
    pub title: String,
    pub url: String,
    pub content: Option<String>,
    pub description: Option<String>,
    pub date: Option<String>,
    pub author: Option<String>,
    pub is_read: bool,
    pub is_favorite: bool,
    pub media_url: Option<String>,
    pub media_type: Option<String>,
}

/// ISO-8601 UTC stamp ("2026-10-31T09:00:00.000000+00:00"),
/// matching Python's datetime.isoformat for aware UTC values.
fn iso_utc(stamp: std::time::Duration) -> String {
    let secs = stamp.as_secs() as i64;
    let micros = stamp.subsec_micros();
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}+00:00",
        tod / 3600,
        (tod / 60) % 60,
        tod % 60
    )
}

pub struct Db {
    conn: Connection,
}

pub type CachedFeed = (Option<String>, Option<String>, Vec<u8>);

/// The shared row shape of the article list queries:
/// (id, feed_id, title, url, content, description, date, author,
/// is_read, is_favorite, media_url, media_type). `content` may be
/// SQL NULL in the light list variants, hence the Option field.
fn article_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Article> {
    Ok(Article {
        id: r.get(0)?,
        feed_id: r.get(1)?,
        title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        url: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        content: r.get(4)?,
        description: r.get(5)?,
        date: r.get(6)?,
        author: r.get(7)?,
        is_read: r.get::<_, i64>(8)? != 0,
        is_favorite: r.get::<_, i64>(9)? != 0,
        media_url: r.get(10)?,
        media_type: r.get(11)?,
    })
}

impl Db {
    pub fn cached_feed(&self, url: &str) -> rusqlite::Result<Option<CachedFeed>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT etag, modified, body FROM rust_feed_http_cache WHERE url = ?1",
                [url],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    pub fn cache_feed(
        &self,
        url: &str,
        etag: Option<&str>,
        modified: Option<&str>,
        body: &[u8],
    ) -> rusqlite::Result<()> {
        self.conn.execute("INSERT INTO rust_feed_http_cache (url, etag, modified, body) VALUES (?1, ?2, ?3, ?4) \
            ON CONFLICT(url) DO UPDATE SET etag = excluded.etag, modified = excluded.modified, body = excluded.body",
            params![url, etag, modified, body])?;
        Ok(())
    }
    /// Open an existing BlindRSS database. Fails if the file is missing:
    /// silently creating an empty database would hide a wrong path and
    /// look like data loss to the user.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if !path.exists() {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(format!("database not found: {}", path.display())),
            ));
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Open the database, creating and initialising it when the
    /// file does not exist yet (Python first-run behaviour — a
    /// brand-new user starts with an empty, valid library).
    pub fn open_or_create(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> rusqlite::Result<()> {
        // Serialize schema init across connections. The
        // migration loop below checks table_info and then
        // ALTERs; outside a write transaction two openers of
        // a not-yet-migrated file both see a column missing
        // and both add it ("duplicate column name" — the
        // v0.1.30 tag-CI failure, and a real upgrade-day
        // crash risk when the app and a worker open the
        // database together). BEGIN IMMEDIATE takes the write
        // lock up front; busy_timeout makes the other opener
        // wait instead of erroring.
        self.conn.execute_batch("PRAGMA busy_timeout=5000;")?;
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = self.init_schema_locked();
        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn init_schema_locked(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch(SCHEMA)?;
        // Early Rust builds created youtube_history_state with
        // (state, attempted_at) columns where Python — the table's
        // owner in dual-run — uses (done, last_attempt); a database
        // created by one app broke the other's history queries.
        // Rename the legacy columns in place (same transaction as
        // every other schema change here); the values carry over:
        // state 2 already meant done, state 1 an attempt.
        {
            let mut stmt = self
                .conn
                .prepare("PRAGMA table_info(youtube_history_state)")?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            if cols.iter().any(|c| c == "state") && !cols.iter().any(|c| c == "done") {
                self.conn.execute_batch(
                    "ALTER TABLE youtube_history_state RENAME COLUMN state TO done;",
                )?;
            }
            if cols.iter().any(|c| c == "attempted_at") && !cols.iter().any(|c| c == "last_attempt")
            {
                self.conn.execute_batch(
                    "ALTER TABLE youtube_history_state RENAME COLUMN attempted_at TO last_attempt;",
                )?;
            }
        }
        for (table, column, decl) in MIGRATION_COLUMNS {
            let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            if !cols.iter().any(|c| c == column) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
            }
        }
        Ok(())
    }

    pub fn categories(&self) -> rusqlite::Result<Vec<Category>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, parent_id FROM categories ORDER BY title")?;
        let rows = stmt.query_map([], |r| {
            Ok(Category {
                id: r.get(0)?,
                title: r.get(1)?,
                parent_id: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn feeds(&self) -> rusqlite::Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, url, title, title_is_custom, category, icon_url \
             FROM feeds ORDER BY title",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Feed {
                id: r.get(0)?,
                url: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                title_is_custom: r.get::<_, i64>(3)? != 0,
                category: r.get(4)?,
                icon_url: r.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Articles for one feed, newest first (Python orders by date in its
    /// list views). `feed_id == None` returns articles across all feeds.
    /// Replace the visible recommendations snapshot atomically,
    /// retaining read/favorite/deleted state (Python's
    /// save_recommendations). `videos` is (video_id, title, author)
    /// in YouTube's order; dates are stamped now minus position
    /// microseconds, exactly as Python does.
    pub fn save_recommendations(
        &self,
        account: &str,
        videos: &[(String, String, String)],
    ) -> rusqlite::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE youtube_recommendations SET current = 0 WHERE account = ?1",
            [account],
        )?;
        for (position, (video_id, title, author)) in videos.iter().enumerate() {
            let stamp = now - std::time::Duration::from_micros(position as u64);
            tx.execute(
                "INSERT INTO youtube_recommendations (account, video_id, title, author, date, position) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(account, video_id) DO UPDATE SET \
                 title = excluded.title, author = excluded.author, \
                 date = excluded.date, position = excluded.position, current = 1",
                rusqlite::params![
                    account,
                    video_id,
                    title,
                    author,
                    iso_utc(stamp),
                    position as i64
                ],
            )?;
        }
        tx.commit()
    }

    /// The account's current recommendations as articles in the
    /// virtual feed (Python's YouTubeAccountProvider._articles).
    pub fn recommendations(&self, account: &str) -> rusqlite::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT video_id, title, author, date, is_read, is_favorite \
             FROM youtube_recommendations \
             WHERE account = ?1 AND current = 1 AND is_deleted = 0 \
             ORDER BY current DESC, position, date DESC",
        )?;
        let rows = stmt.query_map([account], |row| {
            let video_id: String = row.get(0)?;
            let url = format!("https://www.youtube.com/watch?v={video_id}");
            Ok(Article {
                id: format!(
                    "{}:{account}:{video_id}",
                    crate::youtube_account::RECOMMENDATIONS_FEED_ID
                ),
                feed_id: crate::youtube_account::RECOMMENDATIONS_FEED_ID.to_string(),
                title: row.get(1)?,
                url: url.clone(),
                content: None,
                description: None,
                date: row.get(3)?,
                author: row.get(2)?,
                is_read: row.get::<_, i64>(4)? != 0,
                is_favorite: row.get::<_, i64>(5)? != 0,
                media_url: Some(url),
                media_type: Some("video/youtube".to_string()),
            })
        })?;
        rows.collect()
    }

    /// Soft-delete a recommendations article (Python's provider
    /// wrapper sets is_deleted; the row's state survives for a
    /// later snapshot).
    pub fn delete_recommendation(&self, article_id: &str) -> rusqlite::Result<bool> {
        let Some(rest) = article_id.strip_prefix("youtube-account:recommendations:") else {
            return Ok(false);
        };
        let Some((account, video_id)) = rest.split_once(':') else {
            return Ok(false);
        };
        let n = self.conn.execute(
            "UPDATE youtube_recommendations SET is_deleted = 1 WHERE account = ?1 AND video_id = ?2",
            rusqlite::params![account, video_id],
        )?;
        Ok(n > 0)
    }

    /// Read/favorite state for a recommendations article id
    /// ("youtube-account:recommendations:<account>:<video_id>").
    pub fn set_recommendation_state(
        &self,
        article_id: &str,
        read: Option<bool>,
        favorite: Option<bool>,
    ) -> rusqlite::Result<bool> {
        let Some(rest) = article_id.strip_prefix("youtube-account:recommendations:") else {
            return Ok(false);
        };
        let Some((account, video_id)) = rest.split_once(':') else {
            return Ok(false);
        };
        if let Some(read) = read {
            self.conn.execute(
                "UPDATE youtube_recommendations SET is_read = ?1 WHERE account = ?2 AND video_id = ?3",
                rusqlite::params![read as i64, account, video_id],
            )?;
        }
        if let Some(favorite) = favorite {
            self.conn.execute(
                "UPDATE youtube_recommendations SET is_favorite = ?1 WHERE account = ?2 AND video_id = ?3",
                rusqlite::params![favorite as i64, account, video_id],
            )?;
        }
        Ok(true)
    }

    pub fn articles(&self, feed_id: Option<&str>, limit: usize) -> rusqlite::Result<Vec<Article>> {
        self.articles_impl(feed_id, limit, 0, false)
    }

    /// List rows WITHOUT article bodies (`content` is `None`).
    /// The article list never displays bodies, and podcast episode
    /// bodies are the largest text in the library: hauling 300-500
    /// of them per refill was the podcast list lag (2026-11-02
    /// perf audit). Callers that need a body load it on demand
    /// through `article_by_id`. Bulk consumers that process bodies
    /// (smart folders, refresh, providers) keep `articles`.
    pub fn articles_light(
        &self,
        feed_id: Option<&str>,
        limit: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        self.articles_impl(feed_id, limit, 0, true)
    }

    /// One page of full rows (bodies included) in the same
    /// date-DESC, id-DESC order as `articles`. Bulk rule
    /// evaluation pages through the whole library with this
    /// instead of loading it under a row ceiling (Audit 8 D21).
    pub fn articles_page(
        &self,
        feed_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        self.articles_impl(feed_id, limit, offset, false)
    }

    /// One page of light list rows (Audit 7 Finding A): the
    /// article list pages through a view 400 at a time behind
    /// Python's "Load more items (Enter)" placeholder instead of
    /// hard-capping the view. Pages are windows over the same
    /// date-DESC, id-DESC order as `articles_light`.
    pub fn articles_light_page(
        &self,
        feed_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        self.articles_impl(feed_id, limit, offset, true)
    }

    /// Total rows a feed (or the whole library) view can page
    /// through; pairs with `articles_light_page`.
    pub fn articles_count(&self, feed_id: Option<&str>) -> rusqlite::Result<i64> {
        match feed_id {
            Some(feed) => self.conn.query_row(
                "SELECT COUNT(*) FROM articles WHERE feed_id = ?1",
                params![feed],
                |r| r.get(0),
            ),
            None => self
                .conn
                .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0)),
        }
    }

    fn articles_impl(
        &self,
        feed_id: Option<&str>,
        limit: usize,
        offset: usize,
        light: bool,
    ) -> rusqlite::Result<Vec<Article>> {
        let content_col = if light { "NULL" } else { "content" };
        // Two shapes, not one parameterised OR: `(?1 IS NULL OR
        // feed_id = ?1)` defeats the feed index even when it
        // exists (the planner falls back to a date-order scan and
        // filters row by row). The direct shapes use
        // idx_articles_feed_id_date_id / idx_articles_date_id.
        let sql = match feed_id {
            Some(_) => format!(
                "SELECT id, feed_id, title, url, {content_col}, description, date, \
                 author, is_read, is_favorite, media_url, media_type \
                 FROM articles WHERE feed_id = ?1 \
                 ORDER BY date DESC, id DESC LIMIT ?2 OFFSET ?3"
            ),
            None => format!(
                "SELECT id, feed_id, title, url, {content_col}, description, date, \
                 author, is_read, is_favorite, media_url, media_type \
                 FROM articles \
                 ORDER BY date DESC, id DESC LIMIT ?1 OFFSET ?2"
            ),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        match feed_id {
            Some(feed) => {
                let rows =
                    stmt.query_map(params![feed, limit as i64, offset as i64], article_row)?;
                rows.collect()
            }
            None => {
                let rows = stmt.query_map(params![limit as i64, offset as i64], article_row)?;
                rows.collect()
            }
        }
    }

    pub fn unread_count(&self, feed_id: Option<&str>) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE is_read = 0 \
             AND (?1 IS NULL OR feed_id = ?1)",
            params![feed_id],
            |r| r.get(0),
        )
    }

    /// Unread counts for every feed in ONE grouped query
    /// (Python `providers/local.py` does the same). The tree
    /// build used to run `unread_count` per feed — 2-3 times per
    /// feed per rebuild once category totals and labels are
    /// counted — which on an index-less database was the ~30 s
    /// startup (2026-11-02 perf audit).
    pub fn unread_counts(&self) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT feed_id, COUNT(*) FROM articles WHERE is_read = 0 GROUP BY feed_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect()
    }

    pub fn article_by_id(&self, article_id: &str) -> rusqlite::Result<Option<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, feed_id, title, url, content, description, date, author, \
             is_read, is_favorite, media_url, media_type \
             FROM articles WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![article_id], |row| {
            Ok(Article {
                id: row.get(0)?,
                feed_id: row.get(1)?,
                title: row.get(2)?,
                url: row.get(3)?,
                content: row.get(4)?,
                description: row.get(5)?,
                date: row.get(6)?,
                author: row.get(7)?,
                is_read: row.get::<_, i64>(8)? != 0,
                is_favorite: row.get::<_, i64>(9)? != 0,
                media_url: row.get(10)?,
                media_type: row.get(11)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Attach media found after the fact (page detection at play
    /// time), mirroring Python's provider.update_article_media.
    pub fn update_article_media(
        &self,
        article_id: &str,
        url: &str,
        media_type: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE articles SET media_url = ?1, media_type = ?2 WHERE id = ?3",
            params![url, media_type, article_id],
        )?;
        Ok(())
    }

    /// Total stored articles for one feed (listing caps split on
    /// initial vs refresh by whether anything is stored yet).
    pub fn article_count_for_feed(&self, feed_id: &str) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE feed_id = ?1",
            params![feed_id],
            |row| row.get(0),
        )
    }

    /// YouTube channel history-backfill state for a feed, in the
    /// Python table's own columns (`done`, `last_attempt` epoch):
    /// done 2 = history listed (Python `db._YOUTUBE_HISTORY_DONE`);
    /// done 1 = the old Videos-tab listing, which missed live
    /// streams and must list again; done 0 = not done, with
    /// `last_attempt` gating the retry. None when never attempted.
    pub fn youtube_history_state(&self, feed_id: &str) -> rusqlite::Result<Option<(i64, f64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT done, last_attempt FROM youtube_history_state WHERE feed_id = ?1")?;
        let mut rows = stmt.query_map(params![feed_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            ))
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Python `youtube_history_due`: a channel feed needs its
    /// one-time history listing when it was never attempted, when
    /// its recorded listing predates the uploads-playlist change
    /// (done = 1), or when the last failed attempt is a day old.
    pub fn youtube_history_due(&self, feed_id: &str, now_epoch: f64) -> bool {
        match self.youtube_history_state(feed_id) {
            Ok(None) => true,
            Ok(Some((done, last_attempt))) => {
                if done >= 2 {
                    false
                } else if done == 1 {
                    true
                } else {
                    now_epoch - last_attempt >= 86_400.0
                }
            }
            Err(_) => false,
        }
    }

    pub fn set_youtube_history_state(
        &self,
        feed_id: &str,
        done: i64,
        last_attempt: f64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO youtube_history_state (feed_id, done, last_attempt) \
             VALUES (?1, ?2, ?3) ON CONFLICT(feed_id) DO UPDATE SET \
             done = excluded.done, last_attempt = excluded.last_attempt",
            params![feed_id, done, last_attempt],
        )?;
        Ok(())
    }

    /// Hosted-provider media resolved client-side (NPR program
    /// feeds carry no enclosure; Python's HostedArticleMediaMixin).
    /// Returns (media_url, media_type, checked_at epoch) when a row
    /// exists — a row with NULL media is a recorded miss.
    pub fn hosted_article_media(
        &self,
        cache_key: &str,
    ) -> rusqlite::Result<Option<HostedMediaCache>> {
        let mut stmt = self.conn.prepare(
            "SELECT media_url, media_type, checked_at FROM hosted_article_media \
             WHERE cache_key = ?1",
        )?;
        let mut rows = stmt.query_map(params![cache_key], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn set_hosted_article_media(
        &self,
        cache_key: &str,
        media_url: Option<&str>,
        media_type: Option<&str>,
        checked_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO hosted_article_media (cache_key, media_url, media_type, checked_at) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(cache_key) DO UPDATE SET \
             media_url = excluded.media_url, media_type = excluded.media_type, \
             checked_at = excluded.checked_at",
            params![cache_key, media_url, media_type, checked_at],
        )?;
        Ok(())
    }

    pub fn mark_read(&self, article_id: &str, read: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE articles SET is_read = ?1 WHERE id = ?2",
            params![read as i64, article_id],
        )?;
        Ok(())
    }

    pub fn set_favorite(&self, article_id: &str, favorite: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE articles SET is_favorite = ?1 WHERE id = ?2",
            params![favorite as i64, article_id],
        )?;
        Ok(())
    }

    /// Add a feed with a fresh uuid id, as Python's add_feed does.
    /// Returns the stored feed (with its generated id).
    pub fn add_feed(
        &self,
        url: &str,
        title: &str,
        category: Option<&str>,
    ) -> rusqlite::Result<Feed> {
        let feed = Feed {
            id: uuid::Uuid::new_v4().to_string(),
            url: url.to_string(),
            title: title.to_string(),
            title_is_custom: false,
            category: category.map(str::to_string),
            icon_url: None,
        };
        self.conn.execute(
            "INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
             VALUES (?1, ?2, ?3, 0, ?4, NULL)",
            params![feed.id, feed.url, feed.title, feed.category],
        )?;
        Ok(feed)
    }

    /// Insert a feed with a caller-chosen id if missing (hosted
    /// providers key feeds by their remote stream/entry ids).
    pub fn insert_feed_if_missing(&self, feed: &Feed) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO feeds (id, url, title, title_is_custom, category, icon_url) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                feed.id,
                feed.url,
                feed.title,
                feed.title_is_custom as i64,
                feed.category,
                feed.icon_url
            ],
        )?;
        Ok(())
    }

    /// Refresh server-owned feed metadata while keeping a user's custom title.
    pub fn upsert_hosted_feed(&self, feed: &Feed) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(id) DO UPDATE SET \
             url = excluded.url, category = excluded.category, \
             title = CASE WHEN feeds.title_is_custom != 0 THEN feeds.title ELSE excluded.title END, \
             icon_url = COALESCE(excluded.icon_url, feeds.icon_url)",
            params![feed.id, feed.url, feed.title, feed.title_is_custom as i64, feed.category, feed.icon_url],
        )?;
        Ok(())
    }

    /// Hosted-cache row shapes: GReader feeds carry their
    /// stream id (`feed/<url>`), Miniflux feeds are
    /// `miniflux:<server id>`, Miniflux articles are
    /// `miniflux-entry:<entry id>`. Local feeds use UUID ids,
    /// so these shapes never belong to local data. Older syncs
    /// wrote every configured account into whichever database
    /// was active; each hosted account now has its own cache
    /// file (see provider::db_path_for_provider) and its
    /// server is the source of truth, so these rows are safe
    /// to remove wherever they do not belong.
    ///
    /// Removes every hosted-shaped feed and article (plus
    /// dependent chapter/playback/label/version rows).
    /// Returns the number of feeds removed.
    pub fn purge_hosted_rows(&self) -> rusqlite::Result<usize> {
        self.purge_hosted_rows_inner(None)
    }

    /// Like `purge_hosted_rows`, but keeps the hosted feeds
    /// whose ids start with `keep_feed_prefix` (and, for
    /// Miniflux, their `miniflux-entry:` articles): the purge
    /// for one account's cache file, dropping rows a sync for
    /// a different kind of account wrote into it.
    pub fn purge_hosted_rows_except(&self, keep_feed_prefix: &str) -> rusqlite::Result<usize> {
        self.purge_hosted_rows_inner(Some(keep_feed_prefix))
    }

    fn purge_hosted_rows_inner(&self, keep_feed_prefix: Option<&str>) -> rusqlite::Result<usize> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        tx.execute(
            "CREATE TEMP TABLE IF NOT EXISTS purge_feeds (id TEXT PRIMARY KEY)",
            [],
        )?;
        tx.execute("DELETE FROM purge_feeds", [])?;
        match keep_feed_prefix {
            Some(prefix) => {
                let pat = format!("{prefix}%");
                tx.execute(
                    "INSERT INTO purge_feeds SELECT id FROM feeds WHERE (id LIKE 'feed/%' OR id LIKE 'miniflux:%') AND id NOT LIKE ?1",
                    params![pat],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO purge_feeds SELECT id FROM feeds WHERE id LIKE 'feed/%' OR id LIKE 'miniflux:%'",
                    [],
                )?;
            }
        }
        let keep_miniflux_articles = keep_feed_prefix == Some("miniflux:");
        tx.execute(
            "CREATE TEMP TABLE IF NOT EXISTS purge_articles (id TEXT PRIMARY KEY)",
            [],
        )?;
        tx.execute("DELETE FROM purge_articles", [])?;
        tx.execute(
            "INSERT INTO purge_articles SELECT id FROM articles WHERE feed_id IN (SELECT id FROM purge_feeds)",
            [],
        )?;
        if !keep_miniflux_articles {
            tx.execute(
                "INSERT OR IGNORE INTO purge_articles SELECT id FROM articles WHERE id LIKE 'miniflux-entry:%'",
                [],
            )?;
        }
        for table in ["chapters", "article_labels", "article_versions"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE article_id IN (SELECT id FROM purge_articles)"),
                [],
            )?;
        }
        tx.execute(
            "DELETE FROM playback_state WHERE id IN (SELECT id FROM purge_articles)",
            [],
        )?;
        tx.execute(
            "DELETE FROM articles WHERE id IN (SELECT id FROM purge_articles)",
            [],
        )?;
        let removed = tx.execute(
            "DELETE FROM feeds WHERE id IN (SELECT id FROM purge_feeds)",
            [],
        )?;
        tx.execute("DROP TABLE purge_feeds", [])?;
        tx.execute("DROP TABLE purge_articles", [])?;
        tx.commit()?;
        Ok(removed)
    }

    /// Chapters for an article, ordered by start (seconds).
    pub fn chapters(&self, article_id: &str) -> rusqlite::Result<Vec<Chapter>> {
        let mut stmt = self.conn.prepare(
            "SELECT start, title, href FROM chapters WHERE article_id = ?1 ORDER BY start",
        )?;
        let rows = stmt.query_map(params![article_id], |row| {
            Ok(Chapter {
                start: row.get(0)?,
                title: row.get(1)?,
                href: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// Replace an article's chapters (from a podcast chapter file or
    /// yt-dlp info), keyed deterministically by position.
    pub fn replace_chapters(&self, article_id: &str, chapters: &[Chapter]) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM chapters WHERE article_id = ?1",
            params![article_id],
        )?;
        for (i, ch) in chapters.iter().enumerate() {
            self.conn.execute(
                "INSERT INTO chapters (id, article_id, start, title, href) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![format!("{article_id}#{i}"), article_id, ch.start, ch.title, ch.href],
            )?;
        }
        Ok(())
    }

    /// Saved playback position for a playback id (article id).
    pub fn playback_state(&self, playback_id: &str) -> rusqlite::Result<Option<PlaybackState>> {
        let mut stmt = self.conn.prepare(
            "SELECT position_ms, duration_ms, completed, seek_supported, title \
             FROM playback_state WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![playback_id], |row| {
            Ok(PlaybackState {
                position_ms: row.get(0)?,
                duration_ms: row.get(1)?,
                completed: row.get::<_, i64>(2)? != 0,
                seek_supported: row.get::<_, Option<i64>>(3)?.map(|v| v != 0),
                title: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Insert or update a playback position (Python
    /// `upsert_playback_state`); updated_at is unix seconds.
    pub fn upsert_playback_state(
        &self,
        playback_id: &str,
        state: &PlaybackState,
    ) -> rusqlite::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO playback_state (id, position_ms, duration_ms, updated_at, completed, seek_supported, title) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(id) DO UPDATE SET position_ms = ?2, duration_ms = ?3, \
             updated_at = ?4, completed = ?5, seek_supported = ?6, title = ?7",
            params![
                playback_id,
                state.position_ms,
                state.duration_ms,
                now,
                state.completed as i64,
                state.seek_supported.map(|b| b as i64),
                state.title
            ],
        )?;
        Ok(())
    }

    pub fn delete_playback_state(&self, playback_id: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM playback_state WHERE id = ?1",
            params![playback_id],
        )?;
        Ok(())
    }

    // ---- Smart folders + filter rules ----------------------------

    pub fn smart_folders(&self) -> rusqlite::Result<Vec<(String, String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, rule_json, position FROM smart_folders ORDER BY position, name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        rows.collect()
    }

    pub fn create_smart_folder(&self, name: &str, rule_json: &str) -> rusqlite::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let pos: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM smart_folders",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO smart_folders (id, name, rule_json, position) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, rule_json, pos],
        )?;
        Ok(id)
    }

    pub fn delete_smart_folder(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM smart_folders WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn filter_rules(&self) -> rusqlite::Result<Vec<crate::rules::FilterRule>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, rule_json, actions_json, enabled, stop, position \
             FROM filter_rules ORDER BY position",
        )?;
        let rows = stmt.query_map([], |row| {
            let rule_json: String = row.get(2)?;
            let actions_json: String = row.get(3)?;
            Ok(crate::rules::FilterRule {
                id: row.get(0)?,
                name: row.get(1)?,
                rule: serde_json::from_str(&rule_json).unwrap_or(serde_json::Value::Null),
                actions: serde_json::from_str(&actions_json).unwrap_or(serde_json::Value::Null),
                enabled: row.get::<_, i64>(4)? != 0,
                stop: row.get::<_, i64>(5)? != 0,
                position: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    pub fn create_filter_rule(
        &self,
        name: &str,
        rule_json: &str,
        actions_json: &str,
        enabled: bool,
        stop: bool,
    ) -> rusqlite::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let pos: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM filter_rules",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO filter_rules (id, name, rule_json, actions_json, enabled, stop, position) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                name,
                rule_json,
                actions_json,
                enabled as i64,
                stop as i64,
                pos
            ],
        )?;
        Ok(id)
    }

    /// Update a smart folder's name and rule (the editor dialog).
    pub fn update_smart_folder(
        &self,
        id: &str,
        name: &str,
        rule_json: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE smart_folders SET name = ?2, rule_json = ?3 WHERE id = ?1",
            params![id, name, rule_json],
        )?;
        Ok(())
    }

    /// Update a filter rule in place (the editor dialog).
    pub fn update_filter_rule(
        &self,
        id: &str,
        name: &str,
        rule_json: &str,
        actions_json: &str,
        enabled: bool,
        stop: bool,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE filter_rules SET name = ?2, rule_json = ?3, actions_json = ?4, \
             enabled = ?5, stop = ?6 WHERE id = ?1",
            params![
                id,
                name,
                rule_json,
                actions_json,
                enabled as i64,
                stop as i64
            ],
        )?;
        Ok(())
    }

    /// Rewrite filter-rule positions from an ordered id list
    /// (the rules dialog's Move Up/Down).
    pub fn reorder_filter_rules(&self, ordered_ids: &[String]) -> rusqlite::Result<()> {
        for (i, id) in ordered_ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE filter_rules SET position = ?2 WHERE id = ?1",
                params![id, i as i64],
            )?;
        }
        Ok(())
    }

    pub fn delete_filter_rule(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM filter_rules WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Add a label (extra category membership) to an article.
    pub fn add_article_label(&self, article_id: &str, category: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO article_labels (article_id, category) VALUES (?1, ?2)",
            params![article_id, category],
        )?;
        Ok(())
    }

    /// Articles carrying a label (category view membership).
    pub fn article_ids_with_label(&self, category: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT article_id FROM article_labels WHERE category = ?1")?;
        let rows = stmt.query_map(params![category], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Move an article to a category (articles.category_override).
    pub fn set_article_category_override(
        &self,
        article_id: &str,
        category: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE articles SET category_override = ?2 WHERE id = ?1",
            params![article_id, category],
        )?;
        Ok(())
    }

    /// True when a feed with this URL is already subscribed.
    pub fn feed_url_exists(&self, url: &str) -> rusqlite::Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feeds WHERE url = ?1",
            params![url],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Remove a feed and all its articles.
    pub fn remove_feed(&self, feed_id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM articles WHERE feed_id = ?1", params![feed_id])?;
        self.conn
            .execute("DELETE FROM feeds WHERE id = ?1", params![feed_id])?;
        Ok(())
    }

    /// Run `f` inside BEGIN IMMEDIATE … COMMIT, rolling back on
    /// any error (the schema-init transaction pattern). Category
    /// mutations rewrite many rows across tables; a failure
    /// midway must leave the old working data untouched.
    fn category_txn<T>(&self, f: impl FnOnce(&Self) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Rewrite every stored reference to a category path after
    /// the path itself moves (Python `core/db.py`
    /// `rewrite_category_references`): article overrides, article
    /// labels, per-feed delete targets and filter-rule
    /// move/label action targets. The global config
    /// `delete_behavior` target is remapped by the caller through
    /// `remap_delete_behavior` with the same mapping.
    fn rewrite_category_references(&self, old: &str, new: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE articles SET category_override = ?1 WHERE category_override = ?2",
            params![new, old],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO article_labels (article_id, category) \
             SELECT article_id, ?1 FROM article_labels WHERE category = ?2",
            params![new, old],
        )?;
        self.conn.execute(
            "DELETE FROM article_labels WHERE category = ?1",
            params![old],
        )?;
        self.conn.execute(
            "UPDATE feeds SET delete_behavior = ?1 WHERE delete_behavior = ?2",
            params![format!("category:{new}"), format!("category:{old}")],
        )?;
        let rules: Vec<(String, Option<String>)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, actions_json FROM filter_rules")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, raw) in rules {
            let Some(raw) = raw else { continue };
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            let mut changed = false;
            if let Some(obj) = value.as_object_mut() {
                for key in ["move", "label"] {
                    if obj.get(key).and_then(|v| v.as_str()) == Some(old) {
                        obj.insert(key.to_string(), serde_json::Value::String(new.to_string()));
                        changed = true;
                    }
                }
            }
            if changed {
                self.conn.execute(
                    "UPDATE filter_rules SET actions_json = ?1 WHERE id = ?2",
                    params![value.to_string(), id],
                )?;
            }
        }
        Ok(())
    }

    /// Apply a category path mapping inside the current
    /// transaction: category rows, feed assignments and every
    /// reference (see `rewrite_category_references`).
    fn apply_category_mapping(&self, mapping: &[(String, String)]) -> rusqlite::Result<()> {
        for (old, new) in mapping {
            self.conn.execute(
                "UPDATE categories SET title = ?1 WHERE title = ?2",
                params![new, old],
            )?;
            self.conn.execute(
                "UPDATE feeds SET category = ?1 WHERE category = ?2",
                params![new, old],
            )?;
            self.rewrite_category_references(old, new)?;
        }
        Ok(())
    }

    fn category_row(&self, title: &str) -> rusqlite::Result<Option<(String, Option<String>)>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT id, parent_id FROM categories WHERE title = ?1",
                params![title],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    fn category_title_by_id(&self, id: &str) -> rusqlite::Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT title FROM categories WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
    }

    /// Rename a category (Python `LocalProvider.rename_category`):
    /// `new_leaf` is the new LEAF name; the node keeps its parent,
    /// its descendants keep their path tails, and every feed
    /// assignment and reference moves with it, transactionally.
    /// False when the category is missing or the target path is
    /// already taken; on false (or error) nothing has changed.
    pub fn rename_category(&self, old_title: &str, new_leaf: &str) -> rusqlite::Result<bool> {
        let old_path = old_title.trim();
        let leaf = sanitize_category_leaf(new_leaf);
        if old_path.is_empty() || leaf.is_empty() {
            return Ok(false);
        }
        let Some((_, parent_id)) = self.category_row(old_path)? else {
            return Ok(false);
        };
        let parent_path = match &parent_id {
            Some(pid) => self.category_title_by_id(pid)?,
            None => None,
        };
        let new_path = make_category_path(parent_path.as_deref(), &leaf);
        if new_path == old_path {
            return Ok(true);
        }
        if self.category_row(&new_path)?.is_some() {
            return Ok(false);
        }
        let titles: Vec<String> = self.categories()?.into_iter().map(|c| c.title).collect();
        let mapping = category_subtree_mapping(&titles, old_path, &new_path);
        self.category_txn(|db| db.apply_category_mapping(&mapping))?;
        Ok(true)
    }

    /// Move a category under a new parent (Python
    /// `LocalProvider.move_category`, issue #86): the leaf name
    /// is kept, only the ancestry changes; the node's `parent_id`
    /// is updated and every descendant path, feed assignment and
    /// reference is rewritten transactionally. False — with
    /// nothing changed — for the protected sentinel, a missing
    /// category or parent, a move under itself/its own subtree,
    /// or a leaf collision under the destination parent.
    pub fn move_category(&self, title: &str, parent_title: Option<&str>) -> rusqlite::Result<bool> {
        let old_path = title.trim();
        let new_parent = parent_title.map(str::trim).filter(|s| !s.is_empty());
        if old_path.is_empty() || old_path.eq_ignore_ascii_case("uncategorized") {
            return Ok(false);
        }
        if new_parent.is_some_and(|p| p.eq_ignore_ascii_case("uncategorized")) {
            return Ok(false);
        }
        let descendant = format!("{old_path}{CATEGORY_PATH_SEP}");
        if new_parent.is_some_and(|p| p == old_path || p.starts_with(&descendant)) {
            return Ok(false);
        }
        let Some((cat_id, _)) = self.category_row(old_path)? else {
            return Ok(false);
        };
        let new_parent_id = match new_parent {
            Some(p) => match self.category_row(p)? {
                Some((id, _)) => Some(id),
                None => return Ok(false),
            },
            None => None,
        };
        let new_path = make_category_path(new_parent, &category_display_leaf(old_path));
        if new_path == old_path {
            return Ok(true);
        }
        if self.category_row(&new_path)?.is_some() {
            return Ok(false);
        }
        let titles: Vec<String> = self.categories()?.into_iter().map(|c| c.title).collect();
        let mapping = category_subtree_mapping(&titles, old_path, &new_path);
        let new_parent_id2 = new_parent_id.clone();
        self.category_txn(|db| {
            db.apply_category_mapping(&mapping)?;
            db.conn.execute(
                "UPDATE categories SET parent_id = ?1 WHERE id = ?2",
                params![new_parent_id2, cat_id],
            )?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Move one feed into a category (None = uncategorized).
    pub fn set_feed_category(&self, feed_id: &str, category: Option<&str>) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE feeds SET category = ?2 WHERE id = ?1",
            params![feed_id, category],
        )?;
        Ok(())
    }

    /// Edit a feed's title and/or URL. Setting a title marks it
    /// custom (Python `update_feed`); the URL is the subscription
    /// address used at refresh.
    pub fn update_feed(
        &self,
        feed_id: &str,
        title: Option<&str>,
        url: Option<&str>,
    ) -> rusqlite::Result<()> {
        if let Some(t) = title {
            self.conn.execute(
                "UPDATE feeds SET title = ?2, title_is_custom = 1 WHERE id = ?1",
                params![feed_id, t],
            )?;
        }
        if let Some(u) = url {
            self.conn.execute(
                "UPDATE feeds SET url = ?2 WHERE id = ?1",
                params![feed_id, u],
            )?;
        }
        Ok(())
    }

    /// Reset a feed's title to the feed's own title and clear the
    /// custom flag (Python `reset_feed_title`).
    pub fn reset_feed_title(&self, feed_id: &str, feed_title: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE feeds SET title = ?2, title_is_custom = 0 WHERE id = ?1",
            params![feed_id, feed_title],
        )?;
        Ok(())
    }

    pub fn add_category(&self, title: &str) -> rusqlite::Result<Category> {
        let cat = Category {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            parent_id: None,
        };
        self.conn.execute(
            "INSERT INTO categories (id, title) VALUES (?1, ?2)",
            params![cat.id, cat.title],
        )?;
        Ok(cat)
    }

    /// Ensure a category path exists ("News / Tech"), creating any
    /// missing rows with parent links (Python's local provider
    /// stores hierarchy natively in categories.parent_id while the
    /// title carries the full path). Path segments are split on
    /// CATEGORY_PATH_SEP only: a bare "/" is part of a leaf name.
    /// Returns the leaf's id.
    pub fn ensure_category_path(&self, path: &str) -> rusqlite::Result<String> {
        let mut parent_id: Option<String> = None;
        let mut current = String::new();
        for segment in path.split(CATEGORY_PATH_SEP) {
            let leaf = sanitize_category_leaf(segment);
            if leaf.is_empty() {
                continue;
            }
            current = make_category_path(
                if current.is_empty() {
                    None
                } else {
                    Some(current.as_str())
                },
                &leaf,
            );
            let existing: Option<String> = self
                .conn
                .query_row(
                    "SELECT id FROM categories WHERE title = ?1",
                    params![current],
                    |r| r.get(0),
                )
                .ok();
            let id = match existing {
                Some(id) => id,
                None => {
                    // INSERT OR IGNORE + re-read: "ensure" is an
                    // idempotent contract, and a concurrent
                    // opener ensuring the same path must not
                    // fail on the UNIQUE title (the v0.1.33
                    // macOS release-run failure).
                    let candidate = uuid::Uuid::new_v4().to_string();
                    self.conn.execute(
                        "INSERT OR IGNORE INTO categories (id, title, parent_id) VALUES (?1, ?2, ?3)",
                        params![candidate, current, parent_id],
                    )?;
                    self.conn.query_row(
                        "SELECT id FROM categories WHERE title = ?1",
                        params![current],
                        |r| r.get(0),
                    )?
                }
            };
            parent_id = Some(id);
        }
        Ok(parent_id.unwrap_or_default())
    }

    /// Delete a category by full path (Python
    /// `LocalProvider.delete_category`): direct children are
    /// reparented to the deleted node's parent, shortening their
    /// paths; feeds directly in the deleted category fall back to
    /// Uncategorized; every reference follows the same mapping,
    /// all in one transaction. When a reparented child would
    /// collide with an existing category, Python's fallback
    /// applies instead: the whole subtree is dropped and its
    /// feeds go to Uncategorized. False — nothing changed — for
    /// the protected sentinel or a missing category.
    pub fn delete_category_path(&self, path: &str) -> rusqlite::Result<bool> {
        let path = path.trim();
        if path.is_empty() || path.eq_ignore_ascii_case("uncategorized") {
            return Ok(false);
        }
        let Some((cat_id, parent_id)) = self.category_row(path)? else {
            return Ok(false);
        };
        let parent_path: Option<String> = match &parent_id {
            Some(pid) => self.category_title_by_id(pid)?,
            None => None,
        };
        let old_prefix = format!("{path}{CATEGORY_PATH_SEP}");
        let new_prefix = parent_path
            .as_ref()
            .map(|p| format!("{p}{CATEGORY_PATH_SEP}"))
            .unwrap_or_default();
        let titles: Vec<String> = self.categories()?.into_iter().map(|c| c.title).collect();
        let descendants: Vec<String> = titles
            .iter()
            .filter(|t| t.starts_with(&old_prefix))
            .cloned()
            .collect();
        // A shortened descendant path must not land on an existing
        // category outside the subtree (nor on another shortened
        // descendant).
        let mut remaining: std::collections::HashSet<String> = titles.iter().cloned().collect();
        for d in &descendants {
            remaining.remove(d);
        }
        remaining.remove(path);
        let mut mapping: Vec<(String, String)> = Vec::new();
        let mut collision = false;
        for old_p in &descendants {
            let new_p = format!("{new_prefix}{}", &old_p[old_prefix.len()..]);
            if remaining.contains(&new_p) {
                collision = true;
                break;
            }
            remaining.insert(new_p.clone());
            mapping.push((old_p.clone(), new_p));
        }
        if collision {
            // Python's safe fallback: drop the whole subtree,
            // feeds and references go to Uncategorized.
            let mut subtree = vec![path.to_string()];
            subtree.extend(descendants.iter().cloned());
            self.category_txn(|db| {
                for p in &subtree {
                    db.conn.execute(
                        "UPDATE feeds SET category = 'Uncategorized' WHERE category = ?1",
                        params![p],
                    )?;
                    db.rewrite_category_references(p, "Uncategorized")?;
                    db.conn
                        .execute("DELETE FROM categories WHERE title = ?1", params![p])?;
                }
                Ok(())
            })?;
            return Ok(true);
        }
        // Shortest paths first: a same-named child inherits the
        // deleted node's path only after the node row is gone.
        mapping.sort_by_key(|(old, _)| old.len());
        self.category_txn(|db| {
            // Reparent direct children (parent_id is by id, so
            // deeper links hold).
            db.conn.execute(
                "UPDATE categories SET parent_id = ?1 WHERE parent_id = ?2",
                params![parent_id, cat_id],
            )?;
            // Feeds directly in the deleted category fall back to
            // Uncategorized, and references follow.
            db.conn.execute(
                "UPDATE feeds SET category = 'Uncategorized' WHERE category = ?1",
                params![path],
            )?;
            db.rewrite_category_references(path, "Uncategorized")?;
            // Free the old path before a same-named child
            // inherits it.
            db.conn
                .execute("DELETE FROM categories WHERE id = ?1", params![cat_id])?;
            db.apply_category_mapping(&mapping)?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Record a feed refresh failure (Python record_feed_error).
    pub fn record_feed_error(&self, feed_id: &str, error: &str) -> rusqlite::Result<()> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.conn.execute(
            "UPDATE feeds SET last_error = ?2, last_error_at = ?3, \
             consecutive_failures = COALESCE(consecutive_failures, 0) + 1 \
             WHERE id = ?1",
            params![feed_id, error, ts],
        )?;
        Ok(())
    }

    /// Clear a feed's error after a successful refresh.
    pub fn clear_feed_error(&self, feed_id: &str) -> rusqlite::Result<()> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.conn.execute(
            "UPDATE feeds SET last_error = NULL, last_error_at = NULL, \
             last_success_at = ?2, consecutive_failures = 0 WHERE id = ?1",
            params![feed_id, ts],
        )?;
        Ok(())
    }

    /// Feeds whose latest refresh failed, most recent first
    /// (Python get_feed_errors).
    pub fn feed_errors(&self) -> rusqlite::Result<Vec<FeedError>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, category, last_error, last_error_at, \
             last_success_at, COALESCE(consecutive_failures, 0) FROM feeds \
             WHERE last_error IS NOT NULL AND TRIM(last_error) != '' \
             ORDER BY COALESCE(last_error_at, 0) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FeedError {
                feed_id: r.get(0)?,
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                url: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                category: r.get(3)?,
                error: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                error_at: r.get(5)?,
                success_at: r.get(6)?,
                consecutive_failures: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Articles across several feeds, newest first (category
    /// scopes in the tree read through this).
    pub fn articles_for_feeds(
        &self,
        feed_ids: &[String],
        limit: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        if feed_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = feed_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, feed_id, title, url, content, description, date, author, \
             is_read, is_favorite, media_url, media_type FROM articles \
             WHERE feed_id IN ({marks}) ORDER BY COALESCE(date, '') DESC LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = feed_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let limit_i = limit as i64;
        params.push(&limit_i);
        let rows = stmt.query_map(params.as_slice(), article_row)?;
        rows.collect()
    }

    /// Articles belonging to a category view (Python
    /// `providers/local.py::_category_membership_clause`): the
    /// category's whole subtree counts, and membership is
    /// (feed in subtree AND not moved away) OR (moved here via
    /// category_override) OR (labeled with a category in the
    /// subtree). "Uncategorized" is the bucket of feeds with no /
    /// empty / sentinel category.
    pub fn articles_for_category(
        &self,
        path: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        self.articles_for_category_impl(path, limit, 0, false)
    }

    /// `articles_for_category` without article bodies; see
    /// `articles_light` for why the list path uses this.
    pub fn articles_for_category_light(
        &self,
        path: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        self.articles_for_category_impl(path, limit, 0, true)
    }

    /// One page of light category rows; see `articles_light_page`.
    pub fn articles_for_category_light_page(
        &self,
        path: &str,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<Article>> {
        self.articles_for_category_impl(path, limit, offset, true)
    }

    /// Total rows a category view can page through; pairs with
    /// `articles_for_category_light_page`.
    pub fn articles_for_category_count(&self, path: &str) -> rusqlite::Result<i64> {
        let (where_sql, names, uncat) = self.category_membership(path)?;
        let sql = format!(
            "SELECT COUNT(*) FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE {where_sql}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        if !uncat {
            for _ in 0..3 {
                for n in &names {
                    params.push(n as &dyn rusqlite::ToSql);
                }
            }
        }
        stmt.query_row(params.as_slice(), |r| r.get(0))
    }

    /// The category membership clause (Python
    /// `providers/local.py::_category_membership_clause`) as a
    /// WHERE body over aliases `a` (articles) and `f` (feeds),
    /// plus the subtree name set to bind (three times, unless the
    /// Uncategorized bucket, which binds nothing).
    fn category_membership(&self, path: &str) -> rusqlite::Result<(String, Vec<String>, bool)> {
        // The subtree name set: the path plus every descendant path
        // known to the categories table or carried by a feed.
        let mut names: Vec<String> = vec![path.to_string()];
        let prefix = format!("{path}{CATEGORY_PATH_SEP}");
        let mut push_name = |n: &str| {
            if (n == path || n.starts_with(&prefix)) && !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        };
        for c in self.categories()? {
            push_name(&c.title);
        }
        {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT category FROM feeds WHERE category IS NOT NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for n in rows {
                push_name(&n?);
            }
        }
        let marks = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let uncat = path.trim().is_empty() || path.trim().eq_ignore_ascii_case("uncategorized");
        let (feed_match, override_match, label_match) = if uncat {
            (
                "(f.category IS NULL OR TRIM(f.category) = '' \
                  OR LOWER(TRIM(f.category)) = 'uncategorized')"
                    .to_string(),
                "LOWER(TRIM(a.category_override)) = 'uncategorized'".to_string(),
                "a.id IN (SELECT article_id FROM article_labels \
                 WHERE LOWER(TRIM(category)) = 'uncategorized')"
                    .to_string(),
            )
        } else {
            (
                format!("f.category IN ({marks})"),
                format!("a.category_override IN ({marks})"),
                format!(
                    "a.id IN (SELECT article_id FROM article_labels WHERE category IN ({marks}))"
                ),
            )
        };
        let where_sql = format!(
            "((a.category_override IS NULL AND {feed_match}) \
             OR {override_match} OR {label_match})"
        );
        Ok((where_sql, names, uncat))
    }

    fn articles_for_category_impl(
        &self,
        path: &str,
        limit: usize,
        offset: usize,
        light: bool,
    ) -> rusqlite::Result<Vec<Article>> {
        let content_col = if light { "NULL" } else { "a.content" };
        let (where_sql, names, uncat) = self.category_membership(path)?;
        // The id DESC tiebreak (the feed query has one too) makes
        // the order total, which OFFSET paging requires: podcast
        // episodes routinely share a date, and an unstable order
        // would repeat or skip rows across pages.
        let sql = format!(
            "SELECT a.id, a.feed_id, a.title, a.url, {content_col}, a.description, a.date, \
             a.author, a.is_read, a.is_favorite, a.media_url, a.media_type \
             FROM articles a JOIN feeds f ON f.id = a.feed_id \
             WHERE {where_sql} \
             ORDER BY COALESCE(a.date, '') DESC, a.id DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        if !uncat {
            // The name set is bound three times (feed, override, labels).
            for _ in 0..3 {
                for n in &names {
                    params.push(n as &dyn rusqlite::ToSql);
                }
            }
        }
        let limit_i = limit as i64;
        let offset_i = offset as i64;
        params.push(&limit_i);
        params.push(&offset_i);
        let rows = stmt.query_map(params.as_slice(), article_row)?;
        rows.collect()
    }

    pub fn remove_category(&self, category_id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM categories WHERE id = ?1", params![category_id])?;
        Ok(())
    }

    /// Delete an article, keeping a tombstone copy in deleted_articles
    /// (Python schema) so it can be restored and so refresh does not
    /// resurrect it. The snapshot carries the article's read/favorite
    /// state (Python `remember_deleted_article`), and the article's
    /// label/chapter rows go with it (Python's tombstone-and-remove
    /// invariant). Returns false when the article does not exist.
    pub fn delete_article(&self, article_id: &str) -> rusqlite::Result<bool> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        // OR REPLACE: re-deleting an id that already has a tombstone
        // under this feed refreshes the snapshot (Python upserts on
        // the (feed_id, article_id) conflict); older tables without
        // the primary key simply insert.
        let copied = self.conn.execute(
            "INSERT OR REPLACE INTO deleted_articles \
             (feed_id, article_id, url, deleted_at, title, content, description, \
              date, author, media_url, media_type, chapter_url, is_read, \
              is_favorite, purged) \
             SELECT feed_id, id, url, ?1, title, content, description, date, \
                    author, media_url, media_type, chapter_url, is_read, \
                    is_favorite, 0 \
             FROM articles WHERE id = ?2",
            params![now, article_id],
        )?;
        if copied == 0 {
            return Ok(false);
        }
        self.conn.execute(
            "DELETE FROM article_labels WHERE article_id = ?1",
            params![article_id],
        )?;
        self.conn.execute(
            "DELETE FROM chapters WHERE article_id = ?1",
            params![article_id],
        )?;
        self.conn
            .execute("DELETE FROM articles WHERE id = ?1", params![article_id])?;
        Ok(true)
    }

    /// Retention cleanup (Python `cleanup_old_articles`): delete
    /// articles older than `keep_days` — read AND unread — except
    /// favorites, and except undated rows (empty date or the
    /// "0001-01-01" sentinel, which compare below the undated
    /// floor). Dependent chapters and playback state go with the
    /// articles. Tombstones are NEVER aged out here: Python removes
    /// them only on restore or confirmed purge, so a deleted
    /// article's resurrection guard does not expire. Returns
    /// article rows removed.
    /// The effective delete behavior for one feed: its
    /// `feeds.delete_behavior` override wins over the global
    /// `delete_behavior` setting (Python `_resolve_delete_behavior`).
    pub fn effective_delete_behavior(
        &self,
        feed_id: &str,
        global: &str,
    ) -> crate::rules::DeleteBehavior {
        let raw = self
            .feed_delete_behavior(feed_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| global.to_string());
        crate::rules::parse_delete_behavior(&raw)
    }

    /// Delete an article through a resolved delete behavior — the
    /// one shared policy behind the interactive Delete command and
    /// the filter-rule pipeline (Python `LocalProvider.delete_article`):
    /// category behavior MOVES the article (sets its category
    /// override) and keeps the row; purge tombstones it as purged;
    /// deleted soft-deletes it. Returns false when the article does
    /// not exist.
    pub fn delete_article_with_behavior(
        &self,
        article_id: &str,
        behavior: &crate::rules::DeleteBehavior,
    ) -> rusqlite::Result<bool> {
        match behavior {
            crate::rules::DeleteBehavior::Category(path) => {
                let exists = self.article_by_id(article_id)?.is_some();
                if !exists {
                    return Ok(false);
                }
                self.set_article_category_override(article_id, Some(path))?;
                Ok(true)
            }
            crate::rules::DeleteBehavior::Purge => self.purge_article(article_id),
            crate::rules::DeleteBehavior::Deleted => self.delete_article(article_id),
        }
    }

    /// Retention: delete read, non-favorite articles older than
    /// `keep_days` (by article date when parseable, else by row
    /// position — dates are ISO text in this schema), plus
    /// tombstones older than the same window. Returns rows removed.
    pub fn prune_old_articles(&self, keep_days: i64) -> rusqlite::Result<usize> {
        let Some(cutoff) = crate::retention::retention_cutoff_date(Some(keep_days)) else {
            return Ok(0);
        };
        // One WHERE clause shared by the article delete and its
        // dependent cleanups, exactly as in Python.
        let cond = format!(
            "date < ?1 AND date >= '{}' AND is_favorite = 0",
            crate::retention::UNDATED_ARTICLE_FLOOR
        );
        self.conn.execute(
            &format!(
                "DELETE FROM chapters WHERE article_id IN \
                 (SELECT id FROM articles WHERE {cond})"
            ),
            params![cutoff],
        )?;
        self.conn.execute(
            &format!(
                "DELETE FROM playback_state WHERE id IN \
                 (SELECT 'article:' || id FROM articles WHERE {cond})"
            ),
            params![cutoff],
        )?;
        let removed = self.conn.execute(
            &format!("DELETE FROM articles WHERE {cond}"),
            params![cutoff],
        )?;
        Ok(removed)
    }

    /// Permanently delete an article (Python delete_behavior
    /// "purge"): the tombstone is kept but flagged purged, so
    /// it stays out of the Deleted view and cannot be restored.
    pub fn purge_article(&self, article_id: &str) -> rusqlite::Result<bool> {
        let feed_id = self
            .article_by_id(article_id)?
            .map(|a| a.feed_id)
            .unwrap_or_default();
        let deleted = self.delete_article(article_id)?;
        if deleted {
            self.conn.execute(
                "UPDATE deleted_articles SET purged = 1 \
                 WHERE article_id = ?1 AND feed_id = ?2",
                params![article_id, feed_id],
            )?;
        }
        Ok(deleted)
    }

    /// A feed's delete-behavior override (feeds.delete_behavior),
    /// when set: "deleted", "purge", or "category:<path>".
    pub fn feed_delete_behavior(&self, feed_id: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT delete_behavior FROM feeds WHERE id = ?1")?;
        let mut rows = stmt.query(params![feed_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, Option<String>>(0)?.filter(|s| !s.is_empty())),
            None => Ok(None),
        }
    }

    /// Set or clear a feed's delete-behavior override.
    pub fn set_feed_delete_behavior(
        &self,
        feed_id: &str,
        behavior: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE feeds SET delete_behavior = ?2 WHERE id = ?1",
            params![feed_id, behavior],
        )?;
        Ok(())
    }

    /// A feed's per-feed settings object (feeds.feed_settings
    /// JSON: encoding, refresh_interval_seconds, timeout...).
    pub fn feed_settings(&self, feed_id: &str) -> rusqlite::Result<serde_json::Value> {
        let mut stmt = self
            .conn
            .prepare("SELECT feed_settings FROM feeds WHERE id = ?1")?;
        let mut rows = stmt.query(params![feed_id])?;
        match rows.next()? {
            Some(row) => {
                let raw: Option<String> = row.get(0)?;
                Ok(raw
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_else(|| serde_json::json!({})))
            }
            None => Ok(serde_json::json!({})),
        }
    }

    /// Merge keys into a feed's settings object.
    pub fn set_feed_settings(
        &self,
        feed_id: &str,
        patch: &serde_json::Value,
    ) -> rusqlite::Result<()> {
        let mut current = self.feed_settings(feed_id)?;
        if let (Some(dst), Some(src)) = (current.as_object_mut(), patch.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        self.conn.execute(
            "UPDATE feeds SET feed_settings = ?2 WHERE id = ?1",
            params![feed_id, current.to_string()],
        )?;
        Ok(())
    }

    /// Feeds whose category is `path` or a subcategory of it
    /// (Python's delete-category-with-feeds feed collection).
    pub fn feeds_in_category_tree(&self, path: &str) -> rusqlite::Result<Vec<Feed>> {
        let prefix = format!("{path}{CATEGORY_PATH_SEP}");
        Ok(self
            .feeds()?
            .into_iter()
            .filter(|f| {
                f.category
                    .as_deref()
                    .map(|c| c == path || c.starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect())
    }

    /// Delete a category and all its subcategories (rows only —
    /// the caller removes or reparents the feeds first). Returns
    /// false when the category does not exist.
    pub fn delete_category_tree(&self, path: &str) -> rusqlite::Result<bool> {
        let prefix = format!("{path}{CATEGORY_PATH_SEP}");
        let ids: Vec<String> = self
            .categories()?
            .into_iter()
            .filter(|c| c.title == path || c.title.starts_with(&prefix))
            .map(|c| c.id)
            .collect();
        if ids.is_empty() {
            return Ok(false);
        }
        for id in &ids {
            self.conn
                .execute("DELETE FROM categories WHERE id = ?1", params![id])?;
        }
        Ok(true)
    }

    /// A feed's per-feed image-alt override (feeds.show_images,
    /// Python `get_feed_show_images`): None = inherit the global
    /// `show_image_alt` setting, Some(true/false) = override.
    pub fn feed_show_images(&self, feed_id: &str) -> rusqlite::Result<Option<bool>> {
        let mut stmt = self
            .conn
            .prepare("SELECT show_images FROM feeds WHERE id = ?1")?;
        let mut rows = stmt.query(params![feed_id])?;
        match rows.next()? {
            Some(row) => {
                let raw: Option<i64> = row.get(0)?;
                Ok(raw.map(|v| v != 0))
            }
            None => Ok(None),
        }
    }

    /// Set a feed's per-feed image-alt override (Python
    /// `set_feed_show_images`); None clears it back to inherit.
    pub fn set_feed_show_images(&self, feed_id: &str, value: Option<bool>) -> rusqlite::Result<()> {
        let stored: Option<i64> = value.map(|b| if b { 1 } else { 0 });
        self.conn.execute(
            "UPDATE feeds SET show_images = ?2 WHERE id = ?1",
            params![feed_id, stored],
        )?;
        Ok(())
    }

    /// Stamp an article's opened_at (the History view's
    /// source) at selection time.
    pub fn mark_opened(&self, article_id: &str) -> rusqlite::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.conn.execute(
            "UPDATE articles SET opened_at = ?2 WHERE id = ?1",
            params![article_id, now],
        )?;
        Ok(())
    }

    /// Recently opened articles, newest first:
    /// (article, opened_at epoch seconds).
    pub fn opened_articles(&self, limit: usize) -> rusqlite::Result<Vec<(Article, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type, opened_at \
             FROM articles WHERE opened_at IS NOT NULL AND opened_at > 0 \
             ORDER BY opened_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                Article {
                    id: r.get(0)?,
                    feed_id: r.get(1)?,
                    title: r.get(2)?,
                    url: r.get(3)?,
                    content: r.get(4)?,
                    description: r.get(5)?,
                    date: r.get(6)?,
                    author: r.get(7)?,
                    is_read: r.get(8)?,
                    is_favorite: r.get(9)?,
                    media_url: r.get(10)?,
                    media_type: r.get(11)?,
                },
                r.get::<_, f64>(12)?,
            ))
        })?;
        rows.collect()
    }

    /// An article's tags: Python newline text or earlier Rust JSON lists.
    pub fn article_tags(&self, article_id: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tags FROM articles WHERE id = ?1")?;
        let mut rows = stmt.query(params![article_id])?;
        match rows.next()? {
            Some(row) => {
                let raw: Option<String> = row.get(0)?;
                Ok(raw.map(|t| decode_tags(&t)).unwrap_or_default())
            }
            None => Ok(Vec::new()),
        }
    }

    /// All stored article tags at once (smart-folder rule
    /// evaluation reads the Tag field from here).
    pub fn all_article_tags(
        &self,
    ) -> rusqlite::Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, tags FROM articles WHERE tags IS NOT NULL AND tags != ''")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (id, raw) = row?;
            if let Some(tags) = raw.map(|t| decode_tags(&t)) {
                out.insert(id, tags);
            }
        }
        Ok(out)
    }

    /// Apply fetched-page metadata to a stored article
    /// (metadata_enrich.py): fill a missing author, merge tags.
    pub fn apply_enrichment(
        &self,
        article_id: &str,
        meta: &crate::enrich::EnrichMeta,
    ) -> rusqlite::Result<()> {
        if !meta.author.is_empty() {
            self.conn.execute(
                "UPDATE articles SET author = ?2 WHERE id = ?1 \
                 AND (author IS NULL OR author = '')",
                params![article_id, meta.author],
            )?;
        }
        if !meta.tags.is_empty() {
            let existing = self.article_tags(article_id)?;
            let merged = crate::enrich::merge_tags(existing, &meta.tags);
            self.conn.execute(
                "UPDATE articles SET tags = ?2 WHERE id = ?1",
                params![article_id, merged.join("\n")],
            )?;
        }
        Ok(())
    }

    /// Search honouring the `search_mode` setting: title_only
    /// restricts the match to titles.
    pub fn search_articles_mode(
        &self,
        query: &str,
        limit: usize,
        title_only: bool,
    ) -> rusqlite::Result<Vec<Article>> {
        self.search_articles_mode_impl(query, limit, title_only, false)
    }

    /// `search_articles_mode` without article bodies in the
    /// result rows; see `articles_light`.
    pub fn search_articles_mode_light(
        &self,
        query: &str,
        limit: usize,
        title_only: bool,
    ) -> rusqlite::Result<Vec<Article>> {
        self.search_articles_mode_impl(query, limit, title_only, true)
    }

    fn search_articles_mode_impl(
        &self,
        query: &str,
        limit: usize,
        title_only: bool,
        light: bool,
    ) -> rusqlite::Result<Vec<Article>> {
        if !title_only {
            return self.search_articles_impl(query, limit, light);
        }
        let content_col = if light { "NULL" } else { "content" };
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, feed_id, title, url, {content_col}, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles \
             WHERE title LIKE ?1 ESCAPE '\\' \
             ORDER BY date DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![like, limit as i64], article_row)?;
        rows.collect()
    }

    /// Restore a deleted article from its tombstone, with its
    /// snapshotted read/favorite state (Python
    /// `restore_deleted_article`). Unscoped: succeeds only when
    /// exactly one non-purged tombstone carries this article id —
    /// an ambiguous id (tombstoned under several feeds) or a
    /// purged tombstone restores nothing and keeps its row, so
    /// the caller can surface the failure instead of dropping
    /// the Deleted view entry.
    pub fn restore_article(&self, article_id: &str) -> rusqlite::Result<bool> {
        let mut stmt = self.conn.prepare(
            "SELECT feed_id FROM deleted_articles \
             WHERE article_id = ?1 AND COALESCE(purged, 0) = 0",
        )?;
        let feeds: Vec<String> = stmt
            .query_map(params![article_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        match feeds.as_slice() {
            [feed_id] => self.restore_deleted_article(feed_id, article_id),
            _ => Ok(false),
        }
    }

    /// Restore one feed's tombstone for an article (Python
    /// `restore_deleted_article` with a feed id). Purged
    /// tombstones never restore. On success the tombstone row
    /// is dropped; on failure nothing changes.
    pub fn restore_deleted_article(
        &self,
        feed_id: &str,
        article_id: &str,
    ) -> rusqlite::Result<bool> {
        let restored = self.conn.execute(
            "INSERT OR REPLACE INTO articles \
             (id, feed_id, title, url, content, description, date, author, \
              is_read, is_favorite, media_url, media_type, chapter_url) \
             SELECT article_id, feed_id, title, url, content, description, \
                    date, author, COALESCE(is_read, 0), \
                    COALESCE(is_favorite, 0), media_url, media_type, \
                    chapter_url \
             FROM deleted_articles \
             WHERE feed_id = ?1 AND article_id = ?2 \
               AND COALESCE(purged, 0) = 0",
            params![feed_id, article_id],
        )?;
        if restored == 0 {
            return Ok(false);
        }
        self.conn.execute(
            "DELETE FROM deleted_articles WHERE feed_id = ?1 AND article_id = ?2",
            params![feed_id, article_id],
        )?;
        Ok(true)
    }

    /// Permanently delete one Deleted-view entry (Python
    /// `purge_deleted_article`): the snapshot content is dropped
    /// and the tombstone flagged purged, but the tombstone
    /// identity is kept so refresh never recreates the article.
    /// Returns false when no live tombstone matches.
    pub fn purge_deleted_article(&self, feed_id: &str, article_id: &str) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "UPDATE deleted_articles SET purged = 1, title = NULL, \
             content = NULL, description = NULL, author = NULL, \
             media_url = NULL, media_type = NULL, chapter_url = NULL \
             WHERE feed_id = ?1 AND article_id = ?2 \
               AND COALESCE(purged, 0) = 0",
            params![feed_id, article_id],
        )?;
        Ok(n > 0)
    }

    /// Tombstoned article ids, for the refresh resurrection check.
    pub fn deleted_article_ids(&self) -> rusqlite::Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT article_id FROM deleted_articles")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// Tombstones for ONE feed (Python
    /// `deleted_article_tombstones_for_feed`): the deleted article
    /// ids and their URLs, both trimmed and non-empty. Refresh
    /// matches an incoming entry against the ids (raw or
    /// feed-scoped) OR its URL, so a publisher GUID change cannot
    /// revive a deleted URL, and one feed's deletions never
    /// suppress another feed's articles.
    pub fn deleted_tombstones_for_feed(
        &self,
        feed_id: &str,
    ) -> rusqlite::Result<(
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    )> {
        let mut ids = std::collections::HashSet::new();
        let mut urls = std::collections::HashSet::new();
        if feed_id.trim().is_empty() {
            return Ok((ids, urls));
        }
        let mut stmt = self
            .conn
            .prepare("SELECT article_id, url FROM deleted_articles WHERE feed_id = ?1")?;
        let rows = stmt.query_map(params![feed_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })?;
        for row in rows {
            let (article_id, url) = row?;
            if let Some(aid) = article_id
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                ids.insert(aid);
            }
            if let Some(u) = url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                urls.insert(u);
            }
        }
        Ok((ids, urls))
    }

    pub fn deleted_articles(&self) -> rusqlite::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT article_id, feed_id, title, url, content, description, date, \
             author, media_url, media_type, is_read, is_favorite \
             FROM deleted_articles \
             WHERE COALESCE(purged, 0) = 0 \
             ORDER BY deleted_at DESC, article_id DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Article {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                url: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                content: r.get(4)?,
                description: r.get(5)?,
                date: r.get(6)?,
                author: r.get(7)?,
                media_url: r.get(8)?,
                media_type: r.get(9)?,
                is_read: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                is_favorite: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
            })
        })?;
        rows.collect()
    }

    /// Every article matching a smart-folder rule, evaluated by
    /// paging through the whole library — no row ceiling, so a
    /// match older than the first 100,000 rows is still found
    /// (Audit 8 D21; Python evaluates smart folders without a
    /// ceiling). Feed titles, tags and opened/updated facts come
    /// from this database, as the retroactive rule application
    /// reads them.
    pub fn articles_matching_rule(
        &self,
        rule: &serde_json::Value,
    ) -> rusqlite::Result<Vec<Article>> {
        const PAGE: usize = 1_000;
        let feed_titles: std::collections::HashMap<String, String> =
            self.feeds()?.into_iter().map(|f| (f.id, f.title)).collect();
        let tags = self.all_article_tags()?;
        let opened = self.opened_article_ids()?;
        let updated = self.updated_article_ids()?;
        let empty_tags: Vec<String> = Vec::new();
        let mut matched = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self.articles_page(None, PAGE, offset)?;
            let full_page = page.len() == PAGE;
            for a in page {
                let view = crate::rules::RuleArticle {
                    article: &a,
                    feed_title: feed_titles
                        .get(&a.feed_id)
                        .map(|s| s.as_str())
                        .unwrap_or(""),
                    opened: opened.contains(&a.id),
                    updated: updated.contains(&a.id),
                    tags: tags.get(&a.id).unwrap_or(&empty_tags),
                };
                if crate::rules::rule_matches(rule, &view) {
                    matched.push(a);
                }
            }
            if !full_page {
                break;
            }
            offset += PAGE;
        }
        Ok(matched)
    }

    pub fn mark_all_read(&self, feed_id: Option<&str>) -> rusqlite::Result<usize> {
        let n = self.conn.execute(
            "UPDATE articles SET is_read = 1 WHERE is_read = 0 \
             AND (?1 IS NULL OR feed_id = ?1)",
            params![feed_id],
        )?;
        Ok(n)
    }

    pub fn favorites(&self, limit: usize) -> rusqlite::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles WHERE is_favorite = 1 ORDER BY date DESC, id DESC \
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Article {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                title: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                url: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                content: r.get(4)?,
                description: r.get(5)?,
                date: r.get(6)?,
                author: r.get(7)?,
                is_read: r.get::<_, i64>(8)? != 0,
                is_favorite: r.get::<_, i64>(9)? != 0,
                media_url: r.get(10)?,
                media_type: r.get(11)?,
            })
        })?;
        rows.collect()
    }

    /// Plain substring search over title, content, and description
    /// (case-insensitive via LIKE). Python's full search has more
    /// ranking rules; this is the portable core of it.
    pub fn search_articles(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Article>> {
        self.search_articles_impl(query, limit, false)
    }

    fn search_articles_impl(
        &self,
        query: &str,
        limit: usize,
        light: bool,
    ) -> rusqlite::Result<Vec<Article>> {
        // The match still runs against the stored body; only the
        // returned rows skip hauling it (see articles_light).
        let content_col = if light { "NULL" } else { "content" };
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, feed_id, title, url, {content_col}, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles \
             WHERE title LIKE ?1 ESCAPE '\\' OR content LIKE ?1 ESCAPE '\\' \
                OR description LIKE ?1 ESCAPE '\\' \
             ORDER BY date DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![like, limit as i64], article_row)?;
        rows.collect()
    }

    /// Insert an article if its id is new; returns true when inserted.
    /// Refresh dedupes on the article id, mirroring the Python store.
    /// Both outcomes record a change-history version (Python
    /// providers/local.py): a first fetch seeds the original, a
    /// later fetch appends only when (title, content) changed.
    /// A row already owned by a DIFFERENT feed is not this article:
    /// no version is recorded against the other feed's history
    /// (feed-scoped identity is resolved by the caller in refresh).
    pub fn insert_article_if_new(&self, a: &Article) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO articles \
             (id, feed_id, title, url, content, description, date, author, \
              is_read, is_favorite, media_url, media_type) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                a.id,
                a.feed_id,
                a.title,
                a.url,
                a.content,
                a.description,
                a.date,
                a.author,
                a.is_read as i64,
                a.is_favorite as i64,
                a.media_url,
                a.media_type
            ],
        )?;
        if n > 0 {
            let _ = self.record_article_version(&a.id, &a.title, a.content.as_deref());
            return Ok(true);
        }
        let owner = self
            .conn
            .query_row(
                "SELECT feed_id FROM articles WHERE id = ?1",
                params![a.id],
                |r| r.get::<_, String>(0),
            )
            .ok();
        if owner.as_deref() == Some(a.feed_id.as_str()) {
            let _ = self.record_article_version(&a.id, &a.title, a.content.as_deref());
        }
        Ok(false)
    }

    /// Publisher-owned fields of a stored article, for the refresh
    /// metadata sync (Python providers/local.py existing-article
    /// path). Local state (read/favorite, stored body, labels) is
    /// deliberately not part of this shape.
    pub fn existing_article_meta(
        &self,
        article_id: &str,
    ) -> rusqlite::Result<Option<ExistingArticleMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, description, media_url, media_type, chapter_url, url \
             FROM articles WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![article_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(ExistingArticleMeta {
                date: row.get(0)?,
                description: row.get(1)?,
                media_url: row.get(2)?,
                media_type: row.get(3)?,
                chapter_url: row.get(4)?,
                url: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })),
            None => Ok(None),
        }
    }

    /// Sync publisher-owned metadata onto an existing article
    /// (Python updates date, description, URL, enclosure/type and
    /// chapter URL when any of them changed). Read/favorite state,
    /// the stored body, tags and labels are local and are never
    /// touched; a changed feed body is appended to change history
    /// only. `chapter_url` is None when the caller has no chapter
    /// source (the Rust ingestion pipeline carries none today), in
    /// which case the stored value is preserved rather than
    /// cleared. Returns true when a column changed.
    pub fn sync_existing_article_metadata(
        &self,
        article_id: &str,
        a: &Article,
        chapter_url: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let Some(meta) = self.existing_article_meta(article_id)? else {
            return Ok(false);
        };
        // Self-heal stored webpage URLs (Python): adopt the feed's
        // current link; clear a stale stored URL that was really
        // the media enclosure; never wipe a good URL just because
        // the feed temporarily omits its link.
        let url_to_store = if !a.url.is_empty() && a.url != meta.url {
            a.url.clone()
        } else if a.url.is_empty()
            && !meta.url.is_empty()
            && (a.media_url.as_deref() == Some(meta.url.as_str())
                || crate::media::media_type_for_url(&meta.url).is_some())
        {
            String::new()
        } else {
            meta.url.clone()
        };
        let chapter_to_store = chapter_url
            .map(|s| Some(s.to_string()))
            .unwrap_or_else(|| meta.chapter_url.clone());
        let norm = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
        let changed = norm(&a.date) != norm(&meta.date)
            || norm(&a.description) != norm(&meta.description)
            || norm(&a.media_url) != norm(&meta.media_url)
            || norm(&a.media_type) != norm(&meta.media_type)
            || chapter_to_store != meta.chapter_url
            || url_to_store != meta.url;
        if changed {
            self.conn.execute(
                "UPDATE articles SET date = ?2, description = ?3, media_url = ?4, \
                 media_type = ?5, chapter_url = ?6, url = ?7 WHERE id = ?1",
                params![
                    article_id,
                    a.date,
                    a.description,
                    a.media_url,
                    a.media_type,
                    chapter_to_store,
                    url_to_store
                ],
            )?;
        }
        let _ = self.record_article_version(article_id, &a.title, a.content.as_deref());
        Ok(changed)
    }

    /// Sync server-owned article fields without replacing local history, tags, or labels.
    pub fn upsert_hosted_article(&self, a: &Article) -> rusqlite::Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let changed = tx.execute(
            "UPDATE articles SET feed_id = ?2, title = ?3, url = ?4, content = ?5, \
             description = ?6, date = ?7, author = ?8, is_read = ?9, is_favorite = ?10, \
             media_url = COALESCE(?11, media_url), media_type = COALESCE(?12, media_type) WHERE id = ?1",
            params![a.id, a.feed_id, a.title, a.url, a.content, a.description, a.date, a.author,
                a.is_read as i64, a.is_favorite as i64, a.media_url, a.media_type],
        )?;
        if changed == 0 {
            tx.execute(
                "INSERT INTO articles (id, feed_id, title, url, content, description, date, author, \
                 is_read, is_favorite, media_url, media_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![a.id, a.feed_id, a.title, a.url, a.content, a.description, a.date, a.author,
                    a.is_read as i64, a.is_favorite as i64, a.media_url, a.media_type],
            )?;
        }
        self.record_article_version(&a.id, &a.title, a.content.as_deref())?;
        tx.commit()?;
        Ok(changed == 0)
    }

    /// Append a change-history version for an article, deduped
    /// by the SHA-256 of title + NUL + content (Python
    /// `record_article_version`). Returns true when a row was
    /// written.
    pub fn record_article_version(
        &self,
        article_id: &str,
        title: &str,
        content: Option<&str>,
    ) -> rusqlite::Result<bool> {
        use sha2::Digest;
        if article_id.trim().is_empty() {
            return Ok(false);
        }
        let mut h = sha2::Sha256::new();
        h.update(title.as_bytes());
        h.update([0u8]);
        h.update(content.unwrap_or("").as_bytes());
        let hash = h
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let latest: Option<String> = self
            .conn
            .query_row(
                "SELECT content_hash FROM article_versions \
                 WHERE article_id = ?1 \
                 ORDER BY captured_at DESC, id DESC LIMIT 1",
                params![article_id],
                |r| r.get(0),
            )
            .ok();
        if latest.as_deref() == Some(hash.as_str()) {
            return Ok(false);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.conn.execute(
            "INSERT INTO article_versions \
             (article_id, captured_at, content_hash, title, content) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![article_id, now, hash, title, content],
        )?;
        Ok(true)
    }

    /// An article's change history, newest first.
    pub fn article_versions(&self, article_id: &str) -> rusqlite::Result<Vec<ArticleVersion>> {
        let mut stmt = self.conn.prepare(
            "SELECT captured_at, title, content FROM article_versions \
             WHERE article_id = ?1 ORDER BY captured_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![article_id], |r| {
            Ok(ArticleVersion {
                captured_at: r.get(0)?,
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                content: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            })
        })?;
        rows.collect()
    }

    pub fn count_article_versions(&self, article_id: &str) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM article_versions WHERE article_id = ?1",
            params![article_id],
            |r| r.get(0),
        )
    }

    /// Ids of articles with more than one recorded version
    /// (the smart-folder `updated` fact).
    pub fn updated_article_ids(&self) -> rusqlite::Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT article_id FROM article_versions \
             GROUP BY article_id HAVING COUNT(*) > 1",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// Ids of articles the user has opened (the smart-folder
    /// `opened` fact).
    pub fn opened_article_ids(&self) -> rusqlite::Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM articles WHERE opened_at IS NOT NULL AND opened_at > 0")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }
}

// Read the original Python representation and the JSON written by early ports.
fn decode_tags(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_else(|_| {
        raw.lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
}
