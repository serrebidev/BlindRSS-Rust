//! Audit 8 Task 20 regression tests: chapters, row annotations
//! and the remaining player controls (M12, M13, M15, G11, G19
//! core halves).
//!
//! Python's model: chapters arrive asynchronously from the
//! Podcasting 2.0 chapter file, YouTube metadata or the media
//! file's own embedded ID3 chapters and upgrade the matching
//! live player only while the fetch still belongs to the
//! current item (`gui/mainframe.py` `_fetch_chapters_for_player`
//! / `_apply_chapters_for_player`, `core/utils.py`
//! `fetch_and_store_chapters`); chapter navigation selects the
//! previous chapter outright, tracks the active chapter for
//! selection and menu state, and validates chapter links
//! (`gui/player.py`); the speed ladder runs 0.5–4.0 in 0.1 steps
//! with nearest-rung stepping (`core/utils.py`
//! `build_playback_speeds`, `gui/mainframe.py`
//! `_nudge_playback_speed`); article rows carry the playback /
//! chapter annotations from one playback-state scan
//! (`gui/mainframe.py` `_get_display_title`,
//! `_playback_time_annotation`, `_article_media_label`); the
//! chapter-link menu exposes every link, with no slot ceiling
//! (`gui/mainframe.py` `_article_chapter_links`).

use blindrss_core::chapters::{
    ChapterJsonFetch, ChapterRequest, ChapterTransport, article_chapter_links,
    chapter_start_seconds, fetch_and_store_chapters, get_article_chapters, normalize_chapters,
    parse_chapter_document, validated_chapter_href,
};
use blindrss_core::playback::{
    PlaybackSnapshot, article_media_label, chapter_index_for_position, chapter_link_index,
    chapter_next_target, chapter_prev_target, display_title, format_media_time, live_status_text,
    media_time_suffix, playback_speeds, playback_time_annotation, speed_index, speed_step,
    state_for_article,
};
use blindrss_core::player_session::ChapterGate;
use blindrss_core::{Chapter, Db, PlaybackState};
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
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
        "blindrss_audit8_t20_{tag}_{}_{nanos}_{n}.db",
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

fn chapter(start: f64, title: &str, href: Option<&str>) -> Chapter {
    Chapter {
        start,
        title: Some(title.to_string()),
        href: href.map(|h| h.to_string()),
    }
}

// ---------------------------------------------------------------------
// M15: the Python speed ladder and stepping.
// ---------------------------------------------------------------------

#[test]
fn speed_ladder_matches_python_builder() {
    let speeds = playback_speeds();
    assert_eq!(speeds.len(), 36, "0.5..=4.0 in 0.1 steps");
    assert_eq!(speeds.first(), Some(&0.5));
    assert_eq!(speeds.last(), Some(&4.0));
    for want in [1.0, 1.1, 2.5, 3.5, 4.0] {
        assert!(speeds.contains(&want), "ladder must contain {want}");
    }
    let mut sorted = speeds.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(speeds, sorted, "ladder is sorted");
}

#[test]
fn speed_index_finds_rungs_and_rejects_off_ladder() {
    let speeds = playback_speeds();
    assert_eq!(speed_index(&speeds, 1.1), Some(6));
    assert_eq!(speed_index(&speeds, 3.5), Some(30));
    assert_eq!(speed_index(&speeds, 4.0), Some(35));
    // A saved speed that is not a ladder rung shows no picker
    // selection in Python (speeds.index raises ValueError).
    assert_eq!(speed_index(&speeds, 1.13), None);
    assert_eq!(speed_index(&speeds, 0.0), None);
}

#[test]
fn speed_step_snaps_to_nearest_rung_then_steps() {
    let speeds = playback_speeds();
    // Python _nudge_playback_speed: snap current to the nearest
    // available speed, then step, clamped to the ladder ends.
    assert_eq!(speed_step(&speeds, 1.13, 1), 1.2);
    assert_eq!(speed_step(&speeds, 1.13, -1), 1.0);
    assert_eq!(speed_step(&speeds, 1.0, 1), 1.1);
    assert_eq!(speed_step(&speeds, 3.5, 1), 3.6);
    assert_eq!(speed_step(&speeds, 4.0, 1), 4.0);
    assert_eq!(speed_step(&speeds, 0.5, -1), 0.5);
    assert_eq!(speed_step(&speeds, 2.3, 0), 1.0, "reset goes to 1.0");
}

