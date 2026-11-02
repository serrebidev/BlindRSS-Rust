use blindrss_core::{archive, archive_store};
use std::sync::atomic::AtomicBool;

fn document(id: &str, media: &str, title: &str) -> Vec<u8> {
    format!("<rss version='2.0'><channel><title>Show</title><item><guid>{id}</guid><title>{title}</title><pubDate>Tue, 20 Oct 2026 10:00:00 GMT</pubDate><enclosure url='{media}' type='audio/mpeg' length='100'/></item></channel></rss>").into_bytes()
}

#[test]
fn rotating_media_tokens_and_changed_guids_merge_without_losing_current_text() {
    let current = archive::parse_document(
        &document(
            "new-id",
            "https://cdn.example/episode.mp3?token=new&amp;utm_source=feed",
            "Current title",
        ),
        "https://example.com/rss",
        "",
    )
    .unwrap();
    let old = archive::parse_document(
        &document(
            "old-id",
            "https://cdn.example/episode.mp3?token=old",
            "Old title",
        ),
        "https://example.com/rss",
        "20250101",
    )
    .unwrap();
    let episodes = archive::deduplicate_episodes(current.episodes.into_iter().chain(old.episodes));
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].entry.title, "Current title");
    assert_eq!(episodes[0].entry.id, "new-id");
    assert!(episodes[0].identity_keys.iter().any(|k| k == "guid:old-id"));
    assert_eq!(
        archive::canonical_url(
            "https://WEB.archive.org/web/20250101id_/https://Example.com/a//episode/?utm_source=x&b=2&a=1#part",
            false
        ),
        "https://example.com/a/episode?a=1&b=2"
    );
}

#[test]
fn archive_claims_are_atomic_due_and_account_scoped() {
    let path = std::env::temp_dir().join(format!("blindrss-archive-{}.db", uuid::Uuid::new_v4()));
    let db = blindrss_core::Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed("https://example.com/rss", "Show", None)
        .unwrap();
    assert!(
        archive_store::claim(&path, "", &feed.id, &feed.url, 86400.0, false, 100_000.0).unwrap()
    );
    assert!(
        !archive_store::claim(&path, "", &feed.id, &feed.url, 86400.0, false, 100_001.0).unwrap()
    );
    assert!(
        archive_store::claim(
            &path, "other", &feed.id, &feed.url, 86400.0, false, 100_001.0
        )
        .unwrap()
    );
    archive_store::finish(&path, "", &feed.id, 5, 20, "", 100_002.0).unwrap();
    assert!(
        !archive_store::claim(&path, "", &feed.id, &feed.url, 86400.0, false, 100_003.0).unwrap()
    );
    assert!(
        archive_store::claim(&path, "", &feed.id, &feed.url, 86400.0, true, 100_003.0).unwrap()
    );
    archive_store::finish(&path, "", &feed.id, 0, 0, "offline", 100_004.0).unwrap();
    assert_eq!(
        archive_store::state(&path, "", &feed.id).unwrap().status,
        "error"
    );
    archive_store::reset(&path, "", &feed.id).unwrap();
    assert_eq!(
        archive_store::state(&path, "", &feed.id).unwrap().status,
        "never"
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn scanner_reports_progress_and_obeys_cancellation_before_network() {
    let cancel = AtomicBool::new(true);
    let mut calls = 0;
    let mut stages = Vec::new();
    let result = archive::scan_with_fetch(
        &["https://example.com/rss".into()],
        archive::ScanOptions::default(),
        &cancel,
        |p| stages.push(p.stage.to_string()),
        |_| {
            calls += 1;
            Err("must not fetch".into())
        },
    )
    .unwrap();
    assert!(result.canceled);
    assert_eq!(calls, 0);
    assert_eq!(stages.last().map(String::as_str), Some("complete"));
}

#[test]
fn scanner_counts_failed_snapshots_and_returns_archive_only_history() {
    let current = document("new", "https://cdn.example/new.mp3", "New");
    let mut stages = Vec::new();
    let result = archive::scan_with_fetch(&["https://example.com/rss".into()], archive::ScanOptions::default(), &AtomicBool::new(false), |p| stages.push(p.stage), |url| {
        let bytes = if url.contains("/cdx/") { br#"[["timestamp","original"],["20260101","https://example.com/rss"],["20250101","https://example.com/rss"]]"#.to_vec() }
            else if url.contains("20260101id_") { return Err("offline snapshot".into()) }
            else if url.contains("20250101id_") { document("old", "https://cdn.example/old.mp3", "Old") }
            else { current.clone() };
        Ok((bytes, url.to_string()))
    }).unwrap();
    assert_eq!(result.snapshots_found, 2);
    assert_eq!(result.snapshots_loaded, 1);
    assert_eq!(result.failed_snapshots, 1);
    let old = archive::archive_only_episodes(&result, &current).unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].entry.id, "old");
    assert!(stages.contains(&"snapshot"));
}

#[test]
fn media_identity_keeps_episode_queries_and_bridges_old_aliases() {
    let parse = |id, media, title| {
        archive::parse_document(&document(id, media, title), "https://example.com/rss", "")
            .unwrap()
            .episodes
            .remove(0)
    };
    let first = parse(
        "first",
        "https://cdn.example/download?episode=1&amp;token=new",
        "One",
    );
    let second = parse(
        "second",
        "https://cdn.example/download?episode=2&amp;token=old",
        "Two",
    );
    assert_eq!(
        archive::deduplicate_episodes([first.clone(), second.clone()]).len(),
        2
    );
    let bridge = parse(
        "first",
        "https://cdn.example/download?episode=2&amp;token=changed",
        "Bridge",
    );
    let merged = archive::deduplicate_episodes([first, second, bridge]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].entry.title, "One");
    assert!(merged[0].identity_keys.contains("guid:second"));
}

