//! Audit 8 Task 4 regressions: delete/restore state fidelity
//! (D14), filter-delete behavior resolution (D12), notification
//! accounting (D13), rule/smart-folder evaluation past row
//! 100,000 (D21) and Deleted Articles purge/restore core
//! operations (G07). Python references: core/filters.py,
//! core/db.py remember/restore/purge deleted article,
//! providers/local.py rule application.

use blindrss_core::rules::{DeleteBehavior, parse_delete_behavior, resolve_effective_actions};
use blindrss_core::{Article, Db, Feed, ParsedEntry, ParsedFeed};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> (Db, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "blindrss_a8t4_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    (Db::open_or_create(&path).unwrap(), path)
}

fn article(id: &str, feed_id: &str) -> Article {
    Article {
        id: id.into(),
        feed_id: feed_id.into(),
        title: format!("Title {id}"),
        url: format!("https://example.com/{id}"),
        content: Some("body".into()),
        description: Some("desc".into()),
        date: Some("2026-01-01".into()),
        author: Some("Author".into()),
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    }
}

fn parsed(entries: Vec<(&str, &str)>) -> ParsedFeed {
    ParsedFeed {
        title: "Feed".into(),
        entries: entries
            .into_iter()
            .map(|(id, title)| ParsedEntry {
                id: id.into(),
                title: title.into(),
                url: format!("https://example.com/{id}"),
                content_text: "body".into(),
                summary: "desc".into(),
                date: Some("2026-01-01".into()),
                author: Some("Author".into()),
                media_url: None,
                media_type: None,
                tags: Vec::new(),
                duration: None,
            })
            .collect(),
    }
}

fn raw(path: &PathBuf) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

// ── D14: tombstone snapshots keep read/favorite ─────────────────

#[test]
fn tombstone_preserves_read_and_favorite_through_restore() {
    let (db, path) = scratch("flags");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    let mut a = article("a1", &feed.id);
    a.is_read = true;
    a.is_favorite = true;
    db.insert_article_if_new(&a).unwrap();

    assert!(db.delete_article("a1").unwrap());
    let deleted = db.deleted_articles().unwrap();
    assert_eq!(deleted.len(), 1);
    assert!(
        deleted[0].is_read && deleted[0].is_favorite,
        "tombstone snapshot must carry read/favorite (D14)"
    );

    assert!(db.restore_article("a1").unwrap());
    let back = db.article_by_id("a1").unwrap().unwrap();
    assert!(back.is_read && back.is_favorite);
    std::fs::remove_file(&path).ok();
}