// ---------------------------------------------------------------------
// M13: chapter navigation policy (previous selects previous).
// ---------------------------------------------------------------------

#[test]
fn chapter_active_index_tracks_position() {
    let starts = [0, 60_000, 120_000];
    assert_eq!(chapter_index_for_position(&starts, 0), 0);
    assert_eq!(chapter_index_for_position(&starts, 59_999), 0);
    assert_eq!(chapter_index_for_position(&starts, 60_000), 1);
    assert_eq!(chapter_index_for_position(&starts, 999_999), 2);
    // Before the first chapter there is no active chapter.
    assert_eq!(chapter_index_for_position(&[5_000], 4_999), -1);
    assert_eq!(chapter_index_for_position(&[], 0), -1);
}

#[test]
fn chapter_previous_selects_the_previous_chapter() {
    // Python on_prev_chapter: active <= 0 is a no-op; otherwise
    // jump to active - 1. There is no "restart the current
    // chapter when a few seconds in" rule.
    assert_eq!(chapter_prev_target(2), Some(1));
    assert_eq!(chapter_prev_target(1), Some(0));
    assert_eq!(chapter_prev_target(0), None);
    assert_eq!(chapter_prev_target(-1), None);
}

#[test]
fn chapter_next_target_rules() {
    assert_eq!(chapter_next_target(-1, 3), Some(0));
    assert_eq!(chapter_next_target(0, 3), Some(1));
    assert_eq!(chapter_next_target(2, 3), None, "already at the last");
    assert_eq!(chapter_next_target(0, 0), None, "no chapters");
    assert_eq!(chapter_next_target(-1, 0), None);
}

#[test]
fn chapter_link_index_prefers_the_selection() {
    assert_eq!(chapter_link_index(Some(2), 1, 4), 2);
    assert_eq!(chapter_link_index(None, 1, 4), 1);
    assert_eq!(
        chapter_link_index(Some(9), 1, 4),
        1,
        "stale selection ignored"
    );
    assert_eq!(chapter_link_index(None, -1, 4), -1);
    assert_eq!(chapter_link_index(None, 0, 0), -1);
}

#[test]
fn chapter_href_validation_matches_python() {
    for good in [
        "https://example.com/x?y=1",
        "http://a.b:8080/p",
        "https://example.com",
    ] {
        assert_eq!(
            validated_chapter_href(good),
            Some(good.to_string()),
            "{good} must validate"
        );
    }
    for bad in [
        "",
        "   ",
        "https://user:pw@example.com/",
        "https://user@example.com/",
        "ftp://example.com/",
        "https://exa mple.com/",
        "https:\\\\evil.example/",
        "https://example.com:badport/",
        "example.com/no-scheme",
        "https:///nohost",
        "javascript:alert(1)",
    ] {
        assert_eq!(validated_chapter_href(bad), None, "{bad} must be rejected");
    }
}

// ---------------------------------------------------------------------
// G19: every chapter link is exposed (no 16-slot ceiling).
// ---------------------------------------------------------------------

#[test]
fn article_chapter_links_expose_all_links() {
    let chapters: Vec<Chapter> = (0..25)
        .map(|i| {
            chapter(
                i as f64 * 60.0,
                &format!("Part {i}"),
                Some(&format!("https://example.com/p{i}")),
            )
        })
        .collect();
    let links = article_chapter_links("https://example.com/ep", &chapters);
    assert_eq!(links.len(), 25, "chapter 17+ links must survive");
    assert_eq!(links[16].url, "https://example.com/p16");
    assert!(links[16].label.contains("Part 16"));
}