#[test]
fn scanner_cancellation_from_progress_prevents_the_next_fetch() {
    let cancel = AtomicBool::new(false);
    let mut requested = Vec::new();
    let result = archive::scan_with_fetch(
        &["https://example.com/rss".into()],
        archive::ScanOptions::default(),
        &cancel,
        |progress| {
            if progress.stage == "index" {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
        |url| {
            requested.push(url.to_string());
            Ok((
                document("one", "https://cdn.example/one.mp3", "One"),
                url.to_string(),
            ))
        },
    )
    .unwrap();
    assert!(result.canceled);
    assert_eq!(requested, ["https://example.com/rss"]);
    assert_eq!(result.episodes.len(), 1);
}

#[test]
fn scanner_caps_snapshots_and_marks_truncation() {
    let mut snapshots = 0;
    let result = archive::scan_with_fetch(
        &["https://example.com/rss".into()],
        archive::ScanOptions { max_snapshots: 1, ..Default::default() }, &AtomicBool::new(false), |_| {},
        |url| {
            let bytes = if url.contains("/cdx/") {
                br#"[["timestamp","original"],["20260101","https://example.com/rss"],["20250101","https://example.com/rss"]]"#.to_vec()
            } else { if url.contains("id_/") { snapshots += 1; } document("one", "https://cdn.example/one.mp3", "One") };
            Ok((bytes, url.to_string()))
        },
    ).unwrap();
    assert!(result.truncated);
    assert_eq!(result.snapshots_found, 1);
    assert_eq!(result.snapshots_loaded, 1);
    assert_eq!(snapshots, 1);
}

#[test]
fn hosted_archive_entries_merge_only_into_the_matching_feed_and_scope() {
    let path = std::env::temp_dir().join(format!(
        "blindrss-archive-hosted-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = blindrss_core::Db::open_or_create(&path).unwrap();
    let parsed = archive::parse_document(
        &document("old", "https://cdn.example/old.mp3", "Old"),
        "https://example.com/rss",
        "20250101",
    )
    .unwrap();
    archive_store::replace_entries(&path, "account-a", "feed", &parsed.episodes).unwrap();
    let entries = archive_store::entries(&path, "account-a", "feed").unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_read);
    assert!(
        archive_store::entries(&path, "account-b", "feed")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        archive_store::merge_articles(&path, "account-a", "feed", entries.clone())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        archive_store::merge_articles(&path, "account-a", "read:feed", Vec::new())
            .unwrap()
            .len(),
        1
    );
    assert!(
        archive_store::merge_articles(&path, "account-a", "unread:feed", Vec::new())
            .unwrap()
            .is_empty()
    );
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn simultaneous_claims_admit_one_worker() {
    let path = std::env::temp_dir().join(format!(
        "blindrss-archive-claim-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = blindrss_core::Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed("https://example.com/rss", "Show", None)
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let (path, feed, barrier) = (path.clone(), feed.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                archive_store::claim(&path, "", &feed.id, &feed.url, 86400.0, false, 100_000.0)
                    .unwrap()
            })
        })
        .collect();
    let claimed = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap().then_some(()))
        .count();
    assert_eq!(claimed, 1);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
