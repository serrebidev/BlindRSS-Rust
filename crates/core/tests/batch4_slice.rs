//! Batch 4 core: smart-folder/filter-rule update + reorder, and
//! applying the pipeline to existing articles.

use blindrss_core::db::Db;

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

const RULE: &str =
    r#"{"match":"all","conditions":[{"field":"title","op":"contains","value":"rust"}]}"#;
const ACTIONS: &str = r#"{"mark_read": true, "label": "Tech"}"#;

#[test]
fn rule_update_and_reorder() {
    let path = scratch("blindrss_batch4_rules.db");
    let db = Db::open_or_create(&path).unwrap();
    let a = db
        .create_filter_rule("A", RULE, ACTIONS, true, false)
        .unwrap();
    let b = db.create_filter_rule("B", RULE, "{}", true, false).unwrap();
    db.update_filter_rule(&a, "A2", RULE, ACTIONS, false, true)
        .unwrap();
    let rules = db.filter_rules().unwrap();
    let ra = rules.iter().find(|r| r.id == a).unwrap();
    assert_eq!(ra.name, "A2");
    assert!(!ra.enabled && ra.stop);
    db.reorder_filter_rules(&[b.clone(), a.clone()]).unwrap();
    let rules = db.filter_rules().unwrap();
    assert_eq!(rules[0].id, b);
    assert_eq!(rules[1].id, a);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn smart_folder_update() {
    let path = scratch("blindrss_batch4_sf.db");
    let db = Db::open_or_create(&path).unwrap();
    let id = db.create_smart_folder("Old", RULE).unwrap();
    db.update_smart_folder(&id, "New", RULE).unwrap();
    let folders = db.smart_folders().unwrap();
    assert_eq!(folders[0].1, "New");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn apply_rules_to_existing_articles() {
    let path = scratch("blindrss_batch4_apply.db");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    db.create_filter_rule("Rust posts", RULE, ACTIONS, true, false)
        .unwrap();
    // Store two articles directly (bypassing refresh, so the
    // pipeline has not run on them yet).
    for (id, title) in [("a1", "Rust in production"), ("a2", "Cooking notes")] {
        let art = blindrss_core::db::Article {
            id: id.into(),
            feed_id: feed.id.clone(),
            title: title.into(),
            url: format!("https://example.com/{id}"),
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
    }
    let changed = blindrss_core::refresh::apply_rules_to_existing(&db, "deleted").unwrap();
    assert_eq!(changed, 1);
    let articles = db.articles(None, 10).unwrap();
    let a1 = articles.iter().find(|a| a.id == "a1").unwrap();
    let a2 = articles.iter().find(|a| a.id == "a2").unwrap();
    assert!(a1.is_read);
    assert!(!a2.is_read);
    let _ = std::fs::remove_file(&path);
}
