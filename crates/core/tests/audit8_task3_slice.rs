//! Audit 8 Task 3 regressions (D03, D04, D05, D06, D15): ingestion
//! identity is feed-scoped, publisher metadata syncs in place,
//! tombstones match per-feed by id and URL, and retention/prune
//! share one boundary. Python spec: providers/local.py (generic
//! store path) and core/db.py cleanup_old_articles /
//! article_is_outside_retention.

use blindrss_core::refresh::{store_parsed, store_parsed_with_retention};
use blindrss_core::retention::retention_cutoff_date;
use blindrss_core::{Chapter, Db, Feed, ParsedEntry, ParsedFeed, PlaybackState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_db() -> (Db, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "blindrss-a8t3-{}-{nanos}-{n}.db",
        std::process::id()
    ));
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

fn entry(id: &str, title: &str, url: &str) -> ParsedEntry {
    ParsedEntry {
        id: id.to_string(),
        title: title.to_string(),
        url: url.to_string(),
        content_text: format!("body of {title}"),
        summary: format!("summary of {title}"),
        date: Some("2026-10-30 10:00:00".to_string()),
        author: Some("Author".to_string()),
        media_url: None,
        media_type: None,
        tags: Vec::new(),
    }
}

fn parsed(entries: Vec<ParsedEntry>) -> ParsedFeed {
    ParsedFeed {
        title: "Feed".to_string(),
        entries,
    }
}

fn add_feed(db: &Db, url: &str) -> Feed {
    db.add_feed(url, "Feed", None).unwrap()
}

/// D03: the same GUID in two feeds is two articles with independent
/// state, history, media and deletion.
#[test]
fn cross_feed_guid_collision_keeps_independent_articles() {
    let (db, path) = scratch_db();
    let feed_a = add_feed(&db, "https://a.example/feed");
    let feed_b = add_feed(&db, "https://b.example/feed");

    let mut ea = entry("1", "A one", "https://a.example/1");
    ea.media_url = Some("https://a.example/ep.mp3".to_string());
    ea.media_type = Some("audio/mpeg".to_string());
    assert_eq!(store_parsed(&db, &feed_a, &parsed(vec![ea])).unwrap(), 1);

    let eb = entry("1", "B one", "https://b.example/1");
    // Today this stores 0: the raw id is already taken by feed A.
    assert_eq!(store_parsed(&db, &feed_b, &parsed(vec![eb])).unwrap(), 1);

    let a = db.article_by_id("1").unwrap().unwrap();
    assert_eq!(a.feed_id, feed_a.id);
    assert_eq!(a.title, "A one");
    assert_eq!(a.media_url.as_deref(), Some("https://a.example/ep.mp3"));

    let scoped = format!("{}:1", feed_b.id);
    let b = db.article_by_id(&scoped).unwrap().unwrap();
    assert_eq!(b.feed_id, feed_b.id);
    assert_eq!(b.title, "B one");
    assert!(b.media_url.is_none());

    // Independent state: reading/favoriting one twin leaves the other.
    db.mark_read("1", true).unwrap();
    db.set_favorite(&scoped, true).unwrap();
    let a = db.article_by_id("1").unwrap().unwrap();
    let b = db.article_by_id(&scoped).unwrap().unwrap();
    assert!(a.is_read && !a.is_favorite);
    assert!(!b.is_read && b.is_favorite);

    // Independent history: B's body must not be appended to A's
    // version history (and vice versa).
    let versions_a = db.article_versions("1").unwrap();
    assert_eq!(versions_a.len(), 1);
    assert_eq!(versions_a[0].content, "body of A one");
    let versions_b = db.article_versions(&scoped).unwrap();
    assert_eq!(versions_b.len(), 1);
    assert_eq!(versions_b[0].content, "body of B one");

    // Re-refresh of both feeds stores nothing new.
    let ea = entry("1", "A one", "https://a.example/1");
    let eb = entry("1", "B one", "https://b.example/1");
    assert_eq!(store_parsed(&db, &feed_a, &parsed(vec![ea])).unwrap(), 0);
    assert_eq!(store_parsed(&db, &feed_b, &parsed(vec![eb])).unwrap(), 0);

    // Independent deletion: deleting B's twin leaves A's row.
    assert!(db.delete_article(&scoped).unwrap());
    assert!(db.article_by_id("1").unwrap().is_some());
    assert!(db.article_by_id(&scoped).unwrap().is_none());

    std::fs::remove_file(&path).ok();
}

