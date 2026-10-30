//! Provider-depth pass tests: category rename/move, feed edit and
//! title reset, article-by-id, read-state toggling.

use blindrss_core::Db;
use std::path::PathBuf;

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_ops_{tag}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE feeds (id TEXT PRIMARY KEY, url TEXT, title TEXT, \
         title_is_custom INTEGER DEFAULT 0, category TEXT, icon_url TEXT);
         CREATE TABLE articles (id TEXT PRIMARY KEY, feed_id TEXT, title TEXT, \
         url TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0, \
         media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL);
         CREATE TABLE categories (id TEXT PRIMARY KEY, title TEXT UNIQUE);
         CREATE TABLE deleted_articles (feed_id TEXT NOT NULL, \
         article_id TEXT NOT NULL, url TEXT, deleted_at REAL NOT NULL, \
         title TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         media_url TEXT, media_type TEXT, chapter_url TEXT);",
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path)
}

#[test]
fn category_rename_and_feed_ops() {
    let (db, path) = scratch_db("cat");
    db.add_category("Tech").unwrap();
    let feed = db
        .add_feed("https://example.com/x.xml", "Feed X", Some("Tech"))
        .unwrap();
    db.rename_category("Tech", "Technology").unwrap();
    let feeds = db.feeds().unwrap();
    assert_eq!(feeds[0].category.as_deref(), Some("Technology"));
    assert_eq!(db.categories().unwrap()[0].title, "Technology");

    db.set_feed_category(&feed.id, None).unwrap();
    assert_eq!(db.feeds().unwrap()[0].category, None);
    db.set_feed_category(&feed.id, Some("Technology")).unwrap();

    // Edit: title becomes custom; URL updates.
    db.update_feed(&feed.id, Some("My X"), Some("https://example.com/y.xml"))
        .unwrap();
    let f = &db.feeds().unwrap()[0];
    assert_eq!(f.title, "My X");
    assert!(f.title_is_custom);
    assert_eq!(f.url, "https://example.com/y.xml");

    // Reset: back to the feed's own title, custom flag cleared.
    db.reset_feed_title(&feed.id, "Feed X").unwrap();
    let f = &db.feeds().unwrap()[0];
    assert_eq!(f.title, "Feed X");
    assert!(!f.title_is_custom);
    std::fs::remove_file(&path).ok();
}

#[test]
fn article_by_id_and_read_toggle() {
    let (db, path) = scratch_db("art");
    let feed = db.add_feed("https://example.com/a.xml", "A", None).unwrap();
    let parsed = blindrss_core::feeds::parse_feed(
        br#"<?xml version="1.0"?><rss version="2.0"><channel><title>A</title>
        <item><title>One</title><link>https://example.com/1</link><guid>g1</guid></item>
        </channel></rss>"#,
    )
    .unwrap();
    blindrss_core::refresh::store_parsed(&db, &feed, &parsed).unwrap();
    let a = db.article_by_id("g1").unwrap().expect("article exists");
    assert_eq!(a.title, "One");
    assert!(!a.is_read);
    db.mark_read("g1", true).unwrap();
    assert!(db.article_by_id("g1").unwrap().unwrap().is_read);
    db.mark_read("g1", false).unwrap();
    assert!(!db.article_by_id("g1").unwrap().unwrap().is_read);
    assert!(db.article_by_id("missing").unwrap().is_none());
    std::fs::remove_file(&path).ok();
}
