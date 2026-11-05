//! Audit 8 Task 30 (B09): per-article notification payload
//! policy. The planner ports Python's
//! `_queue_new_article_notifications_from_state` assembly:
//! article title + preview bodies, dedupe, a shared
//! per-refresh cap, excluded feeds, the generic count branch,
//! and a capped summary that states the suppressed count
//! honestly. Rule exclusions are upstream of the planner:
//! producers hand it only notification-eligible articles
//! (`StoreOutcome::new_articles`, D13 semantics).

use blindrss_core::config::Config;
use blindrss_core::notify::{
    FeedBatch, NewArticleNote, plan_notifications, resolve_payload_article,
};
use blindrss_core::{Article, Db};
use std::path::PathBuf;

fn cfg_with(pairs: &[(&str, serde_json::Value)]) -> Config {
    let mut cfg = Config::default();
    for (key, value) in pairs {
        cfg.set_path(key, value.clone());
    }
    cfg
}

fn enabled_cfg() -> Config {
    cfg_with(&[("windows_notifications_enabled", serde_json::json!(true))])
}

fn note(id: &str, title: &str, content: &str, url: &str) -> NewArticleNote {
    NewArticleNote {
        id: id.to_string(),
        title: title.to_string(),
        preview_source: content.to_string(),
        url: url.to_string(),
        media_url: String::new(),
        media_type: String::new(),
    }
}

fn batch(feed_id: &str, feed_title: &str, articles: Vec<NewArticleNote>) -> FeedBatch {
    FeedBatch {
        feed_id: feed_id.to_string(),
        feed_title: feed_title.to_string(),
        articles,
        generic_count: 0,
        resolved: Vec::new(),
    }
}

#[test]
fn disabled_notifications_plan_nothing() {
    let plan = plan_notifications(
        &Config::default(),
        &[batch(
            "f1",
            "Feed",
            vec![note("a1", "T", "body", "https://x/1")],
        )],
    );
    assert!(plan.notifications.is_empty());
    assert_eq!(plan.suppressed, 0);
    assert!(plan.summary.is_none());
}

#[test]
fn per_article_payloads_carry_title_preview_and_activation() {
    let plan = plan_notifications(
        &enabled_cfg(),
        &[batch(
            "f1",
            "My Feed",
            vec![
                note(
                    "a1",
                    "First story",
                    "<p>Hello <b>wide</b> world, this is the body.</p>",
                    "https://x/1",
                ),
                note("a2", "", "Second body text", "https://x/2"),
            ],
        )],
    );
    assert_eq!(plan.notifications.len(), 2);
    let first = &plan.notifications[0];
    assert_eq!(first.title, "First story");
    assert_eq!(first.body, "My Feed - Hello wide world, this is the body.");
    let payload = first.payload.as_ref().expect("payload");
    assert_eq!(payload.article_id, "a1");
    assert_eq!(payload.url, "https://x/1");
    assert_eq!(payload.feed_id, "f1");
    // Empty titles fall back to Python's "New article".
    assert_eq!(plan.notifications[1].title, "New article");
    assert_eq!(plan.suppressed, 0);
    assert!(plan.summary.is_none());
}

#[test]
fn feed_name_can_be_omitted_from_bodies() {
    let cfg = cfg_with(&[
        ("windows_notifications_enabled", serde_json::json!(true)),
        (
            "windows_notifications_include_feed_name",
            serde_json::json!(false),
        ),
    ]);
    let plan = plan_notifications(
        &cfg,
        &[batch(
            "f1",
            "My Feed",
            vec![note("a1", "T", "the preview", "https://x/1")],
        )],
    );
    assert_eq!(plan.notifications[0].body, "the preview");
}

#[test]
fn excluded_feeds_never_notify_or_consume_budget() {
    let cfg = cfg_with(&[
        ("windows_notifications_enabled", serde_json::json!(true)),
        (
            "windows_notifications_excluded_feeds",
            serde_json::json!(["f1"]),
        ),
        (
            "windows_notifications_max_per_refresh",
            serde_json::json!(1),
        ),
    ]);
    let plan = plan_notifications(
        &cfg,
        &[
            batch("f1", "Muted", vec![note("a1", "T", "b", "https://x/1")]),
            batch("f2", "Loud", vec![note("a2", "T2", "b2", "https://x/2")]),
        ],
    );
    assert_eq!(plan.notifications.len(), 1);
    assert_eq!(
        plan.notifications[0].payload.as_ref().unwrap().feed_id,
        "f2"
    );
    assert_eq!(plan.suppressed, 0);
}

