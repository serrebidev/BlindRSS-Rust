//! Backup/restore, retention, and release-parse tests.

use blindrss_core::maintenance::{create_backup, parse_latest_release, restore_backup};

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("blindrss_mnt_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn backup_restore_roundtrip() {
    let db_path = scratch("src.db");
    let cfg_path = scratch("src.json");
    let zip_path = scratch("backup.zip");
    std::fs::write(&cfg_path, br#"{"locale": "fr"}"#).unwrap();
    {
        // A real DB through the Python-schema fixture helper path:
        // open via blindrss Db on a schema-less file, then add the
        // articles table the fixture builder normally provides.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE articles (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO articles VALUES ('a1', 'Hello');",
        )
        .unwrap();
    }
    create_backup(&db_path, &cfg_path, &zip_path).unwrap();

    let db2 = scratch("dst.db");
    let cfg2 = scratch("dst.json");
    restore_backup(&zip_path, &db2, &cfg2).unwrap();
    let conn = rusqlite::Connection::open(&db2).unwrap();
    let title: String = conn
        .query_row("SELECT title FROM articles WHERE id='a1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Hello");
    assert_eq!(
        std::fs::read_to_string(&cfg2).unwrap(),
        r#"{"locale": "fr"}"#
    );
}

#[test]
fn prune_removes_old_unread_keeps_favorites_and_tombstones() {
    let path = scratch("prune.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE articles (id TEXT PRIMARY KEY, feed_id TEXT, title TEXT, \
         url TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0, \
         media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL);
         CREATE TABLE deleted_articles (feed_id TEXT, article_id TEXT, url TEXT, \
         deleted_at REAL, title TEXT, content TEXT, description TEXT, date TEXT, \
         author TEXT, media_url TEXT, media_type TEXT, chapter_url TEXT);
         INSERT INTO articles (id, feed_id, date, is_read, is_favorite) VALUES
           ('old-read', 'f1', '2000-01-01', 1, 0),
           ('old-unread', 'f1', '2000-01-01', 0, 0),
           ('old-fav', 'f1', '2000-01-01', 1, 1),
           ('new-read', 'f1', '2999-01-01', 1, 0);
         INSERT INTO deleted_articles (article_id, deleted_at) VALUES ('ancient', 1.0);",
    )
    .unwrap();
    drop(conn);
    let db = blindrss_core::Db::open(&path).unwrap();
    // Python cleanup_old_articles (Audit 8 D04/D05): old read AND
    // unread non-favorites are removed; favorites survive; and
    // tombstones are never aged out by prune (only restore or a
    // confirmed purge removes them), so 'ancient' stays as the
    // resurrection guard for its article.
    let removed = db.prune_old_articles(30).unwrap();
    assert_eq!(removed, 2); // old-read + old-unread articles
    let left = db.articles(None, 10).unwrap();
    assert_eq!(left.len(), 2); // old-fav + new-read
    assert!(db.deleted_article_ids().unwrap().contains("ancient"));
}

#[test]
fn release_parse() {
    let json = r#"{"tag_name": "v0.2.0", "assets": [{"browser_download_url": "https://example.com/app.zip"}]}"#;
    let (tag, url) = parse_latest_release(json).unwrap();
    assert_eq!(tag, "v0.2.0");
    assert_eq!(url.as_deref(), Some("https://example.com/app.zip"));
    assert!(parse_latest_release("{}").is_none());
}
