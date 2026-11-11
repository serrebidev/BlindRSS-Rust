//! Local MP4/M4A/M4B embedded chapters: the `chpl` atom
//! parser behind `embedded_media_chapters`. Python reads
//! these through mutagen, which supports exactly one
//! location — `moov/udta/chpl` — and one layout:
//! version/flags, a reserved u32, a ONE-BYTE chapter
//! count, then per chapter a u64 start in 100-ns units
//! and a length-prefixed UTF-8 title. Fixtures are
//! synthetic box trees built in the test — no binary
//! media files in the repo; real ffmpeg-written files
//! were verified against the same known values during
//! the Audit 9 verification pass (see docs/PARITY.md).

use blindrss_core::chapters::{embedded_media_chapters, mp4_chapters};

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn chpl(chapters: &[(u64, &str)]) -> Vec<u8> {
    let mut payload = vec![1u8, 0, 0, 0]; // version + flags
    payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
    payload.push(chapters.len() as u8); // count: ONE byte
    for (start_100ns, title) in chapters {
        payload.extend_from_slice(&start_100ns.to_be_bytes());
        payload.push(title.len() as u8);
        payload.extend_from_slice(title.as_bytes());
    }
    bx(b"chpl", &payload)
}

/// A version-0 mvhd with a nonzero timescale — mutagen
/// refuses to read chapters without one.
fn mvhd() -> Vec<u8> {
    let mut payload = vec![0u8, 0, 0, 0]; // version + flags
    payload.extend_from_slice(&0u32.to_be_bytes()); // created
    payload.extend_from_slice(&0u32.to_be_bytes()); // modified
    payload.extend_from_slice(&1000u32.to_be_bytes()); // timescale
    payload.extend_from_slice(&12_000u32.to_be_bytes()); // duration
    bx(b"mvhd", &payload)
}

/// moov carrying mvhd plus the given chpl placement:
/// `moov(mvhd, udta(chpl))`, the only shape mutagen reads.
fn moov_with_chpl(chapters: &[(u64, &str)]) -> Vec<u8> {
    let udta = bx(b"udta", &chpl(chapters));
    let mut moov_payload = mvhd();
    moov_payload.extend_from_slice(&udta);
    bx(b"moov", &moov_payload)
}

fn write_temp(name: &str, bytes: &[u8]) -> String {
    let dir =
        std::env::temp_dir().join(format!("blindrss-mp4-test-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn chpl_in_moov_udta_parses_starts_and_titles() {
    let mut file = bx(b"ftyp", b"M4A \0\0\0\0M4A ");
    file.extend_from_slice(&moov_with_chpl(&[
        (0, "Intro"),
        (900_000_000, "Main feature"), // 90 s
    ]));
    let path = write_temp("flat.m4b", &file);
    let found = mp4_chapters(&path);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].start, 0.0);
    assert_eq!(found[0].title.as_deref(), Some("Intro"));
    assert_eq!(found[1].start, 90.0);
    assert_eq!(found[1].title.as_deref(), Some("Main feature"));
}

#[test]
fn chpl_directly_under_moov_is_invisible_like_mutagen() {
    // mutagen supports 'moov.udta.chpl' only; a chpl placed
    // as a direct moov child yields no chapters in Python.
    let mut moov_payload = mvhd();
    moov_payload.extend_from_slice(&chpl(&[(0, "Intro")]));
    let path = write_temp("toplevel.m4b", &bx(b"moov", &moov_payload));
    assert!(mp4_chapters(&path).is_empty());
    assert!(embedded_media_chapters(&path, Some("audio/mp4")).is_empty());
}

#[test]
fn chpl_nested_in_udta_is_found_and_routed_by_suffix() {
    let path = write_temp(
        "nested.m4a",
        &moov_with_chpl(&[(15_000_000, "Chapter 1.5s")]),
    );
    let found = embedded_media_chapters(&path, Some("audio/mp4"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].start, 1.5);
    assert_eq!(found[0].title.as_deref(), Some("Chapter 1.5s"));
}

#[test]
fn mp4_without_chpl_and_missing_files_yield_no_chapters() {
    let moov = bx(b"moov", &bx(b"free", b"nothing here"));
    let path = write_temp("none.mp4", &moov);
    assert!(mp4_chapters(&path).is_empty());
    assert!(mp4_chapters("/nonexistent/definitely-missing.m4b").is_empty());
    // A non-MP4 local file still yields nothing from this path.
    let other = write_temp("plain.bin", b"not media at all");
    assert!(embedded_media_chapters(&other, None).is_empty());
}

#[test]
fn truncated_chpl_yields_no_chapters() {
    // mutagen raises on a truncated entry and Python's
    // fetch turns that into "no chapters" — no partial list.
    let mut payload = chpl(&[(0, "Ok")]);
    payload.truncate(payload.len() - 2); // cut mid-title
    let udta = bx(b"udta", &payload);
    let mut moov_payload = mvhd();
    moov_payload.extend_from_slice(&udta);
    let path = write_temp("trunc.m4b", &bx(b"moov", &moov_payload));
    assert!(mp4_chapters(&path).is_empty());
}

#[test]
fn non_utf8_chpl_title_yields_no_chapters() {
    // mutagen decodes titles as strict UTF-8 and raises on
    // failure; Python surfaces that as no chapters at all.
    let mut payload = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.push(1u8);
    payload.extend_from_slice(&0u64.to_be_bytes());
    payload.push(2u8);
    payload.extend_from_slice(&[0xff, 0xfe]);
    let bad = bx(b"chpl", &payload);
    let udta = bx(b"udta", &bad);
    let mut moov_payload = mvhd();
    moov_payload.extend_from_slice(&udta);
    let path = write_temp("badtitle.m4b", &bx(b"moov", &moov_payload));
    assert!(mp4_chapters(&path).is_empty());
}