#[test]
fn article_chapter_links_resolve_and_validate() {
    let chapters = vec![
        chapter(0.0, "Abs", Some("https://cdn.example.com/a")),
        chapter(10.0, "Root", Some("/rooted")),
        chapter(20.0, "Rel", Some("sibling.html")),
        chapter(30.0, "Bad", Some("https://user:pw@example.com/")),
        chapter(40.0, "None", None),
        chapter(50.0, "Empty", Some("")),
        chapter(60.0, "Space", Some("has space.html")),
    ];
    let links = article_chapter_links("https://example.com/dir/ep.html", &chapters);
    let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://cdn.example.com/a",
            "https://example.com/rooted",
            "https://example.com/dir/sibling.html",
        ]
    );
    assert_eq!(links[0].index, 0);
    assert_eq!(links[1].index, 1);
    assert_eq!(links[2].index, 2);
}

// ---------------------------------------------------------------------
// M12: chapter normalization and the Podcasting 2.0 document.
// ---------------------------------------------------------------------

#[test]
fn chapter_start_seconds_parses_numbers_and_clocks() {
    assert_eq!(chapter_start_seconds(&json!(90)), Some(90.0));
    assert_eq!(chapter_start_seconds(&json!(1.5)), Some(1.5));
    assert_eq!(chapter_start_seconds(&json!("1:02:03")), Some(3723.0));
    assert_eq!(chapter_start_seconds(&json!("02:30")), Some(150.0));
    assert_eq!(chapter_start_seconds(&json!("45")), Some(45.0));
    assert_eq!(chapter_start_seconds(&json!("1:99")), None);
    assert_eq!(chapter_start_seconds(&json!("1:2:3:4")), None);
    assert_eq!(chapter_start_seconds(&json!(-5)), None);
    assert_eq!(chapter_start_seconds(&json!(true)), None);
    assert_eq!(chapter_start_seconds(&json!(null)), None);
    assert_eq!(chapter_start_seconds(&json!("nope")), None);
}

#[test]
fn normalize_chapters_applies_python_rules() {
    let raw = vec![
        // startTime wins by key presence, even when invalid:
        // the entry is dropped, start is NOT consulted.
        json!({"startTime": "bad", "start": 5, "title": "Dropped"}),
        json!({"start": "1:00", "title": "Minute", "url": "https://x/1"}),
        // href precedence: url, then link, then href.
        json!({"start": 30, "title": "", "link": "https://x/link", "href": "https://x/href"}),
        json!({"start": 30, "title": "Merged title"}),
        json!({"start": 10, "toc": false, "title": "Hidden"}),
        json!({"start": 20, "toc": true}),
        json!({"start": -1, "title": "Negative"}),
        json!("not a mapping"),
    ];
    let out = normalize_chapters(&raw);
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].start, 20.0);
    assert_eq!(out[0].title.as_deref(), Some(""));
    // The duplicate at 30 merged the later title into the row
    // that carried the link href.
    assert_eq!(out[1].start, 30.0);
    assert_eq!(out[1].title.as_deref(), Some("Merged title"));
    assert_eq!(out[1].href.as_deref(), Some("https://x/link"));
    assert_eq!(out[2].start, 60.0);
    assert_eq!(out[2].href.as_deref(), Some("https://x/1"));
}

#[test]
fn normalize_chapters_caps_input_at_python_limit() {
    let raw: Vec<serde_json::Value> = (0..10_005)
        .map(|i| json!({"start": i, "title": format!("C{i}")}))
        .collect();
    let out = normalize_chapters(&raw);
    assert_eq!(out.len(), 10_000);
}

#[test]
fn chapter_document_requires_the_podcasting_shape() {
    let good = br#"{"version": "1.2.0", "chapters": [
        {"startTime": 0, "title": "Intro"},
        {"startTime": 95.5, "title": "Deep dive", "url": "https://x/d"}
    ]}"#;
    let out = parse_chapter_document(good).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[1].href.as_deref(), Some("https://x/d"));

    // A UTF-8 BOM is tolerated (Python decodes utf-8-sig).
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(good);
    assert_eq!(parse_chapter_document(&bom).unwrap().len(), 2);

    for bad in [
        &br#"{"chapters": []}"#[..],
        &br#"{"version": "", "chapters": []}"#[..],
        &br#"{"version": "1.2.0", "chapters": {}}"#[..],
        &br#"[{"startTime": 0}]"#[..],
        &br#"not json"#[..],
    ] {
        assert!(parse_chapter_document(bad).is_err(), "{bad:?} must fail");
    }
}

