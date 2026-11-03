//! Refresh integration test: a one-shot local HTTP server serves an
//! RSS document; refresh must store its entries as unread articles in a
//! Python-schema database, and a second refresh must store nothing new.

use blindrss_core::{Db, Feed, refresh_feed};
use std::io::{Read, Write};
use std::path::PathBuf;

const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Served Feed</title>
<item><title>Served One</title><link>https://example.com/s1</link><guid>s1</guid>
<description>Body one</description><pubDate>Fri, 30 Oct 2026 10:00:00 GMT</pubDate></item>
<item><title>Served Two</title><link>https://example.com/s2</link><guid>s2</guid>
<description>Body two</description></item>
</channel></rss>"#;

fn serve_once() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..2 {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    RSS.len(),
                    RSS
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });
    format!("http://{addr}/feed.xml")
}

fn scratch_db() -> (Db, PathBuf) {
    let path =
        std::env::temp_dir().join(format!("blindrss_refresh_test_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // Build with the Python schema (same statements as core/db.py).
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
fn refresh_stores_new_articles_once() {
    let url = serve_once();
    let (db, path) = scratch_db();
    let feed = Feed {
        id: "served".to_string(),
        url,
        title: "Served Feed".to_string(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    };
    let inserted = refresh_feed(&db, &feed, "deleted").unwrap();
    assert_eq!(inserted, 2);
    assert_eq!(db.unread_count(None).unwrap(), 2);
    let articles = db.articles(Some("served"), 10).unwrap();
    assert_eq!(articles.len(), 2);
    assert!(articles.iter().any(|a| a.title == "Served One"));
    // Second refresh: dedupe on entry id, nothing new.
    let inserted = refresh_feed(&db, &feed, "deleted").unwrap();
    assert_eq!(inserted, 0);
    std::fs::remove_file(&path).ok();
}

#[test]
fn refresh_revalidates_cached_feed_and_stores_tags() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let size = stream.read(&mut bytes).unwrap();
            requests.push(String::from_utf8_lossy(&bytes[..size]).to_lowercase());
            let body = "<rss version='2.0'><channel><title>Feed</title><item><guid>cached-item</guid><title>Tagged</title><category>Technology</category></item></channel></rss>";
            let reply = if attempt == 0 {
                format!(
                    "HTTP/1.1 200 OK\r\nETag: \"refresh1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".into()
            };
            stream.write_all(reply.as_bytes()).unwrap();
        }
        requests
    });
    let path = std::env::temp_dir().join(format!("blindrss-refresh-{}.db", uuid::Uuid::new_v4()));
    let db = Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed(&format!("http://{address}/feed"), "Feed", None)
        .unwrap();
    assert_eq!(refresh_feed(&db, &feed, "deleted").unwrap(), 1);
    assert_eq!(db.article_tags("cached-item").unwrap(), vec!["Technology"]);
    assert_eq!(refresh_feed(&db, &feed, "deleted").unwrap(), 0);
    let requests = server.join().unwrap();
    assert!(requests[1].contains("if-none-match: \"refresh1\""));
    drop(db);
    std::fs::remove_file(path).unwrap();
}
