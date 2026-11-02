//! Batch 39 (hosted account storage isolation): per-account
//! cache paths, and the hosted-row purges that keep one
//! account's rows out of another account's database (and out
//! of the local database entirely).

use blindrss_core::config::Config;
use blindrss_core::db::{Article, Chapter, Db, Feed, PlaybackState};
use blindrss_core::provider::db_path_for_provider;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "blindrss_b39_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ))
}

fn cfg_with(providers: serde_json::Value) -> Config {
    let mut cfg = Config::default();
    cfg.set_path("providers", providers);
    cfg
}

#[test]
fn provider_db_paths_are_account_scoped() {
    let base = PathBuf::from("/data/rss.db");
    let cfg = cfg_with(serde_json::json!({
        "theoldreader": {"email": "a@example.com", "password": "x"},
        "bazqux": {"email": "a@example.com", "password": "x"},
        "miniflux": {"url": "https://miniflux.example.com", "api_key": "k1"}
    }));
    // Local is the base file itself.
    assert_eq!(db_path_for_provider(&cfg, &base, "local"), base);
    // Each hosted account gets a distinct file.
    let tor = db_path_for_provider(&cfg, &base, "theoldreader");
    let baz = db_path_for_provider(&cfg, &base, "bazqux");
    let mf = db_path_for_provider(&cfg, &base, "miniflux");
    assert_ne!(tor, base);
    assert_ne!(tor, baz);
    assert_ne!(tor, mf);
    assert_ne!(baz, mf);
    let name = |p: &PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name(&tor).starts_with("rss.hosted-theoldreader-"),
        "{}",
        name(&tor)
    );
    assert!(
        name(&mf).starts_with("rss.hosted-miniflux-"),
        "{}",
        name(&mf)
    );
    // Stable for the same credentials.
    assert_eq!(db_path_for_provider(&cfg, &base, "miniflux"), mf);
    // A different API key is a different account file.
    let cfg2 = cfg_with(serde_json::json!({
        "miniflux": {"url": "https://miniflux.example.com", "api_key": "k2"}
    }));
    assert_ne!(db_path_for_provider(&cfg2, &base, "miniflux"), mf);
    // Unknown names fall back to local.
    assert_eq!(db_path_for_provider(&cfg, &base, "nope"), base);
    // An already-scoped base re-bases onto the plain stem.
    let rescoped = db_path_for_provider(&cfg, &tor, "bazqux");
    assert_eq!(rescoped, baz);
}

fn article(id: &str, feed_id: &str) -> Article {
    Article {
        id: id.into(),
        feed_id: feed_id.into(),
        title: "T".into(),
        url: "https://example.com/x".into(),
        content: None,
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    }
}

fn hosted_feed(id: &str) -> Feed {
    Feed {
        id: id.into(),
        url: "https://example.com/rss".into(),
        title: "H".into(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    }
}

/// One local feed + article, one GReader feed + article, one
/// Miniflux feed + entry article with chapters and a playback
/// row on the hosted articles.
fn seeded(tag: &str) -> (Db, PathBuf, String) {
    let path = scratch(tag);
    let db = Db::open_or_create(&path).unwrap();
    let local = db
        .add_feed("https://example.com/local", "Local", None)
        .unwrap();
    db.insert_article_if_new(&article("local-art", &local.id))
        .unwrap();
    db.upsert_hosted_feed(&hosted_feed("feed/https://example.com/rss"))
        .unwrap();
    db.upsert_hosted_article(&article("gr-item-1", "feed/https://example.com/rss"))
        .unwrap();
    db.upsert_hosted_feed(&hosted_feed("miniflux:42")).unwrap();
    db.upsert_hosted_article(&article("miniflux-entry:1001", "miniflux:42"))
        .unwrap();
    db.replace_chapters(
        "miniflux-entry:1001",
        &[Chapter {
            start: 0.0,
            title: Some("Intro".into()),
            href: None,
        }],
    )
    .unwrap();
    db.upsert_playback_state(
        "miniflux-entry:1001",
        &PlaybackState {
            position_ms: 5_000,
            duration_ms: Some(60_000),
            completed: false,
            seek_supported: Some(true),
            title: Some("T".into()),
        },
    )
    .unwrap();
    (db, path, local.id)
}

#[test]
fn purge_hosted_rows_keeps_only_local() {
    let (db, path, local_id) = seeded("purgeall");
    let removed = db.purge_hosted_rows().unwrap();
    assert_eq!(removed, 2);
    let feeds = db.feeds().unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].id, local_id);
    let articles = db.articles(None, 100).unwrap();
    assert_eq!(articles.len(), 1);
    assert_eq!(articles[0].id, "local-art");
    // Dependent rows went with their articles.
    assert!(db.chapters("miniflux-entry:1001").unwrap().is_empty());
    assert!(db.playback_state("miniflux-entry:1001").unwrap().is_none());
    // Idempotent.
    assert_eq!(db.purge_hosted_rows().unwrap(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn purge_except_keeps_the_owning_kind() {
    let (db, path, _local) = seeded("purgegr");
    // A GReader account cache keeps its feed/ rows only.
    let removed = db.purge_hosted_rows_except("feed/").unwrap();
    assert_eq!(removed, 1);
    let feed_ids: Vec<String> = db.feeds().unwrap().into_iter().map(|f| f.id).collect();
    assert!(feed_ids.iter().any(|i| i == "feed/https://example.com/rss"));
    assert!(!feed_ids.iter().any(|i| i == "miniflux:42"));
    let art_ids: Vec<String> = db
        .articles(None, 100)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert!(art_ids.iter().any(|i| i == "gr-item-1"));
    assert!(!art_ids.iter().any(|i| i == "miniflux-entry:1001"));
    let _ = std::fs::remove_file(&path);

    let (db, path, _local) = seeded("purgemf");
    // A Miniflux account cache keeps miniflux rows, entries
    // and their playback state.
    let removed = db.purge_hosted_rows_except("miniflux:").unwrap();
    assert_eq!(removed, 1);
    let feed_ids: Vec<String> = db.feeds().unwrap().into_iter().map(|f| f.id).collect();
    assert!(feed_ids.iter().any(|i| i == "miniflux:42"));
    assert!(!feed_ids.iter().any(|i| i.starts_with("feed/")));
    assert!(db.playback_state("miniflux-entry:1001").unwrap().is_some());
    let _ = std::fs::remove_file(&path);
}
