//! Audit 7 regression tests (2026-11-02): Finding A — the
//! article list had no paging (Python pages at 400 behind a
//! "Load more items (Enter)" placeholder; the Rust list
//! hard-capped feed views at 300 / category views at 500) —
//! plus the silence merge/pad config plumbing.

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
        "blindrss_audit7_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

fn seed(path: &PathBuf, sql: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
}

fn feed_sql(id: &str, category: &str) -> String {
    format!(
        "INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
         VALUES ('{id}','https://example.com/{id}','Feed {id}',0,'{category}',NULL);"
    )
}

fn article_sql(id: &str, feed: &str, date: &str) -> String {
    format!(
        "INSERT INTO articles (id, feed_id, title, url, content, description, date, \
         author, is_read, is_favorite, media_url, media_type) \
         VALUES ('{id}','{feed}','Title {id}','https://example.com/{id}','body {id}',\
         'd','{date}','A',0,0,NULL,NULL);"
    )
}

#[test]
fn feed_pages_tile_the_whole_view_without_overlap() {
    let (db, path) = scratch("feedpage");
    // 25 episodes in 5 date groups of 5 (shared dates force the
    // id tiebreak to carry page stability).
    let mut sql = feed_sql("f1", "Podcasts");
    for i in 0..25 {
        let date = format!("2026-10-{:02}", 30 - (i / 5));
        sql.push_str(&article_sql(&format!("e{i:02}"), "f1", &date));
    }
    seed(&path, &sql);

    assert_eq!(db.articles_count(Some("f1")).unwrap(), 25);
    assert_eq!(db.articles_count(None).unwrap(), 25);
    assert_eq!(db.articles_count(Some("missing")).unwrap(), 0);

    let mut seen: Vec<String> = Vec::new();
    for (offset, want) in [(0usize, 10usize), (10, 10), (20, 5), (30, 0)] {
        let page = db.articles_light_page(Some("f1"), 10, offset).unwrap();
        assert_eq!(page.len(), want, "page at offset {offset}");
        assert!(page.iter().all(|a| a.content.is_none()), "pages stay light");
        seen.extend(page.iter().map(|a| a.id.clone()));
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 25, "pages overlap or skip rows: {seen:?}");
    // Order is total: date DESC, id DESC.
    let all = db.articles_light_page(Some("f1"), 100, 0).unwrap();
    let ids: Vec<&str> = all.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(seen[..25], ids[..], "paged order != single-page order");
    assert_eq!(ids[0], "e04", "newest date group, highest id first");
}

#[test]
fn category_pages_and_count_cover_the_subtree() {
    let (db, path) = scratch("catpage");
    let mut sql = feed_sql("f1", "Podcasts");
    sql.push_str(&feed_sql("f2", "News"));
    sql.push_str(&feed_sql("f3", "Podcasts/Sub"));
    for i in 0..4 {
        sql.push_str(&article_sql(
            &format!("p{i}"),
            "f1",
            &format!("2026-10-2{i}"),
        ));
        sql.push_str(&article_sql(
            &format!("s{i}"),
            "f3",
            &format!("2026-10-2{i}"),
        ));
    }
    sql.push_str(&article_sql("n0", "f2", "2026-10-29"));
    seed(&path, &sql);

    // Subtree: f1 (4) + f3 (4); the News article is excluded
    // even though it is the newest row in the database.
    assert_eq!(db.articles_for_category_count("Podcasts").unwrap(), 8);
    let full = db.articles_for_category_light("Podcasts", 100).unwrap();
    assert_eq!(full.len(), 8);

    let mut seen: Vec<String> = Vec::new();
    for offset in (0..8).step_by(3) {
        let page = db
            .articles_for_category_light_page("Podcasts", 3, offset)
            .unwrap();
        seen.extend(page.iter().map(|a| a.id.clone()));
    }
    let full_ids: Vec<String> = full.iter().map(|a| a.id.clone()).collect();
    assert_eq!(seen, full_ids, "category pages must tile the full view");
    assert!(!seen.iter().any(|id| id == "n0"));
}

#[test]
fn silence_finalize_honours_tuned_merge_and_padding() {
    use blindrss_core::silence::{DEFAULT_MERGE_GAP_S, DEFAULT_PADDING_S, finalize_ranges};
    // The shipped defaults ARE Python's config defaults
    // (silence_skip_merge_gap_ms 260 / silence_skip_padding_ms 60).
    assert_eq!(DEFAULT_MERGE_GAP_S, 0.26);
    assert_eq!(DEFAULT_PADDING_S, 0.06);

    let raw = vec![(10.0, 12.0), (12.2, 14.0), (30.0, 31.0)];
    // A 0.26 gap merges the first two (0.2 apart); padding then
    // shrinks both sides.
    let merged = finalize_ranges(raw.clone(), DEFAULT_MERGE_GAP_S, DEFAULT_PADDING_S);
    assert_eq!(merged.len(), 2);
    assert!((merged[0].0 - 10.06).abs() < 1e-9);
    assert!((merged[0].1 - 13.94).abs() < 1e-9);
    // A user-tightened gap of 0.1 keeps them separate.
    let split = finalize_ranges(raw, 0.1, 0.0);
    assert_eq!(split.len(), 3);
}
