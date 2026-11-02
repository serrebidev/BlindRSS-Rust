use blindrss_core::{config::Config, providers::MinifluxClient};
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn server(status: u16, delay: Duration) -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    server_with_feeds(status, delay, 8)
}

fn server_with_feeds(
    status: u16,
    delay: Duration,
    count: i64,
) -> (String, Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let (requests, maximum) = (log.clone(), peak.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let (log, active, peak, completed) = (
                requests.clone(),
                active.clone(),
                maximum.clone(),
                completed.clone(),
            );
            std::thread::spawn(move || {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                }
                let head = String::from_utf8_lossy(&bytes);
                let request = head.lines().next().unwrap().to_string();
                log.lock().unwrap().push(request.clone());
                let targeted = request.starts_with("PUT ");
                let (code, body) = if targeted {
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(delay);
                    active.fetch_sub(1, Ordering::SeqCst);
                    if status < 300 {
                        let id: i64 = request.split('/').nth(3).unwrap().parse().unwrap();
                        completed.lock().unwrap().insert(id);
                    }
                    (status, String::new())
                } else if request.contains("/v1/entries?") {
                    let path = request.split_whitespace().nth(1).unwrap();
                    let url = url::Url::parse(&format!("http://localhost{path}")).unwrap();
                    let id: i64 = url
                        .query_pairs()
                        .find(|(key, _)| key == "feed_id")
                        .unwrap()
                        .1
                        .parse()
                        .unwrap();
                    let entries = if completed.lock().unwrap().contains(&id) {
                        vec![serde_json::json!({"id":id,"title":"Late article","status":"unread"})]
                    } else {
                        vec![]
                    };
                    (
                        200,
                        serde_json::json!({"total":entries.len(),"entries":entries}).to_string(),
                    )
                } else {
                    (200, serde_json::json!((1..=count).map(|id| serde_json::json!({"id":id,"feed_url":format!("https://example.com/{id}"),"checked_at":if id <= 3 {"2050-01-01T00:00:00Z"} else {"2000-01-01T00:00:00Z"},"parsing_error_count":if id == 8 {3} else {0}})).collect::<Vec<_>>()).to_string())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (base, log, peak)
}

fn config() -> Config {
    let mut cfg = Config::default();
    cfg.set_path("miniflux_targeted_refresh_workers", serde_json::json!(2));
    cfg.set_path("miniflux_refresh_soft_deadline_s", serde_json::json!(0));
    cfg
}

#[test]
fn manual_refresh_uses_bounded_targeted_requests_and_skips_chronic_feeds() {
    let (base, requests, peak) = server(204, Duration::from_millis(25));
    let client = MinifluxClient::new(&base, "secret").with_refresh_config(&config());
    assert!(client.request_refresh(true).unwrap().is_none());
    let log = requests.lock().unwrap();
    assert_eq!(log.iter().filter(|s| s.starts_with("PUT ")).count(), 7);
    assert!(
        !log.iter()
            .any(|s| s.contains("/feeds/refresh") || s.contains("/feeds/8/refresh"))
    );
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}

#[test]
fn repeated_route_failures_stop_batch_and_survive_client_recreation() {
    let (base, requests, _) = server(503, Duration::from_millis(5));
    let mut cfg = config();
    cfg.set_path("miniflux_targeted_refresh_workers", serde_json::json!(32));
    assert!(
        MinifluxClient::new(&base, "secret")
            .with_refresh_config(&cfg)
            .request_refresh(true)
            .is_err()
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("PUT "))
            .count(),
        3
    );
    assert!(
        MinifluxClient::new(&base, "secret")
            .with_refresh_config(&cfg)
            .request_refresh(true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("PUT "))
            .count(),
        3
    );
}

#[test]
fn force_retries_one_failure_but_respects_repeated_feed_backoff() {
    let (base, requests, _) = server(403, Duration::ZERO);
    let client = MinifluxClient::new(&base, "secret").with_refresh_config(&config());
    assert!(client.request_feed_refreshes(&[1], true).is_err());
    assert!(client.request_feed_refreshes(&[1], false).is_ok());
    assert!(client.request_feed_refreshes(&[1], true).is_err());
    assert!(client.request_feed_refreshes(&[1], true).is_ok());
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[test]
fn soft_deadline_returns_without_cancelling_slow_requests() {
    let (base, requests, _) = server(204, Duration::from_millis(150));
    let mut cfg = config();
    cfg.set_path("miniflux_refresh_soft_deadline_s", serde_json::json!(0.02));
    let client = MinifluxClient::new(&base, "secret").with_refresh_config(&cfg);
    let started = Instant::now();
    let pending = client
        .request_refresh(true)
        .unwrap()
        .expect("background stragglers");
    assert!(started.elapsed() < Duration::from_millis(250));
    pending
        .recv_timeout(Duration::from_secs(4))
        .unwrap()
        .unwrap();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("PUT "))
            .count(),
        7
    );
}

#[test]
fn scheduled_refresh_only_recovers_overdue_feeds_with_fifteen_feed_budget() {
    let (base, requests, _) = server_with_feeds(204, Duration::ZERO, 40);
    let client = MinifluxClient::new(&base, "secret").with_refresh_config(&config());
    assert!(client.request_refresh(false).unwrap().is_none());
    let log = requests.lock().unwrap();
    assert_eq!(log.iter().filter(|s| s.starts_with("PUT ")).count(), 15);
    assert!(!log.iter().any(|s| s.contains("/feeds/1/refresh")
        || s.contains("/feeds/8/refresh")
        || s.contains("/feeds/refresh")));
}

#[test]
fn worker_configuration_has_a_hard_cap_of_thirty_two() {
    // 500 ms (was 50): the peak is only observable while all
    // 32 workers are inside the delay together; at 50 ms the
    // entry ramp on a loaded machine can miss the overlap
    // and the observed peak lands at 27-31 (flake seen on
    // this VM and on GitHub's Windows runner, 2026-11-02).
    let (base, _, peak) = server_with_feeds(204, Duration::from_millis(500), 40);
    let mut cfg = config();
    cfg.set_path("miniflux_targeted_refresh_workers", serde_json::json!(128));
    MinifluxClient::new(&base, "secret")
        .with_refresh_config(&cfg)
        .request_refresh(true)
        .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 32);
}