#[test]
fn dedupe_across_feeds_and_within_a_refresh() {
    let shared = note("dup", "Same", "body", "https://x/dup");
    let plan = plan_notifications(
        &enabled_cfg(),
        &[
            batch("f1", "One", vec![shared.clone()]),
            batch(
                "f2",
                "Two",
                vec![shared, note("uniq", "Other", "body", "https://x/u")],
            ),
        ],
    );
    assert_eq!(plan.notifications.len(), 2);
    assert_eq!(plan.notifications[1].title, "Other");
}

#[test]
fn cap_suppresses_and_summary_states_the_true_count() {
    let cfg = cfg_with(&[
        ("windows_notifications_enabled", serde_json::json!(true)),
        (
            "windows_notifications_max_per_refresh",
            serde_json::json!(2),
        ),
    ]);
    let plan = plan_notifications(
        &cfg,
        &[
            batch(
                "f1",
                "One",
                vec![
                    note("a1", "T1", "b", "https://x/1"),
                    note("a2", "T2", "b", "https://x/2"),
                    note("a3", "T3", "b", "https://x/3"),
                ],
            ),
            batch("f2", "Two", vec![note("a4", "T4", "b", "https://x/4")]),
        ],
    );
    assert_eq!(plan.notifications.len(), 2);
    // One suppressed inside f1's list; f2 arrives with the
    // budget already spent, so Python's early return neither
    // shows nor counts its item.
    assert_eq!(plan.suppressed, 1);
    let summary = plan.summary.expect("summary");
    assert_eq!(summary.title, "BlindRSS");
    assert_eq!(
        summary.body,
        "1 new article notification was suppressed by your cap."
    );
    assert!(summary.payload.is_none());
}

#[test]
fn summary_can_be_turned_off() {
    let cfg = cfg_with(&[
        ("windows_notifications_enabled", serde_json::json!(true)),
        (
            "windows_notifications_max_per_refresh",
            serde_json::json!(1),
        ),
        (
            "windows_notifications_show_summary_when_capped",
            serde_json::json!(false),
        ),
    ]);
    let plan = plan_notifications(
        &cfg,
        &[batch(
            "f1",
            "One",
            vec![
                note("a1", "T1", "b", "https://x/1"),
                note("a2", "T2", "b", "https://x/2"),
                note("a3", "T3", "b", "https://x/3"),
            ],
        )],
    );
    assert_eq!(plan.notifications.len(), 1);
    assert_eq!(plan.suppressed, 2);
    assert!(plan.summary.is_none());
}

#[test]
fn plural_summary_for_multiple_suppressed() {
    let cfg = cfg_with(&[
        ("windows_notifications_enabled", serde_json::json!(true)),
        (
            "windows_notifications_max_per_refresh",
            serde_json::json!(1),
        ),
    ]);
    let plan = plan_notifications(
        &cfg,
        &[batch(
            "f1",
            "One",
            vec![
                note("a1", "T1", "b", "https://x/1"),
                note("a2", "T2", "b", "https://x/2"),
                note("a3", "T3", "b", "https://x/3"),
            ],
        )],
    );
    assert_eq!(
        plan.summary.unwrap().body,
        "2 new article notifications were suppressed by your cap."
    );
}

#[test]
fn generic_branch_resolves_then_falls_back() {
    let mut generic = batch("f1", "Feed", vec![]);
    generic.generic_count = 3;
    generic.resolved = vec![note("r1", "Resolved", "its body", "https://x/r1")];
    let plan = plan_notifications(&enabled_cfg(), &[generic]);
    assert_eq!(plan.notifications.len(), 3);
    assert_eq!(plan.notifications[0].title, "Resolved");
    assert_eq!(plan.notifications[0].body, "Feed - its body");
    // Unresolved remainder: generic bodies naming the feed.
    assert_eq!(plan.notifications[1].title, "New article");
    assert_eq!(plan.notifications[1].body, "Feed");
    assert!(plan.notifications[1].payload.is_none());
    assert_eq!(plan.suppressed, 0);
}

