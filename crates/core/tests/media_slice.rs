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
