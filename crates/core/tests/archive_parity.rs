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

fn episode_with(
    id: &str,
    media: Option<&str>,
    keys: &[&str],
) -> blindrss_core::archive::ArchiveEpisode {
    blindrss_core::archive::ArchiveEpisode {
        entry: blindrss_core::feeds::ParsedEntry {
            id: id.into(),
            title: String::new(),
            url: String::new(),
            content_text: String::new(),
            summary: String::new(),
            date: None,
            author: None,
            media_url: media.map(str::to_string),
            media_type: None,
            tags: Vec::new(),
        },
        source_feed_url: String::new(),
        snapshot_timestamp: String::new(),
        identity_keys: keys.iter().map(|k| k.to_string()).collect(),
    }
}

#[test]
fn episode_key_matches_python_hosted_vectors() {
    // Python: sha256 of the sorted identity keys joined by "\n".
    let keyed = episode_with(
        "abc",
        Some("https://cdn.example/e.mp3"),
        &["media:https://cdn.example/e.mp3", "guid:abc"],
    );
    assert_eq!(
        keyed.key(),
        "9a452925ab8e13297483e89c1796bea43e7c2120d0331e4f587a3321da0cff3b"
    );
    // No identity keys: sha256 of "guid|media|page|title|published".
    let fallback = episode_with("g1", None, &[]);
    assert_eq!(
        fallback.key(),
        "7f39d2ce5132fff62eb70b989370cca1ffefb14b867aff02bb23563005a881be"
    );
    // Python stable_id: "podcast-archive:" + sha256(media url).
    let recovered = episode_with("", Some("https://cdn.example/e.mp3"), &[]);
    assert_eq!(
        recovered.stable_id(),
        "podcast-archive:5d99b36dfa563ea2d0157431dfcd3ee2e5f38908b2c3df16745a4f979b574790"
    );
}

#[test]
fn identity_keys_match_python_guid_and_duration_rules() {
    // A non-URL GUID is only case-folded, never canonicalized.
    let keys = archive::episode_identity_keys("Tag:Example,2026:ABC", "", None, "T", None, None);
    assert!(keys.contains("guid:tag:example,2026:abc"));
    // A URL GUID is canonicalized (tracking params dropped).
    let keys = archive::episode_identity_keys(
        "HTTPS://Example.COM/EP1/?utm_source=x",
        "",
        None,
        "T",
        None,
        None,
    );
    assert!(keys.contains("guid:https://example.com/ep1"));
    // Undated episodes fall back to title-duration; dated ones
    // use title-date and never title-duration.
    let keys = archive::episode_identity_keys("", "", None, "My Episode!", None, Some("1800"));
    assert!(keys.contains("title-duration:my episode|1800"));
    let keys = archive::episode_identity_keys(
        "",
        "",
        None,
        "My Episode!",
        Some("2026-10-20T10:00:00Z"),
        Some("1800"),
    );
    assert!(keys.contains("title-date:my episode|2026-10-20"));
    assert!(!keys.iter().any(|k| k.starts_with("title-duration:")));
}

#[test]
fn parse_document_drops_episodes_without_identity() {
    let doc = b"<rss version='2.0'><channel><title>Show</title>\
        <item><guid>kept</guid><title>Kept</title><enclosure url='https://cdn.example/k.mp3' type='audio/mpeg'/></item>\
        <item><title>Keyless</title></item>\
        </channel></rss>";
    let parsed = archive::parse_document(doc, "https://example.com/rss", "").unwrap();
    assert_eq!(parsed.episodes.len(), 1);
    assert_eq!(parsed.episodes[0].entry.id, "kept");
}

#[test]
fn guideless_episode_keys_never_come_from_the_invented_id() {
    // feed-rs invents a random UUID id (mirrored into the URL)
    // for guid-less entries; Python's feedparser leaves both
    // empty. A guid-less episode with media is kept by its media
    // key alone, and its hosted episode key is stable across
    // parses — the invented id must not leak into either.
    let doc = b"<rss version='2.0'><channel><title>Show</title>\
        <item><title>Loose</title><enclosure url='https://cdn.example/loose.mp3' type='audio/mpeg'/></item>\
        </channel></rss>";
    let first = archive::parse_document(doc, "https://example.com/rss", "").unwrap();
    let second = archive::parse_document(doc, "https://example.com/rss", "").unwrap();
    assert_eq!(first.episodes.len(), 1);
    assert!(
        first.episodes[0]
            .identity_keys
            .iter()
            .all(|key| key.starts_with("media:"))
    );
    assert_eq!(first.episodes[0].key(), second.episodes[0].key());
}