#[test]
fn generic_branch_clamps_to_budget_and_counts_the_rest() {
    let cfg = cfg_with(&[
        ("windows_notifications_enabled", serde_json::json!(true)),
        (
            "windows_notifications_max_per_refresh",
            serde_json::json!(2),
        ),
    ]);
    let mut generic = batch("f1", "Feed", vec![]);
    generic.generic_count = 5;
    let plan = plan_notifications(&cfg, &[generic]);
    assert_eq!(plan.notifications.len(), 2);
    assert_eq!(plan.suppressed, 3);
}

#[test]
fn media_only_item_still_gets_a_payload() {
    let mut item = note("", "Podcast ep", "show notes", "");
    item.media_url = "https://x/ep.mp3".to_string();
    item.media_type = "audio/mpeg".to_string();
    let plan = plan_notifications(&enabled_cfg(), &[batch("f1", "Feed", vec![item])]);
    let payload = plan.notifications[0].payload.as_ref().expect("payload");
    assert_eq!(payload.media_url, "https://x/ep.mp3");
    // Resolving a media-only payload constructs the article
    // Python builds (id falls back to the media url).
    let article = resolve_payload_article(payload, &[], None).expect("article");
    assert_eq!(article.id, "https://x/ep.mp3");
    assert_eq!(article.media_url.as_deref(), Some("https://x/ep.mp3"));
    assert!(!article.is_read);
}

#[test]
fn empty_item_gets_no_payload() {
    let plan = plan_notifications(
        &enabled_cfg(),
        &[batch("f1", "Feed", vec![note("", "T", "b", "")])],
    );
    assert!(plan.notifications[0].payload.is_none());
}

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "blindrss_notify_{tag}_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

fn stored_article(id: &str, url: &str) -> Article {
    Article {
        id: id.to_string(),
        feed_id: "f1".to_string(),
        title: format!("Title {id}"),
        url: url.to_string(),
        content: Some("body".to_string()),
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    }
}

#[test]
fn resolve_prefers_current_list_then_db_then_payload() {
    let (db, path) = scratch_db("resolve");
    db.insert_feed_if_missing(&blindrss_core::Feed {
        id: "f1".to_string(),
        url: "https://feed".to_string(),
        title: "Feed".to_string(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    })
    .unwrap();
    let stored = stored_article("db1", "https://x/db1");
    db.insert_article_if_new(&stored).unwrap();

    let payload = blindrss_core::notify::ActivationPayload {
        article_id: "db1".to_string(),
        title: "Stale title".to_string(),
        url: "https://x/db1".to_string(),
        feed_id: "f1".to_string(),
        media_url: String::new(),
        media_type: String::new(),
    };
    // Not on screen: the store's copy wins over the payload's
    // snapshot fields.
    let resolved = resolve_payload_article(&payload, &[], Some(&db)).unwrap();
    assert_eq!(resolved.title, "Title db1");

    // On screen: the displayed row (same object the user
    // sees) wins.
    let mut on_screen = stored.clone();
    on_screen.title = "On screen".to_string();
    let resolved = resolve_payload_article(&payload, &[on_screen], Some(&db)).unwrap();
    assert_eq!(resolved.title, "On screen");

    // Unknown id with a url: constructed from the payload.
    let unknown = blindrss_core::notify::ActivationPayload {
        article_id: "gone".to_string(),
        url: "https://x/gone".to_string(),
        ..payload.clone()
    };
    let resolved = resolve_payload_article(&unknown, &[], Some(&db)).unwrap();
    assert_eq!(resolved.id, "gone");
    assert_eq!(resolved.url, "https://x/gone");

    // Nothing to open: no id match, no url, no media.
    let empty = blindrss_core::notify::ActivationPayload::default();
    assert!(resolve_payload_article(&empty, &[], Some(&db)).is_none());
    let _ = std::fs::remove_file(&path);
}
