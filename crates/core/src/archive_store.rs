//! Python-compatible persistent archive state and hosted-account sidecar.

use crate::{Article, archive::ArchiveEpisode};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

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
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
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
    for episode in episodes.iter().filter(|e| e.entry.media_url.is_some()) {
        let entry = &episode.entry;
        transaction.execute("INSERT INTO hosted_podcast_archive_entries(provider_scope,feed_id,episode_key,title,url,content,date,author,media_url,media_type) \
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![scope,feed_id,episode.key(),entry.title,entry.url,entry.content_text,entry.date,entry.author,entry.media_url,entry.media_type]).map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())
}

pub fn entries(path: &Path, scope: &str, feed_id: &str) -> Result<Vec<Article>, String> {
    let connection = open(path)?;
    let mut statement = connection.prepare("SELECT episode_key,title,url,content,date,author,media_url,media_type FROM hosted_podcast_archive_entries \
        WHERE provider_scope=?1 AND feed_id=?2 ORDER BY date DESC,title DESC").map_err(|e| e.to_string())?;
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
