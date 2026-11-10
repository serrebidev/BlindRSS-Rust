//! A Miniflux sync downloads everything once, then only what changed since the
//! last complete sync; a failed sync writes nothing and keeps the old watermark.
use blindrss_core::Db;
use blindrss_core::providers::MinifluxClient;
use std::io::{Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// Two feeds; `/v1/entries` for feed 2 fails with 500 while `fail` is set.
fn server(fail: Arc<AtomicBool>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
            }
            let request = String::from_utf8_lossy(&bytes)
                .lines()
                .next()
                .unwrap()
                .to_owned();
            log.lock().unwrap().push(request.clone());
            let target = request.split_whitespace().nth(1).unwrap();
            let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
            let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            let (status, body) = match url.path() {
                "/v1/feeds" => (
                    200,
                    serde_json::json!([
                        {"id": 1, "title": "One", "feed_url": "https://one.example/feed"},
                        {"id": 2, "title": "Two", "feed_url": "https://two.example/feed"},
                    ]),
                ),
                "/v1/entries"
                    if q.get("feed_id").map(String::as_str) == Some("2")
                        && fail.load(Ordering::SeqCst) =>
                {
                    (500, serde_json::json!({"error_message": "boom"}))
                }
                "/v1/entries" => {
                    let feed: i64 = q["feed_id"].parse().unwrap();
                    let entry = serde_json::json!({"id": feed * 100, "title": format!("Post {feed}"),
                        "status": "unread", "starred": false});
                    (200, serde_json::json!({"total": 1, "entries": [entry]}))
                }
                _ => (200, serde_json::json!([])),
            };
            let body = body.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    (base, requests)
}

fn entry_requests(log: &Mutex<Vec<String>>) -> Vec<String> {
    log.lock()
        .unwrap()
        .drain(..)
        .filter(|r| r.contains("/v1/entries?"))
        .collect()
}

#[test]
fn second_sync_asks_only_for_changes_and_a_failed_sync_writes_nothing() {
    let path = std::env::temp_dir().join(format!("blindrss_mf_incr_{}.db", uuid::Uuid::new_v4()));
    let db = Db::open_or_create(&path).unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let (base, log) = server(fail.clone());
    let client = MinifluxClient::new(&base, "key");

    // Feed 2 fails: nothing stored, no watermark, so the next sync is still full.
    assert!(client.sync(&db, 100).is_err());
    assert_eq!(db.hosted_sync_watermark("miniflux").unwrap(), None);
    assert_eq!(
        db.articles_count(None).unwrap(),
        0,
        "a failed sync must write no articles"
    );
    entry_requests(&log);

    fail.store(false, Ordering::SeqCst);
    assert_eq!(client.sync(&db, 100).unwrap(), (2, 2));
    let first = entry_requests(&log);
    assert_eq!(first.len(), 2, "{first:?}");
    assert!(
        first.iter().all(|r| !r.contains("changed_after")),
        "first sync downloads everything: {first:?}"
    );
    let watermark = db
        .hosted_sync_watermark("miniflux")
        .unwrap()
        .expect("watermark after a full sync");

    assert_eq!(client.sync(&db, 100).unwrap(), (2, 0));
    let second = entry_requests(&log);
    assert_eq!(second.len(), 2, "{second:?}");
    let wanted = format!("changed_after={watermark}");
    assert!(
        second.iter().all(|r| r.contains(&wanted)),
        "second sync is incremental: {second:?}"
    );

    drop(db);
    let _ = std::fs::remove_file(path);
}
