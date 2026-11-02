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

/// One recorded change-history version of an article
/// (Python `article_versions` row, newest first when listed).
#[derive(Debug, Clone, PartialEq)]
pub struct ArticleVersion {
    pub captured_at: f64,
    pub title: String,
    pub content: String,
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
CREATE TABLE IF NOT EXISTS deleted_articles (feed_id TEXT,
    article_id TEXT, url TEXT, deleted_at REAL, title TEXT, content TEXT,
    description TEXT, date TEXT, author TEXT, media_url TEXT,
    media_type TEXT, chapter_url TEXT);
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
    feed_id TEXT PRIMARY KEY, state INTEGER NOT NULL DEFAULT 0,
    attempted_at INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS hosted_article_media (
    cache_key TEXT PRIMARY KEY, media_url TEXT, media_type TEXT,
    checked_at INTEGER NOT NULL DEFAULT 0);
";

/// Columns Python added to existing tables by migration; older
/// databases get them on open.
const MIGRATION_COLUMNS: [(&str, &str, &str); 11] = [
    ("feeds", "last_error", "TEXT"),
    ("feeds", "last_error_at", "REAL"),
    ("feeds", "last_success_at", "REAL"),
    ("feeds", "consecutive_failures", "INTEGER DEFAULT 0"),
    ("categories", "parent_id", "TEXT"),
    ("articles", "category_override", "TEXT"),
    ("feeds", "delete_behavior", "TEXT"),
    ("deleted_articles", "purged", "INTEGER DEFAULT 0"),
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
        let sql = "SELECT id, feed_id, title, url, content, description, date, \
                   author, is_read, is_favorite, media_url, media_type \
                   FROM articles \
                   WHERE (?1 IS NULL OR feed_id = ?1) \
                   ORDER BY date DESC, id DESC LIMIT ?2";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![feed_id, limit as i64], |r| {
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

    pub fn unread_count(&self, feed_id: Option<&str>) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM articles WHERE is_read = 0 \
             AND (?1 IS NULL OR feed_id = ?1)",
            params![feed_id],
            |r| r.get(0),
        )
    }