#[test]
fn mp4_m4a_m4b_suffixes_all_route_to_the_chapter_reader() {
    for name in ["suffix.mp4", "suffix.m4a", "suffix.m4b"] {
        let path = write_temp(name, &book_bytes());
        let found = embedded_media_chapters(&path, None);
        assert_eq!(found.len(), 3, "{name}");
        assert_eq!(found[1].start, 30.0, "{name}");
        assert_eq!(found[2].start, 75.5, "{name}");
        assert_eq!(found[2].title.as_deref(), Some("Finale"), "{name}");
    }
}

#[test]
fn file_url_form_reads_local_chapters() {
    let path = write_temp("fileurl.m4b", &book_bytes());
    let url = format!("file://{path}");
    let found = embedded_media_chapters(&url, None);
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].title.as_deref(), Some("Intro"));
}

#[test]
fn out_of_order_and_duplicate_chpl_entries_normalize_like_python() {
    // Python runs mutagen's chapter list through
    // `_normalize_chapters`: sorted by start, and equal starts
    // merged with a later duplicate supplying a missing title
    // (never replacing one already present).
    let path = write_temp(
        "order.m4b",
        &moov_with_chpl(&[
            (900_000_000, "End"),
            (300_000_000, ""),
            (0, "Start"),
            (300_000_000, "Middle"),
        ]),
    );
    let found = embedded_media_chapters(&path, None);
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].start, 0.0);
    assert_eq!(found[0].title.as_deref(), Some("Start"));
    assert_eq!(found[1].start, 30.0);
    assert_eq!(found[1].title.as_deref(), Some("Middle"));
    assert_eq!(found[2].start, 90.0);
    assert_eq!(found[2].title.as_deref(), Some("End"));
}

// ---------------------------------------------------------------------
// Pipeline level: a local file's chapters are fetched, stored and
// navigable through the same pipeline the player uses (Python
// `fetch_and_store_chapters` plus the player's prev/next rules).
// ---------------------------------------------------------------------

use blindrss_core::Chapter;
use blindrss_core::Db;
use blindrss_core::chapters::{
    ChapterJsonFetch, ChapterRequest, ChapterTransport, fetch_and_store_chapters,
};
use blindrss_core::playback::{
    chapter_index_for_position, chapter_next_target, chapter_prev_target,
};

struct NoTransport;

impl ChapterTransport for NoTransport {
    fn get_chapter_json(
        &self,
        _url: &str,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> Result<ChapterJsonFetch, String> {
        Err("embedded-chapter tests never fetch".to_string())
    }

    fn youtube_chapters(&self, _url: &str) -> Vec<Chapter> {
        Vec::new()
    }
}

fn book_bytes() -> Vec<u8> {
    let mut file = bx(b"ftyp", b"M4B \0\0\0\0M4B ");
    file.extend_from_slice(&moov_with_chpl(&[
        (0, "Intro"),
        (300_000_000, "Middle"), // 30 s
        (755_000_000, "Finale"), // 75.5 s
    ]));
    file
}

fn scratch_db(tag: &str) -> (Db, std::path::PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "blindrss-mp4-pipe-{tag}-{}-{nanos}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    (Db::open_or_create(&path).unwrap(), path)
}

#[test]
fn local_book_chapters_flow_through_the_pipeline() {
    let path = write_temp("pipe.m4b", &book_bytes());
    let (db, _db_path) = scratch_db("pipe");
    let req = ChapterRequest {
        article_id: "local-book",
        media_url: Some(&path),
        media_type: Some("audio/mp4"),
        chapter_url: None,
        allow_id3: true,
        cache_key: None,
        force_refresh: false,
    };
    let found = fetch_and_store_chapters(&db, &NoTransport, &req, 1_000.0);
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].start, 0.0);
    assert_eq!(found[0].title.as_deref(), Some("Intro"));
    assert_eq!(found[1].start, 30.0);
    assert_eq!(found[1].title.as_deref(), Some("Middle"));
    assert_eq!(found[2].start, 75.5);
    assert_eq!(found[2].title.as_deref(), Some("Finale"));

    // The stored rows are what the player lists on the next open.
    let stored = db.chapters("local-book").unwrap();
    assert_eq!(stored, found);

    // Python's navigation rules over the stored starts: the
    // active chapter tracks position, Previous selects the
    // previous chapter (a no-op at the first), Next selects the
    // next (a no-op at the last), and selecting a chapter seeks
    // to its start.
    let starts_ms: Vec<i64> = stored.iter().map(|c| (c.start * 1000.0) as i64).collect();
    assert_eq!(chapter_index_for_position(&starts_ms, 0), 0);
    assert_eq!(chapter_index_for_position(&starts_ms, 29_999), 0);
    assert_eq!(chapter_index_for_position(&starts_ms, 30_000), 1);
    assert_eq!(chapter_index_for_position(&starts_ms, 90_000), 2);
    assert_eq!(chapter_prev_target(2), Some(1));
    assert_eq!(chapter_prev_target(0), None);
    assert_eq!(chapter_next_target(0, stored.len()), Some(1));
    assert_eq!(chapter_next_target(2, stored.len()), None);
    let next = chapter_next_target(0, stored.len()).unwrap();
    assert_eq!(stored[next].start, 30.0);
}
