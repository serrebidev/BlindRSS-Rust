//! Audit 8 Task 27: provider-layer import routing (F03),
//! hosted archive merging in provider reads (F02), and the
//! GReader subscribe-with-category request shape.

use blindrss_core::Db;
use blindrss_core::archive_store;
use blindrss_core::config::Config;
use blindrss_core::db::{Article, Feed};
use blindrss_core::provider::{Provider, get_provider};
use blindrss_core::providers::{GReaderClient, GReaderService};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn temp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("blindrss-t27-{tag}-{}.db", uuid::Uuid::new_v4()))
}

/// Read one HTTP request (head + body) and return it raw.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let head_end = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4);
        if let Some(end) = head_end {
            let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= end + len {
                break;
            }
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
}

// ---------------------------------------------------------------------------
// F03, GReader family: subscribe carries the category in the
// form's `t` field, exactly as Python's add_feed sends it.
// ---------------------------------------------------------------------------

#[test]
fn greader_subscribe_sends_category_like_python() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let log = captured.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let request = read_request(&mut stream);
            let first = request.lines().next().unwrap_or("").to_string();
            log.lock().unwrap().push(request.clone());
            if first.contains("/token") {
                respond(&mut stream, 200, "tok123");
            } else {
                respond(&mut stream, 200, "OK");
            }
        }
    });
    let mut client =
        GReaderClient::with_oauth(GReaderService::TheOldReader, "token").with_cache_ttls(0, 0);
    client.api_base = format!("{base}/reader/api/0");
    client
        .subscribe_remote_in("https://example.com/rss.xml", Some("Tech"))
        .unwrap();
    client
        .subscribe_remote_in("https://example.com/plain.xml", None)
        .unwrap();
    let requests = captured.lock().unwrap();
    let edits: Vec<&String> = requests
        .iter()
        .filter(|r| {
            r.lines()
                .next()
                .is_some_and(|l| l.contains("subscription/edit"))
        })
        .collect();
    assert_eq!(edits.len(), 2);
    let form = |req: &str| -> Vec<(String, String)> {
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
        url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect()
    };
    let first = form(edits[0]);
    assert!(first.contains(&("s".into(), "feed/https://example.com/rss.xml".into())));
    assert!(first.contains(&("ac".into(), "subscribe".into())));
    assert!(first.contains(&("t".into(), "Tech".into())), "{first:?}");
    assert!(first.contains(&("T".into(), "tok123".into())));
    let second = form(edits[1]);
    assert!(second.contains(&("t".into(), "".into())), "{second:?}");
}

// ---------------------------------------------------------------------------
// F03, local provider: import_opml on the trait reaches the
// local importer (categories ensured, target applied).
// ---------------------------------------------------------------------------

#[test]
fn local_provider_import_opml_files_feeds() {
    let db_path = temp_path("local");
    // The app creates the database at startup before any
    // provider is built; the provider layer opens it in place.
    drop(Db::open_or_create(&db_path).unwrap());
    let cfg = Config::default();
    let provider = get_provider(&cfg, db_path.clone());
    assert_eq!(provider.name(), "local");
    let opml_path = db_path.with_extension("opml");
    std::fs::write(
        &opml_path,
        "<opml><body><outline text=\"Tech\"><outline text=\"Feed A\" xmlUrl=\"https://a.example/rss\"/></outline></body></opml>",
    )
    .unwrap();
    let summary = provider.import_opml(&opml_path, None).unwrap();
    assert!(summary.contains('1'), "{summary}");
    let db = Db::open(&db_path).unwrap();
    let feeds = db.feeds().unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].category.as_deref(), Some("Tech"));
    assert!(provider.categories().unwrap().contains(&"Tech".to_string()));
    drop(provider);
    drop(db);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&opml_path);
}

// ---------------------------------------------------------------------------
// F03, Miniflux: the provider import POSTs the folder-wrapped
// document to /v1/import, and the following sync retains the
// subscription with its category in the cache.
// ---------------------------------------------------------------------------