// ---------------------------------------------------------------------
// M12: fetch_and_store_chapters against a scripted transport.
// ---------------------------------------------------------------------

/// One recorded transport call: (url, if-none-match,
/// if-modified-since).
type RecordedCall = (String, Option<String>, Option<String>);

struct FakeTransport {
    calls: RefCell<Vec<RecordedCall>>,
    script: RefCell<std::collections::VecDeque<Result<ChapterJsonFetch, String>>>,
    youtube: Vec<Chapter>,
}

impl FakeTransport {
    fn new(script: Vec<Result<ChapterJsonFetch, String>>) -> Self {
        FakeTransport {
            calls: RefCell::new(Vec::new()),
            script: RefCell::new(script.into()),
            youtube: Vec::new(),
        }
    }
}

impl ChapterTransport for FakeTransport {
    fn get_chapter_json(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<ChapterJsonFetch, String> {
        self.calls.borrow_mut().push((
            url.to_string(),
            etag.map(|s| s.to_string()),
            last_modified.map(|s| s.to_string()),
        ));
        self.script
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err("no scripted response".to_string()))
    }

    fn youtube_chapters(&self, _url: &str) -> Vec<Chapter> {
        self.youtube.clone()
    }
}

fn doc_fetch(body: &str, etag: Option<&str>) -> Result<ChapterJsonFetch, String> {
    Ok(ChapterJsonFetch::Document {
        bytes: body.as_bytes().to_vec(),
        etag: etag.map(|s| s.to_string()),
        last_modified: None,
    })
}

const CHAPTER_DOC: &str = r#"{"version": "1.2.0", "chapters": [
    {"startTime": 0, "title": "Intro"},
    {"startTime": 300, "title": "Main", "url": "https://x/main"}
]}"#;

fn request<'a>(article_id: &'a str, chapter_url: Option<&'a str>) -> ChapterRequest<'a> {
    ChapterRequest {
        article_id,
        media_url: None,
        media_type: None,
        chapter_url,
        allow_id3: true,
        cache_key: None,
        force_refresh: false,
    }
}

#[test]
fn chapter_url_fetch_stores_and_then_serves_from_cache() {
    let (db, _p) = scratch("urlfetch");
    let t = FakeTransport::new(vec![doc_fetch(CHAPTER_DOC, Some("\"v1\""))]);
    let out = fetch_and_store_chapters(&db, &t, &request("a1", Some("https://x/ch.json")), 1_000.0);
    assert_eq!(out.len(), 2);
    assert_eq!(db.chapters("a1").unwrap().len(), 2, "rows stored");
    let src = db.chapter_source("local:a1").unwrap().expect("source row");
    assert_eq!(src.source_url, "https://x/ch.json");
    assert_eq!(src.etag.as_deref(), Some("\"v1\""));
    assert_eq!(src.checked_at, 1_000.0);
    assert_eq!(src.fetched_at, 1_000.0);

    // Fifteen minutes later is past Python's 900 s window only
    // just: inside the window the stored rows return without a
    // second network call.
    let t2 = FakeTransport::new(vec![]);
    let out2 =
        fetch_and_store_chapters(&db, &t2, &request("a1", Some("https://x/ch.json")), 1_500.0);
    assert_eq!(out2.len(), 2);
    assert!(t2.calls.borrow().is_empty(), "no refetch inside the window");
}

#[test]
fn chapter_url_304_with_existing_rows_revalidates_source() {
    let (db, _p) = scratch("reval");
    let t = FakeTransport::new(vec![doc_fetch(CHAPTER_DOC, Some("\"v1\""))]);
    fetch_and_store_chapters(&db, &t, &request("a1", Some("https://x/ch.json")), 1_000.0);

    let t2 = FakeTransport::new(vec![Ok(ChapterJsonFetch::NotModified {
        etag: Some("\"v1\"".to_string()),
        last_modified: None,
    })]);
    let out =
        fetch_and_store_chapters(&db, &t2, &request("a1", Some("https://x/ch.json")), 5_000.0);
    assert_eq!(out.len(), 2, "existing rows return on 304");
    let calls = t2.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].1.as_deref(),
        Some("\"v1\""),
        "etag revalidation sent"
    );
    drop(calls);
    let src = db.chapter_source("local:a1").unwrap().unwrap();
    assert_eq!(src.checked_at, 5_000.0, "checked_at refreshed");
    assert_eq!(src.fetched_at, 1_000.0, "fetched_at preserved");
}

