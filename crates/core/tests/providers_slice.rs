//! Slice 5 tests: Google Reader family + Miniflux clients against a
//! scripted local HTTP server; sync must land feeds + articles in a
//! Python-schema database.

use blindrss_core::Db;
use blindrss_core::providers::{GReaderClient, GReaderService, MinifluxClient};
use std::io::{Read, Write};
use std::path::PathBuf;

fn route(method: &str, path: &str) -> (u16, &'static str) {
    if method == "POST" && path.starts_with("/accounts/ClientLogin") {
        return (200, "SID=s\nLSID=l\nAuth=tok123\n");
    }
    if path.starts_with("/reader/api/0/subscription/list") {
        return (
            200,
            r#"{"subscriptions":[
              {"id":"feed/https://example.com/a.xml","title":"Feed A","url":"https://example.com/a.xml","categories":[{"id":"user/-/label/Tech","label":"Tech"}]},
              {"id":"feed/https://example.com/b.xml","title":"Feed B","url":"https://example.com/b.xml","categories":[]}
            ]}"#,
        );
    }
    if path.starts_with("/reader/api/0/stream/contents/") {
        return (
            200,
            r#"{"items":[
              {"id":"tag:google.com,2005:reader/item/0001","title":"Remote One",
               "published":1793400000,"author":"Ann",
               "alternate":[{"href":"https://example.com/1"}],
               "summary":{"content":"Summary one"},
               "origin":{"streamId":"feed/https://example.com/a.xml","title":"Feed A"}},
              {"id":"tag:google.com,2005:reader/item/0002","title":"Remote Two",
               "published":1793400100,"author":"",
               "alternate":[{"href":"https://example.com/2"}],
               "content":{"content":"Full body two"},
               "origin":{"streamId":"feed/https://example.com/a.xml","title":"Feed A"}}
            ]}"#,
        );
    }
    if path.starts_with("/reader/api/0/token") {
        return (200, "edittoken");
    }
    if method == "POST" && path.starts_with("/reader/api/0/edit-tag") {
        return (200, "OK");
    }
    if path.starts_with("/v1/feeds") {
        return (
            200,
            r#"[{"id":42,"title":"MF Feed","feed_url":"https://example.com/mf.xml","site_url":"https://example.com","category":{"id":1,"title":"News"}}]"#,
        );
    }
    if path.starts_with("/v1/entries") && method == "GET" {
        return (
            200,
            r#"{"total":1,"entries":[{"id":7,"title":"MF One","url":"https://example.com/m1","content":"<p>mf body</p>","author":"Bo","published_at":"2026-10-30T10:00:00Z","status":"unread"}]}"#,
        );
    }
    if path.starts_with("/v1/entries") && method == "PUT" {
        return (201, "");
    }
    (404, "not found")
}

fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read headers, then body per Content-Length.
                let mut header_end = None;
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                                header_end = Some(pos + 4);
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let Some(hend) = header_end else { return };
                let head = String::from_utf8_lossy(&buf[..hend]).to_string();
                let mut parts = head.split_whitespace();
                let method = parts.next().unwrap_or("GET").to_string();
                let path = parts.next().unwrap_or("/").to_string();
                let clen: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                let mut body_len = buf.len() - hend;
                while body_len < clen {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => body_len += n,
                        Err(_) => break,
                    }
                }
                let (status, body) = route(&method, &path);
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    format!("http://{addr}")
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_prov_{tag}_{}.db", std::process::id()));
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
fn greader_login_subscriptions_and_sync() {
    let base = serve();
    let (db, path) = scratch_db("gr");
    let mut client = GReaderClient::new(GReaderService::TheOldReader, "u@example.com", "pw");
    client.api_base = format!("{base}/reader/api/0");
    client.login_url = format!("{base}/accounts/ClientLogin");

    let subs = client.subscriptions().unwrap();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].category.as_deref(), Some("Tech"));

    let (feeds, new) = client.sync(&db, 50).unwrap();
    assert_eq!(feeds, 2);
    // Both stream fetches return the same 2 items for feed A; the
    // second store dedupes, feed B's fetch returns the same payload
    // in this scripted server but items carry feed A's origin id...
    // they still insert once. Total stored articles: 2.
    assert_eq!(new, 2);
    let articles = db.articles(None, 10).unwrap();
    assert_eq!(articles.len(), 2);
    assert!(articles.iter().any(|a| a.title == "Remote One"));
    assert_eq!(db.feeds().unwrap().len(), 2);

    client
        .mark_read_remote("tag:google.com,2005:reader/item/0001")
        .unwrap();
    std::fs::remove_file(&path).ok();
}

#[test]
fn miniflux_sync_and_mark_read() {
    let base = serve();
    let (db, path) = scratch_db("mf");
    let client = MinifluxClient::new(&base, "secret-key");
    let (feeds, new) = client.sync(&db, 50).unwrap();
    assert_eq!(feeds, 1);
    assert_eq!(new, 1);
    let stored_feeds = db.feeds().unwrap();
    assert_eq!(stored_feeds[0].id, "miniflux:42");
    assert_eq!(stored_feeds[0].category.as_deref(), Some("News"));
    let articles = db.articles(None, 10).unwrap();
    assert_eq!(articles[0].title, "MF One");
    client.mark_read_remote(&[7]).unwrap();
    std::fs::remove_file(&path).ok();
}
