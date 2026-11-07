//! Audit 8 Task 8 regressions (D16, D22, F01): unified worker-side
//! refresh and source dispatch.
//!
//! D16: an unchanged feed body (200 with only volatile counters
//! moved, or a 304 against a body whose store already committed)
//! must not be parsed/stored again; a FAILED store must be retried
//! even when the publisher then answers 304; a forced refresh
//! bypasses the fast path.
//!
//! D22: fetch + store + media attachment all happen inside the
//! worker-side refresh call, through the worker's own DB
//! connection; the committed outcome is the only thing returned.
//!
//! F01: subscription resolution and refresh dispatch share the
//! listing-source paths (Rumble/Odysee/YouTube search/SoundCloud/
//! Mixcloud); forum sources keep their independent dispatch.

use blindrss_core::refresh::{REFRESH_CANCELLED, WorkerRefresh, refresh_feed_with_options};
use blindrss_core::{Db, Feed};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn scratch_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "blindrss-audit8-task8-{tag}-{}.db",
        uuid::Uuid::new_v4()
    ))
}

fn http_response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Loopback server answering `budget` requests through `handler`
/// (request text, 1-based request index) and recording every
/// request line + headers it saw.
fn start_server<F>(budget: usize, handler: F) -> (String, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str, usize) -> String + Send + Sync + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for i in 0..budget {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            seen.lock().unwrap().push(text.clone());
            let response = handler(&text, i + 1);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{addr}"), requests)
}

fn two_item_feed(views: u32, rating: u32) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:media=\"http://search.yahoo.com/mrss/\">\n\
         <channel><title>Counter Feed</title>\n\
         <item><title>Episode One</title><link>https://example.com/e1</link><guid>e1</guid>\n\
         <description>First</description><pubDate>Fri, 30 Oct 2026 10:00:00 GMT</pubDate>\n\
         <media:statistics views=\"{views}\"/><media:starRating count=\"{rating}\" average=\"4.5\"/></item>\n\
         <item><title>Episode Two</title><link>https://example.com/e2</link><guid>e2</guid>\n\
         <description>Second</description><pubDate>Sat, 31 Oct 2026 10:00:00 GMT</pubDate></item>\n\
         </channel></rss>"
    )
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---------------------------------------------------------------------------
// D16 — unchanged-body fast path
// ---------------------------------------------------------------------------

#[test]
fn body_digest_ignores_volatile_counters_only() {
    let a = blindrss_core::feeds::feed_body_digest(two_item_feed(100, 5).as_bytes());
    let b = blindrss_core::feeds::feed_body_digest(two_item_feed(9999, 42).as_bytes());
    assert_eq!(a, b, "counter-only changes must not change the digest");
    let changed = two_item_feed(100, 5).replace("Episode One", "Episode Uno");
    let c = blindrss_core::feeds::feed_body_digest(changed.as_bytes());
    assert_ne!(a, c, "a real content change must change the digest");
    assert_eq!(a.len(), 40, "SHA-1 hex digest, like Python's hexdigest");
}

