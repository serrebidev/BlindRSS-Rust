//! Batch 1 (full-transfer drive, 2026-10-30): fresh-install schema,
//! category hierarchy ops, feed error tracking, staged restore.

use blindrss_core::db::Db;
use std::io::Write;

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn fresh_database_opens_empty_and_valid() {
    let path = scratch("blindrss_batch1_fresh.db");
    let db = Db::open_or_create(&path).unwrap();
    assert!(db.feeds().unwrap().is_empty());
    assert!(db.categories().unwrap().is_empty());
    // Strict open also works now that the file exists.
    assert!(Db::open(&path).is_ok());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn category_paths_and_delete_reparenting() {
    let path = scratch("blindrss_batch1_cats.db");
    let db = Db::open_or_create(&path).unwrap();
    db.ensure_category_path("News / Tech").unwrap();
    db.ensure_category_path("News / World").unwrap();
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles, vec!["News", "News / Tech", "News / World"]);
    // A feed living in News / Tech, one directly in News.
    let f1 = db
        .add_feed("https://example.com/f1", "f1", Some("News / Tech"))
        .unwrap();
    db.add_feed("https://example.com/f2", "f2", Some("News"))
        .unwrap();
    let f1 = f1.id;
    // Deleting News / Tech sends f1 to Uncategorized (Python
    // delete_category: feeds in the deleted category fall back
    // to the sentinel, not up to the parent).
    assert!(db.delete_category_path("News / Tech").unwrap());
    let feeds = db.feeds().unwrap();
    assert_eq!(
        feeds
            .iter()
            .find(|f| f.id == f1)
            .unwrap()
            .category
            .as_deref(),
        Some("Uncategorized")
    );
    // Deleting News sends its feeds to Uncategorized too and
    // reparents News / World to the root as "World".
    assert!(db.delete_category_path("News").unwrap());
    let feeds = db.feeds().unwrap();
    assert_eq!(
        feeds
            .iter()
            .find(|f| f.id == f1)
            .unwrap()
            .category
            .as_deref(),
        Some("Uncategorized")
    );
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles, vec!["World"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn rename_category_moves_the_subtree() {
    let path = scratch("blindrss_batch1_rename.db");
    let db = Db::open_or_create(&path).unwrap();
    db.ensure_category_path("Old / Sub").unwrap();
    db.rename_category("Old", "New").unwrap();
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles, vec!["New", "New / Sub"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn feed_error_lifecycle() {
    let path = scratch("blindrss_batch1_errors.db");
    let db = Db::open_or_create(&path).unwrap();
    let fx = db.add_feed("https://example.com/fx", "FX", None).unwrap();
    db.record_feed_error(&fx.id, "HTTP 404").unwrap();
    db.record_feed_error(&fx.id, "HTTP 404 again").unwrap();
    let errors = db.feed_errors().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, "HTTP 404 again");
    assert_eq!(errors[0].consecutive_failures, 2);
    db.clear_feed_error(&fx.id).unwrap();
    assert!(db.feed_errors().unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn staged_restore_applies_at_start() {
    // Build a tiny database, back it up, stage the restore into a
    // fresh data dir, and confirm apply_pending_restore moves it in.
    let src = scratch("blindrss_batch1_stage_src.db");
    let db = Db::open_or_create(&src).unwrap();
    db.add_feed("https://example.com/s1", "S1", None).unwrap();
    drop(db);
    let dir = std::env::temp_dir().join("blindrss_batch1_stage_dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let zip_path = dir.join("backup.zip");
    {
        let mut cfgs = std::fs::File::create(dir.join("config.json")).unwrap();
        cfgs.write_all(b"{}").unwrap();
    }
    blindrss_core::maintenance::create_backup(&src, &dir.join("config.json"), &dir, &zip_path)
        .unwrap();
    blindrss_core::maintenance::stage_restore(&zip_path, &dir).unwrap();
    assert!(dir.join("restore_pending/rss.db").exists());
    assert!(blindrss_core::maintenance::apply_pending_restore(
        &dir,
        &dir.join("rss.db")
    ));
    assert!(dir.join("rss.db").exists());
    assert!(!dir.join("restore_pending").exists());
    let db = Db::open(&dir.join("rss.db")).unwrap();
    assert_eq!(db.feeds().unwrap().len(), 1);
    drop(db);
    // A second restore must apply too: the first one left a
    // rss.db.pre-restore behind, and deleting the staged copy before
    // a successful swap lost restores silently.
    blindrss_core::maintenance::stage_restore(&zip_path, &dir).unwrap();
    assert!(blindrss_core::maintenance::apply_pending_restore(
        &dir,
        &dir.join("rss.db")
    ));
    assert!(dir.join("rss.db.pre-restore").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&src);
}

const NESTED_OPML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0"><head><title>t</title></head><body>
<outline text="News"><outline text="Tech">
<outline text="Example" type="rss" xmlUrl="https://example.com/feed.xml"/>
</outline></outline>
<outline text="Solo" type="rss" xmlUrl="https://example.com/solo.xml"/>
</body></opml>"#;

#[test]
fn opml_import_builds_full_category_paths() {
    let path = scratch("blindrss_batch1_opml.db");
    let db = Db::open_or_create(&path).unwrap();
    let n = blindrss_core::opml::import_opml(&db, NESTED_OPML.as_bytes()).unwrap();
    assert_eq!(n, 2);
    let feeds = db.feeds().unwrap();
    let nested = feeds.iter().find(|f| f.title == "Example").unwrap();
    assert_eq!(nested.category.as_deref(), Some("News / Tech"));
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles, vec!["News", "News / Tech"]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn opml_import_under_prefixes_and_export_nests() {
    let path = scratch("blindrss_batch1_opml2.db");
    let db = Db::open_or_create(&path).unwrap();
    let n = blindrss_core::opml::import_opml_under(&db, NESTED_OPML.as_bytes(), Some("Imports"))
        .unwrap();
    assert_eq!(n, 2);
    let feeds = db.feeds().unwrap();
    let nested = feeds.iter().find(|f| f.title == "Example").unwrap();
    assert_eq!(nested.category.as_deref(), Some("Imports / News / Tech"));
    let solo = feeds.iter().find(|f| f.title == "Solo").unwrap();
    assert_eq!(solo.category.as_deref(), Some("Imports"));
    // Export nests the path as an outline hierarchy that the
    // importer reads back to the same full path.
    let xml = blindrss_core::opml::export_opml_feeds(&feeds).unwrap();
    let path2 = scratch("blindrss_batch1_opml3.db");
    let db2 = Db::open_or_create(&path2).unwrap();
    blindrss_core::opml::import_opml(&db2, xml.as_bytes()).unwrap();
    let feeds2 = db2.feeds().unwrap();
    let nested2 = feeds2.iter().find(|f| f.title == "Example").unwrap();
    assert_eq!(nested2.category.as_deref(), Some("Imports / News / Tech"));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
}