#[test]
fn scanner_retries_transient_failures_but_not_permanent_ones() {
    let mut live_calls = 0;
    let result = archive::scan_with_fetch(
        &["https://example.com/rss".into()],
        archive::ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
        |url| {
            if url.contains("/cdx/") {
                return Ok((b"[]".to_vec(), url.to_string()));
            }
            live_calls += 1;
            if live_calls < 3 {
                return Err("HTTP 503 Service Unavailable".into());
            }
            Ok((
                document("one", "https://cdn.example/one.mp3", "One"),
                url.to_string(),
            ))
        },
    )
    .unwrap();
    assert_eq!(live_calls, 3);
    assert_eq!(result.episodes.len(), 1);

    let mut live_calls = 0;
    let result = archive::scan_with_fetch(
        &["https://example.com/rss".into()],
        archive::ScanOptions::default(),
        &AtomicBool::new(false),
        |_| {},
        |url| {
            if url.contains("/cdx/") {
                return Ok((b"[]".to_vec(), url.to_string()));
            }
            live_calls += 1;
            Err("HTTP 404 Not Found".into())
        },
    )
    .unwrap();
    assert_eq!(live_calls, 1);
    assert!(result.episodes.is_empty());
}

#[test]
fn scanner_uses_supplied_current_document_and_reports_episode_progress() {
    let current = document("cur", "https://cdn.example/cur.mp3", "Current");
    let mut fetched = Vec::new();
    let mut progress_seen = Vec::new();
    let result = archive::scan_with_fetch(
        &["https://example.com/rss".into()],
        archive::ScanOptions {
            current_document: Some(current),
            ..Default::default()
        },
        &AtomicBool::new(false),
        |p| progress_seen.push((p.stage, p.feed_urls, p.episodes_found)),
        |url| {
            fetched.push(url.to_string());
            let bytes = if url.contains("/cdx/") {
                br#"[["timestamp","original"],["20250101","https://example.com/rss"]]"#.to_vec()
            } else {
                document("old", "https://cdn.example/old.mp3", "Old")
            };
            Ok((bytes, url.to_string()))
        },
    )
    .unwrap();
    // The live feed itself was never fetched; only the CDX index
    // and the one snapshot were.
    assert_eq!(fetched.len(), 2);
    assert!(fetched.iter().all(|u| u != "https://example.com/rss"));
    assert_eq!(result.episodes.len(), 2);
    let complete = progress_seen.last().unwrap();
    assert_eq!(complete.0, "complete");
    assert_eq!(complete.1, 1);
    assert_eq!(complete.2, 2);
}

#[test]
fn archive_settings_follow_python_config_defaults_and_formulas() {
    let mut cfg = blindrss_core::config::Config::default();
    let settings = archive_store::ArchiveSettings::from_config(&cfg);
    assert!(settings.enabled);
    assert_eq!(settings.max_snapshots, 5000);
    assert_eq!(settings.local_rescan_seconds(), 30.0 * 86400.0);
    assert_eq!(settings.hosted_rescan_seconds(), 30.0 * 86400.0);
    cfg.set_path("podcast_archive_rescan_days", serde_json::json!(0.5));
    cfg.set_path("podcast_archive_max_snapshots", serde_json::json!(0));
    cfg.set_path("podcast_archive_enabled", serde_json::json!(false));
    let settings = archive_store::ArchiveSettings::from_config(&cfg);
    assert!(!settings.enabled);
    assert_eq!(settings.max_snapshots, 5000);
    // Local clamps days to >= 1; hosted floors the interval at a day.
    assert_eq!(settings.local_rescan_seconds(), 86400.0);
    assert_eq!(settings.hosted_rescan_seconds(), 86400.0);
}

#[test]
fn archive_scopes_match_python_vectors() {
    assert_eq!(
        archive_store::hosted_scope(
            "BazQux",
            "https://www.bazqux.com/reader/api/0|user@example.com"
        ),
        "bazqux:fe47ab258b3361f20cb2"
    );
    assert_eq!(
        archive_store::miniflux_scope("https://miniflux.example/", "secretkey"),
        "miniflux:2870c77633e22aa2e6cf"
    );
    assert_eq!(
        archive_store::podcast_source_url("feed/https://example.com/rss", "https://other/rss"),
        "https://example.com/rss"
    );
    assert_eq!(
        archive_store::podcast_source_url("miniflux:42", "https://other/rss"),
        "https://other/rss"
    );
    // scope_for_config: local is the empty scope; a BazQux account
    // scopes by its lowercased email identity.
    let mut cfg = blindrss_core::config::Config::default();
    assert_eq!(archive_store::scope_for_config(&cfg), "");
    cfg.set_path("active_provider", serde_json::json!("bazqux"));
    cfg.set_path(
        "providers.bazqux.email",
        serde_json::json!("User@Example.com"),
    );
    assert_eq!(
        archive_store::scope_for_config(&cfg),
        "bazqux:fe47ab258b3361f20cb2"
    );
}

