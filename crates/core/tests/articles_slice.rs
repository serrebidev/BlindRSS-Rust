//! Slice 4 tests: delete + tombstone + restore, mark-all-read,
//! favorites, search, and refresh respecting tombstones.

use blindrss_core::{Db, Feed, ParsedEntry, ParsedFeed, store_parsed};
use std::path::PathBuf;

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_art_{tag}_{}.db", std::process::id()));
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
         media_url TEXT, media_type TEXT, chapter_url TEXT);
         INSERT INTO feeds VALUES ('f1','https://example.com/f','Feed',0,NULL,NULL);
         INSERT INTO articles (id,feed_id,title,url,content,description,date,is_read,is_favorite) \
         VALUES ('a1','f1','Rust news','https://example.com/1','All about rust','d1','2026-10-30',0,1);
         INSERT INTO articles (id,feed_id,title,url,content,description,date,is_read,is_favorite) \
         VALUES ('a2','f1','Other topic','https://example.com/2','Nothing here','d2','2026-10-29',0,0);",
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path)
}

#[test]
fn delete_tombstone_restore_cycle() {
    let (db, path) = scratch_db("del");
    assert!(db.delete_article("a1").unwrap());
    assert_eq!(db.articles(None, 10).unwrap().len(), 1);
    let deleted = db.deleted_articles().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].title, "Rust news");
    // Refresh must not resurrect it.
    let feed = Feed {
        id: "f1".into(),
        url: "https://example.com/f".into(),
        title: "Feed".into(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    };
    let parsed = ParsedFeed {
        title: "Feed".into(),
        entries: vec![ParsedEntry {
            id: "a1".into(),
            title: "Rust news".into(),
            url: "https://example.com/1".into(),
            content_text: "All about rust".into(),
            summary: String::new(),
            date: None,
            author: None,
            media_url: None,
            media_type: None,
            tags: Vec::new(),
        }],
    };
    assert_eq!(store_parsed(&db, &feed, &parsed).unwrap(), 0);
    // Restore brings it back.
    assert!(db.restore_article("a1").unwrap());
    assert_eq!(db.articles(None, 10).unwrap().len(), 2);
    assert!(db.deleted_articles().unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn mark_all_read_favorites_search() {
    let (db, path) = scratch_db("misc");
    assert_eq!(db.unread_count(None).unwrap(), 2);
    assert_eq!(db.mark_all_read(None).unwrap(), 2);
    assert_eq!(db.unread_count(None).unwrap(), 0);
    let favs = db.favorites(10).unwrap();
    assert_eq!(favs.len(), 1);
    assert_eq!(favs[0].id, "a1");
    let hits = db.search_articles("rust", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(db.search_articles("zzz-no-match", 10).unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}
