//! B16 refresh-half tests: the per-feed impersonation
//! stage in `fetch_feed_document_cancellable` (Python
//! providers/local.py's reserved impersonated attempt,
//! issue #29). The loopback server plays a WAF: it answers
//! every request with a 200 HTML block page unless the
//! request carries the impersonated client's
//! Sec-Fetch navigation headers (the plain ureq pipeline
//! never sends Sec-Fetch-*), in which case it serves the
//! feed. Loopback only; live acceptance is a manual
//! check.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

const RSS: &str = "<rss version='2.0'><channel><title>Impersonated Feed</title><item><guid>g1</guid><title>Item One</title></item></channel></rss>";
const BLOCK_PAGE: &str =
    "<html><head><title>Blocked</title></head><body><p>Access denied.</p></body></html>";

fn waf_server() -> (String, std::sync::Arc<AtomicUsize>) {
    let requests = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let counter = &counter;
            let mut raw: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            counter.fetch_add(1, Ordering::SeqCst);
            let head = String::from_utf8_lossy(&raw).to_lowercase();
            let (status, content_type, body) = if head.contains("sec-fetch-mode: navigate") {
                (200, "application/rss+xml", RSS)
            } else {
                (200, "text/html", BLOCK_PAGE)
            };
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (format!("http://{addr}/feed"), requests)
}

fn temp_db(name: &str) -> (std::path::PathBuf, blindrss_core::Db) {
    let dir = std::env::temp_dir().join(format!(
        "blindrss-imp-refresh-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = blindrss_core::Db::open_or_create(&dir.join("rss.db")).unwrap();
    (dir, db)
}

#[test]
fn refresh_recovers_a_blocked_feed_through_impersonation() {
    use blindrss_core::feeds;
    let (url, _requests) = waf_server();
    let (dir, db) = temp_db("auto");
    let fetched = feeds::fetch_feed_document_cancellable(
        &db,
        &url,
        false,
        None,
        None,
        "",
        &feeds::FeedFetchOverrides::default(),
    )
    .unwrap();
    match fetched {
        feeds::FeedFetch::Fetched {
            parsed,
            body_digest,
        } => {
            assert_eq!(parsed.title, "Impersonated Feed");
            assert_eq!(parsed.entries.len(), 1);
            // Recovered-body contract: the digest is
            // never recorded.
            assert_eq!(body_digest, "");
        }
        other => panic!("expected an impersonation-recovered fetch, got {other:?}"),
    }
    // Validators cleared in the HTTP cache.
    let cached = db.cached_feed(&url).unwrap().unwrap();
    assert_eq!(cached.0, None);
    assert_eq!(cached.1, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refresh_never_mode_skips_the_impersonation_stage() {
    use blindrss_core::feeds;
    let (url, requests) = waf_server();
    let (dir, db) = temp_db("never");
    let overrides = feeds::FeedFetchOverrides::from_settings(&serde_json::json!({
        "impersonate": "never"
    }));
    assert_eq!(overrides.impersonate, "never");
    let result =
        feeds::fetch_feed_document_cancellable(&db, &url, false, None, None, "", &overrides);
    assert!(
        result.is_err(),
        "the block page must stay an error: {result:?}"
    );
    // Exactly one request reached the server: the plain
    // fetch. No impersonated second attempt.
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