#[test]
fn due_archive_feeds_applies_claim_backoff_rules() {
    let now = 1_000_000.0;
    let state = |feed_id: &str, status: &str, attempt: Option<f64>, success: Option<f64>| {
        archive_store::ArchiveState {
            feed_id: feed_id.into(),
            feed_url: String::new(),
            status: status.into(),
            last_attempt: attempt,
            last_success: success,
            snapshot_count: 0,
            episode_count: 0,
            error: String::new(),
        }
    };
    let states = vec![
        state("fresh", "complete", Some(now - 100.0), Some(now - 100.0)),
        state("errored", "error", Some(now - 100.0), None),
        state("stale-scan", "scanning", Some(now - 7.0 * 3600.0), None),
    ];
    let feeds: Vec<(String, String)> = ["fresh", "errored", "stale-scan", "never", "no-url"]
        .iter()
        .map(|id| {
            (
                id.to_string(),
                if *id == "no-url" {
                    String::new()
                } else {
                    "https://example.com/rss".to_string()
                },
            )
        })
        .collect();
    let due = archive_store::due_archive_feeds(&states, &feeds, 86400.0, now);
    let ids: Vec<&str> = due.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["stale-scan", "never"]);
}

#[test]
fn local_scan_stores_recovered_episodes_and_tracks_state() {
    let path = std::env::temp_dir().join(format!(
        "blindrss-archive-local-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = blindrss_core::Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed("https://example.com/rss", "Show", None)
        .unwrap();
    drop(db);
    let current = document("cur", "https://cdn.example/cur.mp3", "Current");
    let settings = archive_store::ArchiveSettings::default();
    let outcome = archive::perform_local_scan_with_fetch(
        &path,
        &feed,
        &settings,
        archive::Claim::Force,
        None,
        &AtomicBool::new(false),
        |_| {},
        |url| {
            let bytes = if url.contains("/cdx/") {
                br#"[["timestamp","original"],["20250101","https://example.com/rss"]]"#.to_vec()
            } else if url.contains("20250101id_") {
                document("old", "https://cdn.example/old.mp3", "Old")
            } else {
                current.clone()
            };
            Ok((bytes, url.to_string()))
        },
    )
    .unwrap();
    assert_eq!(outcome.kind, archive::LocalScanKind::Stored);
    assert_eq!(outcome.recovered, 1);
    assert_eq!(outcome.episodes, 2);
    assert_eq!(outcome.snapshots_loaded, 1);
    let state = archive_store::state(&path, "", &feed.id).unwrap();
    assert_eq!(state.status, "complete");
    assert_eq!(state.snapshot_count, 1);
    assert_eq!(state.episode_count, 2);
    let db = blindrss_core::Db::open(&path).unwrap();
    let articles = db.articles(Some(&feed.id), 10).unwrap();
    assert_eq!(articles.len(), 1);
    assert_eq!(articles[0].title, "Old");
    drop(db);
    // A due-claim rescan right after a success is refused.
    let outcome = archive::perform_local_scan_with_fetch(
        &path,
        &feed,
        &settings,
        archive::Claim::Due,
        None,
        &AtomicBool::new(false),
        |_| {},
        |url| Ok((current.clone(), url.to_string())),
    )
    .unwrap();
    assert_eq!(outcome.kind, archive::LocalScanKind::NotDue);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn local_scan_skips_feeds_without_podcast_media() {
    let path = std::env::temp_dir().join(format!(
        "blindrss-archive-nomedia-{}.db",
        uuid::Uuid::new_v4()
    ));
    let db = blindrss_core::Db::open_or_create(&path).unwrap();
    let feed = db
        .add_feed("https://example.com/blog", "Blog", None)
        .unwrap();
    drop(db);
    let plain = b"<rss version='2.0'><channel><title>Blog</title>\
        <item><guid>p1</guid><title>Post</title><link>https://example.com/p1</link></item>\
        </channel></rss>"
        .to_vec();
    let outcome = archive::perform_local_scan_with_fetch(
        &path,
        &feed,
        &archive_store::ArchiveSettings::default(),
        archive::Claim::Force,
        None,
        &AtomicBool::new(false),
        |_| {},
        |url| Ok((plain.clone(), url.to_string())),
    )
    .unwrap();
    assert_eq!(outcome.kind, archive::LocalScanKind::NotPodcast);
    assert_eq!(
        archive_store::state(&path, "", &feed.id).unwrap().status,
        "never"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn hosted_scan_writes_only_the_owning_scope_and_file() {
    let dir = std::env::temp_dir();
    let path_a = dir.join(format!(
        "blindrss-archive-hosted-a-{}.db",
        uuid::Uuid::new_v4()
    ));
    let path_b = dir.join(format!(
        "blindrss-archive-hosted-b-{}.db",
        uuid::Uuid::new_v4()
    ));
    for path in [&path_a, &path_b] {
        drop(blindrss_core::Db::open_or_create(path).unwrap());
    }
    let scope = archive_store::hosted_scope(
        "BazQux",
        "https://www.bazqux.com/reader/api/0|user@example.com",
    );
    let feed_id = "feed/https://example.com/rss";
    let feed_url = "https://example.com/rss";
    let settings = archive_store::ArchiveSettings::default();
    let current = document("cur", "https://cdn.example/cur.mp3", "Current");
    let fetch = |current: Vec<u8>| {
        move |url: &str| {
            let bytes = if url.contains("/cdx/") {
                br#"[["timestamp","original"],["20250101","https://example.com/rss"]]"#.to_vec()
            } else if url.contains("20250101id_") {
                document("old", "https://cdn.example/old.mp3", "Old")
            } else {
                current.clone()
            };
            Ok((bytes, url.to_string()))
        }
    };
    assert!(
        archive_store::claim(
            &path_a,
            &scope,
            feed_id,
            feed_url,
            86400.0,
            true,
            archive_store::now_seconds()
        )
        .unwrap()
    );
    let outcome = archive::run_hosted_scan_with_fetch(
        &path_a,
        &scope,
        feed_id,
        feed_url,
        &settings,
        &AtomicBool::new(false),
        |_| {},
        fetch(current.clone()),
    );
    assert_eq!(outcome.kind, archive::HostedScanKind::Stored);
    assert_eq!(outcome.recovered, 1);
    let entries = archive_store::entries(&path_a, &scope, feed_id).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "Old");
    // Nothing leaked into the other account's file or scope, and
    // nothing into the local state table of the account file.
    assert!(
        archive_store::entries(&path_b, &scope, feed_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        archive_store::entries(&path_a, "other-scope", feed_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        archive_store::state(&path_a, "", feed_id).unwrap().status,
        "never"
    );
    let state = archive_store::state(&path_a, &scope, feed_id).unwrap();
    assert_eq!(state.status, "complete");
    assert_eq!(state.episode_count, 2);

    // A failed rescan records the error but keeps recovered rows.
    let parsed = archive::parse_document(
        &document("old", "https://cdn.example/old.mp3", "Old"),
        "https://example.com/rss",
        "20250101",
    )
    .unwrap();
    archive_store::replace_entries(&path_a, &scope, feed_id, &parsed.episodes).unwrap();
    assert!(
        archive_store::claim(
            &path_a,
            &scope,
            feed_id,
            feed_url,
            86400.0,
            true,
            archive_store::now_seconds()
        )
        .unwrap()
    );
    let outcome = archive::run_hosted_scan_with_fetch(
        &path_a,
        &scope,
        feed_id,
        feed_url,
        &settings,
        &AtomicBool::new(false),
        |_| {},
        |url| {
            if url.contains("/cdx/") {
                return Err("HTTP 404 Not Found".into());
            }
            Ok((current.clone(), url.to_string()))
        },
    );
    assert_eq!(outcome.kind, archive::HostedScanKind::Failed);
    assert_eq!(
        archive_store::entries(&path_a, &scope, feed_id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        archive_store::state(&path_a, &scope, feed_id)
            .unwrap()
            .status,
        "error"
    );

    // A feed whose current document has no media clears its rows
    // and finishes complete (Python's migration probe).
    let plain = b"<rss version='2.0'><channel><title>Blog</title>\
        <item><guid>p1</guid><title>Post</title><link>https://example.com/p1</link></item>\
        </channel></rss>"
        .to_vec();
    assert!(
        archive_store::claim(
            &path_a,
            &scope,
            feed_id,
            feed_url,
            86400.0,
            true,
            archive_store::now_seconds()
        )
        .unwrap()
    );
    let outcome = archive::run_hosted_scan_with_fetch(
        &path_a,
        &scope,
        feed_id,
        feed_url,
        &settings,
        &AtomicBool::new(false),
        |_| {},
        |url| Ok((plain.clone(), url.to_string())),
    );
    assert_eq!(outcome.kind, archive::HostedScanKind::Cleared);
    assert!(
        archive_store::entries(&path_a, &scope, feed_id)
            .unwrap()
            .is_empty()
    );
    std::fs::remove_file(path_a).unwrap();
    std::fs::remove_file(path_b).unwrap();
}

#[test]
fn archive_job_queue_runs_jobs_serially_in_order() {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log1 = log.clone();
    archive::enqueue_archive_job(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        log1.lock().unwrap().push(1);
    });
    let log2 = log.clone();
    archive::enqueue_archive_job(move || log2.lock().unwrap().push(2));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while log.lock().unwrap().len() < 2 {
        assert!(std::time::Instant::now() < deadline, "archive jobs stalled");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(*log.lock().unwrap(), [1, 2]);
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