#[test]
fn unchanged_200_skips_store_and_persists_new_validators() {
    let bodies = [
        two_item_feed(100, 5),
        two_item_feed(250, 9), // counters only: same digest
        two_item_feed(250, 9),
    ];
    let etags = ["\"a\"", "\"b\"", "\"b\""];
    let (base, requests) = start_server(3, move |request, n| {
        if request.to_lowercase().contains("if-none-match: \"b\"") {
            return http_response("304 Not Modified", "", "");
        }
        http_response(
            "200 OK",
            &format!("ETag: {}\r\n", etags[n - 1]),
            &bodies[n - 1],
        )
    });
    let path = scratch_path("unchanged");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed(&format!("{base}/feed"), "Feed", None).unwrap();

    let first =
        refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    assert!(!first.not_modified);
    assert_eq!(first.outcome.notify_count, 2);

    // Identical body apart from volatile counters: no parse/store.
    let second =
        refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    assert!(
        second.not_modified,
        "unchanged body must take the fast path"
    );
    assert_eq!(second.outcome.notify_count, 0);
    assert_eq!(db.articles(Some(&feed.id), 10).unwrap().len(), 2);

    // The unchanged 200 still persisted its new ETag: the next
    // refresh revalidates with "b", gets a 304, and the recorded
    // digest proves the store already committed — still no work.
    let third =
        refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    assert!(third.not_modified);
    let seen = requests.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(
        seen[2].to_lowercase().contains("if-none-match: \"b\""),
        "third request must carry the persisted validator, got: {}",
        seen[2]
    );

    // Forced refresh bypasses the fast path (it re-stores; the
    // entries already exist, so nothing is newly notified).
    let (base2, _req2) = start_server(2, move |_request, _n| {
        http_response("200 OK", "", &two_item_feed(250, 9))
    });
    let feed2 = db
        .add_feed(&format!("{base2}/feed"), "Feed 2", None)
        .unwrap();
    let _ = refresh_feed_with_options(&db, &feed2, "deleted", None, false, &no_cancel()).unwrap();
    let forced =
        refresh_feed_with_options(&db, &feed2, "deleted", None, true, &no_cancel()).unwrap();
    assert!(
        !forced.not_modified,
        "force must bypass the unchanged-body fast path"
    );
    drop(db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn failed_store_is_retried_when_publisher_then_answers_304() {
    let body = two_item_feed(7, 1);
    let (base, requests) = start_server(2, move |request, n| {
        if n == 2 {
            assert!(
                request.to_lowercase().contains("if-none-match: \"v1\""),
                "second request must revalidate, got: {request}"
            );
            return http_response("304 Not Modified", "", "");
        }
        http_response("200 OK", "ETag: \"v1\"\r\n", &body)
    });
    let path = scratch_path("retrystore");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed(&format!("{base}/feed"), "Feed", None).unwrap();

    // Make every article insert fail: the store must error and the
    // body digest must NOT be recorded as stored.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER audit8_fail_insert BEFORE INSERT ON articles \
             BEGIN SELECT RAISE(ABORT, 'audit8 store failure'); END;",
        )
        .unwrap();
    }
    let failed = refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel());
    assert!(failed.is_err(), "a failing store must surface as an error");
    assert_eq!(db.articles(Some(&feed.id), 10).unwrap().len(), 0);

    // Store healthy again; the publisher now answers 304. The
    // validated cached body must be parsed and stored — a 304 is
    // only a skip when the store of that body already committed.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER audit8_fail_insert;")
            .unwrap();
    }
    let retried =
        refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    assert!(!retried.not_modified);
    assert_eq!(retried.outcome.notify_count, 2);
    assert_eq!(db.articles(Some(&feed.id), 10).unwrap().len(), 2);
    assert_eq!(requests.lock().unwrap().len(), 2);
    drop(db);
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------------
// D22 — store and media attachment live on the worker
// ---------------------------------------------------------------------------

