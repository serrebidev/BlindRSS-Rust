//! Batch 5 core: download format presets, retention policies,
//! user-agent resolution, delete behaviours, search modes.

use blindrss_core::db::Db;
use blindrss_core::download::{normalize_download_format, ytdlp_format_args};
use blindrss_core::retention::retention_days;
use blindrss_core::user_agents::resolve;

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn format_args_match_python_specs() {
    let best = ytdlp_format_args("video_best");
    assert_eq!(best[0], "-f");
    assert!(
        best[1].starts_with("bv*[vcodec^=avc1]+ba[acodec^=mp4a]/bv*+ba/b"),
        "{}",
        best[1]
    );
    assert!(best.contains(&"--merge-output-format".to_string()));
    let mp3 = ytdlp_format_args("audio_mp3_192");
    assert!(mp3.contains(&"-x".to_string()));
    assert!(mp3.contains(&"192K".to_string()));
    let audio_best = ytdlp_format_args("audio_best");
    assert!(audio_best.contains(&"best".to_string()));
    assert_eq!(normalize_download_format("bogus"), "video_best");
    let capped = ytdlp_format_args("video_720");
    assert!(capped[1].contains("[height<=?720]"), "{}", capped[1]);
}

#[test]
fn retention_and_user_agent() {
    assert_eq!(retention_days("2_weeks"), Some(14));
    assert_eq!(retention_days("unlimited"), None);
    assert!(resolve("edge_windows", "").contains("Edg/"));
    assert!(resolve("installed:chrome", "").contains("Chrome/"));
}

#[test]
fn purge_and_delete_behavior() {
    let path = scratch("blindrss_batch5_purge.db");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    let art = blindrss_core::db::Article {
        id: "x1".into(),
        feed_id: feed.id.clone(),
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
    };
    db.insert_article_if_new(&art).unwrap();
    db.purge_article("x1").unwrap();
    // Purged tombstones stay out of the Deleted view.
    assert!(db.deleted_articles().unwrap().is_empty());
    assert!(db.articles(None, 10).unwrap().is_empty());
    // Feed delete-behaviour override round-trips.
    assert_eq!(db.feed_delete_behavior(&feed.id).unwrap(), None);
    db.set_feed_delete_behavior(&feed.id, Some("purge"))
        .unwrap();
    assert_eq!(
        db.feed_delete_behavior(&feed.id).unwrap().as_deref(),
        Some("purge")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn search_modes() {
    let path = scratch("blindrss_batch5_search.db");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    let art = blindrss_core::db::Article {
        id: "s1".into(),
        feed_id: feed.id.clone(),
        title: "Cooking".into(),
        url: "https://example.com/s".into(),
        content: Some("all about rust lang".into()),
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    };
    db.insert_article_if_new(&art).unwrap();
    assert_eq!(db.search_articles_mode("rust", 10, false).unwrap().len(), 1);
    assert_eq!(db.search_articles_mode("rust", 10, true).unwrap().len(), 0);
    assert_eq!(
        db.search_articles_mode("cooking", 10, true).unwrap().len(),
        1
    );
    let _ = std::fs::remove_file(&path);
}
