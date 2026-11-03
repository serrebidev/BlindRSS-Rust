//! Batch 27 (v0.1.26): category-view membership — the port of
//! Python providers/local.py::_category_membership_clause. An
//! article belongs to a category view when its feed is in the
//! subtree and it was not moved away, or it was moved here
//! (category_override), or it is labeled with a subtree category.

use blindrss_core::db::{Article, Db, Feed};

fn unique_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn temp_db() -> Db {
    let dir = std::env::temp_dir().join(format!("blindrss-batch27-{}", unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    Db::open_or_create(&dir.join("rss.db")).unwrap()
}

fn feed(db: &Db, id: &str, category: Option<&str>) {
    db.insert_feed_if_missing(&Feed {
        id: id.to_string(),
        url: format!("https://example.com/{id}"),
        title: id.to_string(),
        title_is_custom: false,
        category: category.map(str::to_string),
        icon_url: None,
    })
    .unwrap();
}

fn article(db: &Db, id: &str, feed_id: &str, date: &str) {
    db.insert_article_if_new(&Article {
        id: id.to_string(),
        feed_id: feed_id.to_string(),
        title: id.to_string(),
        url: format!("https://example.com/{id}"),
        content: None,
        description: None,
        date: Some(date.to_string()),
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    })
    .unwrap();
}

fn ids(db: &Db, path: &str) -> Vec<String> {
    let mut v: Vec<String> = db
        .articles_for_category(path, 100)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    v.sort();
    v
}

fn setup() -> Db {
    let db = temp_db();
    db.ensure_category_path("News / Tech").unwrap();
    feed(&db, "f_news", Some("News"));
    feed(&db, "f_tech", Some("News / Tech"));
    feed(&db, "f_loose", None);
    feed(&db, "f_sentinel", Some("Uncategorized"));
    article(&db, "a1", "f_news", "2026-10-01");
    article(&db, "a2", "f_tech", "2026-10-02");
    article(&db, "a3", "f_loose", "2026-10-03");
    article(&db, "a4", "f_sentinel", "2026-10-04");
    // Moved out of News into Sports by a filter rule.
    article(&db, "a5", "f_news", "2026-10-05");
    db.set_article_category_override("a5", Some("Sports"))
        .unwrap();
    // Labeled into News while living on an uncategorized feed.
    article(&db, "a6", "f_loose", "2026-10-06");
    db.add_article_label("a6", "News").unwrap();
    db
}

#[test]
fn category_view_includes_subtree_feeds_and_labels() {
    let db = setup();
    assert_eq!(ids(&db, "News"), vec!["a1", "a2", "a6"]);
    assert_eq!(ids(&db, "News / Tech"), vec!["a2"]);
}

#[test]
fn moved_article_leaves_its_feed_category_and_joins_target() {
    let db = setup();
    assert!(!ids(&db, "News").contains(&"a5".to_string()));
    assert_eq!(ids(&db, "Sports"), vec!["a5"]);
}

#[test]
fn uncategorized_bucket_covers_null_empty_and_sentinel() {
    let db = setup();
    assert_eq!(ids(&db, "Uncategorized"), vec!["a3", "a4", "a6"]);
}

#[test]
fn unknown_category_is_empty() {
    let db = setup();
    assert!(ids(&db, "Nope").is_empty());
}
