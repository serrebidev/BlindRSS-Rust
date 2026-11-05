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

/// What one `store_parsed` call did (Python's per-feed refresh
/// bookkeeping): how many articles were newly stored, and which
/// of them are notification-eligible — stored, not removed by a
/// rule, and not covered by a rule's skip-notification action
/// (Python `_apply_rules_to_new_article` pops exactly those from
/// the new-items count and summaries).
#[derive(Debug, Clone, Default)]
pub struct StoreOutcome {
    pub inserted: usize,
    pub notify_count: usize,
    pub new_articles: Vec<Article>,
}

/// Store an already-parsed feed. Split from fetching so worker
/// paths that already hold a document (subscription resolution,
/// dialogs) can store through their own connection; the refresh
/// path itself fetches and stores in one worker call
/// (`refresh_feed_with_options`) — storage never runs on a UI
/// thread (Audit 8 D22). `global_delete_behavior` is
/// the global `delete_behavior` config value; a feed's own
/// `feeds.delete_behavior` override wins over it when a filter
/// rule deletes (Audit 8 D12).
pub fn store_parsed(
    db: &Db,
    feed: &Feed,
    parsed: &crate::feeds::ParsedFeed,
    global_delete_behavior: &str,
) -> Result<StoreOutcome, String> {
    store_parsed_with_retention(db, feed, parsed, global_delete_behavior, None)
}

/// Store a parsed feed, refusing NEW entries outside the retention
/// boundary (`retention_cutoff` is a `YYYY-MM-DD` string from
/// `retention::retention_cutoff_date`; None = no boundary).
pub fn store_parsed_with_retention(
    db: &Db,
    feed: &Feed,
    parsed: &crate::feeds::ParsedFeed,
    global_delete_behavior: &str,
    retention_cutoff: Option<&str>,
) -> Result<StoreOutcome, String> {
    // Deleted articles must not resurrect on refresh. Tombstones
    // are feed-scoped (Python `deleted_article_tombstones_for_feed`):
    // an entry matches on its raw id, its feed-scoped id, or its
    // URL — so a changed GUID cannot revive a deleted URL, and one
    // feed's deletions never suppress another feed's articles.
    let (tomb_ids, tomb_urls) = db.deleted_tombstones_for_feed(&feed.id).unwrap_or_default();
    // Filter-rule pipeline (Python applies enabled rules to articles
    // as they arrive). Missing table (older fixtures) = no rules.
    let rules = db.filter_rules().unwrap_or_default();
    // The feed's effective delete behavior is resolved once, the
    // way Python resolves it per feed before the entry loop.
    let behavior = db.effective_delete_behavior(&feed.id, global_delete_behavior);
    let mut outcome = StoreOutcome::default();
    for entry in &parsed.entries {
        let base_id = entry.id.trim().to_string();
        if base_id.is_empty() {
            continue;
        }
        // Feed-scoped identity (Python): when another feed already
        // owns the raw id, this feed's copy lives under
        // `{feed_id}:{base_id}` with fully independent state.
        let scoped_id = format!("{}:{}", feed.id, base_id);
        let entry_url = entry.url.trim();
        if tomb_ids.contains(&base_id)
            || tomb_ids.contains(&scoped_id)
            || (!entry_url.is_empty() && tomb_urls.contains(entry_url))
        {
            continue;
        }
        let mut article = entry_to_article(&feed.id, entry);
        // Existing row for THIS feed, under the raw or scoped id:
        // sync publisher-owned metadata and keep local state.
        let existing_id = match db.article_by_id(&base_id) {
            Ok(Some(row)) if row.feed_id == feed.id => Some(base_id.clone()),
            _ => match db.article_by_id(&scoped_id) {
                Ok(Some(row)) if row.feed_id == feed.id => Some(scoped_id.clone()),
                _ => None,
            },
        };
        if let Some(eid) = existing_id {
            db.sync_existing_article_metadata(&eid, &article, None)
                .map_err(|e| format!("store failed: {e}"))?;
            continue;
        }
        // New row. A raw id owned by a different feed means this
        // entry is stored under the scoped id instead.
        if matches!(db.article_by_id(&base_id), Ok(Some(_))) {
            article.id = scoped_id;
        } else {
            article.id = base_id;
        }
        // One retention boundary (Python issue #106): a feed keeps
        // listing entries retention has already purged; inserting
        // one would resurrect it as unread.
        if crate::retention::article_is_outside_retention(entry.date.as_deref(), retention_cutoff) {
            continue;
        }
        if db
            .insert_article_if_new(&article)
            .map_err(|e| format!("store failed: {e}"))?
        {
            outcome.inserted += 1;
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
            let mut removed = false;
            let mut skip_notification = false;
            if !rules.is_empty() {
                let view = crate::rules::RuleArticle {
                    article: &article,
                    feed_title: &parsed.title,
                    opened: false,
                    updated: false,
                    tags: &entry.tags,
                };
                let resolved = crate::rules::resolve_effective_actions(
                    &crate::rules::evaluate_pipeline(&rules, &view),
                    &behavior,
                );
                skip_notification = resolved.skip_notification;
                removed = apply_filter_pipeline(db, &resolved, &article);
            }
            // Notification eligibility (Python D13 semantics):
            // rule-removed and notification-suppressed articles
            // were stored but are not "new article" news.
            if !removed && !skip_notification {
                outcome.notify_count += 1;
                outcome.new_articles.push(article);
            }
        }
    }
    Ok(outcome)
}