/// D15: a deleted URL stays deleted when the publisher changes the
/// GUID; the same GUID in another feed is not suppressed.
#[test]
fn tombstone_matches_feed_scoped_url_and_id() {
    let (db, path) = scratch_db();
    let feed_a = add_feed(&db, "https://a.example/feed");
    let feed_b = add_feed(&db, "https://b.example/feed");

    let e = entry("g1", "Story", "https://a.example/story");
    assert_eq!(store_parsed(&db, &feed_a, &parsed(vec![e])).unwrap(), 1);
    assert!(db.delete_article("g1").unwrap());

    // Same feed, changed GUID, same URL: stays deleted.
    let e = entry("g2", "Story", "https://a.example/story");
    assert_eq!(store_parsed(&db, &feed_a, &parsed(vec![e])).unwrap(), 0);
    assert!(db.article_by_id("g2").unwrap().is_none());

    // Same feed, original GUID, new URL: stays deleted (id match).
    let e = entry("g1", "Story", "https://a.example/other");
    assert_eq!(store_parsed(&db, &feed_a, &parsed(vec![e])).unwrap(), 0);

    // Different feed, same GUID, its own URL: NOT suppressed by
    // feed A's tombstone.
    let e = entry("g1", "Story", "https://b.example/story");
    assert_eq!(store_parsed(&db, &feed_b, &parsed(vec![e])).unwrap(), 1);
    let b = db.article_by_id("g1").unwrap().unwrap();
    assert_eq!(b.feed_id, feed_b.id);

    std::fs::remove_file(&path).ok();
}

/// D06: a later refresh syncs publisher-owned metadata (date,
/// description, URL, enclosure/type) onto the existing row while
/// local state and the stored body survive.
#[test]
fn publisher_metadata_correction_updates_existing_row() {
    let (db, path) = scratch_db();
    let feed = add_feed(&db, "https://a.example/feed");

    let mut e = entry("m1", "Episode", "https://a.example/old");
    e.content_text = "FULL EXTRACTED TEXT".to_string();
    e.summary = "first summary".to_string();
    e.date = Some("2026-10-01 10:00:00".to_string());
    assert_eq!(store_parsed(&db, &feed, &parsed(vec![e])).unwrap(), 1);

    db.mark_read("m1", true).unwrap();
    db.set_favorite("m1", true).unwrap();

    // Publisher correction: new URL, new summary, new date, and an
    // enclosure that only appeared later.
    let mut e = entry("m1", "Episode", "https://a.example/new");
    e.content_text = "short feed body v2".to_string();
    e.summary = "corrected summary".to_string();
    e.date = Some("2026-10-02 10:00:00".to_string());
    e.media_url = Some("https://cdn.example/ep.mp3".to_string());
    e.media_type = Some("audio/mpeg".to_string());
    assert_eq!(store_parsed(&db, &feed, &parsed(vec![e])).unwrap(), 0);

    let a = db.article_by_id("m1").unwrap().unwrap();
    assert_eq!(a.url, "https://a.example/new");
    assert_eq!(a.description.as_deref(), Some("corrected summary"));
    assert_eq!(a.date.as_deref(), Some("2026-10-02 10:00:00"));
    assert_eq!(a.media_url.as_deref(), Some("https://cdn.example/ep.mp3"));
    assert_eq!(a.media_type.as_deref(), Some("audio/mpeg"));
    // Local state and the fuller stored body are not publisher-owned.
    assert!(a.is_read && a.is_favorite);
    assert_eq!(a.content.as_deref(), Some("FULL EXTRACTED TEXT"));
    // The changed feed body lands in change history only.
    assert_eq!(db.count_article_versions("m1").unwrap(), 2);

    std::fs::remove_file(&path).ok();
}

/// D05: prune removes old read AND unread non-favorites, protects
/// favorites and undated sentinels, and cleans dependent chapters
/// and playback state for the rows it removes.
#[test]
fn prune_matches_python_cleanup_policy() {
    let (db, path) = scratch_db();
    let feed = add_feed(&db, "https://a.example/feed");
    let old = "2020-01-01 00:00:00";

    let seed = |id: &str, date: Option<&str>, read: bool, fav: bool| {
        let mut e = entry(id, id, "https://a.example/x");
        e.date = date.map(str::to_string);
        store_parsed(&db, &feed, &parsed(vec![e])).unwrap();
        if read {
            db.mark_read(id, true).unwrap();
        }
        if fav {
            db.set_favorite(id, true).unwrap();
        }
    };
    seed("old-read", Some(old), true, false);
    seed("old-unread", Some(old), false, false);
    seed("old-fav", Some(old), true, true);
    seed("old-undated", Some("0001-01-01 00:00:00"), false, false);
    seed("old-nodate", None, false, false);
    seed("recent-read", Some("2999-01-01 00:00:00"), true, false);

    // Dependent state for one doomed and one surviving article.
    db.replace_chapters(
        "old-read",
        &[Chapter {
            start: 0.0,
            title: Some("Ch".to_string()),
            href: None,
        }],
    )
    .unwrap();
    let ps = PlaybackState {
        position_ms: 5_000,
        duration_ms: Some(60_000),
        completed: false,
        seek_supported: Some(true),
        title: None,
    };
    db.upsert_playback_state("article:old-read", &ps).unwrap();
    db.upsert_playback_state("article:recent-read", &ps)
        .unwrap();

    db.prune_old_articles(7).unwrap();

    assert!(db.article_by_id("old-read").unwrap().is_none());
    assert!(
        db.article_by_id("old-unread").unwrap().is_none(),
        "old unread non-favorites are pruned by Python cleanup"
    );
    assert!(db.article_by_id("old-fav").unwrap().is_some());
    assert!(
        db.article_by_id("old-undated").unwrap().is_some(),
        "undated sentinel rows are protected"
    );
    assert!(db.article_by_id("old-nodate").unwrap().is_some());
    assert!(db.article_by_id("recent-read").unwrap().is_some());

    assert!(db.chapters("old-read").unwrap().is_empty());
    assert!(
        db.playback_state("article:old-read").unwrap().is_none(),
        "playback state is cleaned with the article"
    );
    assert!(db.playback_state("article:recent-read").unwrap().is_some());

    std::fs::remove_file(&path).ok();
}

