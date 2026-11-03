//! Audit 8 Task 2 (D01, D02, D11): category mutations must use
//! Python's canonical spaced path separator (" / "), rewrite every
//! reference transactionally, and expose the provider sentinel
//! capability. Seeded data is Python-shaped: spaced full paths,
//! repeated leaf names under different parents, labels, overrides,
//! filter-rule targets and per-feed delete targets.

use blindrss_core::Db;
use std::path::PathBuf;

fn scratch_db(name: &str) -> (Db, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "blindrss_a8cat_{name}_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);
    // Db::open creates the Python schema on an empty file.
    std::fs::write(&path, b"").unwrap();
    (Db::open(&path).unwrap(), path)
}

fn raw(path: &PathBuf) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

fn titles(db: &Db) -> Vec<String> {
    let mut t: Vec<String> = db
        .categories()
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    t.sort();
    t
}

/// Python-shaped seed: "Podcasts / Others" and "News / Others"
/// share the leaf "Others"; references point at spaced paths.
fn seed(db: &Db, path: &PathBuf) -> (String, String) {
    db.ensure_category_path("Podcasts / Others").unwrap();
    db.ensure_category_path("News / Others").unwrap();
    db.ensure_category_path("Podcasts / Others / Deep").unwrap();
    let f1 = db
        .add_feed("https://example.com/a", "Feed A", Some("Podcasts / Others"))
        .unwrap();
    let f2 = db
        .add_feed("https://example.com/b", "Feed B", Some("News / Others"))
        .unwrap();
    let conn = raw(path);
    conn.execute(
        "INSERT INTO articles (id, feed_id, title, url) VALUES ('art1', ?1, 'T', 'https://example.com/1')",
        rusqlite::params![f1.id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO filter_rules (id, name, rule_json, actions_json, enabled, stop, position) \
         VALUES ('rule1', 'R', '{}', '{\"move\": \"Podcasts / Others\", \"label\": \"News / Others\"}', 1, 0, 0)",
        [],
    )
    .unwrap();
    drop(conn);
    db.set_article_category_override("art1", Some("Podcasts / Others"))
        .unwrap();
    db.add_article_label("art1", "Podcasts / Others").unwrap();
    db.set_feed_delete_behavior(&f2.id, Some("category:Podcasts / Others"))
        .unwrap();
    (f1.id, f2.id)
}

#[test]
fn ensure_path_uses_spaced_separator_and_parent_links() {
    let (db, path) = scratch_db("ensure");
    let leaf_id = db.ensure_category_path("Podcasts / Others").unwrap();
    assert_eq!(titles(&db), vec!["Podcasts", "Podcasts / Others"]);
    let cats = db.categories().unwrap();
    let parent = cats.iter().find(|c| c.title == "Podcasts").unwrap();
    let leaf = cats.iter().find(|c| c.id == leaf_id).unwrap();
    assert_eq!(leaf.parent_id.as_deref(), Some(parent.id.as_str()));
    // Bare "/" inside a leaf is not a path separator in Python.
    db.ensure_category_path("AC/DC Fans").unwrap();
    assert!(titles(&db).contains(&"AC/DC Fans".to_string()));
    std::fs::remove_file(&path).ok();
}

#[test]
fn membership_and_tree_queries_see_python_children() {
    let (db, path) = scratch_db("member");
    let (f1, _f2) = seed(&db, &path);
    // Feed A sits in "Podcasts / Others": the parent view and the
    // exact view must both include it, and the subtree feed query
    // used by delete-with-feeds must find it.
    let in_parent: Vec<String> = db
        .articles_for_category("Podcasts", 10)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert!(in_parent.contains(&"art1".to_string()), "{in_parent:?}");
    let tree = db.feeds_in_category_tree("Podcasts").unwrap();
    assert!(tree.iter().any(|f| f.id == f1), "{tree:?}");
    std::fs::remove_file(&path).ok();
}

#[test]
fn rename_rewrites_every_reference_and_parent_id_holds() {
    let (db, path) = scratch_db("rename");
    let (_f1, f2) = seed(&db, &path);
    // Python rename_category takes the new LEAF; the node keeps
    // its parent, descendants keep their tails.
    db.rename_category("Podcasts / Others", "Others Renamed")
        .unwrap();
    assert_eq!(
        titles(&db),
        vec![
            "News",
            "News / Others",
            "Podcasts",
            "Podcasts / Others Renamed",
            "Podcasts / Others Renamed / Deep"
        ]
    );
    let cats = db.categories().unwrap();
    let parent = cats.iter().find(|c| c.title == "Podcasts").unwrap();
    let renamed = cats
        .iter()
        .find(|c| c.title == "Podcasts / Others Renamed")
        .unwrap();
    assert_eq!(renamed.parent_id.as_deref(), Some(parent.id.as_str()));
    let feeds = db.feeds().unwrap();
    assert_eq!(
        feeds
            .iter()
            .find(|f| f.title == "Feed A")
            .unwrap()
            .category
            .as_deref(),
        Some("Podcasts / Others Renamed")
    );
    // The identically named leaf under News is untouched.
    assert_eq!(
        feeds
            .iter()
            .find(|f| f.title == "Feed B")
            .unwrap()
            .category
            .as_deref(),
        Some("News / Others")
    );
    let conn = raw(&path);
    let over: Option<String> = conn
        .query_row(
            "SELECT category_override FROM articles WHERE id = 'art1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(over.as_deref(), Some("Podcasts / Others Renamed"));
    let actions: String = conn
        .query_row(
            "SELECT actions_json FROM filter_rules WHERE id = 'rule1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&actions).unwrap();
    assert_eq!(v["move"], "Podcasts / Others Renamed");
    assert_eq!(v["label"], "News / Others");
    drop(conn);
    assert_eq!(
        db.article_ids_with_label("Podcasts / Others Renamed")
            .unwrap(),
        vec!["art1".to_string()]
    );
    assert!(
        db.article_ids_with_label("Podcasts / Others")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.feed_delete_behavior(&f2).unwrap().as_deref(),
        Some("category:Podcasts / Others Renamed")
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn rename_failure_rolls_back_everything() {
    let (db, path) = scratch_db("rollback");
    seed(&db, &path);
    // Renaming "Podcasts / Others" to the leaf "Others" under News
    // is not this call; renaming Podcasts -> "News" collides with
    // the existing top-level News category: Python refuses, and no
    // reference may move.
    let r = db.rename_category("Podcasts", "News");
    assert!(r.is_err() || titles(&db).contains(&"Podcasts".to_string()));
    assert_eq!(
        titles(&db),
        vec![
            "News",
            "News / Others",
            "Podcasts",
            "Podcasts / Others",
            "Podcasts / Others / Deep"
        ]
    );
    let conn = raw(&path);
    let over: Option<String> = conn
        .query_row(
            "SELECT category_override FROM articles WHERE id = 'art1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(over.as_deref(), Some("Podcasts / Others"));
    drop(conn);
    std::fs::remove_file(&path).ok();
}

#[test]
fn move_category_reparents_and_rewrites_subtree() {
    let (db, path) = scratch_db("move");
    let (_f1, _f2) = seed(&db, &path);
    // Moving "Podcasts / Others" under News would collide with
    // the existing "News / Others": Python refuses, nothing moves.
    assert!(!db.move_category("Podcasts / Others", Some("News")).unwrap());
    assert!(titles(&db).contains(&"Podcasts / Others".to_string()));
    // A category cannot move under itself or its own descendant.
    assert!(
        !db.move_category("Podcasts", Some("Podcasts / Others"))
            .unwrap()
    );
    // Deep moves under News: only its own parent link changes,
    // and references to its old path follow.
    let conn = raw(&path);
    conn.execute(
        "UPDATE articles SET category_override = 'Podcasts / Others / Deep' WHERE id = 'art1'",
        [],
    )
    .unwrap();
    drop(conn);
    assert!(
        db.move_category("Podcasts / Others / Deep", Some("News"))
            .unwrap()
    );
    assert_eq!(
        titles(&db),
        vec![
            "News",
            "News / Deep",
            "News / Others",
            "Podcasts",
            "Podcasts / Others"
        ]
    );
    let cats = db.categories().unwrap();
    let news = cats.iter().find(|c| c.title == "News").unwrap();
    let deep = cats.iter().find(|c| c.title == "News / Deep").unwrap();
    assert_eq!(deep.parent_id.as_deref(), Some(news.id.as_str()));
    let conn = raw(&path);
    let over: Option<String> = conn
        .query_row(
            "SELECT category_override FROM articles WHERE id = 'art1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(over.as_deref(), Some("News / Deep"));
    drop(conn);
    std::fs::remove_file(&path).ok();
}

#[test]
fn delete_reparents_children_and_rewrites_references() {
    let (db, path) = scratch_db("delete");
    let (_f1, f2) = seed(&db, &path);
    assert!(db.delete_category_path("Podcasts / Others").unwrap());
    // Child "Deep" is reparented to Podcasts with a shortened path.
    assert_eq!(
        titles(&db),
        vec!["News", "News / Others", "Podcasts", "Podcasts / Deep"]
    );
    let cats = db.categories().unwrap();
    let podcasts = cats.iter().find(|c| c.title == "Podcasts").unwrap();
    let deep = cats.iter().find(|c| c.title == "Podcasts / Deep").unwrap();
    assert_eq!(deep.parent_id.as_deref(), Some(podcasts.id.as_str()));
    // Feeds directly in the deleted category fall to Uncategorized
    // (Python), and references follow the same mapping.
    let feeds = db.feeds().unwrap();
    assert_eq!(
        feeds
            .iter()
            .find(|f| f.title == "Feed A")
            .unwrap()
            .category
            .as_deref(),
        Some("Uncategorized")
    );
    let conn = raw(&path);
    let over: Option<String> = conn
        .query_row(
            "SELECT category_override FROM articles WHERE id = 'art1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(over.as_deref(), Some("Uncategorized"));
    drop(conn);
    assert_eq!(
        db.article_ids_with_label("Uncategorized").unwrap(),
        vec!["art1".to_string()]
    );
    assert_eq!(
        db.feed_delete_behavior(&f2).unwrap().as_deref(),
        Some("category:Uncategorized")
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn local_provider_protects_the_sentinel() {
    // D11: the capability defaults to protected; only Miniflux's
    // real category steps aside (its provider-level override is
    // covered in hosted_gui_slice.rs).
    let (_db, path) = scratch_db("cap");
    let local = blindrss_core::provider::get_provider(
        &blindrss_core::config::Config::default(),
        path.clone(),
    );
    assert!(!local.uncategorized_is_real_category());
    let mut cfg = blindrss_core::config::Config::default();
    cfg.set_path("active_provider", serde_json::json!("miniflux"));
    let miniflux = blindrss_core::provider::get_provider(&cfg, path.clone());
    assert!(miniflux.uncategorized_is_real_category());
    assert!(blindrss_core::provider::uncategorized_is_real_category(
        &cfg
    ));
    std::fs::remove_file(&path).ok();
}
