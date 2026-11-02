//! Feed refresh: fetch, parse, store new articles (Stage 2, slice 2).
//!
//! Mirrors Python `providers/local.py` semantics: the article id is the
//! feed entry's own id, existing articles keep their stored state (only
//! a changed date is updated), new articles start unread. One deliberate
//! simplification for this slice: the body the feed itself carries
//! (content or summary) is stored immediately, where Python's listing
//! path stores an empty body and fills content through later paths.

use crate::db::{Article, Db, Feed};
use crate::feeds::ParsedEntry;

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
        media_type: e.media_type.clone(),
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
            // NPR / Sky media attach (Python's generic parse path):
            // those program feeds ship no enclosure, so resolve the
            // audio for media-less new articles at store time.
            if article.media_url.is_none() {
                let attach = if crate::sources::is_npr_url(&article.url) {
                    crate::sources::npr_audio(&article.url)
                } else if crate::sources::is_sky_news_url(&article.url) {
                    crate::sources::sky_podcast_audio(
                        &article.url,
                        &article.title,
                        &entry.content_text,
                    )
                } else {
                    None
                };
                if let Some((mu, mt)) = attach {
                    let _ = db.update_article_media(&article.id, &mu, Some(&mt));
                }
            }
            if !entry.tags.is_empty() {
                db.apply_enrichment(
                    &article.id,
                    &crate::enrich::EnrichMeta {
                        tags: entry.tags.clone(),
                        ..Default::default()
                    },
                )
                .map_err(|e| format!("store tags failed: {e}"))?;
            }
            if !rules.is_empty() {
                let view = crate::rules::RuleArticle {
                    article: &article,
                    feed_title: &parsed.title,
                    opened: false,
                    updated: false,
                    tags: &entry.tags,
                };
                apply_filter_pipeline(
                    db,
                    &crate::rules::evaluate_pipeline(&rules, &view),
                    &article,
                );
            }
        }
    }
    Ok(inserted)
}

/// Apply the filter-rule pipeline to one freshly stored article.
fn apply_filter_pipeline(db: &Db, eff: &crate::rules::EffectiveActions, article: &Article) {
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
    // Listing sources (Batch 35): subscriptions with no native RSS
    // are enumerated by their source fetcher, mirroring the branch
    // order of Python `providers/local.py::_refresh_single_feed`.
    if let Some(kind) = crate::sources::classify(&feed.url) {
        // Python persists a normalized listing URL back into the
        // feeds row at refresh (the Rumble and Odysee branches of
        // `_refresh_single_feed` run `UPDATE feeds SET url`).
        let normalized = match kind {
            crate::sources::SourceKind::Odysee => {
                Some(crate::sources::normalize_odysee_url(&feed.url))
            }
            crate::sources::SourceKind::Rumble => {
                Some(crate::sources::normalize_rumble_feed_url(&feed.url))
            }
            _ => None,
        };
        let mut feed_owned: Option<Feed> = None;
        if let Some(n) = normalized
            && n != feed.url
        {
            let _ = db.update_feed(&feed.id, None, Some(&n));
            let mut f = feed.clone();
            f.url = n;
            feed_owned = Some(f);
        }
        let feed = feed_owned.as_ref().unwrap_or(feed);
        let existing = db.article_count_for_feed(&feed.id).unwrap_or(0);
        let (title, items) = crate::sources::fetch_listing(&feed.url, kind, &feed.id, existing)?;
        let parsed = crate::feeds::ParsedFeed {
            title,
            entries: crate::sources::items_to_entries(items),
        };
        return store_parsed(db, feed, &parsed);
    }
    let parsed = match crate::feeds::fetch_with_cache(db, &feed.url, false) {
        Ok(p) => p,
        Err(e) => {
            // YouTube channel feeds answer 404/500 at random for
            // some channels (issue #107): fall back to listing the
            // uploads playlist through yt-dlp.
            if let Some(cid) = crate::sources::youtube_channel_id(&feed.url) {
                let (title, items) =
                    crate::sources::fetch_youtube_channel_items(&cid, 30).map_err(|_| e.clone())?;
                let parsed = crate::feeds::ParsedFeed {
                    title,
                    entries: crate::sources::items_to_entries(shape_stable_ids(db, feed, items)),
                };
                return store_parsed(db, feed, &parsed);
            }
            return Err(e);
        }
    };
    let stored = store_parsed(db, feed, &parsed)?;
    // One-time channel history backfill (issue #109): the RSS feed
    // carries only the newest 15 uploads, so list the whole uploads
    // playlist once and store it under the same stable ids.
    if let Some(cid) = crate::sources::youtube_channel_id(&feed.url) {
        youtube_history_backfill(db, feed, &cid);
    }
    Ok(stored)
}

