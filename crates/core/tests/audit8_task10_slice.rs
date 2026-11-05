//! Audit 8 Task 10: YouTube merged state and built-in favorites
//! (D17, D18, D19, D20, G06 core side).
//!
//! Python's spec is `providers/youtube_account.py` (the local
//! wrapper) over `core/youtube_account.py` (the snapshot store):
//! a recommendation and the channel-feed article for the same
//! video (`yt:video:<id>`) share read state in both directions;
//! a recommendation keeps the date it was FIRST recommended in
//! views that mix it with RSS articles, while the dedicated
//! Recommendations view re-dates rows down from the newest
//! stored date in YouTube's order; merged views include current
//! recommendations, the Favorites view also includes favorites
//! that have fallen out of the latest snapshot; Mark All Read
//! persists to the recommendations table (and its twins).

use blindrss_core::{Db, ParsedEntry, ParsedFeed, store_parsed};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_db(tag: &str) -> (Db, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "blindrss-a8t10-{tag}-{}-{nanos}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rss.db");
    std::fs::write(&path, b"").unwrap();
    (Db::open(&path).unwrap(), path)
}

const ACCOUNT_TOKEN: &str = "refresh-token-task10";

fn account() -> String {
    blindrss_core::youtube_account::account_key(ACCOUNT_TOKEN)
}

fn videos(ids: &[&str]) -> Vec<(String, String, String)> {
    ids.iter()
        .map(|id| (id.to_string(), format!("Video {id}"), "Channel".to_string()))
        .collect()
}

/// Store a channel-feed article for a video, under the id
/// YouTube's RSS gives it (`yt:video:<id>`), as Python's local
/// provider does.
fn store_twin(db: &Db, video_id: &str) {
    let feed = db
        .add_feed(
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCtask10",
            "Channel",
            Some("YouTube"),
        )
        .unwrap();
    let parsed = ParsedFeed {
        title: "Channel".into(),
        entries: vec![ParsedEntry {
            id: format!("yt:video:{video_id}"),
            title: format!("Video {video_id}"),
            url: format!("https://www.youtube.com/watch?v={video_id}"),
            content_text: String::new(),
            summary: String::new(),
            date: None,
            author: None,
            media_url: None,
            media_type: None,
            tags: Vec::new(),
            duration: None,
        }],
    };
    // add_feed returns the existing feed on a second call, so
    // repeated twins share it; store_parsed is idempotent here.
    store_parsed(db, &feed, &parsed, "deleted").unwrap();
}

const A: &str = "vid00000001";
const B: &str = "vid00000002";
const C: &str = "vid00000003";

#[test]
fn d18_snapshot_keeps_first_recommended_dates() {
    let (db, _path) = scratch_db("d18");
    let account = account();
    db.save_recommendations(&account, &videos(&[A, B])).unwrap();
    let first: Vec<(String, Option<String>)> = db
        .recommendations_merged(&account)
        .unwrap()
        .iter()
        .map(|a| (a.id.clone(), a.date.clone()))
        .collect();
    assert_eq!(first.len(), 2);
    // A later snapshot (reordered, one new video) must not
    // re-date the videos it already knew: Python's upsert
    // updates title/author/position/current, never date, so
    // still-recommended videos do not jump to the top of mixed
    // views as if new.
    db.save_recommendations(&account, &videos(&[C, B, A]))
        .unwrap();
    let merged = db.recommendations_merged(&account).unwrap();
    assert_eq!(merged.len(), 3);
    // Merged order follows YouTube's current order (position).
    assert!(merged[0].id.ends_with(C));
    assert!(merged[1].id.ends_with(B));
    assert!(merged[2].id.ends_with(A));
    for (id, date) in &first {
        let now = merged.iter().find(|a| &a.id == id).unwrap();
        assert_eq!(&now.date, date, "date churned for {id}");
    }
}

#[test]
fn d18_dedicated_view_redates_from_newest_by_position() {
    let (db, _path) = scratch_db("d18b");
    let account = account();
    // A is recommended first; a later snapshot adds B above it.
    // A's stored (first-seen) date stays the older one.
    db.save_recommendations(&account, &videos(&[A])).unwrap();
    db.save_recommendations(&account, &videos(&[B, A])).unwrap();
    let merged = db.recommendations_merged(&account).unwrap();
    let stored_a = merged.iter().find(|a| a.id.ends_with(A)).unwrap();
    let stored_b = merged.iter().find(|a| a.id.ends_with(B)).unwrap();
    assert!(stored_b.date > stored_a.date);
    // The dedicated view dates rows down from the newest stored
    // date by position microseconds (Python's wrapper), so the
    // list's date sort reproduces YouTube's order exactly and
    // the displayed dates cluster at the newest.
    let dedicated = db.recommendations(&account).unwrap();
    assert_eq!(dedicated.len(), 2);
    assert!(dedicated[0].id.ends_with(B));
    assert!(dedicated[1].id.ends_with(A));
    assert_eq!(dedicated[0].date, stored_b.date);
    assert!(dedicated[1].date < dedicated[0].date);
    // One position step = one microsecond below the newest.
    let frac = |d: &Option<String>| -> u64 {
        let d = d.as_deref().unwrap();
        d.rsplit('.')
            .next()
            .unwrap()
            .trim_end_matches("+00:00")
            .parse()
            .unwrap()
    };
    assert_eq!(frac(&dedicated[0].date) - frac(&dedicated[1].date), 1);
}

