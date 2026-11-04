//! Audit 8 Task 19 regression tests: verified seek/resume and
//! asynchronous persistence (M05, M06, M07, M08, M21 core halves).
//!
//! Python's model: `core/playback_state.py` upserts preserve the
//! optional shared fields (duration, seek support, title) when a
//! write carries None; position writes are batched into a
//! latest-wins pending dict and flushed with retry when SQLite is
//! locked (`gui/player.py` `_queue_resume_db_upsert` /
//! `_flush_pending_resume_db_writes`); resume and user seeks are
//! verified against the backend's real position with bounded
//! retries (the seek guard and the resume-restore tick), learn
//! seek support from the outcome, and a seek that never lands
//! speaks once; Stop then Play re-runs the restore against the
//! saved position with the configured rewind.

use blindrss_core::config::Config;
use blindrss_core::playback::{PendingPlaybackWrites, ResumePolicy};
use blindrss_core::{Db, PlaybackState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "blindrss_audit8_t19_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn scratch(tag: &str) -> (Db, PathBuf) {
    let path = scratch_path(tag);
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

fn full_state(position_ms: i64) -> PlaybackState {
    PlaybackState {
        position_ms,
        duration_ms: Some(3_600_000),
        completed: false,
        seek_supported: Some(true),
        title: Some("Episode One".to_string()),
    }
}

// ---------------------------------------------------------------------
// M06: upsert preserves optional shared metadata on None
// ---------------------------------------------------------------------

#[test]
fn upsert_preserves_optional_fields_on_none() {
    // Python core/playback_state.py: the ON CONFLICT clause keeps
    // the stored duration / seek_supported / title whenever the
    // incoming write carries NULL for them. A bare position write
    // (the player's periodic save) must not erase what an earlier
    // write learned.
    let (db, _path) = scratch("m06_keep");
    db.upsert_playback_state("article:1", &full_state(120_000))
        .unwrap();
    let bare = PlaybackState {
        position_ms: 180_000,
        duration_ms: None,
        completed: false,
        seek_supported: None,
        title: None,
    };
    db.upsert_playback_state("article:1", &bare).unwrap();
    let got = db.playback_state("article:1").unwrap().unwrap();
    assert_eq!(got.position_ms, 180_000);
    assert_eq!(got.duration_ms, Some(3_600_000), "duration erased by None");
    assert_eq!(
        got.seek_supported,
        Some(true),
        "seek support erased by None"
    );
    assert_eq!(
        got.title.as_deref(),
        Some("Episode One"),
        "title erased by None"
    );
}

#[test]
fn upsert_explicit_values_replace_and_bad_values_sanitize() {
    // Explicit values DO replace (Python's CASE WHEN picks the
    // excluded value when it is not NULL), a non-positive duration
    // counts as absent, and a negative position clamps to zero.
    let (db, _path) = scratch("m06_replace");
    db.upsert_playback_state("article:1", &full_state(120_000))
        .unwrap();
    let mut explicit = full_state(200_000);
    explicit.duration_ms = Some(7_200_000);
    explicit.seek_supported = Some(false);
    explicit.title = Some("Renamed".to_string());
    db.upsert_playback_state("article:1", &explicit).unwrap();
    let got = db.playback_state("article:1").unwrap().unwrap();
    assert_eq!(got.duration_ms, Some(7_200_000));
    assert_eq!(got.seek_supported, Some(false));
    assert_eq!(got.title.as_deref(), Some("Renamed"));

    let mut bad_duration = full_state(-50);
    bad_duration.duration_ms = Some(0);
    db.upsert_playback_state("article:1", &bad_duration)
        .unwrap();
    let got = db.playback_state("article:1").unwrap().unwrap();
    assert_eq!(got.position_ms, 0, "negative position must clamp");
    assert_eq!(
        got.duration_ms,
        Some(7_200_000),
        "duration_ms <= 0 is absent in Python and must preserve"
    );
}

#[test]
fn policy_save_preserves_learned_seek_support() {
    // The audit's player.rs half of M06: the player's saves always
    // supply seek_supported: None. A stream Python learned was
    // unseekable must stay marked unseekable across position
    // saves, and its duration must survive a save that could not
    // read a length.
    let (db, _path) = scratch("m06_policy");
    let policy = ResumePolicy::from_config(&Config::default());
    db.upsert_playback_state("article:9", &full_state(60_000))
        .unwrap();
    db.set_seek_supported("article:9", false).unwrap();
    let save = PlaybackState {
        position_ms: 300_000,
        duration_ms: None,
        completed: false,
        seek_supported: None,
        title: Some("Episode One".to_string()),
    };
    assert!(policy.save_state(&db, "9", "", &save).unwrap());
    let got = db.playback_state("article:9").unwrap().unwrap();
    assert_eq!(got.position_ms, 300_000);
    assert_eq!(got.seek_supported, Some(false), "learned support lost");
    assert_eq!(got.duration_ms, Some(3_600_000), "duration lost");
}

#[test]
fn set_seek_supported_updates_existing_row_only() {
    // Python's set_seek_supported is a plain UPDATE: no row, no
    // insert — a stray learn event must not create state.
    let (db, _path) = scratch("m06_setseek");
    db.set_seek_supported("article:missing", true).unwrap();
    assert_eq!(db.playback_state("article:missing").unwrap(), None);
    db.upsert_playback_state("article:1", &full_state(10_000))
        .unwrap();
    db.set_seek_supported("article:1", false).unwrap();
    let got = db.playback_state("article:1").unwrap().unwrap();
    assert_eq!(got.seek_supported, Some(false));
    assert_eq!(got.position_ms, 10_000, "position must be untouched");
    assert_eq!(got.duration_ms, Some(3_600_000));
}

// ---------------------------------------------------------------------
// M07: pending writes are latest-wins and survive a locked database
// ---------------------------------------------------------------------

#[test]
fn pending_writes_flush_latest_wins_and_report_written() {
    let (db, _path) = scratch("m07_latest");
    let policy = ResumePolicy::from_config(&Config::default());
    let mut queue = PendingPlaybackWrites::new();
    queue.queue_save("7", "", full_state(100_000));
    queue.queue_save("7", "", full_state(200_000));
    queue.queue_seek_supported("article:7", false);
    assert_eq!(queue.pending_len(), 2, "same-article saves coalesce");
    let written = queue.flush(&db, &policy);
    assert!(queue.is_empty());
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].0, "article:7");
    assert_eq!(written[0].1.position_ms, 200_000);
    let got = db.playback_state("article:7").unwrap().unwrap();
    assert_eq!(got.position_ms, 200_000, "latest queued save must win");
    assert_eq!(got.seek_supported, Some(false));
}

#[test]
fn pending_writes_survive_a_locked_database_and_recover() {
    // Hold an exclusive lock from a second connection: the flush
    // must fail fast (short busy timeout, Python's 500 ms model),
    // keep the write queued, and land it once the lock clears —
    // never drop it and never report it written early.
    let (db, path) = scratch("m07_locked");
    db.set_busy_timeout_ms(50).unwrap();
    let policy = ResumePolicy::from_config(&Config::default());
    let mut queue = PendingPlaybackWrites::new();
    queue.queue_save("8", "", full_state(420_000));

    let locker = rusqlite::Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let written = queue.flush(&db, &policy);
    assert!(written.is_empty(), "locked write reported as written");
    assert_eq!(queue.pending_len(), 1, "locked write was dropped");
    locker.execute_batch("ROLLBACK").unwrap();
    drop(locker);

    let written = queue.flush(&db, &policy);
    assert_eq!(written.len(), 1, "write did not land after unlock");
    assert!(queue.is_empty());
    let got = db.playback_state("article:8").unwrap().unwrap();
    assert_eq!(got.position_ms, 420_000);
}
