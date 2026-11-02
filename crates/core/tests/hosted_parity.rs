//! Hosted caches must follow server read/bookmark state and retain all pages.
use blindrss_core::Db;
use blindrss_core::config::Config;
use blindrss_core::providers::{GReaderClient, GReaderService, MinifluxClient};
use std::io::{Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

fn server() -> (String, Arc<AtomicBool>, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let changed = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (revision, log) = (changed.clone(), requests.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                let count = stream.read(&mut buf).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..count]);
            }
            let head = String::from_utf8_lossy(&bytes);
            let request = head.lines().next().unwrap().to_owned();
            log.lock().unwrap().push(request.clone());
            let target = request.split_whitespace().nth(1).unwrap();
            let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
            let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            let changed = revision.load(Ordering::SeqCst);
            let body = match url.path() {
                "/v1/feeds" => serde_json::json!([{"id":42,"title":if changed {"New title"} else {"Old title"},"feed_url":"https://example.com/feed","category":{"title":if changed {"New category"} else {"Old category"}}}]),
                "/v1/entries/1" => serde_json::json!({"id":1,"starred":changed}),
                "/v1/entries/1/bookmark" => { revision.store(!changed, Ordering::SeqCst); serde_json::json!({}) },
                "/v1/entries" => {
                    assert!(!params.contains_key("status"), "read entries must also sync");
                    let offset: usize = params.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let entries: Vec<_> = (offset..(offset+2).min(5)).map(|i| serde_json::json!({"id":i+1,"title":format!("Article {i}"),"content":if changed {"Updated body"} else {"Original body"},"status":if changed {"unread"} else {"read"},"starred":!changed})).collect();
                    serde_json::json!({"total":5,"entries":entries})
                },
                "/reader/api/0/subscription/list" => serde_json::json!({"subscriptions":[{"id":"feed/https://example.com/feed","title":if changed {"Other account feed"} else {"Feed"},"url":"https://example.com/feed"}]}),
                path if path.starts_with("/reader/api/0/stream/contents/") => {
                    let second = params.get("c").is_some_and(|s| s == "next/+ token");
                    let mut body = serde_json::json!({"items":[{"id":if second {"g2"} else {"g1"},"title":"Remote","categories":if changed {vec![]} else {vec!["user/123/state/com.google/read","user/123/state/com.google/starred"]},"origin":{"streamId":"feed/https://example.com/feed"}}]});
                    if !second { body["continuation"] = serde_json::json!("next/+ token"); }
                    body
                },
                _ => serde_json::json!({}),
            }.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    (base, changed, requests)
}

fn database() -> (Db, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_hosted_{}.db", uuid::Uuid::new_v4()));
    (Db::open_or_create(&path).unwrap(), path)
}

