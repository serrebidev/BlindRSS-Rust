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
}

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

pub struct Db {
    conn: Connection,
}

impl Db {
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
        Ok(Self { conn })
    }

    pub fn categories(&self) -> rusqlite::Result<Vec<Category>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title FROM categories ORDER BY title")?;
        let rows = stmt.query_map([], |r| {
            Ok(Category {
                id: r.get(0)?,
                title: r.get(1)?,
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
        self.conn.execute(
            "UPDATE categories SET title = ?2 WHERE title = ?1",
            params![old_title, new_title],
        )?;
        self.conn.execute(
            "UPDATE feeds SET category = ?2 WHERE category = ?1",
            params![old_title, new_title],
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
        };
        self.conn.execute(
            "INSERT INTO categories (id, title) VALUES (?1, ?2)",
            params![cat.id, cat.title],
        )?;
        Ok(cat)
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
        Ok(n > 0)
    }
}