/// Shape channel-listing items for storage: an id already owned by
/// a DIFFERENT feed gets this feed's prefix (Python's rule), so two
/// subscriptions to one channel never steal each other's rows.
fn shape_stable_ids(
    db: &Db,
    feed: &Feed,
    items: Vec<crate::sources::SourceItem>,
) -> Vec<crate::sources::SourceItem> {
    items
        .into_iter()
        .map(|mut it| {
            if let Ok(Some(existing)) = db.article_by_id(&it.id)
                && existing.feed_id != feed.id
            {
                it.id = format!("{}:{}", feed.id, it.id);
            }
            it
        })
        .collect()
}

fn youtube_history_backfill(db: &Db, feed: &Feed, channel_id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (state, attempted) = db.youtube_history_state(&feed.id).unwrap_or((0, 0));
    if state == 2 {
        return; // done (Python's _YOUTUBE_HISTORY_DONE)
    }
    if state == 1 && now - attempted < 86_400 {
        return; // failed attempt retries after a day
    }
    match crate::sources::fetch_youtube_channel_items(channel_id, 500) {
        Ok((_title, items)) => {
            let parsed = crate::feeds::ParsedFeed {
                title: feed.title.clone(),
                entries: crate::sources::items_to_entries(shape_stable_ids(db, feed, items)),
            };
            // History items are stored unread but are NOT new-item
            // news: the count is deliberately discarded.
            let _ = store_parsed(db, feed, &parsed);
            let _ = db.set_youtube_history_state(&feed.id, 2, now);
        }
        Err(_) => {
            let _ = db.set_youtube_history_state(&feed.id, 1, now);
        }
    }
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

/// Apply the whole filter-rule pipeline to articles already in
/// the database (the rules dialog's "Apply to existing articles").
/// Returns how many articles any rule acted on.
pub fn apply_rules_to_existing(db: &Db) -> Result<usize, String> {
    let rules = db.filter_rules().map_err(|e| format!("{e}"))?;
    if rules.is_empty() {
        return Ok(0);
    }
    let feed_titles: std::collections::HashMap<String, String> = db
        .feeds()
        .map_err(|e| format!("{e}"))?
        .into_iter()
        .map(|f| (f.id, f.title))
        .collect();
    let articles = db.articles(None, 100_000).map_err(|e| format!("{e}"))?;
    let mut changed = 0usize;
    let tags = db.all_article_tags().map_err(|e| format!("{e}"))?;
    let opened = db.opened_article_ids().map_err(|e| format!("{e}"))?;
    let updated = db.updated_article_ids().map_err(|e| format!("{e}"))?;
    let empty_tags = Vec::new();
    for article in &articles {
        let view = crate::rules::RuleArticle {
            article,
            feed_title: feed_titles
                .get(&article.feed_id)
                .map(|s| s.as_str())
                .unwrap_or(""),
            opened: opened.contains(&article.id),
            updated: updated.contains(&article.id),
            tags: tags.get(&article.id).unwrap_or(&empty_tags),
        };
        let eff = crate::rules::evaluate_pipeline(&rules, &view);
        if eff.mark_read
            || eff.mark_favorite
            || eff.delete
            || eff.move_to.is_some()
            || !eff.labels.is_empty()
        {
            changed += 1;
            apply_filter_pipeline(db, &eff, article);
        }
    }
    Ok(changed)
}
