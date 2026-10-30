//! Smart folders + filter rules tests: rule evaluation mirrors the
//! Python semantics, and the store pipeline applies actions to new
//! articles as they arrive.

use blindrss_core::Db;
use blindrss_core::rules::{EffectiveActions, RuleArticle, evaluate_pipeline, rule_matches};
use serde_json::json;
use std::path::PathBuf;

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss_rules_{tag}_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE feeds (id TEXT PRIMARY KEY, url TEXT, title TEXT, \
         title_is_custom INTEGER DEFAULT 0, category TEXT, icon_url TEXT);
         CREATE TABLE articles (id TEXT PRIMARY KEY, feed_id TEXT, title TEXT, \
         url TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0, \
         media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL, \
         category_override TEXT);
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
         seek_supported INTEGER, title TEXT);
         CREATE TABLE smart_folders (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
         rule_json TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE filter_rules (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
         rule_json TEXT NOT NULL, actions_json TEXT NOT NULL, \
         enabled INTEGER NOT NULL DEFAULT 1, stop INTEGER NOT NULL DEFAULT 0, \
         position INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE article_labels (article_id TEXT NOT NULL, \
         category TEXT NOT NULL, PRIMARY KEY (article_id, category));",
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path)
}

fn sample_article() -> blindrss_core::Article {
    blindrss_core::Article {
        id: "a1".into(),
        feed_id: "f1".into(),
        title: "Rust release notes".into(),
        url: "https://example.com/1".into(),
        content: Some("All about Rust".into()),
        description: Some("notes".into()),
        date: None,
        author: Some("Ferris".into()),
        is_read: false,
        is_favorite: true,
        media_url: None,
        media_type: None,
    }
}

#[test]
fn rule_evaluation_matches_python_semantics() {
    let art = sample_article();
    let view = RuleArticle {
        article: &art,
        feed_title: "Blog",
        opened: false,
        updated: false,
        tags: &[],
    };
    // all-group: title contains + favorite is true
    let rule = json!({"match":"all","conditions":[
        {"field":"title","op":"contains","value":"rust"},
        {"field":"favorite","value":true}
    ]});
    assert!(rule_matches(&rule, &view));
    // any-group with one failing, one passing
    let rule = json!({"match":"any","conditions":[
        {"field":"title","op":"contains","value":"python"},
        {"field":"author","op":"equals","value":"ferris"}
    ]});
    assert!(rule_matches(&rule, &view));
    // not_contains
    let rule = json!({"match":"all","conditions":[
        {"field":"content","op":"not_contains","value":"python"}
    ]});
    assert!(rule_matches(&rule, &view));
    // read = false matches an unread article; read = true does not
    let rule = json!({"match":"all","conditions":[{"field":"read","value":false}]});
    assert!(rule_matches(&rule, &view));
    let rule = json!({"match":"all","conditions":[{"field":"read","value":true}]});
    assert!(!rule_matches(&rule, &view));
    // nested group
    let rule = json!({"match":"all","conditions":[
        {"match":"any","conditions":[{"field":"url","op":"starts_with","value":"https://example"}]},
        {"field":"feed","op":"equals","value":"blog"}
    ]});
    assert!(rule_matches(&rule, &view));
    // empty any matches nothing; empty all matches everything
    assert!(!rule_matches(
        &json!({"match":"any","conditions":[]}),
        &view
    ));
    assert!(rule_matches(&json!({"match":"all","conditions":[]}), &view));
}

#[test]
fn pipeline_applies_on_store_with_stop_and_labels() {
    let (db, path) = scratch_db("pipe");
    let feed = db
        .add_feed("https://example.com/x.xml", "Blog", None)
        .unwrap();
    // Rule 1: title contains "rust" -> mark read + label "Tech" + fav
    db.create_filter_rule(
        "rust-rule",
        &json!({"match":"all","conditions":[{"field":"title","op":"contains","value":"rust"}]})
            .to_string(),
        &json!({"mark_read": true, "mark_favorite": true, "label": "Tech"}).to_string(),
        true,
        false,
    )
    .unwrap();
    // Rule 2: everything -> delete (must NOT hit the rust article if
    // rule 1 had stop... it doesn't; but delete rule only matches
    // title containing "spam")
    db.create_filter_rule(
        "spam-rule",
        &json!({"match":"all","conditions":[{"field":"title","op":"contains","value":"spam"}]})
            .to_string(),
        &json!({"delete": true}).to_string(),
        true,
        false,
    )
    .unwrap();
    let parsed = blindrss_core::feeds::parse_feed(
        br#"<?xml version="1.0"?><rss version="2.0"><channel><title>Blog</title>
        <item><title>Rust news</title><link>https://example.com/1</link><guid>g1</guid></item>
        <item><title>Spam offer</title><link>https://example.com/2</link><guid>g2</guid></item>
        <item><title>Plain post</title><link>https://example.com/3</link><guid>g3</guid></item>
        </channel></rss>"#,
    )
    .unwrap();
    blindrss_core::refresh::store_parsed(&db, &feed, &parsed).unwrap();
    let g1 = db.article_by_id("g1").unwrap().unwrap();
    assert!(g1.is_read && g1.is_favorite);
    assert_eq!(
        db.article_ids_with_label("Tech").unwrap(),
        vec!["g1".to_string()]
    );
    // Spam article deleted via pipeline (tombstoned)
    assert!(db.article_by_id("g2").unwrap().is_none());
    assert!(db.deleted_article_ids().unwrap().contains("g2"));
    // Plain untouched
    let g3 = db.article_by_id("g3").unwrap().unwrap();
    assert!(!g3.is_read && !g3.is_favorite);
    std::fs::remove_file(&path).ok();
}

#[test]
fn smart_folder_crud_and_pipeline_stop() {
    let (db, path) = scratch_db("sf");
    let id = db
        .create_smart_folder("Unread Rust", "{\"match\":\"all\",\"conditions\":[]}")
        .unwrap();
    let folders = db.smart_folders().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].1, "Unread Rust");
    db.delete_smart_folder(&id).unwrap();
    assert!(db.smart_folders().unwrap().is_empty());

    // evaluate_pipeline: stop halts later rules
    let art = sample_article();
    let view = RuleArticle {
        article: &art,
        feed_title: "Blog",
        opened: false,
        updated: false,
        tags: &[],
    };
    let mk = |actions: serde_json::Value, stop: bool| blindrss_core::rules::FilterRule {
        id: "x".into(),
        name: "x".into(),
        rule: json!({"match":"all","conditions":[]}),
        actions,
        enabled: true,
        stop,
        position: 0,
    };
    let rules = vec![
        mk(json!({"mark_read": true}), true),
        mk(json!({"mark_favorite": true}), false),
    ];
    let eff = evaluate_pipeline(&rules, &view);
    assert_eq!(
        eff,
        EffectiveActions {
            mark_read: true,
            ..Default::default()
        }
    );
    std::fs::remove_file(&path).ok();
}