#[test]
fn chapter_url_304_without_rows_retries_unconditionally() {
    let (db, _p) = scratch("retry304");
    let t = FakeTransport::new(vec![
        Ok(ChapterJsonFetch::NotModified {
            etag: None,
            last_modified: None,
        }),
        doc_fetch(CHAPTER_DOC, None),
    ]);
    let out = fetch_and_store_chapters(&db, &t, &request("a1", Some("https://x/ch.json")), 100.0);
    assert_eq!(out.len(), 2);
    let calls = t.calls.borrow();
    assert_eq!(calls.len(), 2, "the bare 304 is retried once");
    assert_eq!(calls[1].1, None, "retry sends no validators");
}

#[test]
fn chapter_fetch_failure_returns_existing_rows() {
    let (db, _p) = scratch("failkeep");
    db.replace_chapters("a1", &[chapter(0.0, "Old", None)])
        .unwrap();
    let t = FakeTransport::new(vec![Err("boom".to_string())]);
    let mut req = request("a1", Some("https://x/ch.json"));
    req.force_refresh = true;
    let out = fetch_and_store_chapters(&db, &t, &req, 100.0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title.as_deref(), Some("Old"));
}

#[test]
fn embedded_id3_chapters_come_from_a_local_file() {
    // A hand-built ID3v2.4 tag with two CHAP frames (one with a
    // WXXX link) written to a local .mp3 path: the embedded
    // branch reads it with no network involved.
    let dir = std::env::temp_dir();
    let name = format!(
        "blindrss_t20_id3_{}_{}.mp3",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let path = dir.join(name);
    std::fs::write(&path, test_id3_tag()).unwrap();
    let (db, _p) = scratch("id3");
    let t = FakeTransport::new(vec![]);
    let mut req = request("a9", None);
    req.media_url = Some(path.to_str().unwrap());
    req.media_type = Some("audio/mpeg");
    let out = fetch_and_store_chapters(&db, &t, &req, 100.0);
    let _ = std::fs::remove_file(&path);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].title.as_deref(), Some("Intro"));
    assert_eq!(out[1].start, 61.0);
    assert_eq!(out[1].title.as_deref(), Some("Middle"));
    assert_eq!(out[1].href.as_deref(), Some("https://example.com/mid"));
    assert_eq!(db.chapters("a9").unwrap().len(), 2, "embedded rows stored");
}

fn seed(path: &PathBuf, sql: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
}

#[test]
fn get_article_chapters_reads_db_then_fetches() {
    let (db, path) = scratch("provider");
    // No article row: no chapters.
    let t = FakeTransport::new(vec![]);
    assert!(get_article_chapters(&db, &t, "missing", 0.0).is_empty());

    // Seed an article whose only chapter source is its
    // Podcasting 2.0 chapter URL.
    seed(
        &path,
        "INSERT INTO articles (id, feed_id, title, url, media_url, media_type, chapter_url) \
         VALUES ('a1', 'f1', 'Ep', 'https://x/ep', 'https://x/ep.mp3', 'audio/mpeg', 'https://x/ch.json');",
    );
    let t2 = FakeTransport::new(vec![doc_fetch(CHAPTER_DOC, None)]);
    let out = get_article_chapters(&db, &t2, "a1", 10.0);
    assert_eq!(out.len(), 2);
    // Second call is served from the DB rows, no transport use.
    let t3 = FakeTransport::new(vec![]);
    assert_eq!(get_article_chapters(&db, &t3, "a1", 20.0).len(), 2);
    assert!(t3.calls.borrow().is_empty());
}