#[test]
fn python_origin_tombstone_restores_its_flags() {
    let (db, path) = scratch("pyflags");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    drop(db);
    // A tombstone written by the Python app (snapshot columns,
    // flags set), plus one legacy row whose flags are NULL.
    let conn = raw(&path);
    conn.execute(
        "INSERT INTO deleted_articles (feed_id, article_id, url, deleted_at, \
         title, content, description, date, author, media_url, media_type, \
         chapter_url, is_read, is_favorite, purged) VALUES \
         (?1, 'py1', 'https://example.com/py1', 100.0, 'Py', 'b', 'd', \
          '2026-01-01', 'A', NULL, NULL, NULL, 1, 1, 0)",
        rusqlite::params![feed.id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO deleted_articles (feed_id, article_id, url, deleted_at, \
         title, is_read, is_favorite, purged) VALUES \
         (?1, 'py2', 'https://example.com/py2', 99.0, 'Legacy', NULL, NULL, 0)",
        rusqlite::params![feed.id],
    )
    .unwrap();
    drop(conn);

    let db = Db::open(&path).unwrap();
    let listed = db.deleted_articles().unwrap();
    assert_eq!(listed.len(), 2);
    let py1 = listed.iter().find(|a| a.id == "py1").unwrap();
    assert!(py1.is_read && py1.is_favorite);
    assert!(db.restore_article("py1").unwrap());
    let back = db.article_by_id("py1").unwrap().unwrap();
    assert!(back.is_read && back.is_favorite);
    // NULL flags degrade to unread / not favorite, never an error.
    assert!(db.restore_article("py2").unwrap());
    let back = db.article_by_id("py2").unwrap().unwrap();
    assert!(!back.is_read && !back.is_favorite);
    std::fs::remove_file(&path).ok();
}

#[test]
fn restore_refuses_purged_and_ambiguous_tombstones() {
    let (db, path) = scratch("refuse");
    let f1 = db.add_feed("https://example.com/f1", "One", None).unwrap();
    let f2 = db.add_feed("https://example.com/f2", "Two", None).unwrap();
    // Purged: restore must fail and the tombstone must survive.
    db.insert_article_if_new(&article("p1", &f1.id)).unwrap();
    db.purge_article("p1").unwrap();
    assert!(!db.restore_article("p1").unwrap());
    assert!(db.deleted_article_ids().unwrap().contains("p1"));

    // Same article id tombstoned under two feeds: the unscoped
    // restore is ambiguous (Python returns None); the scoped one
    // restores exactly its own row.
    db.insert_article_if_new(&article("dup", &f1.id)).unwrap();
    assert!(db.delete_article("dup").unwrap());
    let conn = raw(&path);
    conn.execute(
        "INSERT INTO deleted_articles (feed_id, article_id, url, deleted_at, title) \
         VALUES (?1, 'dup', 'https://example.com/dup', 50.0, 'Other feed copy')",
        rusqlite::params![f2.id],
    )
    .unwrap();
    drop(conn);
    assert!(!db.restore_article("dup").unwrap());
    assert_eq!(db.deleted_articles().unwrap().len(), 1 + 1);
    assert!(db.restore_deleted_article(&f2.id, "dup").unwrap());
    assert!(!db.restore_deleted_article(&f2.id, "dup").unwrap());
    assert!(db.restore_deleted_article(&f1.id, "dup").unwrap());
    std::fs::remove_file(&path).ok();
}

// ── G07 core: confirmed permanent purge of a tombstone ─────────

#[test]
fn purge_deleted_article_nulls_snapshot_but_keeps_tombstone() {
    let (db, path) = scratch("purge_tomb");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    db.insert_article_if_new(&article("a1", &feed.id)).unwrap();
    assert!(db.delete_article("a1").unwrap());
    assert!(db.purge_deleted_article(&feed.id, "a1").unwrap());
    // Gone from the Deleted view, still tombstoned against refresh.
    assert!(db.deleted_articles().unwrap().is_empty());
    assert!(db.deleted_article_ids().unwrap().contains("a1"));
    let conn = raw(&path);
    let (purged, title): (i64, Option<String>) = conn
        .query_row(
            "SELECT purged, title FROM deleted_articles WHERE article_id = 'a1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(purged, 1);
    assert!(title.is_none(), "purge drops the snapshot content");
    drop(conn);
    // Second purge is a no-op.
    assert!(!db.purge_deleted_article(&feed.id, "a1").unwrap());
    std::fs::remove_file(&path).ok();
}

// ── D12: filter delete resolves the configured behavior ────────

fn delete_rule(db: &Db, needle: &str, actions: serde_json::Value) {
    db.create_filter_rule(
        "deleter",
        &json!({"match":"all","conditions":[{"field":"title","op":"contains","value":needle}]})
            .to_string(),
        &actions.to_string(),
        true,
        false,
    )
    .unwrap();
}

#[test]
fn parse_and_resolve_delete_behavior_matches_python() {
    assert_eq!(parse_delete_behavior(""), DeleteBehavior::Deleted);
    assert_eq!(parse_delete_behavior("purge"), DeleteBehavior::Purge);
    assert_eq!(parse_delete_behavior(" PURGE "), DeleteBehavior::Purge);
    assert_eq!(parse_delete_behavior("deleted"), DeleteBehavior::Deleted);
    assert_eq!(
        parse_delete_behavior("category: Tech Stuff "),
        DeleteBehavior::Category("Tech Stuff".into())
    );
    assert_eq!(parse_delete_behavior("category:"), DeleteBehavior::Deleted);
    assert_eq!(parse_delete_behavior("bogus"), DeleteBehavior::Deleted);

    let agg = blindrss_core::rules::EffectiveActions {
        delete: true,
        move_to: Some("Elsewhere".into()),
        ..Default::default()
    };
    // Delete-to-category beats an explicit move (Python precedence).
    let eff = resolve_effective_actions(&agg, &DeleteBehavior::Category("Filed".into()));
    assert_eq!(eff.move_to.as_deref(), Some("Filed"));
    assert!(!eff.remove && !eff.purge);
    let eff = resolve_effective_actions(&agg, &DeleteBehavior::Purge);
    assert!(eff.purge && !eff.remove);
    let eff = resolve_effective_actions(&agg, &DeleteBehavior::Deleted);
    assert!(eff.remove && !eff.purge);
    // No delete action: nothing is removed, the move survives.
    let agg = blindrss_core::rules::EffectiveActions {
        move_to: Some("Elsewhere".into()),
        ..Default::default()
    };
    let eff = resolve_effective_actions(&agg, &DeleteBehavior::Purge);
    assert!(!eff.remove && !eff.purge);
    assert_eq!(eff.move_to.as_deref(), Some("Elsewhere"));
}

#[test]
fn filter_delete_with_purge_behavior_does_not_soft_delete() {
    let (db, path) = scratch("d12purge");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    delete_rule(&db, "spam", json!({"delete": true}));
    let outcome = blindrss_core::refresh::store_parsed(
        &db,
        &feed,
        &parsed(vec![("s1", "Spam offer"), ("k1", "Keep me")]),
        "purge",
    )
    .unwrap();
    assert_eq!(outcome.inserted, 2);
    assert!(db.article_by_id("s1").unwrap().is_none());
    // Purged: NOT in the Deleted Articles view, still tombstoned.
    assert!(db.deleted_articles().unwrap().is_empty());
    assert!(db.deleted_article_ids().unwrap().contains("s1"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn filter_delete_with_category_behavior_moves_instead_of_deleting() {
    let (db, path) = scratch("d12cat");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    // The rule both moves (to Elsewhere) and deletes; the
    // configured delete-to-category target must win.
    delete_rule(&db, "spam", json!({"delete": true, "move": "Elsewhere"}));
    let outcome = blindrss_core::refresh::store_parsed(
        &db,
        &feed,
        &parsed(vec![("s1", "Spam offer")]),
        "category:Tech Stuff",
    )
    .unwrap();
    assert_eq!(outcome.inserted, 1);
    let kept = db.article_by_id("s1").unwrap();
    assert!(kept.is_some(), "delete-to-category keeps the article row");
    let in_target = db.articles_for_category("Tech Stuff", 10).unwrap();
    assert!(in_target.iter().any(|a| a.id == "s1"));
    let in_move = db.articles_for_category("Elsewhere", 10).unwrap();
    assert!(!in_move.iter().any(|a| a.id == "s1"));
    assert!(db.deleted_articles().unwrap().is_empty());
    std::fs::remove_file(&path).ok();
}

#[test]
fn filter_delete_feed_override_beats_global() {
    let (db, path) = scratch("d12override");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    db.set_feed_delete_behavior(&feed.id, Some("deleted"))
        .unwrap();
    delete_rule(&db, "spam", json!({"delete": true}));
    blindrss_core::refresh::store_parsed(
        &db,
        &feed,
        &parsed(vec![("s1", "Spam offer")]),
        "purge", // global says purge; the feed override says soft delete
    )
    .unwrap();
    assert_eq!(
        db.deleted_articles().unwrap().len(),
        1,
        "per-feed override must win over the global behavior"
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn apply_rules_to_existing_honors_delete_behavior() {
    let (db, path) = scratch("d12existing");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    db.insert_article_if_new(&article("s1", &feed.id)).unwrap();
    // insert_article_if_new kept the first copy; retitle directly.
    let conn = raw(&path);
    conn.execute(
        "UPDATE articles SET title = 'Spam offer' WHERE id = 's1'",
        [],
    )
    .unwrap();
    drop(conn);
    delete_rule(&db, "spam", json!({"delete": true}));
    let changed = blindrss_core::refresh::apply_rules_to_existing(&db, "purge").unwrap();
    assert_eq!(changed, 1);
    assert!(db.deleted_articles().unwrap().is_empty());
    assert!(db.deleted_article_ids().unwrap().contains("s1"));
    std::fs::remove_file(&path).ok();
}

// ── D13: notification accounting ────────────────────────────────

#[test]
fn store_outcome_excludes_deleted_and_notification_suppressed() {
    let (db, path) = scratch("d13");
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    db.create_filter_rule(
        "quiet",
        &json!({"match":"all","conditions":[{"field":"title","op":"contains","value":"quiet"}]})
            .to_string(),
        &json!({"skip_notification": true}).to_string(),
        true,
        false,
    )
    .unwrap();
    delete_rule(&db, "spam", json!({"delete": true}));
    let outcome = blindrss_core::refresh::store_parsed(
        &db,
        &feed,
        &parsed(vec![
            ("n1", "Hello world"),
            ("n2", "A quiet update"),
            ("n3", "Spam deal"),
        ]),
        "deleted",
    )
    .unwrap();
    assert_eq!(outcome.inserted, 3);
    assert_eq!(
        outcome.notify_count, 1,
        "suppressed and rule-deleted articles are not notification-eligible"
    );
    assert_eq!(outcome.new_articles.len(), 1);
    assert_eq!(outcome.new_articles[0].id, "n1");
    // The suppressed article is stored; only its notification is skipped.
    assert!(db.article_by_id("n2").unwrap().is_some());
    std::fs::remove_file(&path).ok();
}

// ── D21: evaluation reaches beyond row 100,000 ─────────────────

fn big_library(tag: &str) -> (Db, PathBuf, Feed) {
    let (db, path) = scratch(tag);
    let feed = db.add_feed("https://example.com/big", "Big", None).unwrap();
    drop(db);
    let conn = raw(&path);
    conn.execute_batch(&format!(
        "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq \
         WHERE n < 100001) \
         INSERT INTO articles (id, feed_id, title, url, content, description, \
         date, author, is_read, is_favorite) \
         SELECT 'bulk-' || n, '{fid}', 'Bulk item', \
         'https://example.com/' || n, '', '', '2020-01-01', '', 0, 0 FROM seq;",
        fid = feed.id
    ))
    .unwrap();
    // The needle sorts last (oldest date): row 100,002 overall.
    conn.execute(
        "INSERT INTO articles (id, feed_id, title, url, content, description, \
         date, author, is_read, is_favorite) VALUES \
         ('needle', ?1, 'Needle post', 'https://example.com/needle', '', '', \
          '2019-01-01', '', 0, 0)",
        rusqlite::params![feed.id],
    )
    .unwrap();
    drop(conn);
    (Db::open(&path).unwrap(), path, feed)
}

#[test]
fn apply_rules_to_existing_reaches_beyond_100k_rows() {
    let (db, path, _feed) = big_library("d21apply");
    delete_rule(&db, "needle", json!({"delete": true}));
    let changed = blindrss_core::refresh::apply_rules_to_existing(&db, "deleted").unwrap();
    assert_eq!(changed, 1);
    assert!(
        db.deleted_article_ids().unwrap().contains("needle"),
        "the sole match beyond row 100,000 must be evaluated (D21)"
    );
    assert_eq!(db.articles_count(None).unwrap(), 100_001);
    std::fs::remove_file(&path).ok();
}

#[test]
fn smart_folder_matching_reaches_beyond_100k_rows() {
    let (db, path, _feed) = big_library("d21smart");
    let rule = json!({"match":"all","conditions":[
        {"field":"title","op":"contains","value":"needle"}
    ]});
    let matched = db.articles_matching_rule(&rule).unwrap();
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].id, "needle");
    std::fs::remove_file(&path).ok();
}