/// D04 (insert half): refresh refuses NEW entries outside the
/// retention window, so an entry prune removed cannot resurrect as
/// unread on the next refresh. Undated entries are never outside.
#[test]
fn refresh_respects_retention_boundary() {
    let (db, path) = scratch_db();
    let feed = add_feed(&db, "https://a.example/feed");
    let cutoff = retention_cutoff_date(Some(7)).unwrap();

    // Already outside the window: not inserted.
    let mut e = entry("old", "Old", "https://a.example/old");
    e.date = Some("2020-01-01 00:00:00".to_string());
    assert_eq!(
        store_parsed_with_retention(&db, &feed, &parsed(vec![e]), Some(&cutoff)).unwrap(),
        0
    );
    assert!(db.article_by_id("old").unwrap().is_none());

    // Undated sentinel and missing date: inserted (never outside).
    let mut e = entry("undated", "Undated", "https://a.example/u");
    e.date = Some("0001-01-01 00:00:00".to_string());
    assert_eq!(
        store_parsed_with_retention(&db, &feed, &parsed(vec![e]), Some(&cutoff)).unwrap(),
        1
    );
    let mut e = entry("nodate", "NoDate", "https://a.example/n");
    e.date = None;
    assert_eq!(
        store_parsed_with_retention(&db, &feed, &parsed(vec![e]), Some(&cutoff)).unwrap(),
        1
    );

    // Dated exactly at the cutoff: inside the window, inserted.
    let mut e = entry("edge", "Edge", "https://a.example/e");
    e.date = Some(format!("{cutoff} 00:00:00"));
    assert_eq!(
        store_parsed_with_retention(&db, &feed, &parsed(vec![e]), Some(&cutoff)).unwrap(),
        1
    );

    // Prune-then-refresh: an old entry stored without the boundary
    // is pruned, and the boundary keeps it from coming back unread.
    let mut e = entry("zombie", "Zombie", "https://a.example/z");
    e.date = Some("2020-01-01 00:00:00".to_string());
    assert_eq!(
        store_parsed(&db, &feed, &parsed(vec![e.clone()])).unwrap(),
        1
    );
    db.prune_old_articles(7).unwrap();
    assert!(db.article_by_id("zombie").unwrap().is_none());
    let unread_before = db.unread_count(None).unwrap();
    assert_eq!(
        store_parsed_with_retention(&db, &feed, &parsed(vec![e]), Some(&cutoff)).unwrap(),
        0
    );
    assert!(db.article_by_id("zombie").unwrap().is_none());
    assert_eq!(db.unread_count(None).unwrap(), unread_before);

    std::fs::remove_file(&path).ok();
}

/// D04 (tombstone half): prune never ages out tombstones — Python
/// deletes them only on restore or confirmed purge, so a deleted
/// article cannot lose its resurrection guard with time.
#[test]
fn prune_never_deletes_tombstones() {
    let (db, path) = scratch_db();
    let feed = add_feed(&db, "https://a.example/feed");
    let e = entry("t1", "Gone", "https://a.example/gone");
    assert_eq!(store_parsed(&db, &feed, &parsed(vec![e])).unwrap(), 1);
    assert!(db.delete_article("t1").unwrap());
    assert!(db.deleted_article_ids().unwrap().contains("t1"));

    // A zero-day window is the harshest prune; the tombstone (whose
    // deleted_at is already in the past) must still survive.
    db.prune_old_articles(0).unwrap();
    assert!(
        db.deleted_article_ids().unwrap().contains("t1"),
        "tombstone must survive prune"
    );

    std::fs::remove_file(&path).ok();
}
