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
    // Deleted articles must not resurrect on refresh (Python checks
    // tombstones the same way).
    let tombstones = db.deleted_article_ids().unwrap_or_default();
    // Filter-rule pipeline (Python applies enabled rules to articles
    // as they arrive). Missing table (older fixtures) = no rules.
    let rules = db.filter_rules().unwrap_or_default();
    let mut inserted = 0;
    for entry in &parsed.entries {
        if tombstones.contains(&entry.id) {
            continue;
        }
        let article = entry_to_article(&feed.id, entry);
        if db
            .insert_article_if_new(&article)
            .map_err(|e| format!("store failed: {e}"))?
        {
            inserted += 1;
            if !rules.is_empty() {
                apply_filter_pipeline(db, &rules, &article, &parsed.title);
            }
        }
    }
    Ok(inserted)
}

/// Apply the filter-rule pipeline to one freshly stored article.
fn apply_filter_pipeline(
    db: &Db,
    rules: &[crate::rules::FilterRule],
    article: &Article,
    feed_title: &str,
) {
    let view = crate::rules::RuleArticle {
        article,
        feed_title,
        opened: false,
        updated: false,
        tags: &[],
    };
    let eff = crate::rules::evaluate_pipeline(rules, &view);
    if eff.mark_read {
        let _ = db.mark_read(&article.id, true);
    }
    if eff.mark_favorite {
        let _ = db.set_favorite(&article.id, true);
    }
    for label in &eff.labels {
        let _ = db.add_article_label(&article.id, label);
    }
    if let Some(target) = &eff.move_to {
        let _ = db.set_article_category_override(&article.id, Some(target));
    }
    if eff.delete {
        let _ = db.delete_article(&article.id);
    }
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