/// Build an ID3v2.4 tag holding two CHAP frames.
fn test_id3_tag() -> Vec<u8> {
    fn synchsafe(n: usize) -> [u8; 4] {
        [
            ((n >> 21) & 0x7F) as u8,
            ((n >> 14) & 0x7F) as u8,
            ((n >> 7) & 0x7F) as u8,
            (n & 0x7F) as u8,
        ]
    }
    fn frame(id: &str, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&synchsafe(payload.len()));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(payload);
        out
    }
    fn text_frame(id: &str, text: &str) -> Vec<u8> {
        let mut payload = vec![3u8];
        payload.extend_from_slice(text.as_bytes());
        frame(id, &payload)
    }
    fn chap(element: &str, start_ms: u32, subs: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(element.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&start_ms.to_be_bytes());
        payload.extend_from_slice(&(start_ms + 60_000).to_be_bytes());
        payload.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        payload.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        payload.extend_from_slice(subs);
        frame("CHAP", &payload)
    }
    let mut body = Vec::new();
    body.extend_from_slice(&chap("c0", 0, &text_frame("TIT2", "Intro")));
    let mut subs = text_frame("TIT2", "Middle");
    // WXXX: encoding byte, description + NUL, then the URL.
    let mut wxxx = vec![3u8];
    wxxx.extend_from_slice(b"desc");
    wxxx.push(0);
    wxxx.extend_from_slice(b"https://example.com/mid");
    subs.extend_from_slice(&frame("WXXX", &wxxx));
    body.extend_from_slice(&chap("c1", 61_000, &subs));
    let mut tag = Vec::new();
    tag.extend_from_slice(b"ID3");
    tag.extend_from_slice(&[4, 0, 0]);
    tag.extend_from_slice(&synchsafe(body.len()));
    tag.extend_from_slice(&body);
    tag.extend_from_slice(&[0u8; 64]); // fake audio bytes
    tag
}

// ---------------------------------------------------------------------
// M12: the chapter-update gate refuses stale results.
// ---------------------------------------------------------------------

#[test]
fn chapter_gate_rejects_stale_article_and_sequence() {
    let mut gate = ChapterGate::new();
    let first = gate.begin("art-a");
    // The queue advanced before art-a's fetch returned.
    gate.begin("art-b");
    assert!(!gate.accepts("art-a", first), "stale article refused");
    // A re-fetch for the same article supersedes the older one.
    let older = gate.begin("art-a");
    let newer = gate.begin("art-a");
    assert!(!gate.accepts("art-a", older));
    assert!(gate.accepts("art-a", newer));
    // A new load / close invalidates everything outstanding.
    gate.invalidate();
    assert!(!gate.accepts("art-a", newer));
}

// ---------------------------------------------------------------------
// G11: row annotations from one playback-state scan.
// ---------------------------------------------------------------------

fn state(position_ms: i64, duration_ms: Option<i64>, completed: bool) -> PlaybackState {
    PlaybackState {
        position_ms,
        duration_ms,
        completed,
        seek_supported: None,
        title: None,
    }
}

#[test]
fn format_media_time_matches_python() {
    assert_eq!(format_media_time(0), "0:00");
    assert_eq!(format_media_time(65_000), "1:05");
    assert_eq!(format_media_time(297_000), "4:57");
    assert_eq!(format_media_time(3_723_000), "1:02:03");
    assert_eq!(format_media_time(-10), "0:00");
}

#[test]
fn media_time_suffix_matrix() {
    assert_eq!(media_time_suffix(None), "");
    assert_eq!(media_time_suffix(Some(&state(0, None, false))), "");
    assert_eq!(
        media_time_suffix(Some(&state(270_000, Some(297_000), true))),
        ", played, 4:57"
    );
    assert_eq!(media_time_suffix(Some(&state(0, None, true))), ", played");
    assert_eq!(
        media_time_suffix(Some(&state(270_000, Some(297_000), false))),
        ", 4:30 of 4:57"
    );
    assert_eq!(
        media_time_suffix(Some(&state(0, Some(297_000), false))),
        ", 4:57"
    );
    assert_eq!(
        media_time_suffix(Some(&state(90_000, None, false))),
        ", 1:30 played"
    );
}

#[test]
fn playback_time_annotation_matrix() {
    assert_eq!(playback_time_annotation(None), "not played");
    assert_eq!(
        playback_time_annotation(Some(&state(270_000, Some(297_000), false))),
        "4:57, played 4:30"
    );
    assert_eq!(
        playback_time_annotation(Some(&state(297_000, Some(297_000), true))),
        "4:57, played"
    );
    assert_eq!(
        playback_time_annotation(Some(&state(0, None, false))),
        "not played"
    );
}