#[test]
fn miniflux_pages_read_history_and_updates_existing_state() {
    let (base, changed, _) = server();
    let client = MinifluxClient::new(&base, "secret");
    let (db, path) = database();
    assert_eq!(client.sync(&db, 2).unwrap(), (1, 5));
    let article = db.article_by_id("miniflux-entry:1").unwrap().unwrap();
    assert!(article.is_read && article.is_favorite);
    changed.store(true, Ordering::SeqCst);
    assert_eq!(client.sync(&db, 2).unwrap(), (1, 0));
    let article = db.article_by_id("miniflux-entry:1").unwrap().unwrap();
    assert!(!article.is_read && !article.is_favorite);
    assert_eq!(article.content.as_deref(), Some("Updated body"));
    assert_eq!(
        db.feeds().unwrap()[0].category.as_deref(),
        Some("New category")
    );
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn greader_follows_continuation_and_updates_read_favorite_state() {
    let (base, changed, _) = server();
    let mut client =
        GReaderClient::with_oauth(GReaderService::Inoreader, "token").with_cache_ttls(0, 0);
    client.api_base = format!("{base}/reader/api/0");
    let (db, path) = database();
    assert_eq!(client.sync(&db, 2).unwrap(), (1, 2));
    assert!(db.article_by_id("g2").unwrap().unwrap().is_favorite);
    assert!(db.article_by_id("g1").unwrap().unwrap().is_read);
    changed.store(true, Ordering::SeqCst);
    client.invalidate_caches();
    assert_eq!(client.sync(&db, 2).unwrap(), (1, 0));
    assert!(!db.article_by_id("g1").unwrap().unwrap().is_read);
    assert!(!db.article_by_id("g2").unwrap().unwrap().is_favorite);
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn setting_bookmark_to_existing_state_does_not_toggle_server() {
    let (base, changed, requests) = server();
    changed.store(true, Ordering::SeqCst);
    let (db, path) = database();
    MinifluxClient::new(&base, "key").sync(&db, 2).unwrap();
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    cfg.set_path("providers.miniflux.url", serde_json::json!(base));
    cfg.set_path("providers.miniflux.api_key", serde_json::json!("key"));
    let provider = blindrss_core::provider::get_provider(&cfg, path.clone());
    let article = db.article_by_id("miniflux-entry:1").unwrap().unwrap();
    provider.set_favorite(&article, true).unwrap();
    assert!(changed.load(Ordering::SeqCst));
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("/bookmark"))
    );
    drop(provider);
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn miniflux_import_sends_raw_xml_like_python() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let xml = "<?xml version=\"1.0\"?><opml version=\"2.0\"><body/></opml>";
    let captured = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0; 4096];
        let mut expected = usize::MAX;
        while bytes.len() < expected {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let length: usize = header
                    .lines()
                    .find_map(|s| {
                        s.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse().unwrap())
                    })
                    .unwrap();
                expected = end + 4 + length;
            }
        }
        write!(
            stream,
            "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    MinifluxClient::new(&base, "key").opml_import(xml).unwrap();
    let request = captured.join().unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/xml; charset=utf-8")
    );
    assert_eq!(request.split_once("\r\n\r\n").unwrap().1, xml);
}

#[test]
fn miniflux_category_creation_and_feed_edit_use_server_api() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let captured = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for body in ["{\"id\":9}", "[{\"id\":9,\"title\":\"News\"}]", "{}"] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            let mut expected = usize::MAX;
            while bytes.len() < expected {
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = header
                        .lines()
                        .find_map(|s| {
                            s.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    expected = end + 4 + length;
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
        requests
    });
    let client = MinifluxClient::new(&base, "key");
    assert_eq!(client.create_category_remote("News").unwrap(), 9);
    client
        .update_feed_remote(
            42,
            Some("Renamed"),
            Some("https://example.com/new.xml"),
            Some("news"),
        )
        .unwrap();
    let requests = captured.join().unwrap();
    assert!(requests[0].starts_with("POST /v1/categories "));
    assert!(requests[2].starts_with("PUT /v1/feeds/42 "));
    let body: serde_json::Value =
        serde_json::from_str(requests[2].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"title":"Renamed","feed_url":"https://example.com/new.xml","category_id":9})
    );
}

#[test]
fn inoreader_caches_do_not_cross_accounts() {
    let (base, changed, _) = server();
    let mut first = GReaderClient::with_oauth(GReaderService::Inoreader, "first-account-token");
    first.api_base = format!("{base}/reader/api/0");
    first.invalidate_caches();
    assert_eq!(first.subscriptions().unwrap()[0].title, "Feed");
    assert!(
        first
            .stream_articles("feed/https://example.com/feed", 20)
            .unwrap()[0]
            .is_read
    );
    changed.store(true, Ordering::SeqCst);
    let mut second = GReaderClient::with_oauth(GReaderService::Inoreader, "second-account-token");
    second.api_base = first.api_base.clone();
    assert_eq!(
        second.subscriptions().unwrap()[0].title,
        "Other account feed"
    );
    assert!(
        !second
            .stream_articles("feed/https://example.com/feed", 20)
            .unwrap()[0]
            .is_read
    );
}