#[test]
fn d17_feed_article_read_state_shows_on_recommendation() {
    let (db, _path) = scratch_db("d17");
    let account = account();
    store_twin(&db, A);
    db.save_recommendations(&account, &videos(&[A, B])).unwrap();
    let recs = db.recommendations(&account).unwrap();
    assert!(!recs[0].is_read);
    // Reading the channel-feed copy reads the recommendation
    // (Python's _read_sql EXISTS clause), in every view shape.
    db.mark_read(&format!("yt:video:{A}"), true).unwrap();
    let recs = db.recommendations(&account).unwrap();
    assert!(recs[0].is_read, "twin read state not merged");
    assert!(!recs[1].is_read);
    let merged = db.recommendations_merged(&account).unwrap();
    assert!(merged[0].is_read);
    // Unreading the twin unreads the recommendation again.
    db.mark_read(&format!("yt:video:{A}"), false).unwrap();
    assert!(!db.recommendations(&account).unwrap()[0].is_read);
}

#[test]
fn d19_merged_views_current_only_favorites_include_old() {
    let (db, _path) = scratch_db("d19");
    let account = account();
    db.save_recommendations(&account, &videos(&[A, B])).unwrap();
    // Favorite A, then a new snapshot drops A entirely.
    let recs = db.recommendations(&account).unwrap();
    let a_id = recs.iter().find(|a| a.id.ends_with(A)).unwrap().id.clone();
    db.set_recommendation_state(&a_id, None, Some(true))
        .unwrap();
    db.save_recommendations(&account, &videos(&[B])).unwrap();
    // Merged (All/category) views show only current rows.
    let merged = db.recommendations_merged(&account).unwrap();
    assert_eq!(merged.len(), 1);
    assert!(merged[0].id.ends_with(B));
    assert!(db.has_recommendation_snapshot(&account).unwrap());
    // The Favorites view keeps the old favorite (Python's
    // wrapper: favorite queries drop the current=1 condition).
    let favs = db.favorite_recommendations(&account).unwrap();
    assert_eq!(favs.len(), 1);
    assert!(favs[0].id.ends_with(A));
    assert!(favs[0].is_favorite);
    // get_article_by_id resolves it too (no current filter).
    let by_id = db.article_by_id(&a_id).unwrap().unwrap();
    assert!(by_id.id.ends_with(A));
    assert!(by_id.is_favorite);
    // A deleted favorite leaves the Favorites view.
    db.delete_recommendation(&a_id).unwrap();
    assert!(db.favorite_recommendations(&account).unwrap().is_empty());
    // An account with no current rows has no snapshot.
    db.save_recommendations(&account, &[]).unwrap();
    assert!(!db.has_recommendation_snapshot(&account).unwrap());
}

#[test]
fn d20_mark_all_read_persists_recommendations_and_twins() {
    let (db, path) = scratch_db("d20");
    let account = account();
    store_twin(&db, A);
    db.save_recommendations(&account, &videos(&[A, B])).unwrap();
    let n = db.mark_recommendations_read(&account, false).unwrap();
    assert_eq!(n, 2);
    // Persisted: a fresh open (the app's reopen) still shows
    // them read — the pre-fix app only flipped displayed flags.
    drop(db);
    let db = Db::open(&path).unwrap();
    let recs = db.recommendations(&account).unwrap();
    assert!(recs.iter().all(|a| a.is_read));
    // The twin feed article was marked too (Python's
    // mark_read_batch routes through the twin rule).
    let twin = db.article_by_id(&format!("yt:video:{A}")).unwrap().unwrap();
    assert!(twin.is_read);
}

#[test]
fn d20_mark_favorites_read_scope() {
    let (db, _path) = scratch_db("d20f");
    let account = account();
    db.save_recommendations(&account, &videos(&[A, B])).unwrap();
    let recs = db.recommendations(&account).unwrap();
    let a_id = recs.iter().find(|a| a.id.ends_with(A)).unwrap().id.clone();
    db.set_recommendation_state(&a_id, None, Some(true))
        .unwrap();
    // A falls out of the snapshot; the favorites scope still
    // reaches it, and touches nothing else.
    db.save_recommendations(&account, &videos(&[B])).unwrap();
    let n = db.mark_recommendations_read(&account, true).unwrap();
    assert_eq!(n, 1);
    let favs = db.favorite_recommendations(&account).unwrap();
    assert!(favs[0].is_read);
    let merged = db.recommendations_merged(&account).unwrap();
    assert!(!merged[0].is_read, "non-favorite B must stay unread");
}

#[test]
fn g06_favorites_view_rows_carry_effective_read_state() {
    let (db, _path) = scratch_db("g06");
    let account = account();
    store_twin(&db, A);
    db.save_recommendations(&account, &videos(&[A])).unwrap();
    let recs = db.recommendations(&account).unwrap();
    db.set_recommendation_state(&recs[0].id, None, Some(true))
        .unwrap();
    db.mark_read(&format!("yt:video:{A}"), true).unwrap();
    // The Favorites view's rows honor the shared read state, so
    // the app's read filter behaves there like everywhere else.
    let favs = db.favorite_recommendations(&account).unwrap();
    assert_eq!(favs.len(), 1);
    assert!(favs[0].is_read);
    // Unfavoriting removes the row from the view immediately.
    db.set_recommendation_state(&recs[0].id, None, Some(false))
        .unwrap();
    assert!(db.favorite_recommendations(&account).unwrap().is_empty());
}