struct MinifluxFixture {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

fn miniflux_fixture(import_status: u16) -> MinifluxFixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    let imported = Arc::new(Mutex::new(false));
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let request = read_request(&mut stream);
            let first = request.lines().next().unwrap_or("").to_string();
            log.lock().unwrap().push(request.clone());
            let feeds_body = if *imported.lock().unwrap() {
                r#"[{"id":7,"title":"Imported Feed","feed_url":"https://imp.example/rss","site_url":"","category":{"id":1,"title":"Tech"}}]"#
            } else {
                "[]"
            };
            if first.starts_with("POST ") && first.contains("/v1/import") {
                if import_status == 200 {
                    *imported.lock().unwrap() = true;
                }
                respond(&mut stream, import_status, "{}");
            } else if first.starts_with("POST ") && first.contains("/v1/feeds") {
                *imported.lock().unwrap() = true;
                respond(&mut stream, 200, r#"{"feed_id":7}"#);
            } else if first.contains("/v1/categories") {
                respond(&mut stream, 200, r#"[{"id":1,"title":"Tech"}]"#);
            } else if first.contains("/v1/feeds") {
                respond(&mut stream, 200, feeds_body);
            } else if first.contains("/v1/entries") {
                respond(&mut stream, 200, r#"{"total":0,"entries":[]}"#);
            } else {
                respond(&mut stream, 200, "{}");
            }
        }
    });
    MinifluxFixture { base, requests }
}

fn miniflux_cfg(base: &str) -> Config {
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    cfg.set_path("providers.miniflux.url", serde_json::json!(base));
    cfg.set_path("providers.miniflux.api_key", serde_json::json!("key"));
    cfg
}

fn miniflux_provider(base: &str, db_path: std::path::PathBuf) -> Box<dyn Provider + Send> {
    get_provider(&miniflux_cfg(base), db_path)
}

const IMPORT_OPML: &str = "<opml version=\"2.0\"><head><title>x</title></head><body><outline text=\"Imported Feed\" xmlUrl=\"https://imp.example/rss\" /></body></opml>";

#[test]
fn miniflux_provider_import_posts_wrapped_document_then_syncs() {
    let fixture = miniflux_fixture(200);
    let db_path = temp_path("mfwrap");
    let provider = miniflux_provider(&fixture.base, db_path.clone());
    let opml_path = db_path.with_extension("opml");
    std::fs::write(&opml_path, IMPORT_OPML).unwrap();
    provider.import_opml(&opml_path, Some("Tech")).unwrap();
    let requests = fixture.requests.lock().unwrap();
    let import_req = requests
        .iter()
        .find(|r| r.lines().next().is_some_and(|l| l.contains("/v1/import")))
        .expect("bulk import request");
    assert!(
        import_req
            .to_lowercase()
            .contains("content-type: application/xml"),
        "{import_req}"
    );
    let body = import_req.split("\r\n\r\n").nth(1).unwrap_or("");
    // The target category wraps the body's children in one
    // folder outline (Python's Miniflux preparation).
    assert!(
        body.contains("<outline text=\"Tech\" title=\"Tech\">"),
        "{body}"
    );
    assert!(
        body.contains("xmlUrl=\"https://imp.example/rss\""),
        "{body}"
    );
    assert!(body.contains("</outline></body>"), "{body}");
    // The post-import sync retained the subscription and its
    // category in the cache.
    let feeds = provider.feeds().unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].title, "Imported Feed");
    assert_eq!(feeds[0].category.as_deref(), Some("Tech"));
    drop(provider);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&opml_path);
}

#[test]
fn miniflux_provider_import_falls_back_to_per_feed_on_404() {
    let fixture = miniflux_fixture(404);
    let db_path = temp_path("mf404");
    let provider = miniflux_provider(&fixture.base, db_path.clone());
    let opml_path = db_path.with_extension("opml");
    std::fs::write(&opml_path, IMPORT_OPML).unwrap();
    provider.import_opml(&opml_path, Some("Tech")).unwrap();
    let requests = fixture.requests.lock().unwrap();
    let create = requests
        .iter()
        .find(|r| {
            r.lines()
                .next()
                .is_some_and(|l| l.starts_with("POST ") && l.contains("/v1/feeds"))
        })
        .expect("per-feed create request");
    let body = create.split("\r\n\r\n").nth(1).unwrap_or("");
    assert!(body.contains("https://imp.example/rss"), "{body}");
    // Category "Tech" exists on the server with id 1.
    assert!(body.contains("\"category_id\":1"), "{body}");
    let feeds = provider.feeds().unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].category.as_deref(), Some("Tech"));
    drop(provider);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&opml_path);
}

