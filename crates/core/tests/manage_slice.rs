//! Slice 3 tests: feed/category management + OPML roundtrip, on a
//! scratch Python-schema database.

use blindrss_core::Db;
use blindrss_core::opml::{export_opml, import_opml};
use std::path::PathBuf;

fn scratch_db(name: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_{name}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE feeds (id TEXT PRIMARY KEY, url TEXT, title TEXT, \
         title_is_custom INTEGER DEFAULT 0, category TEXT, icon_url TEXT);
         CREATE TABLE articles (id TEXT PRIMARY KEY, feed_id TEXT, title TEXT, \
         url TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0, \
         media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL);
         CREATE TABLE categories (id TEXT PRIMARY KEY, title TEXT UNIQUE);",
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path)
}

#[test]
fn add_remove_feed_and_category() {
    let (db, path) = scratch_db("manage");
    let feed = db
        .add_feed("https://example.com/feed", "Example", Some("Tech"))
        .unwrap();
    assert!(db.feed_url_exists("https://example.com/feed").unwrap());
    assert_eq!(db.feeds().unwrap().len(), 1);
    let cat = db.add_category("Tech").unwrap();
    assert_eq!(db.categories().unwrap()[0].title, "Tech");
    db.remove_category(&cat.id).unwrap();
    assert!(db.categories().unwrap().is_empty());
    db.remove_feed(&feed.id).unwrap();
    assert!(db.feeds().unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn opml_roundtrip_with_categories() {
    let (db, path) = scratch_db("opml");
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0"><head><title>subs</title></head><body>
<outline text="Tech" title="Tech">
<outline type="rss" text="BBC News" title="BBC News" xmlUrl="https://feeds.bbci.co.uk/news/rss.xml"/>
<outline type="rss" text="MacRumors" title="MacRumors" xmlUrl="https://feeds.macrumors.com/MacRumors-All"/>
</outline>
<outline type="rss" text="Solo" title="Solo" xmlUrl="https://example.com/solo.xml"/>
</body></opml>"#;
    let added = import_opml(&db, xml).unwrap();
    assert_eq!(added, 3);
    // Importing again adds nothing (URL dedupe).
    assert_eq!(import_opml(&db, xml).unwrap(), 0);
    let feeds = db.feeds().unwrap();
    let bbc = feeds.iter().find(|f| f.title == "BBC News").unwrap();
    assert_eq!(bbc.category.as_deref(), Some("Tech"));
    let out = export_opml(&db).unwrap();
    assert!(out.contains("feeds.bbci.co.uk/news/rss.xml"));
    assert!(out.contains("Tech"));
    // Export -> fresh DB import keeps the same URLs.
    let (db2, path2) = scratch_db("opml2");
    assert_eq!(import_opml(&db2, &out).unwrap(), 3);
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(&path2).ok();
}