#[test]
fn cancelled_refresh_fetches_nothing_and_stores_nothing() {
    // Cancel already set: no fetch may happen (the URL points at a
    // closed port, so any fetch attempt errors differently).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let path = scratch_path("cancelpre");
    let db = Db::open_or_create(&path).unwrap();
    let feed = Feed {
        id: "cancelpre".to_string(),
        url: format!("http://{addr}/feed"),
        title: "Feed".to_string(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    };
    let cancel = AtomicBool::new(true);
    let err = refresh_feed_with_options(&db, &feed, "deleted", None, false, &cancel).unwrap_err();
    assert_eq!(err, REFRESH_CANCELLED);
    drop(db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn cancel_landing_during_fetch_prevents_the_store() {
    let cancel = Arc::new(AtomicBool::new(false));
    let flip = cancel.clone();
    let (base, _requests) = start_server(1, move |_request, _n| {
        flip.store(true, Ordering::Relaxed);
        http_response("200 OK", "", &two_item_feed(3, 1))
    });
    let path = scratch_path("cancelmid");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed(&format!("{base}/feed"), "Feed", None).unwrap();
    let err = refresh_feed_with_options(&db, &feed, "deleted", None, false, &cancel).unwrap_err();
    assert_eq!(err, REFRESH_CANCELLED);
    assert_eq!(db.articles(Some(&feed.id), 10).unwrap().len(), 0);
    drop(db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn worker_refresh_commits_through_its_own_connection() {
    let (base, _requests) = start_server(1, move |_request, _n| {
        http_response("200 OK", "", &two_item_feed(1, 1))
    });
    let path = scratch_path("ownconn");
    let worker_db = Db::open_or_create(&path).unwrap();
    let feed = worker_db
        .add_feed(&format!("{base}/feed"), "Feed", None)
        .unwrap();
    let done: WorkerRefresh =
        refresh_feed_with_options(&worker_db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    assert_eq!(done.outcome.notify_count, 2);
    assert_eq!(done.outcome.new_articles.len(), 2);
    // A second connection (the UI's) sees only committed rows:
    // the worker's store is complete before the result returns.
    let ui_db = Db::open(&path).unwrap();
    assert_eq!(ui_db.unread_count(Some(&feed.id)).unwrap(), 2);
    assert_eq!(ui_db.articles(Some(&feed.id), 10).unwrap().len(), 2);
    drop(worker_db);
    drop(ui_db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn slow_npr_attach_runs_inside_the_worker_refresh() {
    // The story page answers slowly (1.2 s) with an ondemand MP3
    // marker. The attach is network work: it must complete inside
    // the worker refresh call, never in a UI drain. The item link
    // must point back at this server, so the base URL is shared
    // into the handler through a mutex.
    let feed_url = Arc::new(Mutex::new(String::new()));
    let feed_url2 = feed_url.clone();
    let (base, _requests) = start_server(2, move |request, _n| {
        if request.contains("/npr.org/") {
            std::thread::sleep(std::time::Duration::from_millis(1200));
            return http_response(
                "200 OK",
                "Content-Type: text/html\r\n",
                "<html><body><p>Listen: \
                 https://ondemand.npr.org/anon.npr-mp3/npr/atc/2026/11/episode.mp3\
                 </p></body></html>",
            );
        }
        let link = feed_url2.lock().unwrap().clone();
        http_response(
            "200 OK",
            "",
            &format!(
                "<rss version=\"2.0\"><channel><title>NPR Program</title>\n\
                 <item><title>Slow Story</title><link>{link}/npr.org/story-page</link>\
                 <guid>slow-1</guid><description>Story</description></item></channel></rss>"
            ),
        )
    });
    *feed_url.lock().unwrap() = base.clone();
    let path = scratch_path("nprslow");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed(&format!("{base}/feed.xml"), "NPR", None)
        .unwrap();
    let started = std::time::Instant::now();
    let done = refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(done.outcome.notify_count, 1);
    assert!(
        elapsed >= std::time::Duration::from_millis(1000),
        "the slow attach must happen inside the worker refresh, took {elapsed:?}"
    );
    let articles = db.articles(Some(&feed.id), 10).unwrap();
    assert_eq!(articles.len(), 1);
    assert_eq!(
        articles[0].media_url.as_deref(),
        Some("https://ondemand.npr.org/anon.npr-mp3/npr/atc/2026/11/episode.mp3")
    );
    assert_eq!(articles[0].media_type.as_deref(), Some("audio/mpeg"));
    drop(db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn npr_audio_is_resolved_per_session_for_existing_articles() {
    // Python resolves NPR audio for every entry at every
    // refresh (providers/local.py phase 1) and never fetches
    // NPR feeds conditionally; the port resolved only at first
    // insert and let NPR feeds take the unchanged-body fast
    // path, so a story whose audio posted late NEVER gained it
    // (Brandon, 2026-11-07). The story slug carries no parseable
    // story id, so resolution deterministically uses the story
    // page fallback against this loopback server.
    let audio_posted = Arc::new(Mutex::new(false));
    let posted = audio_posted.clone();
    let feed_url = Arc::new(Mutex::new(String::new()));
    let feed_url2 = feed_url.clone();
    let (base, requests) = start_server(6, move |request, _n| {
        if request.contains("story-page") {
            let line = if *posted.lock().unwrap() {
                "Listen: https://ondemand.npr.org/anon.npr-mp3/npr/me/2026/11/episode.mp3"
            } else {
                "Audio posts later today."
            };
            return http_response(
                "200 OK",
                "Content-Type: text/html\r\n",
                &format!("<html><body><p>{line}</p></body></html>"),
            );
        }
        let link = feed_url2.lock().unwrap().clone();
        http_response(
            "200 OK",
            "ETag: \"np1\"\r\n",
            &format!(
                "<rss version=\"2.0\"><channel><title>NPR Program</title>\n\
                 <item><title>Story</title><link>{link}/npr.org/story-page</link>\
                 <guid>npr-1</guid><description>Story</description></item></channel></rss>"
            ),
        )
    });
    *feed_url.lock().unwrap() = base.clone();
    let path = scratch_path("nprsession");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed(&format!("{base}/feeds.npr.org/feed.xml"), "NPR", None)
        .unwrap();
    // Session 1: the story is up, its audio is not.
    refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    let articles = db.articles(Some(&feed.id), 10).unwrap();
    assert_eq!(articles.len(), 1);
    assert_eq!(articles[0].media_url, None);
    // Session 2: the audio has posted; the feed body is byte-
    // identical and the server holds an ETag from session 1.
    *audio_posted.lock().unwrap() = true;
    refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    let articles = db.articles(Some(&feed.id), 10).unwrap();
    assert_eq!(
        articles[0].media_url.as_deref(),
        Some("https://ondemand.npr.org/anon.npr-mp3/npr/me/2026/11/episode.mp3")
    );
    assert_eq!(articles[0].media_type.as_deref(), Some("audio/mpeg"));
    // The second feed fetch must not have gone conditional.
    let seen = requests.lock().unwrap();
    let feed_requests: Vec<&String> = seen
        .iter()
        .filter(|r| r.contains("feeds.npr.org"))
        .collect();
    assert_eq!(feed_requests.len(), 2);
    assert!(
        !feed_requests[1].to_lowercase().contains("if-none-match"),
        "NPR feeds are fetched unconditionally, got: {}",
        feed_requests[1]
    );
    drop(seen);
    // Session 3: the resolver no longer confirms the audio —
    // per-session truth replaces the stored value (Python).
    *audio_posted.lock().unwrap() = false;
    refresh_feed_with_options(&db, &feed, "deleted", None, false, &no_cancel()).unwrap();
    let articles = db.articles(Some(&feed.id), 10).unwrap();
    assert_eq!(articles[0].media_url, None);
    drop(db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn db_files_run_in_wal_mode_with_python_connection_setup() {
    // Python core/db.py sets journal_mode=WAL,
    // synchronous=NORMAL and busy_timeout=60000 on every
    // connection; the port set none of them, so refresh workers
    // writing in rollback-journal mode blocked the UI's reads
    // and feeds failed with "database is locked" (Brandon,
    // 2026-11-07). WAL persists in the database file, so a
    // fresh raw connection still reports it after the fact.
    let path = scratch_path("wal");
    let db = Db::open_or_create(&path).unwrap();
    drop(db);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    drop(conn);
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(format!("{}-wal", path.display())).ok();
    std::fs::remove_file(format!("{}-shm", path.display())).ok();
}

// ---------------------------------------------------------------------------
// F01 — shared subscribe/refresh source dispatch
// ---------------------------------------------------------------------------

#[test]
fn source_listing_urls_normalize_like_python() {
    use blindrss_core::discovery::source_listing_url;
    assert_eq!(
        source_listing_url("https://rumble.com/c/example?tracking=x").as_deref(),
        Some("https://rumble.com/c/example/videos")
    );
    assert_eq!(
        source_listing_url("https://odysee.com/@chan:1?x=1").as_deref(),
        Some("https://odysee.com/@chan:1")
    );
    assert_eq!(
        source_listing_url("https://www.youtube.com/results?search_query=lofi").as_deref(),
        Some("https://www.youtube.com/results?search_query=lofi")
    );
    assert_eq!(
        source_listing_url("https://soundcloud.com/someartist").as_deref(),
        Some("https://soundcloud.com/someartist")
    );
    assert_eq!(
        source_listing_url("https://example.com/feed.xml").as_deref(),
        None
    );
    // Forum sources keep their independent working dispatch.
    assert_eq!(
        source_listing_url("https://groups.google.com/g/some-group").as_deref(),
        None
    );
}

#[test]
fn youtube_search_subscribe_resolves_without_a_feed_document() {
    // Python's add_feed titles a YouTube search subscription from
    // the query and fetches nothing; refresh enumerates it later.
    let (feed_url, parsed) = blindrss_core::discovery::resolve_subscribe_url(
        "https://www.youtube.com/results?search_query=lofi+beats",
    )
    .unwrap();
    assert_eq!(
        feed_url,
        "https://www.youtube.com/results?search_query=lofi+beats"
    );
    assert_eq!(parsed.title, "YouTube: lofi beats");
    assert!(parsed.entries.is_empty());
}

#[test]
fn subscribe_url_stores_a_plain_feed_on_the_worker() {
    let (base, _requests) = start_server(1, move |_request, _n| {
        http_response("200 OK", "", &two_item_feed(1, 1))
    });
    let path = scratch_path("subscribe");
    let db = Db::open_or_create(&path).unwrap();
    let feed =
        blindrss_core::refresh::subscribe_url(&db, &format!("{base}/feed"), None, "deleted", None)
            .unwrap();
    assert_eq!(feed.title, "Counter Feed");
    assert_eq!(db.articles(Some(&feed.id), 10).unwrap().len(), 2);
    assert_eq!(db.unread_count(Some(&feed.id)).unwrap(), 2);
    drop(db);
    std::fs::remove_file(&path).ok();
}

#[test]
fn subscribe_youtube_search_creates_the_row_for_refresh_dispatch() {
    // Like Python's add flow: the row (resolved URL + query title)
    // is the subscription; a failed first enumeration must not
    // unsub it — refresh owns article storage from here.
    let path = scratch_path("subytsearch");
    let db = Db::open_or_create(&path).unwrap();
    let feed = blindrss_core::refresh::subscribe_url(
        &db,
        "https://www.youtube.com/results?search_query=lofi+beats",
        None,
        "deleted",
        None,
    )
    .unwrap();
    assert_eq!(feed.title, "YouTube: lofi beats");
    assert_eq!(
        feed.url,
        "https://www.youtube.com/results?search_query=lofi+beats"
    );
    let rows = db.feeds().unwrap();
    assert!(rows.iter().any(|f| f.id == feed.id));
    drop(db);
    std::fs::remove_file(&path).ok();
}
