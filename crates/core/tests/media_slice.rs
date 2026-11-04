//! Slice 7 tests: playback state + chapters roundtrip against the
//! Python schema, and yt-dlp info parsing.

use blindrss_core::Db;
use blindrss_core::db::{Chapter, PlaybackState};
use std::path::PathBuf;

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_med_{tag}_{}.db", std::process::id()));
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
         media_url TEXT, media_type TEXT, chapter_url TEXT);
         CREATE TABLE chapters (id TEXT PRIMARY KEY, article_id TEXT, \
         start REAL, title TEXT, href TEXT);
         CREATE TABLE chapter_cache (id TEXT PRIMARY KEY, \
         cache_key TEXT NOT NULL, start REAL, title TEXT, href TEXT);
         CREATE TABLE chapter_sources (cache_key TEXT PRIMARY KEY, \
         source_url TEXT NOT NULL, etag TEXT, last_modified TEXT, \
         checked_at REAL NOT NULL DEFAULT 0, \
         fetched_at REAL NOT NULL DEFAULT 0);
         CREATE TABLE playback_state (id TEXT PRIMARY KEY, \
         position_ms INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER, \
         updated_at INTEGER NOT NULL, completed INTEGER NOT NULL DEFAULT 0, \
         seek_supported INTEGER, title TEXT);",
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path)
}

#[test]
fn playback_state_roundtrip() {
    let (db, path) = scratch_db("pb");
    assert!(db.playback_state("a1").unwrap().is_none());
    db.upsert_playback_state(
        "a1",
        &PlaybackState {
            position_ms: 61_000,
            duration_ms: Some(3_600_000),
            completed: false,
            seek_supported: Some(true),
            title: Some("Episode 1".into()),
        },
    )
    .unwrap();
    let s = db.playback_state("a1").unwrap().unwrap();
    assert_eq!(s.position_ms, 61_000);
    assert_eq!(s.duration_ms, Some(3_600_000));
    // Update in place (resume advances).
    db.upsert_playback_state(
        "a1",
        &PlaybackState {
            position_ms: 120_000,
            completed: true,
            ..s.clone()
        },
    )
    .unwrap();
    let s = db.playback_state("a1").unwrap().unwrap();
    assert_eq!(s.position_ms, 120_000);
    assert!(s.completed);
    db.delete_playback_state("a1").unwrap();
    assert!(db.playback_state("a1").unwrap().is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn chapters_replace_and_read() {
    let (db, path) = scratch_db("ch");
    db.replace_chapters(
        "a1",
        &[
            Chapter {
                start: 0.0,
                title: Some("Intro".into()),
                href: None,
            },
            Chapter {
                start: 95.5,
                title: Some("Main".into()),
                href: Some("https://example.com".into()),
            },
        ],
    )
    .unwrap();
    let ch = db.chapters("a1").unwrap();
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[1].start, 95.5);
    db.replace_chapters("a1", &[]).unwrap();
    assert!(db.chapters("a1").unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}

/// Audit 8 Task 20: the hosted chapter cache and its source
/// record (freshness bookkeeping for the Podcasting 2.0
/// pipeline) roundtrip against the Python-shaped schema, and
/// the single playback-state scan the row annotations consume
/// returns every row.
#[test]
fn chapter_cache_source_and_state_scan() {
    let (db, path) = scratch_db("cc");
    assert!(db.cached_chapters("prov:art1").unwrap().is_empty());
    assert!(db.chapter_source("prov:art1").unwrap().is_none());
    db.replace_cached_chapters(
        "prov:art1",
        &[
            Chapter {
                start: 0.0,
                title: Some("Cold Open".into()),
                href: None,
            },
            Chapter {
                start: 45.0,
                title: Some("Segment".into()),
                href: Some("https://example.com/s".into()),
            },
        ],
    )
    .unwrap();
    let cached = db.cached_chapters("prov:art1").unwrap();
    assert_eq!(cached.len(), 2);
    assert_eq!(cached[0].title.as_deref(), Some("Cold Open"));
    assert_eq!(cached[1].href.as_deref(), Some("https://example.com/s"));
    // Replace clears the old set.
    db.replace_cached_chapters("prov:art1", &[]).unwrap();
    assert!(db.cached_chapters("prov:art1").unwrap().is_empty());

    db.save_chapter_source(
        "prov:art1",
        &blindrss_core::db::ChapterSource {
            source_url: "https://example.com/chapters.json".into(),
            etag: Some("\"v1\"".into()),
            last_modified: None,
            checked_at: 100.0,
            fetched_at: 90.0,
        },
    )
    .unwrap();
    let src = db.chapter_source("prov:art1").unwrap().unwrap();
    assert_eq!(src.etag.as_deref(), Some("\"v1\""));
    assert_eq!(src.checked_at, 100.0);
    // Upsert updates in place (the 304 revalidation path).
    db.save_chapter_source(
        "prov:art1",
        &blindrss_core::db::ChapterSource {
            checked_at: 200.0,
            ..src.clone()
        },
    )
    .unwrap();
    assert_eq!(
        db.chapter_source("prov:art1").unwrap().unwrap().checked_at,
        200.0
    );

    // The grouped counts the row annotations consume.
    db.replace_chapters(
        "a1",
        &[
            Chapter {
                start: 0.0,
                title: None,
                href: None,
            },
            Chapter {
                start: 10.0,
                title: None,
                href: None,
            },
        ],
    )
    .unwrap();
    let counts = db
        .chapter_counts(&["a1".to_string(), "missing".to_string()])
        .unwrap();
    assert_eq!(counts.get("a1"), Some(&2));
    assert!(!counts.contains_key("missing"));

    // The single playback-state scan.
    db.upsert_playback_state(
        "article:a1",
        &PlaybackState {
            position_ms: 5_000,
            duration_ms: Some(90_000),
            completed: false,
            seek_supported: None,
            title: None,
        },
    )
    .unwrap();
    let all = db.all_playback_states().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, "article:a1");
    assert_eq!(all[0].1.position_ms, 5_000);
    std::fs::remove_file(&path).ok();
}

#[test]
fn ytdlp_info_parsing() {
    let json = r#"{"title":"Talk","duration":3723.5,"webpage_url":"https://youtube.com/watch?v=x",
      "chapters":[{"start_time":0,"end_time":60,"title":"Intro"},{"start_time":60,"end_time":3723,"title":"Body"}]}"#;
    let info = blindrss_core::media::parse_info(json).unwrap();
    assert_eq!(info.title, "Talk");
    assert_eq!(info.duration_secs, Some(3723.5));
    assert_eq!(info.chapters.len(), 2);
    assert_eq!(info.chapters[1].title.as_deref(), Some("Body"));
    assert!(blindrss_core::media::parse_info("not json").is_err());
}