    /// One page of articles for an Accessible Browser view id
    /// (Python provider `get_articles_page`): "all",
    /// "unread:all", "read:all", "favorites:all",
    /// "category:<path>", or a bare feed id. Returns the page
    /// (date DESC, id DESC) and the view's total count.
    pub fn articles_page(
        &self,
        view_id: &str,
        offset: usize,
        limit: usize,
    ) -> rusqlite::Result<(Vec<Article>, i64)> {
        let (filter, base) = match view_id.split_once(':') {
            Some(("unread", rest)) => (" AND is_read = 0", rest),
            Some(("read", rest)) => (" AND is_read = 1", rest),
            Some(("favorites", rest)) => (" AND is_favorite = 1", rest),
            _ => ("", view_id),
        };
        if let Some(path) = base.strip_prefix("category:") {
            // Category membership carries override/label rules;
            // reuse the full query and slice the page from it.
            let all = self.articles_for_category(path, usize::MAX / 2)?;
            let filtered: Vec<Article> = all
                .into_iter()
                .filter(|a| {
                    filter.is_empty()
                        || (filter.contains("is_read = 0") && !a.is_read)
                        || (filter.contains("is_read = 1") && a.is_read)
                        || (filter.contains("is_favorite") && a.is_favorite)
                })
                .collect();
            let total = filtered.len() as i64;
            let page = filtered.into_iter().skip(offset).take(limit).collect();
            return Ok((page, total));
        }
        let feed_clause = if base == "all" || base.is_empty() {
            String::new()
        } else {
            " AND feed_id = ?1".to_string()
        };
        let total: i64 = if feed_clause.is_empty() {
            self.conn.query_row(
                &format!("SELECT COUNT(*) FROM articles WHERE 1 = 1{filter}"),
                [],
                |r| r.get(0),
            )?
        } else {
            self.conn.query_row(
                &format!("SELECT COUNT(*) FROM articles WHERE 1 = 1{filter}{feed_clause}"),
                params![base],
                |r| r.get(0),
            )?
        };
        let sql = format!(
            "SELECT id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles WHERE 1 = 1{filter}{feed_clause} \
             ORDER BY date DESC, id DESC LIMIT ? OFFSET ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let map_row = |r: &rusqlite::Row| {
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
        };
        let rows = if feed_clause.is_empty() {
            stmt.query_map(params![limit as i64, offset as i64], map_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map(params![base, limit as i64, offset as i64], map_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok((rows, total))
    }

    /// One article by id (provider `get_article_by_id`).
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

    /// YouTube channel history-backfill state for a feed:
    /// (state, attempted_at epoch). State 2 = done (Python
    /// `db._YOUTUBE_HISTORY_DONE`); 1 = attempted, retry after a day.
    pub fn youtube_history_state(&self, feed_id: &str) -> rusqlite::Result<(i64, i64)> {
        let mut stmt = self
            .conn
            .prepare("SELECT state, attempted_at FROM youtube_history_state WHERE feed_id = ?1")?;
        let mut rows = stmt.query_map(params![feed_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        match rows.next() {
            Some(r) => Ok(r?),
            None => Ok((0, 0)),
        }
    }

    pub fn set_youtube_history_state(
        &self,
        feed_id: &str,
        state: i64,
        attempted_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO youtube_history_state (feed_id, state, attempted_at) \
             VALUES (?1, ?2, ?3) ON CONFLICT(feed_id) DO UPDATE SET \
             state = excluded.state, attempted_at = excluded.attempted_at",
            params![feed_id, state, attempted_at],
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

    /// Rename a category: the categories row and every feed's
    /// category title string move together (Python semantics).
    pub fn rename_category(&self, old_title: &str, new_title: &str) -> rusqlite::Result<()> {
        // Renaming a category renames its whole subtree: descendant
        // category paths and feed assignments keep their tails.
        self.conn.execute(
            "UPDATE categories SET title = ?2 WHERE title = ?1",
            params![old_title, new_title],
        )?;
        self.conn.execute(
            "UPDATE categories SET title = ?2 || substr(title, ?3) \
             WHERE title LIKE ?1",
            params![
                format!("{old_title}/%"),
                format!("{new_title}/"),
                (old_title.len() + 2) as i64
            ],
        )?;
        self.conn.execute(
            "UPDATE feeds SET category = ?2 WHERE category = ?1",
            params![old_title, new_title],
        )?;
        self.conn.execute(
            "UPDATE feeds SET category = ?2 || substr(category, ?3) \
             WHERE category LIKE ?1",
            params![
                format!("{old_title}/%"),
                format!("{new_title}/"),
                (old_title.len() + 2) as i64
            ],
        )?;
        Ok(())
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

    /// Ensure a category path exists ("News/Tech"), creating any
    /// missing rows with parent links (Python's local provider
    /// stores hierarchy natively in categories.parent_id while the
    /// title carries the full path). Returns the leaf's id.
    pub fn ensure_category_path(&self, path: &str) -> rusqlite::Result<String> {
        let mut parent_id: Option<String> = None;
        let mut current = String::new();
        for segment in path.split('/').filter(|s| !s.trim().is_empty()) {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(segment.trim());
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

    /// Delete a category by full path (Python delete_category):
    /// direct children and their feeds are reparented to the
    /// deleted node's parent, shortening their paths; the node
    /// itself is removed. Refuses when a reparented child would
    /// collide with an existing category of the same name.
    pub fn delete_category_path(&self, path: &str) -> rusqlite::Result<bool> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, parent_id FROM categories WHERE title = ?1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((cat_id, parent_id)) = row else {
            return Ok(false);
        };
        let parent_path: Option<String> = match &parent_id {
            Some(pid) => self
                .conn
                .query_row(
                    "SELECT title FROM categories WHERE id = ?1",
                    params![pid],
                    |r| r.get(0),
                )
                .ok(),
            None => None,
        };
        let old_prefix = format!("{path}/");
        let new_prefix = parent_path
            .as_ref()
            .map(|p| format!("{p}/"))
            .unwrap_or_default();
        let cats = self.categories()?;
        // Collision check: a shortened descendant path must not
        // land on an existing category outside the subtree.
        for c in &cats {
            if let Some(rest) = c.title.strip_prefix(&old_prefix) {
                let target = format!("{new_prefix}{rest}");
                if cats
                    .iter()
                    .any(|o| o.title == target && o.id != c.id && !o.title.starts_with(&old_prefix))
                {
                    return Ok(false);
                }
            }
        }
        // Rewrite descendant category paths, then feeds, then the
        // node itself.
        for c in &cats {
            if let Some(rest) = c.title.strip_prefix(&old_prefix) {
                let target = format!("{new_prefix}{rest}");
                self.conn.execute(
                    "UPDATE categories SET title = ?2 WHERE id = ?1",
                    params![c.id, target],
                )?;
            }
        }
        self.conn.execute(
            "UPDATE feeds SET category = ?2 WHERE category = ?1",
            params![path, parent_path],
        )?;
        self.conn.execute(
            "UPDATE feeds SET category = ?2 || substr(category, ?3) \
             WHERE category LIKE ?1",
            params![
                format!("{old_prefix}%"),
                new_prefix,
                (old_prefix.len() + 1) as i64
            ],
        )?;
        // Children of the deleted node now belong to its parent.
        self.conn.execute(
            "UPDATE categories SET parent_id = ?2 WHERE parent_id = ?1",
            params![cat_id, parent_id],
        )?;
        self.conn
            .execute("DELETE FROM categories WHERE id = ?1", params![cat_id])?;
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
        let rows = stmt.query_map(params.as_slice(), |r| {
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
        // The subtree name set: the path plus every descendant path
        // known to the categories table or carried by a feed.
        let mut names: Vec<String> = vec![path.to_string()];
        let prefix = format!("{path}/");
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
        let sql = format!(
            "SELECT a.id, a.feed_id, a.title, a.url, a.content, a.description, a.date, \
             a.author, a.is_read, a.is_favorite, a.media_url, a.media_type \
             FROM articles a JOIN feeds f ON f.id = a.feed_id \
             WHERE ((a.category_override IS NULL AND {feed_match}) \
             OR {override_match} OR {label_match}) \
             ORDER BY COALESCE(a.date, '') DESC LIMIT ?"
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
        params.push(&limit_i);
        let rows = stmt.query_map(params.as_slice(), |r| {
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

    pub fn remove_category(&self, category_id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM categories WHERE id = ?1", params![category_id])?;
        Ok(())
    }

    /// Delete an article, keeping a tombstone copy in deleted_articles
    /// (Python schema) so it can be restored and so refresh does not
    /// resurrect it. Returns false when the article does not exist.
    pub fn delete_article(&self, article_id: &str) -> rusqlite::Result<bool> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let copied = self.conn.execute(
            "INSERT INTO deleted_articles \
             (feed_id, article_id, url, deleted_at, title, content, description, \
              date, author, media_url, media_type, chapter_url) \
             SELECT feed_id, id, url, ?1, title, content, description, date, \
                    author, media_url, media_type, chapter_url \
             FROM articles WHERE id = ?2",
            params![now, article_id],
        )?;
        if copied == 0 {
            return Ok(false);
        }
        self.conn
            .execute("DELETE FROM articles WHERE id = ?1", params![article_id])?;
        Ok(true)
    }

    /// Retention: delete read, non-favorite articles older than
    /// `keep_days` (by article date when parseable, else by row
    /// position — dates are ISO text in this schema), plus
    /// tombstones older than the same window. Returns rows removed.
    pub fn prune_old_articles(&self, keep_days: i64) -> rusqlite::Result<usize> {
        let cutoff = {
            // ISO date string keep_days in the past, matching the
            // schema's ISO `date` text format.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let then = now - keep_days * 86_400;
            let days = then.div_euclid(86_400);
            // Days since epoch -> (y, m, d) via civil algorithm.
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z.rem_euclid(146_097);
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let y = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = if m <= 2 { y + 1 } else { y };
            format!("{y:04}-{m:02}-{d:02}")
        };
        let removed = self.conn.execute(
            "DELETE FROM articles WHERE is_read = 1 AND is_favorite = 0 \
             AND date != '' AND date < ?1",
            params![cutoff],
        )?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let tomb_cutoff = now - (keep_days as f64) * 86_400.0;
        let removed_tomb = self.conn.execute(
            "DELETE FROM deleted_articles WHERE deleted_at < ?1",
            params![tomb_cutoff],
        )?;
        Ok(removed + removed_tomb)
    }

    /// Permanently delete an article (Python delete_behavior
    /// "purge"): the tombstone is kept but flagged purged, so
    /// it stays out of the Deleted view and cannot be restored.
    pub fn purge_article(&self, article_id: &str) -> rusqlite::Result<bool> {
        let deleted = self.delete_article(article_id)?;
        self.conn.execute(
            "UPDATE deleted_articles SET purged = 1 WHERE article_id = ?1",
            params![article_id],
        )?;
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
        let prefix = format!("{path}/");
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
        let prefix = format!("{path}/");
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
        if !title_only {
            return self.search_articles(query, limit);
        }
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare(
            "SELECT id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles \
             WHERE title LIKE ?1 ESCAPE '\\' \
             ORDER BY date DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, limit as i64], |r| {
            Ok(Article {
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
            })
        })?;
        rows.collect()
    }

    /// Restore a deleted article from its tombstone. Read/favorite state
    /// is not part of the tombstone schema, so a restored article comes
    /// back unread and unfavorited (same as Python).
    pub fn restore_article(&self, article_id: &str) -> rusqlite::Result<bool> {
        let restored = self.conn.execute(
            "INSERT OR IGNORE INTO articles \
             (id, feed_id, title, url, content, description, date, author, \
              is_read, is_favorite, media_url, media_type, chapter_url) \
             SELECT article_id, feed_id, title, url, content, description, \
                    date, author, 0, 0, media_url, media_type, chapter_url \
             FROM deleted_articles WHERE article_id = ?1",
            params![article_id],
        )?;
        if restored == 0 {
            return Ok(false);
        }
        self.conn.execute(
            "DELETE FROM deleted_articles WHERE article_id = ?1",
            params![article_id],
        )?;
        Ok(true)
    }

    /// Tombstoned article ids, for the refresh resurrection check.
    pub fn deleted_article_ids(&self) -> rusqlite::Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT article_id FROM deleted_articles")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    pub fn deleted_articles(&self) -> rusqlite::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT article_id, feed_id, title, url, content, description, date, \
             author, media_url, media_type FROM deleted_articles \
             WHERE COALESCE(purged, 0) = 0 \
             ORDER BY deleted_at DESC",
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
                is_read: false,
                is_favorite: false,
                media_url: r.get(8)?,
                media_type: r.get(9)?,
            })
        })?;
        rows.collect()
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
        let like = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare(
            "SELECT id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles \
             WHERE title LIKE ?1 ESCAPE '\\' OR content LIKE ?1 ESCAPE '\\' \
                OR description LIKE ?1 ESCAPE '\\' \
             ORDER BY date DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, limit as i64], |r| {
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

    /// Insert an article if its id is new; returns true when inserted.
    /// Refresh dedupes on the article id, mirroring the Python store.
    /// Both outcomes record a change-history version (Python
    /// providers/local.py): a first fetch seeds the original, a
    /// later fetch appends only when (title, content) changed.
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
        let _ = self.record_article_version(&a.id, &a.title, a.content.as_deref());
        Ok(n > 0)
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