/// Apply resolved filter-rule actions to one article (Python
/// `apply_effective_actions`): read/favorite flags, labels, the
/// move (an explicit move, or delete-to-category), then removal
/// through the shared delete policy. Returns true when the
/// article was removed from normal views.
fn apply_filter_pipeline(db: &Db, eff: &crate::rules::ResolvedActions, article: &Article) -> bool {
    if eff.mark_read {
        let _ = db.mark_read(&article.id, true);
    }
    if eff.mark_favorite {
        let _ = db.set_favorite(&article.id, true);
    }
    for label in &eff.labels {
        let _ = db.add_article_label(&article.id, label);
    }
    if eff.removes_article() {
        let behavior = if eff.purge {
            crate::rules::DeleteBehavior::Purge
        } else {
            crate::rules::DeleteBehavior::Deleted
        };
        let _ = db.delete_article_with_behavior(&article.id, &behavior);
        return true;
    }
    if let Some(target) = &eff.move_to {
        let _ = db.set_article_category_override(&article.id, Some(target));
    }
    false
}

/// Error returned when a refresh is cancelled before its store
/// (Python `_refresh_single_feed` returns silently on cancel; the
/// app treats this exact message as a user stop, not a failure).
pub const REFRESH_CANCELLED: &str = "refresh cancelled";

/// What one worker-side refresh did. The store — including the
/// NPR/Sky media attachment, which is network work — has fully
/// committed through the worker's own DB connection before this is
/// returned; only committed deltas travel back to the caller
/// (Audit 8 D22).
#[derive(Debug, Clone, Default)]
pub struct WorkerRefresh {
    pub outcome: StoreOutcome,
    /// The publisher's body was unchanged since the last
    /// committed store: nothing was parsed or stored (D16).
    pub not_modified: bool,
}

// Digests of the last feed bodies whose stores committed, keyed
// by feed id (Python's `_stored_feed_body_digests` on the local
// provider). In-memory on purpose: Python's map lives on the
// provider instance, so a restart re-stores each feed once.
static STORED_BODY_DIGESTS: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

fn stored_body_digest(feed_id: &str) -> Option<String> {
    let guard = STORED_BODY_DIGESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    guard.as_ref().and_then(|map| map.get(feed_id).cloned())
}

fn record_body_digest(feed_id: &str, digest: &str) {
    let mut guard = STORED_BODY_DIGESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    guard
        .get_or_insert_with(std::collections::HashMap::new)
        .insert(feed_id.to_string(), digest.to_string());
}

