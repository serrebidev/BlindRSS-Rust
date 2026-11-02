//! Perf regression tests (2026-11-02 audit: ~30 s startup,
//! podcast list lag). Brandon's report traced to three root
//! causes: the Rust schema never created Python's article
//! indexes, the tree build ran one COUNT per feed (2-3x per
//! feed), and the list queries hauled full article bodies
//! through a query shape that defeats the feed index.

use blindrss_core::Db;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> (Db, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "blindrss_perf_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    // Open through the production path so the real SCHEMA
    // (tables + indexes) is what gets seeded and queried.
    let db = Db::open_or_create(&path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
         VALUES ('f1','https://example.com/f1','Podcast One',0,'Podcasts',NULL);\
         INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
         VALUES ('f2','https://example.com/f2','News Two',0,'News',NULL);\
         INSERT INTO articles (id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type) \
         VALUES ('a1','f1','Episode 1','https://example.com/1','shownotes zebra body','d1','2026-10-30','Host',0,0,'https://example.com/1.mp3','audio/mpeg');\
         INSERT INTO articles (id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type) \
         VALUES ('a2','f1','Episode 2','https://example.com/2','more shownotes','d2','2026-10-29','Host',1,0,'https://example.com/2.mp3','audio/mpeg');\
         INSERT INTO articles (id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type) \
         VALUES ('a3','f2','Story','https://example.com/3','news body','d3','2026-10-28','Wire',0,0,NULL,NULL);",
    )
    .unwrap();
    drop(conn);
    (db, path)
}

#[test]
fn fresh_db_carries_python_indexes() {
    let (_db, path) = scratch("idx");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' \
             AND tbl_name IN ('articles', 'deleted_articles', 'chapters')",
        )
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Python core/db.py init_db's set (minus idx_articles_id_unique,
    // which only duplicates the primary key).
    for expected in [
        "idx_articles_feed_id",
        "idx_articles_is_read",
        "idx_articles_date",
        "idx_articles_is_read_feed_id",
        "idx_articles_date_id",
        "idx_articles_feed_id_date_id",
        "idx_deleted_articles_feed_id",
        "idx_deleted_articles_feed_url",
        "idx_chapters_article_id_start",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing index {expected}; have {names:?}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn unread_counts_match_per_feed_counts() {
    let (db, path) = scratch("counts");
    let map = db.unread_counts().unwrap();
    assert_eq!(map.get("f1"), Some(&1));
    assert_eq!(map.get("f2"), Some(&1));
    for feed in ["f1", "f2"] {
        assert_eq!(
            map.get(feed).copied().unwrap_or(0),
            db.unread_count(Some(feed)).unwrap(),
            "grouped count disagrees with per-feed count for {feed}"
        );
    }
    assert_eq!(db.unread_count(None).unwrap(), 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn feed_refill_plan_uses_feed_index() {
    let (_db, path) = scratch("plan");
    let conn = rusqlite::Connection::open(&path).unwrap();
    // The exact SQL Db::articles runs for a single feed; if the
    // shape regresses to the old parameterised OR, the planner
    // falls back to a scan and this test fails.
    let mut stmt = conn
        .prepare(
            "EXPLAIN QUERY PLAN \
             SELECT id, feed_id, title, url, content, description, date, \
             author, is_read, is_favorite, media_url, media_type \
             FROM articles WHERE feed_id = ?1 \
             ORDER BY date DESC, id DESC LIMIT ?2",
        )
        .unwrap();
    let details: Vec<String> = stmt
        .query_map(rusqlite::params!["f1", 300], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        details
            .iter()
            .any(|d| d.contains("USING INDEX idx_articles_feed_id_date_id")),
        "feed refill does not use the feed index: {details:?}"
    );
    assert!(
        !details.iter().any(|d| d.starts_with("SCAN articles")),
        "feed refill scans the articles table: {details:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn light_loads_skip_bodies_full_loads_keep_them() {
    let (db, path) = scratch("light");
    let light = db.articles_light(Some("f1"), 10).unwrap();
    assert_eq!(light.len(), 2);
    assert!(light.iter().all(|a| a.content.is_none()));
    assert_eq!(light[0].title, "Episode 1");
    assert_eq!(
        light[0].media_url.as_deref(),
        Some("https://example.com/1.mp3")
    );

    let full = db.articles(Some("f1"), 10).unwrap();
    assert_eq!(
        full[0].content.as_deref(),
        Some("shownotes zebra body"),
        "the full load must keep bodies for bulk consumers"
    );

    let cat = db.articles_for_category_light("Podcasts", 10).unwrap();
    assert_eq!(cat.len(), 2);
    assert!(cat.iter().all(|a| a.content.is_none()));

    // Content search still matches on the stored body even
    // though the returned rows do not carry it.
    let hits = db.search_articles_mode_light("zebra", 10, false).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.is_none());

    // The on-demand path the reader uses returns the body.
    let one = db.article_by_id("a1").unwrap().unwrap();
    assert_eq!(one.content.as_deref(), Some("shownotes zebra body"));
    let _ = std::fs::remove_file(&path);
}