// ---------------------------------------------------------------------------
// F02: recovered archive episodes merge into hosted provider
// reads for plain feed views, deduped against live articles,
// and stay out of the virtual views Python excludes.
// ---------------------------------------------------------------------------

#[test]
fn hosted_articles_merge_archive_episodes() {
    let db_path = temp_path("merge");
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    cfg.set_path(
        "providers.miniflux.url",
        serde_json::json!("https://example.invalid"),
    );
    cfg.set_path("providers.miniflux.api_key", serde_json::json!("key"));
    let scope = archive_store::scope_for_config(&cfg);
    // The provider reads its credential-scoped cache file.
    let db_path = blindrss_core::provider::active_db_path(&cfg, &db_path);

    let db = Db::open_or_create(&db_path).unwrap();
    db.upsert_hosted_feed(&Feed {
        id: "miniflux:7".into(),
        url: "https://pod.example/rss".into(),
        title: "Pod".into(),
        title_is_custom: false,
        category: Some("Tech".into()),
        icon_url: None,
    })
    .unwrap();
    // The live article shares its media URL with one archive
    // episode: the merge must not duplicate it.
    db.upsert_hosted_article(&Article {
        id: "miniflux-entry:1".into(),
        feed_id: "miniflux:7".into(),
        title: "Current Episode".into(),
        url: "https://pod.example/ep1".into(),
        content: Some("live".into()),
        description: Some("live".into()),
        date: Some("2026-01-03".into()),
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: Some("https://cdn.example/ep1.mp3".into()),
        media_type: Some("audio/mpeg".into()),
    })
    .unwrap();
    drop(db);

    let document = b"<rss version='2.0'><channel><title>Pod</title>\
<item><guid>g1</guid><title>Current Episode</title><link>https://pod.example/ep1</link>\
<pubDate>Sat, 03 Jan 2026 10:00:00 GMT</pubDate>\
<enclosure url='https://cdn.example/ep1.mp3' type='audio/mpeg' length='1'/></item>\
<item><guid>g0</guid><title>Recovered Episode</title><link>https://pod.example/ep0</link>\
<pubDate>Thu, 01 Jan 2026 10:00:00 GMT</pubDate>\
<enclosure url='https://cdn.example/ep0.mp3' type='audio/mpeg' length='1'/></item>\
</channel></rss>";
    let parsed =
        blindrss_core::archive::parse_document(document, "https://pod.example/rss", "").unwrap();
    archive_store::replace_entries(&db_path, &scope, "miniflux:7", &parsed.episodes).unwrap();

    let provider = get_provider(&cfg, db_path.clone());
    let merged = provider.articles(Some("miniflux:7"), 100).unwrap();
    assert_eq!(merged.len(), 2, "{merged:?}");
    let recovered = merged
        .iter()
        .find(|a| a.id.starts_with("podcast-archive:"))
        .expect("archive episode merged into the feed read");
    assert_eq!(recovered.title, "Recovered Episode");
    assert!(recovered.is_read);
    assert_eq!(
        recovered.media_url.as_deref(),
        Some("https://cdn.example/ep0.mp3")
    );
    // Virtual views Python excludes stay unmerged.
    let unread = provider.articles(Some("unread:miniflux:7"), 100).unwrap();
    assert!(
        !unread.iter().any(|a| a.id.starts_with("podcast-archive:")),
        "{unread:?}"
    );
    let all = provider.articles(None, 100).unwrap();
    assert!(
        !all.iter().any(|a| a.id.starts_with("podcast-archive:")),
        "{all:?}"
    );
    drop(provider);
    let _ = std::fs::remove_file(&db_path);
}
