//! Batch 32 (v0.1.33): schema-init race regression. The
//! v0.1.30 tag CI failed on Ubuntu with "duplicate column
//! name: feed_settings": init_schema checked PRAGMA
//! table_info and then ALTERed outside any transaction, so
//! two connections opening a not-yet-migrated database at
//! the same time both saw the column missing and both tried
//! to add it. These tests pin the fix: concurrent opens of
//! one fresh file, and repeated opens, must all succeed.

use blindrss_core::db::Db;

fn unique_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn fresh_path(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("blindrss-batch32-{tag}-{}", unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("rss.db")
}

#[test]
fn concurrent_opens_of_one_fresh_db_all_succeed() {
    let path = fresh_path("race");
    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = path.clone();
        handles.push(std::thread::spawn(move || {
            Db::open_or_create(&p).map(|_| ())
        }));
    }
    for h in handles {
        let result = h.join().unwrap();
        assert!(result.is_ok(), "concurrent open failed: {result:?}");
    }
    // And the migrated schema is actually usable afterwards.
    let db = Db::open(&path).unwrap();
    assert!(db.categories().unwrap().is_empty());
}

#[test]
fn repeated_opens_are_idempotent() {
    let path = fresh_path("twice");
    for _ in 0..3 {
        let db = Db::open_or_create(&path).unwrap();
        drop(db);
    }
    let db = Db::open(&path).unwrap();
    assert!(db.feeds().unwrap().is_empty());
}

#[test]
fn concurrent_ensure_category_path_is_idempotent() {
    // The v0.1.33 macOS release run failed here: two openers
    // ensuring the same category path raced SELECT-then-
    // INSERT and one died on the UNIQUE title constraint.
    let path = fresh_path("cats");
    let db = Db::open_or_create(&path).unwrap();
    drop(db);
    let mut handles = Vec::new();
    for _ in 0..6 {
        let p = path.clone();
        handles.push(std::thread::spawn(move || {
            let db = Db::open(&p).unwrap();
            db.ensure_category_path("News / Tech")
        }));
    }
    let mut ids = Vec::new();
    for h in handles {
        ids.push(h.join().unwrap().unwrap());
    }
    assert!(ids.iter().all(|i| i == &ids[0]), "ids diverged: {ids:?}");
    let db = Db::open(&path).unwrap();
    let cats = db.categories().unwrap();
    assert_eq!(cats.iter().filter(|c| c.title == "News / Tech").count(), 1);
    assert_eq!(cats.iter().filter(|c| c.title == "News").count(), 1);
}
