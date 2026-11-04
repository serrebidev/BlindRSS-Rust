//! Audit 8 Task 9: hosted account and authentication lifecycle.
//!
//! D07 — Inoreader token refresh must be reachable from the
//! provider: expired tokens refresh through the OAuth helper,
//! rotated tokens persist, a revoked grant clears the stored
//! tokens, and a transient failure keeps them.
//! D08 — Inoreader requests carry the configured AppId/AppKey
//! headers (Python `providers/inoreader.py::_headers`).
//! D09 — a complete successful hosted sync reconciles the cache
//! with the remote subscription inventory: a feed removed on
//! the server disappears locally; a failed or partial sync
//! changes nothing.
//! D10 — remote category inventories sync even when a category
//! has no feeds (Python `get_categories`), so an empty category
//! survives sync and can be assigned; a category removed on the
//! server and unreferenced locally is pruned.
//! D23 — hosted category Mark All Read uses provider category
//! membership (GReader label stream / Miniflux category
//! endpoint), so an article labeled into a category from an
//! outside feed is marked read remotely and in the cache.
//! Fixture HTTP servers on loopback only; no live accounts.

use blindrss_core::config::Config;
use blindrss_core::oauth::{InoreaderAuth, TokenAction};
use blindrss_core::provider;
use blindrss_core::providers::{GReaderClient, GReaderService, MinifluxClient};
use blindrss_core::{Article, Db};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Fixture server: records every raw request, answers through a
// caller-supplied router over the raw request text.
// ---------------------------------------------------------------------------

struct Fixture {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

fn fixture<F>(router: F) -> Fixture
where
    F: Fn(&str) -> (u16, String) + Send + Sync + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let router = Arc::new(router);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut data = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let head_end = data
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4);
                if let Some(end) = head_end {
                    let head = String::from_utf8_lossy(&data[..end]).into_owned();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= end + len {
                        break;
                    }
                }
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => data.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let raw = String::from_utf8_lossy(&data).into_owned();
            recorded.lock().unwrap().push(raw.clone());
            let (status, body) = router(&raw);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    Fixture { base, requests }
}

fn request_line(raw: &str) -> &str {
    raw.lines().next().unwrap_or("")
}

fn path_of(raw: &str) -> String {
    let target = request_line(raw).split_whitespace().nth(1).unwrap_or("");
    target.split('?').next().unwrap_or("").to_string()
}

fn body_of(raw: &str) -> &str {
    raw.split("\r\n\r\n").nth(1).unwrap_or("")
}

fn database() -> (Db, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_task9_{}.db", uuid::Uuid::new_v4()));
    (Db::open_or_create(&path).unwrap(), path)
}

// ---------------------------------------------------------------------------
// D07 — token refresh lifecycle against a loopback token endpoint
// ---------------------------------------------------------------------------

fn auth_at(token_url: &str) -> InoreaderAuth {
    InoreaderAuth {
        app_id: "app-id".into(),
        app_key: "app-key".into(),
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        // Expired an hour ago.
        token_expires_at: 1_000,
        token_url: token_url.into(),
    }
}

#[test]
fn d07_refresh_success_rotates_and_stores_tokens() {
    let fx = fixture(|raw| {
        assert!(request_line(raw).starts_with("POST /oauth2/token"));
        let body = body_of(raw);
        assert!(body.contains("grant_type=refresh_token"), "{body}");
        assert!(body.contains("refresh_token=old-refresh"), "{body}");
        assert!(body.contains("client_id=app-id"), "{body}");
        (
            200,
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#
                .into(),
        )
    });
    let mut auth = auth_at(&format!("{}/oauth2/token", fx.base));
    let action = auth.ensure_fresh(2_000).unwrap();
    assert_eq!(action, TokenAction::Refreshed);
    assert_eq!(auth.access_token, "new-access");
    assert_eq!(auth.refresh_token, "new-refresh");
    // Python _set_tokens: now + expires_in - 60.
    assert_eq!(auth.token_expires_at, 2_000 + 3_600 - 60);
    assert_eq!(fx.requests.lock().unwrap().len(), 1);
}