#[test]
fn article_media_label_combines_base_and_annotation() {
    assert_eq!(article_media_label(false, None), "No audio");
    assert_eq!(
        article_media_label(false, Some(&state(10, Some(20), false))),
        "No audio",
        "no annotation without media"
    );
    assert_eq!(
        article_media_label(true, None),
        "Contains audio, not played"
    );
    assert_eq!(
        article_media_label(true, Some(&state(270_000, Some(297_000), false))),
        "Contains audio, 4:57, played 4:30"
    );
}

#[test]
fn display_title_appends_suffix_then_chapter_marker() {
    let st = state(270_000, Some(297_000), false);
    assert_eq!(
        display_title("Episode 9", Some(&st), true),
        "Episode 9, 4:30 of 4:57, Chapters available"
    );
    assert_eq!(display_title("Plain", None, false), "Plain");
    assert_eq!(
        display_title("Plain", None, true),
        "Plain, Chapters available"
    );
}

#[test]
fn state_for_article_uses_python_lookup_order() {
    let mut states: HashMap<String, PlaybackState> = HashMap::new();
    states.insert("article:a1".to_string(), state(1, None, false));
    states.insert("https://x/ep.mp3".to_string(), state(2, None, false));
    states.insert("https://x/ep".to_string(), state(3, None, false));
    // article:{id} wins over the media URL, which wins over the
    // article URL.
    let found = state_for_article(&states, "a1", Some("https://x/ep.mp3"), "https://x/ep");
    assert_eq!(found.map(|s| s.position_ms), Some(1));
    let found = state_for_article(&states, "other", Some("https://x/ep.mp3"), "https://x/ep");
    assert_eq!(found.map(|s| s.position_ms), Some(2));
    let found = state_for_article(&states, "other", None, "https://x/ep");
    assert_eq!(found.map(|s| s.position_ms), Some(3));
    assert!(state_for_article(&states, "other", None, "https://x/none").is_none());
}

#[test]
fn db_scans_feed_the_annotations() {
    let (db, _p) = scratch("scans");
    db.upsert_playback_state("article:a1", &state(270_000, Some(297_000), false))
        .unwrap();
    db.upsert_playback_state("https://x/ep.mp3", &state(5, None, false))
        .unwrap();
    let all = db.all_playback_states().unwrap();
    assert_eq!(all.len(), 2);
    let map: HashMap<String, PlaybackState> = all.into_iter().collect();
    assert_eq!(map["article:a1"].duration_ms, Some(297_000));

    db.replace_chapters("a1", &[chapter(0.0, "A", None), chapter(9.0, "B", None)])
        .unwrap();
    db.replace_chapters("a2", &[chapter(0.0, "A", None)])
        .unwrap();
    let counts = db
        .chapter_counts(&["a1".to_string(), "a2".to_string(), "a3".to_string()])
        .unwrap();
    assert_eq!(counts.get("a1"), Some(&2));
    assert_eq!(counts.get("a2"), Some(&1));
    assert_eq!(counts.get("a3"), None);
}

// ---------------------------------------------------------------------
// M15: the main window's live playback status text.
// ---------------------------------------------------------------------

#[test]
fn live_status_text_matches_python_field() {
    let mut snap = PlaybackSnapshot {
        has_media: true,
        playing: true,
        state: "Playing".to_string(),
        title: "Episode 9".to_string(),
        article_id: "a1".to_string(),
        media_url: "https://x/ep.mp3".to_string(),
        position_ms: 65_000,
        duration_ms: 297_000,
    };
    assert_eq!(
        live_status_text(&snap),
        "Playing: 1:05 / 4:57 (3:52 left) — Episode 9"
    );
    // Live / unknown duration: elapsed only.
    snap.duration_ms = 0;
    assert_eq!(live_status_text(&snap), "Playing: 1:05 — Episode 9");
    // An empty state verb falls back to Playing / Paused.
    snap.state = String::new();
    snap.playing = false;
    assert_eq!(live_status_text(&snap), "Paused: 1:05 — Episode 9");
    // No media: the field clears.
    snap.has_media = false;
    assert_eq!(live_status_text(&snap), "");
}