/// Forget a feed's stored-body digest (Python pops it whenever
/// YouTube listing items are stored: their day-only dates must
/// not make the next full RSS body look unchanged).
pub fn forget_body_digest(feed_id: &str) {
    let mut guard = STORED_BODY_DIGESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(map) = guard.as_mut() {
        map.remove(feed_id);
    }
}

fn cancelled(cancel: &std::sync::atomic::AtomicBool) -> bool {
    cancel.load(std::sync::atomic::Ordering::Relaxed)
}

/// Refresh one feed end to end on the caller's (worker) thread:
/// dispatch, fetch, store and media attachment all happen here,
/// through `db` — which must be the worker's own connection.
/// `force` bypasses the conditional request and the unchanged-
/// body fast path (Python's single-feed refresh is forced);
/// `retention_cutoff` is the shared retention boundary (`None` =
/// unlimited). Cancellation is checked before the fetch and again
/// before the store, and reported as `REFRESH_CANCELLED`.
pub fn refresh_feed_with_options(
    db: &Db,
    feed: &Feed,
    global_delete_behavior: &str,
    retention_cutoff: Option<&str>,
    force: bool,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<WorkerRefresh, String> {
    if cancelled(cancel) {
        return Err(REFRESH_CANCELLED.to_string());
    }
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
        if cancelled(cancel) {
            return Err(REFRESH_CANCELLED.to_string());
        }
        let parsed = crate::feeds::ParsedFeed {
            title,
            entries: crate::sources::items_to_entries(items),
        };
        let outcome = store_parsed_with_retention(
            db,
            feed,
            &parsed,
            global_delete_behavior,
            retention_cutoff,
        )?;
        if kind == crate::sources::SourceKind::YoutubeSearch {
            // `_store_youtube_items` pops the stored-body digest:
            // listing dates are day-only, so the next RSS body of
            // the channel must be stored again to restore times.
            forget_body_digest(&feed.id);
        }
        return Ok(WorkerRefresh {
            outcome,
            not_modified: false,
        });
    }
    // One-time channel history backfill (issue #109) runs before
    // the fetch, as in Python: the RSS feed carries only the newest
    // 15 uploads, so list the whole uploads playlist once and store
    // it under the same stable ids, whatever the RSS fetch then does.
    let channel_id = crate::sources::youtube_channel_id(&feed.url);
    if let Some(cid) = &channel_id {
        youtube_history_backfill(db, feed, cid, global_delete_behavior, retention_cutoff);
    }
    let stored = stored_body_digest(&feed.id);
    // The feed's configured proxy reaches the browser
    // recovery stage (Python local.py passes
    // feed_settings["proxy"] to the browser fallback).
    let feed_proxy = db
        .feed_settings(&feed.id)
        .ok()
        .and_then(|settings| {
            settings
                .get("proxy")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();
    let (parsed, body_digest) = match crate::feeds::fetch_feed_document_cancellable(
        db,
        &feed.url,
        force,
        stored.as_deref(),
        Some(cancel),
        &feed_proxy,
    ) {
        Ok(crate::feeds::FeedFetch::NotModified) => {
            return Ok(WorkerRefresh {
                outcome: StoreOutcome::default(),
                not_modified: true,
            });
        }
        Ok(crate::feeds::FeedFetch::Fetched {
            parsed,
            body_digest,
        }) => (parsed, body_digest),
        Err(e) => {
            // YouTube channel feeds answer 404/500 at random for
            // some channels (issue #107): fall back to listing the
            // uploads playlist through yt-dlp. An empty listing is
            // not a recovery — Python re-raises the RSS error.
            if let Some(cid) = &channel_id {
                let (title, items) =
                    crate::sources::fetch_youtube_channel_items(cid, 30).map_err(|_| e.clone())?;
                if items.is_empty() {
                    return Err(e);
                }
                if cancelled(cancel) {
                    return Err(REFRESH_CANCELLED.to_string());
                }
                let parsed = crate::feeds::ParsedFeed {
                    title,
                    entries: crate::sources::items_to_entries(shape_stable_ids(db, feed, items)),
                };
                let outcome = store_parsed_with_retention(
                    db,
                    feed,
                    &parsed,
                    global_delete_behavior,
                    retention_cutoff,
                )?;
                forget_body_digest(&feed.id);
                return Ok(WorkerRefresh {
                    outcome,
                    not_modified: false,
                });
            }
            return Err(e);
        }
    };
    if cancelled(cancel) {
        return Err(REFRESH_CANCELLED.to_string());
    }
    let outcome =
        store_parsed_with_retention(db, feed, &parsed, global_delete_behavior, retention_cutoff)?;
    // Record the digest only now: a failed store above must be
    // retried by the next refresh (D16 failed-store semantics).
    if !body_digest.is_empty() {
        record_body_digest(&feed.id, &body_digest);
    }
    Ok(WorkerRefresh {
        outcome,
        not_modified: false,
    })
}

/// Refresh one feed; returns how many new articles are
/// notification-eligible (Python's per-feed `new_items`: stored
/// and neither rule-removed nor notification-suppressed).
pub fn refresh_feed(db: &Db, feed: &Feed, global_delete_behavior: &str) -> Result<usize, String> {
    refresh_feed_with_options(
        db,
        feed,
        global_delete_behavior,
        None,
        false,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .map(|w| w.outcome.notify_count)
}

/// Subscribe to a feed URL end to end, on the caller's (worker)
/// thread (Audit 8 F01 + D22): resolution, the feeds-row insert
/// and the first store all happen here, through the worker's own
/// connection. Listing sources store their articles through the
/// unified refresh — listing ids are shaped with the real feed
/// id, which only exists once the row does — with Python's
/// add-flow tolerance: a failed first enumeration does not
/// unsubscribe the feed (the row stands; refresh owns retries).
pub fn subscribe_url(
    db: &Db,
    url: &str,
    category: Option<&str>,
    global_delete_behavior: &str,
    retention_cutoff: Option<&str>,
) -> Result<Feed, String> {
    let (feed_url, parsed) = crate::discovery::resolve_subscribe_url(url)?;
    let title = if parsed.title.trim().is_empty() {
        feed_url.clone()
    } else {
        parsed.title.clone()
    };
    let feed = db
        .add_feed(&feed_url, &title, category)
        .map_err(|e| format!("{e}"))?;
    if crate::discovery::source_listing_url(&feed_url).is_some() {
        if let Err(e) = refresh_feed_with_options(
            db,
            &feed,
            global_delete_behavior,
            retention_cutoff,
            true,
            &std::sync::atomic::AtomicBool::new(false),
        ) {
            eprintln!(
                "blindrss: first refresh of new feed {} failed: {e}",
                feed.title
            );
        }
    } else {
        store_parsed_with_retention(db, &feed, &parsed, global_delete_behavior, retention_cutoff)?;
    }
    // Re-read the row: the first refresh may have persisted a
    // normalized listing URL back into it.
    Ok(db
        .feeds()
        .map_err(|e| format!("{e}"))?
        .into_iter()
        .find(|f| f.id == feed.id)
        .unwrap_or(feed))
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

/// Python's history constants: the full uploads playlist, with a
/// five-minute listing budget.
const YOUTUBE_HISTORY_MAX_ITEMS: usize = 5000;
const YOUTUBE_HISTORY_TIMEOUT_S: u64 = 300;

fn youtube_history_backfill(
    db: &Db,
    feed: &Feed,
    channel_id: &str,
    global_delete_behavior: &str,
    retention_cutoff: Option<&str>,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    if !db.youtube_history_due(&feed.id, now) {
        return;
    }
    // Python marks the attempt (done = 0, last_attempt = now) before
    // listing, so a crash mid-listing still waits out the retry day.
    let _ = db.set_youtube_history_state(&feed.id, 0, now);
    if let Ok((_title, items)) = crate::sources::fetch_youtube_channel_items_with_timeout(
        channel_id,
        YOUTUBE_HISTORY_MAX_ITEMS,
        YOUTUBE_HISTORY_TIMEOUT_S,
    ) && !items.is_empty()
    {
        let parsed = crate::feeds::ParsedFeed {
            title: feed.title.clone(),
            entries: crate::sources::items_to_entries(shape_stable_ids(db, feed, items)),
        };
        // History items are stored unread but are NOT new-item
        // news: the outcome is deliberately discarded. Python marks
        // the history done only when the listing produced items
        // and they were stored.
        if store_parsed_with_retention(db, feed, &parsed, global_delete_behavior, retention_cutoff)
            .is_ok()
        {
            // `_store_youtube_items` pops the stored-body digest.
            forget_body_digest(&feed.id);
            let _ = db.set_youtube_history_state(&feed.id, 2, now);
        }
    }
}

/// Refresh every feed, collecting per-feed results. One feed failing
/// does not stop the others (Python behaves the same way).
pub fn refresh_all(db: &Db, global_delete_behavior: &str) -> Vec<(String, Result<usize, String>)> {
    let feeds = match db.feeds() {
        Ok(f) => f,
        Err(e) => return vec![("<database>".to_string(), Err(format!("{e}")))],
    };
    feeds
        .iter()
        .map(|f| (f.title.clone(), refresh_feed(db, f, global_delete_behavior)))
        .collect()
}

/// Apply the whole filter-rule pipeline to articles already in
/// the database (the rules dialog's "Apply to existing articles").
/// Returns how many articles any rule acted on. Evaluation pages
/// through the whole library — Python applies rules to existing
/// articles without a row ceiling, so a match beyond the first
/// 100,000 rows is still reached (Audit 8 D21). Each article's
/// delete action resolves through its own feed's effective
/// delete behavior (Audit 8 D12).
pub fn apply_rules_to_existing(db: &Db, global_delete_behavior: &str) -> Result<usize, String> {
    let rules = db.filter_rules().map_err(|e| format!("{e}"))?;
    if rules.is_empty() {
        return Ok(0);
    }
    let feeds = db.feeds().map_err(|e| format!("{e}"))?;
    let feed_titles: std::collections::HashMap<String, String> = feeds
        .iter()
        .map(|f| (f.id.clone(), f.title.clone()))
        .collect();
    // Resolve each feed's delete behavior once, not per article.
    let feed_behaviors: std::collections::HashMap<String, crate::rules::DeleteBehavior> = feeds
        .iter()
        .map(|f| {
            (
                f.id.clone(),
                db.effective_delete_behavior(&f.id, global_delete_behavior),
            )
        })
        .collect();
    let global_behavior = crate::rules::parse_delete_behavior(global_delete_behavior);
    let tags = db.all_article_tags().map_err(|e| format!("{e}"))?;
    let opened = db.opened_article_ids().map_err(|e| format!("{e}"))?;
    let updated = db.updated_article_ids().map_err(|e| format!("{e}"))?;
    let empty_tags = Vec::new();
    // Two phases, like Python (fetch all rows, then apply):
    // evaluating is read-only paging, applying happens after,
    // so a rule that removes rows cannot shift the windows and
    // skip later articles.
    const PAGE: usize = 1_000;
    let mut work: Vec<(Article, crate::rules::ResolvedActions)> = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = db
            .articles_page(None, PAGE, offset)
            .map_err(|e| format!("{e}"))?;
        let full_page = page.len() == PAGE;
        for article in &page {
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
            let behavior = feed_behaviors
                .get(&article.feed_id)
                .unwrap_or(&global_behavior);
            let resolved = crate::rules::resolve_effective_actions(
                &crate::rules::evaluate_pipeline(&rules, &view),
                behavior,
            );
            if resolved.any() {
                work.push((article.clone(), resolved));
            }
        }
        if !full_page {
            break;
        }
        offset += PAGE;
    }
    for (article, resolved) in &work {
        apply_filter_pipeline(db, resolved, article);
    }
    Ok(work.len())
}