#[test]
fn d07_refresh_without_rotation_keeps_refresh_token() {
    let fx = fixture(|_| {
        (
            200,
            r#"{"access_token":"new-access","expires_in":3600}"#.into(),
        )
    });
    let mut auth = auth_at(&format!("{}/oauth2/token", fx.base));
    assert_eq!(auth.ensure_fresh(2_000).unwrap(), TokenAction::Refreshed);
    assert_eq!(auth.access_token, "new-access");
    assert_eq!(auth.refresh_token, "old-refresh");
}

#[test]
fn d07_revoked_grant_clears_tokens() {
    let fx = fixture(|_| (400, r#"{"error":"invalid_grant"}"#.into()));
    let mut auth = auth_at(&format!("{}/oauth2/token", fx.base));
    let action = auth.ensure_fresh(2_000).unwrap();
    assert_eq!(action, TokenAction::Revoked);
    assert_eq!(auth.access_token, "");
    assert_eq!(auth.refresh_token, "");
    assert_eq!(auth.token_expires_at, 0);
}

#[test]
fn d07_transient_failure_preserves_tokens() {
    let fx = fixture(|_| (500, r#"{"error":"server_error"}"#.into()));
    let mut auth = auth_at(&format!("{}/oauth2/token", fx.base));
    assert!(auth.ensure_fresh(2_000).is_err());
    assert_eq!(auth.access_token, "old-access");
    assert_eq!(auth.refresh_token, "old-refresh");
    assert_eq!(auth.token_expires_at, 1_000);
}

#[test]
fn d07_unreachable_endpoint_preserves_tokens() {
    // Nothing listens on this port: a network failure is
    // transient, never a revocation.
    let mut auth = auth_at("http://127.0.0.1:1/oauth2/token");
    assert!(auth.ensure_fresh(2_000).is_err());
    assert_eq!(auth.access_token, "old-access");
    assert_eq!(auth.refresh_token, "old-refresh");
}

#[test]
fn d07_fresh_token_makes_no_request() {
    let fx = fixture(|_| panic!("a fresh token must not hit the token endpoint"));
    let mut auth = auth_at(&format!("{}/oauth2/token", fx.base));
    auth.token_expires_at = 9_999_999;
    assert_eq!(auth.ensure_fresh(2_000).unwrap(), TokenAction::Fresh);
    assert!(fx.requests.lock().unwrap().is_empty());
}

#[test]
fn d07_stale_token_without_refresh_token_is_used_as_is() {
    // Python _has_required_auth: no refresh token -> fall back
    // to whatever access token exists.
    let mut auth = auth_at("http://127.0.0.1:1/oauth2/token");
    auth.refresh_token = String::new();
    assert_eq!(auth.ensure_fresh(2_000).unwrap(), TokenAction::Fresh);
    let mut auth = auth_at("http://127.0.0.1:1/oauth2/token");
    auth.access_token = String::new();
    auth.refresh_token = String::new();
    assert!(auth.ensure_fresh(2_000).is_err());
}

// ---------------------------------------------------------------------------
// D08 — AppId/AppKey headers on every Inoreader request
// ---------------------------------------------------------------------------

#[test]
fn d08_inoreader_requests_carry_app_headers() {
    let fx = fixture(|raw| {
        let path = path_of(raw);
        if path.ends_with("/token") {
            return (200, "edit-token".into());
        }
        if path.ends_with("/subscription/list") {
            return (200, r#"{"subscriptions":[]}"#.into());
        }
        (200, "{}".into())
    });
    let mut client = GReaderClient::with_oauth(GReaderService::Inoreader, "access-token")
        .with_app_credentials("the-app-id", "the-app-key");
    client.api_base = format!("{}/reader/api/0", fx.base);
    client.subscriptions().unwrap();
    client
        .set_read_remote("tag:google.com,2005:reader/item/1", true)
        .unwrap();
    let requests = fx.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for raw in requests.iter() {
        let lower = raw.to_ascii_lowercase();
        assert!(lower.contains("appid: the-app-id"), "{raw}");
        assert!(lower.contains("appkey: the-app-key"), "{raw}");
        assert!(
            lower.contains("authorization: bearer access-token"),
            "{raw}"
        );
    }
}

#[test]
fn d08_other_services_send_no_app_headers() {
    let fx = fixture(|_| (200, r#"{"subscriptions":[]}"#.into()));
    let mut client = GReaderClient::new(GReaderService::TheOldReader, "a@example.com", "pw")
        .with_app_credentials("the-app-id", "the-app-key");
    client.api_base = format!("{}/reader/api/0", fx.base);
    client.login_url = format!("{}/accounts/ClientLogin", fx.base);
    // ClientLogin against the fixture returns no Auth token, so
    // the call fails — but the login request itself must not
    // carry Inoreader app headers either.
    let _ = client.subscriptions();
    let requests = fx.requests.lock().unwrap();
    assert!(!requests.is_empty());
    for raw in requests.iter() {
        assert!(!raw.to_ascii_lowercase().contains("appid:"), "{raw}");
    }
}

// ---------------------------------------------------------------------------
// D09/D10 — GReader sync reconciliation and label inventory
// ---------------------------------------------------------------------------

struct GReaderWorld {
    subs: Mutex<Vec<(&'static str, &'static str)>>, // (feed url, category)
    fail_subs: std::sync::atomic::AtomicBool,
    fail_stream_b: std::sync::atomic::AtomicBool,
}

fn greader_fixture() -> (Fixture, Arc<GReaderWorld>) {
    let world = Arc::new(GReaderWorld {
        subs: Mutex::new(vec![
            ("https://a.example/rss", "News"),
            ("https://b.example/rss", "News"),
        ]),
        fail_subs: std::sync::atomic::AtomicBool::new(false),
        fail_stream_b: std::sync::atomic::AtomicBool::new(false),
    });
    let routed = world.clone();
    let fx = fixture(move |raw| {
        let path = path_of(raw);
        if path.ends_with("/subscription/list") {
            if routed.fail_subs.load(std::sync::atomic::Ordering::SeqCst) {
                return (500, "{}".into());
            }
            let subs: Vec<serde_json::Value> = routed
                .subs
                .lock()
                .unwrap()
                .iter()
                .map(|(url, cat)| {
                    serde_json::json!({
                        "id": format!("feed/{url}"),
                        "title": format!("Feed {url}"),
                        "url": url,
                        "categories": [{"id": format!("user/-/label/{cat}"), "label": cat}],
                    })
                })
                .collect();
            return (200, serde_json::json!({"subscriptions": subs}).to_string());
        }
        if path.ends_with("/tag/list") {
            return (
                200,
                r#"{"tags":[{"id":"user/-/state/com.google/read"},{"id":"user/-/label/News"},{"id":"user/-/label/Reading List"}]}"#
                    .into(),
            );
        }
        if path.contains("/stream/contents/") {
            if routed
                .fail_stream_b
                .load(std::sync::atomic::Ordering::SeqCst)
                && raw.contains("b.example")
            {
                return (500, "{}".into());
            }
            let item = serde_json::json!({
                "id": format!("item-{}", if raw.contains("b.example") { "b" } else { "a" }),
                "title": "Post",
                "published": 1700000000,
                "origin": {"streamId": format!("feed/{}", if raw.contains("b.example") { "https://b.example/rss" } else { "https://a.example/rss" })},
            });
            return (200, serde_json::json!({"items": [item]}).to_string());
        }
        (200, "{}".into())
    });
    (fx, world)
}

fn greader_client(base: &str) -> GReaderClient {
    let mut client = GReaderClient::with_oauth(GReaderService::Inoreader, "sync-token");
    client.api_base = format!("{base}/reader/api/0");
    client
}

#[test]
fn d09_greader_sync_removes_remotely_deleted_feed() {
    let (fx, world) = greader_fixture();
    let client = greader_client(&fx.base);
    let (db, path) = database();
    // A local-shaped row in the same file is not the server's
    // business and must survive reconciliation.
    let local = db
        .add_feed("https://local.example/rss", "Local", None)
        .unwrap();
    assert_eq!(client.sync(&db, 10).unwrap().0, 2);
    assert!(db.article_by_id("item-b").unwrap().is_some());
    // B is unsubscribed on the server; the next complete sync
    // drops it (feed, articles) from the cache.
    world
        .subs
        .lock()
        .unwrap()
        .retain(|(url, _)| *url != "https://b.example/rss");
    client.invalidate_caches();
    assert_eq!(client.sync(&db, 10).unwrap().0, 1);
    let ids: Vec<String> = db.feeds().unwrap().into_iter().map(|f| f.id).collect();
    assert!(
        ids.iter().any(|id| id == "feed/https://a.example/rss"),
        "{ids:?}"
    );
    assert!(!ids.iter().any(|id| id.contains("b.example")), "{ids:?}");
    assert!(ids.contains(&local.id), "{ids:?}");
    assert!(db.article_by_id("item-b").unwrap().is_none());
    assert!(db.article_by_id("item-a").unwrap().is_some());
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn d09_greader_failed_or_partial_sync_keeps_removed_feed() {
    let (fx, world) = greader_fixture();
    let client = greader_client(&fx.base);
    let (db, path) = database();
    assert_eq!(client.sync(&db, 10).unwrap().0, 2);
    world
        .subs
        .lock()
        .unwrap()
        .retain(|(url, _)| *url != "https://b.example/rss");
    // Subscription fetch fails: no reconciliation.
    world
        .fail_subs
        .store(true, std::sync::atomic::Ordering::SeqCst);
    client.invalidate_caches();
    assert!(client.sync(&db, 10).is_err());
    assert_eq!(db.feeds().unwrap().len(), 2);
    // A stream fetch fails mid-sync (partial): no reconciliation.
    world
        .fail_subs
        .store(false, std::sync::atomic::Ordering::SeqCst);
    world
        .fail_stream_b
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // Bring B back so its stream is fetched (and fails).
    world
        .subs
        .lock()
        .unwrap()
        .push(("https://b.example/rss", "News"));
    client.invalidate_caches();
    assert!(client.sync(&db, 10).is_err());
    assert_eq!(db.feeds().unwrap().len(), 2);
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn d10_greader_empty_label_survives_sync() {
    let (fx, _world) = greader_fixture();
    let client = greader_client(&fx.base);
    let (db, path) = database();
    client.sync(&db, 10).unwrap();
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    // "Reading List" has no subscription; it comes from the
    // tag/list inventory, like Python's get_categories.
    assert!(titles.iter().any(|t| t == "Reading List"), "{titles:?}");
    assert!(titles.iter().any(|t| t == "News"), "{titles:?}");
    drop(db);
    let _ = std::fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// D09/D10 — Miniflux sync reconciliation and category inventory
// ---------------------------------------------------------------------------

struct MfWorld {
    feeds: Mutex<Vec<i64>>,
    fail_feeds: std::sync::atomic::AtomicBool,
    categories: Mutex<Vec<(i64, &'static str)>>,
}

fn mf_fixture() -> (Fixture, Arc<MfWorld>) {
    let world = Arc::new(MfWorld {
        feeds: Mutex::new(vec![1, 2]),
        fail_feeds: std::sync::atomic::AtomicBool::new(false),
        categories: Mutex::new(vec![(10, "News"), (11, "Empty Corner")]),
    });
    let routed = world.clone();
    let fx = fixture(move |raw| {
        let path = path_of(raw);
        if path == "/v1/feeds" {
            if routed.fail_feeds.load(std::sync::atomic::Ordering::SeqCst) {
                return (500, "{}".into());
            }
            let feeds: Vec<serde_json::Value> = routed
                .feeds
                .lock()
                .unwrap()
                .iter()
                .map(|id| {
                    serde_json::json!({
                        "id": id,
                        "title": format!("Feed {id}"),
                        "feed_url": format!("https://f{id}.example/rss"),
                        "category": {"id": 10, "title": "News"},
                    })
                })
                .collect();
            return (200, serde_json::Value::Array(feeds).to_string());
        }
        if path == "/v1/categories" {
            let cats: Vec<serde_json::Value> = routed
                .categories
                .lock()
                .unwrap()
                .iter()
                .map(|(id, title)| serde_json::json!({"id": id, "title": title}))
                .collect();
            return (200, serde_json::Value::Array(cats).to_string());
        }
        if path == "/v1/entries" {
            let feed_id = if raw.contains("feed_id=2") { 2 } else { 1 };
            let entry = serde_json::json!({
                "id": 100 + feed_id, "title": "Post", "url": "https://x.example/",
                "content": "Body", "published_at": "2024-01-01T00:00:00Z",
                "status": "unread", "starred": false, "enclosures": [],
            });
            return (
                200,
                serde_json::json!({"total": 1, "entries": [entry]}).to_string(),
            );
        }
        if path.starts_with("/v1/categories/") && path.ends_with("/mark-all-as-read") {
            return (200, "{}".into());
        }
        (200, "{}".into())
    });
    (fx, world)
}

#[test]
fn d09_miniflux_sync_removes_remotely_deleted_feed() {
    let (fx, world) = mf_fixture();
    let client = MinifluxClient::new(&fx.base, "key");
    let (db, path) = database();
    assert_eq!(client.sync(&db, 10).unwrap().0, 2);
    world.feeds.lock().unwrap().retain(|id| *id != 2);
    assert_eq!(client.sync(&db, 10).unwrap().0, 1);
    let ids: Vec<String> = db.feeds().unwrap().into_iter().map(|f| f.id).collect();
    assert_eq!(ids, vec!["miniflux:1".to_string()]);
    assert!(db.article_by_id("miniflux-entry:102").unwrap().is_none());
    assert!(db.article_by_id("miniflux-entry:101").unwrap().is_some());
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn d09_miniflux_failed_sync_keeps_removed_feed() {
    let (fx, world) = mf_fixture();
    let client = MinifluxClient::new(&fx.base, "key");
    let (db, path) = database();
    assert_eq!(client.sync(&db, 10).unwrap().0, 2);
    world.feeds.lock().unwrap().retain(|id| *id != 2);
    world
        .fail_feeds
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(client.sync(&db, 10).is_err());
    assert_eq!(db.feeds().unwrap().len(), 2);
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn d10_miniflux_empty_category_survives_sync_and_takes_assignment() {
    let (fx, _world) = mf_fixture();
    let client = MinifluxClient::new(&fx.base, "key");
    let (db, path) = database();
    client.sync(&db, 10).unwrap();
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    assert!(titles.iter().any(|t| t == "Empty Corner"), "{titles:?}");
    // Assignment: the feed edit resolves the empty category's
    // server id like any other.
    client
        .update_feed_remote(1, None, None, Some("Empty Corner"))
        .unwrap();
    let requests = fx.requests.lock().unwrap();
    let put = requests
        .iter()
        .find(|r| request_line(r).starts_with("PUT /v1/feeds/1"))
        .expect("feed update request");
    assert!(
        body_of(put).contains(r#""category_id":11"#),
        "{}",
        body_of(put)
    );
    drop(db);
    let _ = std::fs::remove_file(path);
}

#[test]
fn d10_miniflux_removed_unreferenced_category_is_pruned() {
    let (fx, world) = mf_fixture();
    let client = MinifluxClient::new(&fx.base, "key");
    let (db, path) = database();
    client.sync(&db, 10).unwrap();
    assert!(
        db.categories()
            .unwrap()
            .iter()
            .any(|c| c.title == "Empty Corner")
    );
    world
        .categories
        .lock()
        .unwrap()
        .retain(|(_, t)| *t != "Empty Corner");
    client.sync(&db, 10).unwrap();
    assert!(
        !db.categories()
            .unwrap()
            .iter()
            .any(|c| c.title == "Empty Corner")
    );
    // A category still carrying feeds is never pruned.
    assert!(db.categories().unwrap().iter().any(|c| c.title == "News"));
    drop(db);
    let _ = std::fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// D23 — category mark-all uses provider membership semantics
// ---------------------------------------------------------------------------

#[test]
fn d23_greader_category_mark_uses_label_stream() {
    let fx = fixture(|raw| {
        if path_of(raw).ends_with("/token") {
            return (200, "edit-token".into());
        }
        (200, "OK".into())
    });
    let mut client = GReaderClient::with_oauth(GReaderService::Inoreader, "access-token");
    client.api_base = format!("{}/reader/api/0", fx.base);
    client.mark_category_read_remote("News").unwrap();
    let requests = fx.requests.lock().unwrap();
    let post = requests
        .iter()
        .find(|r| request_line(r).starts_with("POST"))
        .expect("mark-all-as-read request");
    assert!(request_line(post).contains("/mark-all-as-read"), "{post}");
    assert!(
        body_of(post).contains("s=user%2F-%2Flabel%2FNews"),
        "{}",
        body_of(post)
    );
}

#[test]
fn d23_miniflux_category_mark_uses_category_endpoint_and_cache_membership() {
    let (fx, _world) = mf_fixture();
    let base =
        std::env::temp_dir().join(format!("blindrss_task9_prov_{}.db", uuid::Uuid::new_v4()));
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    cfg.set_path("providers.miniflux.url", serde_json::json!(fx.base));
    cfg.set_path("providers.miniflux.api_key", serde_json::json!("key"));
    let db_path = provider::active_db_path(&cfg, &base);
    let db = Db::open_or_create(&db_path).unwrap();
    let p = provider::get_provider(&cfg, base.clone());
    // Feed 1 lives in News; feed 2 lives in Other, but one of
    // its articles is labeled into News (moved-in): provider
    // category membership covers it, a per-feed fan-out of
    // News feeds would miss it.
    db.upsert_hosted_feed(&blindrss_core::Feed {
        id: "miniflux:1".into(),
        url: "https://f1.example/rss".into(),
        title: "In News".into(),
        title_is_custom: false,
        category: Some("News".into()),
        icon_url: None,
    })
    .unwrap();
    db.upsert_hosted_feed(&blindrss_core::Feed {
        id: "miniflux:2".into(),
        url: "https://f2.example/rss".into(),
        title: "In Other".into(),
        title_is_custom: false,
        category: Some("Other".into()),
        icon_url: None,
    })
    .unwrap();
    let mk = |id: &str, feed: &str| Article {
        id: id.into(),
        feed_id: feed.into(),
        title: "Post".into(),
        url: "https://x.example/".into(),
        content: None,
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    };
    db.upsert_hosted_article(&mk("miniflux-entry:101", "miniflux:1"))
        .unwrap();
    db.upsert_hosted_article(&mk("miniflux-entry:201", "miniflux:2"))
        .unwrap();
    db.add_article_label("miniflux-entry:201", "News").unwrap();
    p.mark_category_read("News").unwrap();
    // The server got the category endpoint, not per-feed marks.
    let requests = fx.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| request_line(r).starts_with("PUT /v1/categories/10/mark-all-as-read")),
        "{requests:?}"
    );
    assert!(
        !requests.iter().any(|r| request_line(r).contains("/feeds/")),
        "{requests:?}"
    );
    drop(requests);
    assert!(
        db.article_by_id("miniflux-entry:101")
            .unwrap()
            .unwrap()
            .is_read
    );
    assert!(
        db.article_by_id("miniflux-entry:201")
            .unwrap()
            .unwrap()
            .is_read
    );
    drop(p);
    drop(db);
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_file(base);
}

// ---------------------------------------------------------------------------
// Account cache isolation across a token rotation (D07 support)
// ---------------------------------------------------------------------------

#[test]
fn inoreader_cache_path_is_stable_across_access_token_rotation() {
    let base =
        std::env::temp_dir().join(format!("blindrss_task9_path_{}.db", uuid::Uuid::new_v4()));
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!("inoreader"));
    cfg.set_path("providers.inoreader.token", serde_json::json!("access-1"));
    cfg.set_path(
        "providers.inoreader.refresh_token",
        serde_json::json!("refresh-1"),
    );
    cfg.set_path("providers.inoreader.app_id", serde_json::json!("app"));
    let before = provider::active_db_path(&cfg, &base);
    // A refresh rotates ONLY the access token: the account —
    // and its cache file — must be the same.
    cfg.set_path("providers.inoreader.token", serde_json::json!("access-2"));
    assert_eq!(provider::active_db_path(&cfg, &base), before);
    // A different account (different refresh token) gets a
    // different cache file.
    cfg.set_path(
        "providers.inoreader.refresh_token",
        serde_json::json!("refresh-2"),
    );
    assert_ne!(provider::active_db_path(&cfg, &base), before);
}