#[test]
fn disabling_chronic_skip_and_breaker_reaches_all_feeds() {
    let (base, requests, _) = server(503, Duration::ZERO);
    let mut cfg = config();
    cfg.set_path("miniflux_force_skip_error_count", serde_json::json!(0));
    cfg.set_path("miniflux_targeted_refresh_5xx_limit", serde_json::json!(0));
    let error = MinifluxClient::new(&base, "secret")
        .with_refresh_config(&cfg)
        .request_refresh(true)
        .unwrap_err();
    assert!(error.contains("HTTP 503"));
    assert!(!error.contains("secret"));
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("PUT "))
            .count(),
        8
    );
}

#[test]
fn per_feed_timeout_uses_finite_base_floor_without_retrying() {
    let (base, requests, _) = server(204, Duration::from_secs(6));
    let mut cfg = config();
    cfg.set_path("feed_timeout_seconds", serde_json::json!(1));
    cfg.set_path("miniflux_per_feed_refresh_timeout_s", serde_json::json!(1));
    let started = Instant::now();
    let error = MinifluxClient::new(&base, "secret")
        .with_refresh_config(&cfg)
        .request_feed_refreshes(&[1], true)
        .unwrap_err();
    assert!(error.contains("timed out"));
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < Duration::from_secs(6));
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn local_database_override_is_preserved_exactly() {
    let custom = std::path::Path::new("C:/arbitrary/custom.library.sqlite");
    assert_eq!(
        blindrss_core::provider::active_db_path(&Config::default(), custom),
        custom
    );
}

#[test]
fn stragglers_sync_and_report_into_the_original_account_cache() {
    use blindrss_core::{
        Db,
        provider::{active_db_path, get_provider},
    };
    // 2 s PUT delay (was 300 ms): the mock serves a feed's
    // entry only after that feed's PUT completes, so the
    // first (synchronous) sync must finish before the first
    // PUT lands. At 300 ms a slow machine lets later entry
    // fetches cross the line and the "still empty" assert
    // fails (flake seen on this VM and on GitHub's Windows
    // runner, 2026-11-02). All 8 PUTs run in one wave
    // (workers = 8), so the background completion still
    // lands well inside the 5 s callback budget.
    let (base, _, _) = server(204, Duration::from_secs(2));
    let path = std::env::temp_dir().join(format!("blindrss-late-{}.db", uuid::Uuid::new_v4()));
    let mut cfg = config();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    cfg.set_path("providers.miniflux.url", serde_json::json!(base));
    cfg.set_path(
        "providers.miniflux.api_key",
        serde_json::json!("original-account"),
    );
    cfg.set_path("miniflux_refresh_soft_deadline_s", serde_json::json!(0.02));
    let original = active_db_path(&cfg, &path);
    let provider = get_provider(&cfg, path.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    provider
        .refresh_with_updates(
            true,
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    assert!(
        Db::open(&original)
            .unwrap()
            .articles(None, 20)
            .unwrap()
            .is_empty()
    );
    cfg.set_path(
        "providers.miniflux.api_key",
        serde_json::json!("other-account"),
    );
    let other_path = active_db_path(&cfg, &path);
    let other = get_provider(&cfg, path);
    assert!(other.feeds().unwrap().is_empty());
    assert!(
        // 20 s (was 5): with the 2 s PUT delay the pool's
        // probing waves (3-wide until first success) plus the
        // background re-sync legitimately take ~4-6 s; the
        // property under test is WHERE the stragglers land,
        // not the wall-clock budget.
        rx.recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap()
            .contains("7 new article")
    );
    assert_eq!(
        Db::open(&original)
            .unwrap()
            .articles(None, 20)
            .unwrap()
            .len(),
        7
    );
    assert!(other.articles(None, 20).unwrap().is_empty());
    drop((provider, other));
    std::fs::remove_file(original).unwrap();
    std::fs::remove_file(other_path).unwrap();
}

#[test]
fn failed_feed_refresh_preserves_cached_articles_and_reports_error() {
    use blindrss_core::{Article, Db, Feed};
    let (base, _, _) = server(503, Duration::ZERO);
    let path = std::env::temp_dir().join(format!(
        "blindrss-refresh-failure-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = Db::open_or_create(&path).unwrap();
    db.insert_feed_if_missing(&Feed {
        id: "miniflux:1".into(),
        url: "https://example.com/feed".into(),
        title: "Feed".into(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    })
    .unwrap();
    db.insert_article_if_new(&Article {
        id: "miniflux-entry:1".into(),
        feed_id: "miniflux:1".into(),
        title: "Keep".into(),
        url: "https://example.com/item".into(),
        content: Some("Keep body".into()),
        description: None,
        date: None,
        author: None,
        is_read: true,
        is_favorite: true,
        media_url: None,
        media_type: None,
    })
    .unwrap();
    assert!(
        MinifluxClient::new(&base, "secret")
            .refresh_feed(&db, 1)
            .unwrap_err()
            .contains("HTTP 503")
    );
    let article = db.article_by_id("miniflux-entry:1").unwrap().unwrap();
    assert_eq!(article.content.as_deref(), Some("Keep body"));
    assert!(article.is_read && article.is_favorite);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
