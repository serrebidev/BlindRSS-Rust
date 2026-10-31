//! Batch 6: the active-provider factory and the Local
//! provider's round trip.

use blindrss_core::config::Config;
use blindrss_core::provider::{active_name, get_provider};

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn cfg_with_active(name: &str) -> Config {
    let mut cfg = Config::default();
    cfg.set_path("active_provider", serde_json::json!(name));
    cfg
}

#[test]
fn factory_selects_by_name() {
    let path = scratch("blindrss_batch6_factory.db");
    let _ = blindrss_core::db::Db::open_or_create(&path).unwrap();
    assert_eq!(
        get_provider(&cfg_with_active("local"), path.clone()).name(),
        "local"
    );
    assert!(!get_provider(&cfg_with_active("local"), path.clone()).is_remote());
    let mf = get_provider(&cfg_with_active("miniflux"), path.clone());
    assert_eq!(mf.name(), "miniflux");
    assert!(mf.is_remote());
    assert_eq!(
        get_provider(&cfg_with_active("theoldreader"), path.clone()).name(),
        "theoldreader"
    );
    // Unknown names fall back to Local, like Python's else.
    assert_eq!(
        get_provider(&cfg_with_active("bogus"), path.clone()).name(),
        "local"
    );
    assert_eq!(active_name(&cfg_with_active("bogus")), "local");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn local_provider_round_trip() {
    let path = scratch("blindrss_batch6_local.db");
    let db = blindrss_core::db::Db::open_or_create(&path).unwrap();
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    let art = blindrss_core::db::Article {
        id: "p1".into(),
        feed_id: feed.id.clone(),
        title: "Post".into(),
        url: "https://example.com/p".into(),
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
    drop(db);
    let provider = get_provider(&cfg_with_active("local"), path.clone());
    assert_eq!(provider.feeds().unwrap().len(), 1);
    let articles = provider.articles(None, 10).unwrap();
    assert_eq!(articles.len(), 1);
    provider.mark_read(&articles[0], true).unwrap();
    provider.set_favorite(&articles[0], true).unwrap();
    let after = provider.articles(None, 10).unwrap();
    assert!(after[0].is_read && after[0].is_favorite);
    provider.remove_feed(&provider.feeds().unwrap()[0]).unwrap();
    assert!(provider.feeds().unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn hosted_refresh_reports_missing_credentials() {
    let path = scratch("blindrss_batch6_hosted.db");
    let _ = blindrss_core::db::Db::open_or_create(&path).unwrap();
    let provider = get_provider(&cfg_with_active("miniflux"), path.clone());
    let err = provider.refresh().unwrap_err();
    assert!(err.contains("Miniflux"), "{err}");
    let _ = std::fs::remove_file(&path);
}
