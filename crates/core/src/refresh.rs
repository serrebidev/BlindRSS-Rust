//! Feed refresh: fetch, parse, store new articles (Stage 2, slice 2).
//!
//! Mirrors Python `providers/local.py` semantics: the article id is the
//! feed entry's own id, existing articles keep their stored state (only
//! a changed date is updated), new articles start unread. One deliberate
//! simplification for this slice: the body the feed itself carries
//! (content or summary) is stored immediately, where Python's listing
//! path stores an empty body and fills content through later paths.

use crate::db::{Article, Db, Feed};
use crate::feeds::{ParsedEntry, fetch_and_parse};

fn entry_to_article(feed_id: &str, e: &ParsedEntry) -> Article {
    let content = if !e.content_text.trim().is_empty() {
        e.content_text.clone()
    } else {
        e.summary.clone()
    };
    Article {
        id: e.id.clone(),
        feed_id: feed_id.to_string(),
        title: if e.title.trim().is_empty() {
            "No Title".to_string()
        } else {
            e.title.clone()
        },
        url: e.url.clone(),
        content: Some(content),
        description: Some(e.summary.clone()),
        date: e.date.clone(),
        author: e.author.clone(),
        is_read: false,
        is_favorite: false,
        media_url: e.media_url.clone(),
        media_type: None,
    }
}

/// Store an already-parsed feed; returns how many new articles were
/// stored. Split from fetching so a UI can fetch on a worker thread and
/// store on its main thread, where the database connection lives.
pub fn store_parsed(
    db: &Db,
    feed: &Feed,
    parsed: &crate::feeds::ParsedFeed,
) -> Result<usize, String> {
    let mut inserted = 0;
    for entry in &parsed.entries {
        let article = entry_to_article(&feed.id, entry);
        if db
            .insert_article_if_new(&article)
            .map_err(|e| format!("store failed: {e}"))?
        {
            inserted += 1;
        }
    }
    Ok(inserted)
}

/// Refresh one feed; returns how many new articles were stored.
pub fn refresh_feed(db: &Db, feed: &Feed) -> Result<usize, String> {
    let parsed = fetch_and_parse(&feed.url)?;
    store_parsed(db, feed, &parsed)
}

/// Refresh every feed, collecting per-feed results. One feed failing
/// does not stop the others (Python behaves the same way).
pub fn refresh_all(db: &Db) -> Vec<(String, Result<usize, String>)> {
    let feeds = match db.feeds() {
        Ok(f) => f,
        Err(e) => return vec![("<database>".to_string(), Err(format!("{e}")))],
    };
    feeds
        .iter()
        .map(|f| (f.title.clone(), refresh_feed(db, f)))
        .collect()
}
