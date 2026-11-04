//! Audit 7 regression tests (2026-11-02): Finding A — the
//! article list had no paging (Python pages at 400 behind a
//! "Load more items (Enter)" placeholder; the Rust list
//! hard-capped feed views at 300 / category views at 500) —
//! plus the silence merge/pad config plumbing.

use blindrss_core::Db;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> (Db, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "blindrss_audit7_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

fn seed(path: &PathBuf, sql: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
}

fn feed_sql(id: &str, category: &str) -> String {
    format!(
        "INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
         VALUES ('{id}','https://example.com/{id}','Feed {id}',0,'{category}',NULL);"
    )
}

fn article_sql(id: &str, feed: &str, date: &str) -> String {
    format!(
        "INSERT INTO articles (id, feed_id, title, url, content, description, date, \
         author, is_read, is_favorite, media_url, media_type) \
         VALUES ('{id}','{feed}','Title {id}','https://example.com/{id}','body {id}',\
         'd','{date}','A',0,0,NULL,NULL);"
    )
}

#[test]
fn feed_pages_tile_the_whole_view_without_overlap() {
    let (db, path) = scratch("feedpage");
    // 25 episodes in 5 date groups of 5 (shared dates force the
    // id tiebreak to carry page stability).
    let mut sql = feed_sql("f1", "Podcasts");
    for i in 0..25 {
        let date = format!("2026-10-{:02}", 30 - (i / 5));
        sql.push_str(&article_sql(&format!("e{i:02}"), "f1", &date));
    }
    seed(&path, &sql);

    assert_eq!(db.articles_count(Some("f1")).unwrap(), 25);
    assert_eq!(db.articles_count(None).unwrap(), 25);
    assert_eq!(db.articles_count(Some("missing")).unwrap(), 0);

    let mut seen: Vec<String> = Vec::new();
    for (offset, want) in [(0usize, 10usize), (10, 10), (20, 5), (30, 0)] {
        let page = db.articles_light_page(Some("f1"), 10, offset).unwrap();
        assert_eq!(page.len(), want, "page at offset {offset}");
        assert!(page.iter().all(|a| a.content.is_none()), "pages stay light");
        seen.extend(page.iter().map(|a| a.id.clone()));
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 25, "pages overlap or skip rows: {seen:?}");
    // Order is total: date DESC, id DESC.
    let all = db.articles_light_page(Some("f1"), 100, 0).unwrap();
    let ids: Vec<&str> = all.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(seen[..25], ids[..], "paged order != single-page order");
    assert_eq!(ids[0], "e04", "newest date group, highest id first");
}

#[test]
fn category_pages_and_count_cover_the_subtree() {
    let (db, path) = scratch("catpage");
    let mut sql = feed_sql("f1", "Podcasts");
    sql.push_str(&feed_sql("f2", "News"));
    sql.push_str(&feed_sql("f3", "Podcasts / Sub"));
    for i in 0..4 {
        sql.push_str(&article_sql(
            &format!("p{i}"),
            "f1",
            &format!("2026-10-2{i}"),
        ));
        sql.push_str(&article_sql(
            &format!("s{i}"),
            "f3",
            &format!("2026-10-2{i}"),
        ));
    }
    sql.push_str(&article_sql("n0", "f2", "2026-10-29"));
    seed(&path, &sql);

    // Subtree: f1 (4) + f3 (4); the News article is excluded
    // even though it is the newest row in the database.
    assert_eq!(db.articles_for_category_count("Podcasts").unwrap(), 8);
    let full = db.articles_for_category_light("Podcasts", 100).unwrap();
    assert_eq!(full.len(), 8);

    let mut seen: Vec<String> = Vec::new();
    for offset in (0..8).step_by(3) {
        let page = db
            .articles_for_category_light_page("Podcasts", 3, offset)
            .unwrap();
        seen.extend(page.iter().map(|a| a.id.clone()));
    }
    let full_ids: Vec<String> = full.iter().map(|a| a.id.clone()).collect();
    assert_eq!(seen, full_ids, "category pages must tile the full view");
    assert!(!seen.iter().any(|id| id == "n0"));
}

#[test]
fn silence_finalize_honours_tuned_merge_and_padding() {
    use blindrss_core::silence::{DEFAULT_MERGE_GAP_S, DEFAULT_PADDING_S, finalize_ranges};
    // The shipped defaults ARE Python's config defaults
    // (silence_skip_merge_gap_ms 260 / silence_skip_padding_ms 60).
    assert_eq!(DEFAULT_MERGE_GAP_S, 0.26);
    assert_eq!(DEFAULT_PADDING_S, 0.06);

    let raw = vec![(10.0, 12.0), (12.2, 14.0), (30.0, 31.0)];
    // Python's publish arithmetic (Audit 8 M09): padding
    // EXPANDS each range by 60 ms per side, then ranges merge
    // across gaps up to 260 ms — the first two (0.08 apart
    // after padding) become one range.
    let merged = finalize_ranges(raw.clone(), DEFAULT_MERGE_GAP_S, DEFAULT_PADDING_S);
    assert_eq!(merged.len(), 2);
    assert!((merged[0].0 - 9.94).abs() < 1e-9);
    assert!((merged[0].1 - 14.06).abs() < 1e-9);
    assert!((merged[1].0 - 29.94).abs() < 1e-9);
    assert!((merged[1].1 - 31.06).abs() < 1e-9);
    // A user-tightened gap of 0.1 keeps them separate.
    let split = finalize_ranges(raw, 0.1, 0.0);
    assert_eq!(split.len(), 3);
}

// =====================================================================
// Audit 8 Task 21 (M09, S17): incremental, cancellable silence
// skipping. Python's spec is core/audio_silence.py (detectors,
// paced ffmpeg scan) and gui/player.py's silence regions (scan
// scheduling, publish padding, the guarded skip loop and its
// landing verification).
// =====================================================================

use blindrss_core::silence::{
    PcmSilenceDetector, VadSilenceDetector, merge_ranges_ms, merge_ranges_with_gap_ms,
    pad_and_merge_ms,
};
#[cfg(unix)]
use blindrss_core::silence::{SilenceScanConfig, scan_silence};
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicI64};

fn pcm_tone(ms: i64, sample_rate: i64, dbfs: f64, freq: f64) -> Vec<u8> {
    let samples = (ms * sample_rate / 1000) as usize;
    let amp = 32768.0 * 10f64.powf(dbfs / 20.0) * std::f64::consts::FRAC_1_SQRT_2;
    let mut out = Vec::with_capacity(samples * 2);
    for i in 0..samples {
        let v = (2.0 * std::f64::consts::PI * freq * i as f64 / sample_rate as f64).sin() * amp;
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

fn pcm_silence(ms: i64, sample_rate: i64) -> Vec<u8> {
    vec![0u8; (ms * sample_rate / 1000) as usize * 2]
}

/// Amplitude-modulated harmonic stack: the closest cheap
/// synthetic stand-in for voiced speech (fundamental +
/// harmonics under a syllabic envelope).
fn pcm_voiced(ms: i64, sample_rate: i64) -> Vec<u8> {
    let samples = (ms * sample_rate / 1000) as usize;
    let mut out = Vec::with_capacity(samples * 2);
    for i in 0..samples {
        let t = i as f64 / sample_rate as f64;
        let env = 0.55 + 0.45 * (2.0 * std::f64::consts::PI * 4.0 * t).sin();
        let mut v = 0.0;
        for (h, w) in [(120.0, 1.0), (240.0, 0.6), (360.0, 0.4), (480.0, 0.25)] {
            v += w * (2.0 * std::f64::consts::PI * h * t).sin();
        }
        let v = v / 2.25 * env * 0.5 * 32768.0;
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

#[test]
fn merge_ranges_ms_matches_python_adjacency() {
    // Python merge_ranges merges when next.start <= cur.end + 1.
    assert_eq!(merge_ranges_ms(vec![(0, 100), (101, 200)]), vec![(0, 200)]);
    assert_eq!(
        merge_ranges_ms(vec![(0, 100), (102, 200)]),
        vec![(0, 100), (102, 200)]
    );
    // Invalid ranges (end < start) are dropped, input is sorted.
    assert_eq!(
        merge_ranges_ms(vec![(300, 400), (50, 40), (0, 100)]),
        vec![(0, 100), (300, 400)]
    );
    // merge_ranges_with_gap bridges up to gap_ms.
    assert_eq!(
        merge_ranges_with_gap_ms(vec![(0, 100), (350, 400)], 260),
        vec![(0, 400)]
    );
    assert_eq!(
        merge_ranges_with_gap_ms(vec![(0, 100), (361, 400)], 260),
        vec![(0, 100), (361, 400)]
    );
}

#[test]
fn pad_and_merge_ms_expands_like_python_publish() {
    // Python _publish: (max(0, s - pad), e + pad), then
    // merge_ranges_with_gap with the SAME gap used at publish.
    assert_eq!(
        pad_and_merge_ms(vec![(10_000, 12_000), (12_200, 14_000)], 60, 260),
        vec![(9_940, 14_060)]
    );
    // Expansion clamps at zero.
    assert_eq!(pad_and_merge_ms(vec![(30, 100)], 60, 260), vec![(0, 160)]);
}

#[test]
fn pcm_detector_finds_silence_around_a_tone() {
    let rate = 16_000;
    let mut pcm = pcm_silence(1500, rate);
    pcm.extend(pcm_tone(1000, rate, -20.0, 440.0));
    pcm.extend(pcm_silence(1500, rate));
    let mut det = PcmSilenceDetector::new(rate, 30, 700, -38.0);
    det.feed(&pcm);
    let ranges = det.finish();
    assert_eq!(ranges.len(), 2, "ranges: {ranges:?}");
    assert!(
        ranges[0].0 == 0 && (ranges[0].1 - 1500).abs() <= 30,
        "{ranges:?}"
    );
    assert!(
        (ranges[1].0 - 2500).abs() <= 30 && (ranges[1].1 - 4000).abs() <= 30,
        "{ranges:?}"
    );
    // A tone below the threshold counts as silence (Python's
    // RMS rule is a plain dBFS comparison).
    let mut det = PcmSilenceDetector::new(rate, 30, 700, -38.0);
    det.feed(&pcm_tone(1500, rate, -50.0, 440.0));
    assert_eq!(det.finish(), vec![(0, 1500)]);
}

#[test]
fn pcm_detector_finalize_matches_python_quirks() {
    // A run still open at the end closes in finalize; the
    // trailing partial window is dropped, exactly like
    // Python's StreamingSilenceDetector.finalize (its flush
    // feeds b"" and is a no-op).
    let rate = 16_000;
    let mut det = PcmSilenceDetector::new(rate, 30, 700, -38.0);
    det.feed(&pcm_silence(755, rate));
    assert_eq!(det.finish(), vec![(0, 750)]);
    // Below the minimum, an open run produces nothing.
    let mut det = PcmSilenceDetector::new(rate, 30, 700, -38.0);
    det.feed(&pcm_silence(600, rate));
    assert!(det.finish().is_empty());
}

#[test]
fn vad_detector_separates_silence_from_voiced_audio() {
    if !VadSilenceDetector::available(16_000) {
        // The native VAD is compiled into this crate; absence
        // would mean an unsupported rate — 16 kHz is supported,
        // so this is only a guard for exotic build setups.
        return;
    }
    let rate = 16_000;
    let mut pcm = pcm_silence(2000, rate);
    pcm.extend(pcm_voiced(2000, rate));
    pcm.extend(pcm_silence(2000, rate));
    let mut det = VadSilenceDetector::new(rate, 30, 700, 1, 260, -38.0);
    det.feed(&pcm);
    let ranges = det.finish();
    // Both silent spans are found...
    assert!(
        ranges.iter().any(|r| r.0 == 0 && r.1 >= 1700),
        "leading silence missing: {ranges:?}"
    );
    assert!(
        ranges.iter().any(|r| r.1 >= 5700 && r.0 <= 4300),
        "trailing silence missing: {ranges:?}"
    );
    // ...and no range reaches into the middle of the voiced
    // span: the VAD must not call sustained voicing silence.
    assert!(
        !ranges.iter().any(|r| r.0 < 3400 && r.1 > 2600),
        "voiced span swallowed: {ranges:?}"
    );
    // Frame sizes outside 10/20/30 coerce to 30 (Python).
    let det = VadSilenceDetector::new(rate, 25, 700, 1, 260, -38.0);
    assert_eq!(det.frame_ms(), 30);
    // Aggressiveness clamps to 0..=3 (Python).
    let det = VadSilenceDetector::new(rate, 30, 700, 9, 260, -38.0);
    assert_eq!(det.aggressiveness(), 3);
}

// ---------------------------------------------------------------------
// Scanner lifecycle (fake ffmpeg; unix only — the scripts are sh).
// ---------------------------------------------------------------------

#[cfg(unix)]
fn task21_scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("blindrss-t21-{}-{n}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(tag)
}

#[cfg(unix)]
fn write_fake_ffmpeg(tag: &str, body: &str) -> PathBuf {
    let path = task21_scratch(tag);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"#!/bin/sh\n").unwrap();
    f.write_all(body.as_bytes()).unwrap();
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

#[cfg(unix)]
fn scan_cfg(use_vad: bool) -> SilenceScanConfig {
    SilenceScanConfig {
        sample_rate: 16_000,
        window_ms: 30,
        min_silence_ms: 700,
        threshold_db: -38.0,
        merge_gap_ms: 260,
        vad_aggressiveness: 1,
        vad_frame_ms: 30,
        use_vad,
        headers: Vec::new(),
        default_user_agent: "BlindRSS-Test".to_string(),
        threads: None,
        low_priority: false,
        lead_ms: 0,
    }
}

#[cfg(unix)]
#[test]
fn scan_completes_and_reports_progress_incrementally() {
    // 6 s silence, 1 s voiced, 5 s silence, 0.5 s voiced,
    // emitted at once. The final voiced blip closes the
    // trailing silence run before EOF so the last progress
    // set can equal the final set.
    let rate = 16_000;
    let mut pcm = pcm_silence(6000, rate);
    pcm.extend(pcm_voiced(1000, rate));
    pcm.extend(pcm_silence(5000, rate));
    pcm.extend(pcm_voiced(500, rate));
    let pcm_path = task21_scratch("completion.pcm");
    std::fs::write(&pcm_path, &pcm).unwrap();
    let script = write_fake_ffmpeg("ffmpeg-dump", &format!("cat '{}'\n", pcm_path.display()));
    let abort = AtomicBool::new(false);
    let mut progress: Vec<(Vec<(i64, i64)>, i64)> = Vec::new();
    let result = scan_silence(
        &script,
        "episode.mp3",
        &scan_cfg(true),
        &abort,
        &|| None,
        &mut |ranges, scanned_ms| {
            progress.push((ranges.to_vec(), scanned_ms));
            true
        },
    );
    let ranges = result.expect("scan completes");
    assert_eq!(ranges.len(), 2, "ranges: {ranges:?}");
    assert!(
        ranges[0].0 == 0 && (ranges[0].1 - 6000).abs() <= 90,
        "{ranges:?}"
    );
    // The second start tolerance is wider: the WebRTC VAD
    // holds "speech" briefly past the voiced burst (hangover),
    // exactly as Python's identical detector does.
    assert!(
        (ranges[1].0 - 7000).abs() <= 250 && (ranges[1].1 - 12000).abs() <= 90,
        "{ranges:?}"
    );
    // Progress fires on Python's 5 s-of-audio cadence; early
    // progress cannot contain the second range yet, and every
    // progress set is a prefix of the final set — progress
    // never contradicts the result, it only converges to it.
    // Whether the LAST progress call already carries the
    // trailing range is timing, not contract: that range ends
    // exactly at EOF, so it closes only at finalize, and pipe
    // chunking decides whether a cadence tick lands after the
    // close (Linux) or before it (macOS CI, 2026-11-04).
    assert!(progress.len() >= 2, "progress calls: {}", progress.len());
    assert!(
        !progress[0].0.iter().any(|r| r.0 >= 7000),
        "first progress: {:?}",
        progress[0]
    );
    let last = &progress.last().unwrap().0;
    assert_eq!(&ranges[..last.len()], last.as_slice());
}

#[cfg(unix)]
#[test]
fn scan_passes_headers_only_for_http_sources() {
    let argv_path = task21_scratch("argv.txt");
    let script = write_fake_ffmpeg(
        "ffmpeg-argv",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\nhead -c 3200 /dev/zero\n",
            argv_path.display()
        ),
    );
    let mut cfg = scan_cfg(false);
    cfg.headers = vec![
        ("User-Agent".to_string(), "TestUA/1.0".to_string()),
        ("Cookie".to_string(), "session=abc".to_string()),
    ];
    let abort = AtomicBool::new(false);
    scan_silence(
        &script,
        "https://example.com/ep.mp3?token=secret",
        &cfg,
        &abort,
        &|| None,
        &mut |_, _| true,
    )
    .expect("http scan runs");
    let argv = std::fs::read_to_string(&argv_path).unwrap();
    assert!(argv.contains("-user_agent"), "argv: {argv}");
    assert!(argv.contains("TestUA/1.0"), "argv: {argv}");
    assert!(argv.contains("Cookie: session=abc"), "argv: {argv}");
    // Local sources get no header options at all (Python:
    // ffmpeg rejects them for local inputs).
    std::fs::remove_file(&argv_path).unwrap();
    scan_silence(
        &script,
        "/tmp/ep.mp3",
        &cfg,
        &abort,
        &|| None,
        &mut |_, _| true,
    )
    .expect("local scan runs");
    let argv = std::fs::read_to_string(&argv_path).unwrap();
    assert!(!argv.contains("-user_agent"), "argv: {argv}");
    assert!(!argv.contains("-headers"), "argv: {argv}");
}

#[cfg(unix)]
#[test]
fn scan_paces_behind_the_playback_position() {
    // Emits 10 s of PCM instantly, then hangs: with a 2 s lead
    // over a position stuck at 0, the scan must still be
    // waiting (not finished) well after the bytes arrived.
    let pcm_path = task21_scratch("paced.pcm");
    std::fs::write(&pcm_path, pcm_silence(10_000, 16_000)).unwrap();
    let script = write_fake_ffmpeg(
        "ffmpeg-pace",
        &format!("cat '{}'\nsleep 30\n", pcm_path.display()),
    );
    let mut cfg = scan_cfg(false);
    cfg.lead_ms = 2000;
    let abort = std::sync::Arc::new(AtomicBool::new(false));
    let abort_worker = abort.clone();
    let pos = AtomicI64::new(0);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = scan_silence(
            &script,
            "episode.mp3",
            &cfg,
            &abort_worker,
            &|| Some(pos.load(Ordering::Relaxed)),
            &mut |_, _| true,
        );
        let _ = done_tx.send(result);
    });
    assert!(
        done_rx
            .recv_timeout(std::time::Duration::from_millis(1500))
            .is_err(),
        "paced scan must not run ahead of the listener"
    );
    abort.store(true, Ordering::Relaxed);
    let result = done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("abort releases the paced scan");
    assert!(matches!(
        result,
        Err(blindrss_core::silence::ScanError::Cancelled)
    ));
    handle.join().unwrap();
}

#[cfg(unix)]
#[test]
fn scan_cancellation_leaves_no_surviving_ffmpeg() {
    // The fake ffmpeg heartbeats from its own shell loop: if
    // the process survived cancellation, the heartbeat file
    // would keep growing after the scan returned.
    let beat_path = task21_scratch("heartbeat.txt");
    let script = write_fake_ffmpeg(
        "ffmpeg-forever",
        &format!(
            "while true; do echo x >> '{}'; head -c 64000 /dev/zero; sleep 0.05; done\n",
            beat_path.display()
        ),
    );
    let abort = std::sync::Arc::new(AtomicBool::new(false));
    let abort_worker = abort.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = scan_silence(
            &script,
            "episode.mp3",
            &scan_cfg(false),
            &abort_worker,
            &|| None,
            &mut |_, _| true,
        );
        let _ = done_tx.send(result);
    });
    // Let the scan get going (heartbeats landing), then cancel.
    std::thread::sleep(std::time::Duration::from_millis(600));
    let beats_before = std::fs::read_to_string(&beat_path)
        .map(|t| t.lines().count())
        .unwrap_or(0);
    assert!(beats_before > 0, "fake ffmpeg never started");
    abort.store(true, Ordering::Relaxed);
    let result = done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("cancellation returns");
    assert!(matches!(
        result,
        Err(blindrss_core::silence::ScanError::Cancelled)
    ));
    handle.join().unwrap();
    let beats_at_return = std::fs::read_to_string(&beat_path)
        .map(|t| t.lines().count())
        .unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(700));
    let beats_later = std::fs::read_to_string(&beat_path)
        .map(|t| t.lines().count())
        .unwrap_or(0);
    assert_eq!(
        beats_at_return, beats_later,
        "ffmpeg kept running after cancellation"
    );
}

// ---------------------------------------------------------------------
// Skip policy (Python gui/player.py _maybe_skip_silence + verify).
// ---------------------------------------------------------------------

use blindrss_core::silence::{SilenceSkipPolicy, SkipContext};
use std::time::{Duration, Instant};

fn skip_ctx() -> SkipContext {
    SkipContext {
        ready: true,
        seekable: None,
        resume_pending: false,
        dragging: false,
        casting: false,
        is_remote: false,
        resume_backoff_ms: 360,
        retrigger_backoff_ms: 1400,
    }
}

#[test]
fn skip_policy_guards_block_the_skip() {
    let ranges = vec![(10_000, 20_000)];
    let t0 = Instant::now();
    // Scan not ready.
    let mut p = SilenceSkipPolicy::new();
    let mut ctx = skip_ctx();
    ctx.ready = false;
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &ctx), None);
    // Backend reports unseekable.
    let mut p = SilenceSkipPolicy::new();
    let mut ctx = skip_ctx();
    ctx.seekable = Some(false);
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &ctx), None);
    // Seekable unknown (None) does not block (Python proceeds).
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    // Resume restore pending.
    let mut p = SilenceSkipPolicy::new();
    let mut ctx = skip_ctx();
    ctx.resume_pending = true;
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &ctx), None);
    // Slider being dragged.
    let mut p = SilenceSkipPolicy::new();
    let mut ctx = skip_ctx();
    ctx.dragging = true;
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &ctx), None);
}

#[test]
fn skip_policy_hit_arithmetic_matches_python() {
    let ranges = vec![(10_000, 20_000)];
    let t0 = Instant::now();
    // Containment is start-100 <= pos <= end-100; the target
    // is end + resume_backoff.
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 9_900, t0, &skip_ctx()), Some(20_360));
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 19_900, t0, &skip_ctx()), Some(20_360));
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 19_901, t0, &skip_ctx()), None);
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 9_899, t0, &skip_ctx()), None);
    // A range starting more than 1 s ahead ends the scan of
    // the sorted list (no later range is considered).
    let ranges = vec![(10_000, 20_000), (50_000, 60_000)];
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 5_000, t0, &skip_ctx()), None);
}

#[test]
fn skip_policy_cooldown_and_user_seek_pause() {
    let ranges = vec![(10_000, 20_000), (30_000, 40_000)];
    let t0 = Instant::now();
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    // Local cooldown is 2.5 s: a second range 1 s later waits.
    assert_eq!(
        p.evaluate(
            &ranges,
            30_500,
            t0 + Duration::from_millis(1000),
            &skip_ctx()
        ),
        None
    );
    assert_eq!(
        p.evaluate(
            &ranges,
            30_500,
            t0 + Duration::from_millis(2600),
            &skip_ctx()
        ),
        Some(40_360)
    );
    // A user seek pauses skipping for 1.2 s (local).
    let mut p = SilenceSkipPolicy::new();
    p.note_user_seek(t0, false);
    assert_eq!(
        p.evaluate(
            &ranges,
            10_500,
            t0 + Duration::from_millis(1000),
            &skip_ctx()
        ),
        None
    );
    assert_eq!(
        p.evaluate(
            &ranges,
            10_500,
            t0 + Duration::from_millis(1300),
            &skip_ctx()
        ),
        Some(20_360)
    );
    // Remote pause is 2.5 s and remote cooldown is 5 s.
    let mut p = SilenceSkipPolicy::new();
    p.note_user_seek(t0, true);
    let mut ctx = skip_ctx();
    ctx.is_remote = true;
    assert_eq!(
        p.evaluate(&ranges, 10_500, t0 + Duration::from_millis(2000), &ctx),
        None
    );
    assert_eq!(
        p.evaluate(&ranges, 10_500, t0 + Duration::from_millis(2600), &ctx),
        Some(20_360)
    );
    assert_eq!(
        p.evaluate(&ranges, 30_500, t0 + Duration::from_millis(7000), &ctx),
        None
    );
    assert_eq!(
        p.evaluate(&ranges, 30_500, t0 + Duration::from_millis(7700), &ctx),
        Some(40_360)
    );
}

#[test]
fn skip_policy_floor_and_loop_guard() {
    let t0 = Instant::now();
    // The floor ratchets to the furthest legitimate position;
    // a position well behind it (stall / backward jump) blocks.
    let mut p = SilenceSkipPolicy::new();
    p.update_floor(30_000, t0);
    assert_eq!(p.floor_ms(), 30_000);
    assert_eq!(
        p.evaluate(&[(10_000, 20_000)], 10_500, t0, &skip_ctx()),
        None,
        "pos + 500 < floor must block"
    );
    // A recent app seek target ahead lifts the floor; a skip
    // target below the floor is refused inside the loop.
    let mut p = SilenceSkipPolicy::new();
    p.note_app_seek(11_000, 10_500, t0);
    p.update_floor(10_500, t0);
    assert_eq!(p.floor_ms(), 11_000);
    assert_eq!(
        p.evaluate(&[(10_000, 10_600)], 10_500, t0, &skip_ctx()),
        None,
        "target 10960 < floor 11000 must be refused"
    );
    // Loop guard: after skipping to a target, landing back in
    // the same range does not re-seek to (nearly) the same
    // target once the cooldown has passed.
    let ranges = vec![(10_000, 20_000)];
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    assert_eq!(
        p.evaluate(
            &ranges,
            10_500,
            t0 + Duration::from_millis(2600),
            &skip_ctx()
        ),
        None
    );
    // Retrigger cleanup: passing the last range's end by more
    // than retrigger + 300 ms retires the last-index memory.
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    assert_eq!(p.last_idx(), Some(0));
    assert_eq!(
        p.evaluate(
            &ranges,
            20_000 + 1400 + 301,
            t0 + Duration::from_millis(2600),
            &skip_ctx()
        ),
        None
    );
    assert_eq!(p.last_idx(), None);
}

#[test]
fn skip_policy_verify_reseeks_once_then_settles() {
    let ranges = vec![(10_000, 20_000)];
    let t0 = Instant::now();
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    // Too early for the verify re-seek (min wait 0.35 s local).
    assert_eq!(
        p.verify_tick(15_000, t0 + Duration::from_millis(200), false),
        None
    );
    // Still short of the target past the wait: re-seek once.
    assert_eq!(
        p.verify_tick(15_000, t0 + Duration::from_millis(400), false),
        Some(20_360)
    );
    // Already attempted: no second re-seek.
    assert_eq!(
        p.verify_tick(15_000, t0 + Duration::from_millis(600), false),
        None
    );
    // Landing within the local threshold (700 ms) settles it.
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    assert_eq!(
        p.verify_tick(19_700, t0 + Duration::from_millis(400), false),
        None
    );
    assert!(!p.verify_active());
    // The window (3 s local) expiring also settles it.
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    assert_eq!(
        p.verify_tick(15_000, t0 + Duration::from_millis(3100), false),
        None
    );
    assert!(!p.verify_active());
    // A user seek cancels verification outright (Python
    // _note_user_seek clears the verify state).
    let mut p = SilenceSkipPolicy::new();
    assert_eq!(p.evaluate(&ranges, 10_500, t0, &skip_ctx()), Some(20_360));
    p.note_user_seek(t0 + Duration::from_millis(300), false);
    assert!(!p.verify_active());
}

// ---------------------------------------------------------------------
// S17: range-cache diagnostic redaction (Python redact_url_for_log).
// ---------------------------------------------------------------------

#[test]
fn redact_url_for_log_strips_credentials_queries_and_signed_paths() {
    use blindrss_core::rangecache::redact_url_for_log;
    assert_eq!(
        redact_url_for_log("https://user:pass@example.com:8443/a/b.mp3?token=secret&x=1"),
        "https://example.com:8443/a/b.mp3?<redacted>"
    );
    assert_eq!(
        redact_url_for_log("https://rr1.googlevideo.com/videoplayback?expire=9&sig=zz"),
        "https://rr1.googlevideo.com/<signed-media-path>?<redacted>"
    );
    assert_eq!(
        redact_url_for_log("http://127.0.0.1:8765/AbCdEf/proxy"),
        "http://127.0.0.1:8765/<capability>/proxy"
    );
    assert_eq!(
        redact_url_for_log("/local/file.mp3"),
        "<local-or-invalid-url>"
    );
    assert_eq!(redact_url_for_log(""), "<local-or-invalid-url>");
    // The toggle the Settings checkbox writes is the one the
    // proxy config reads (S17 persistence path).
    let mut cfg = blindrss_core::config::Config::default();
    cfg.set_path("range_cache_debug", serde_json::json!(true));
    let rc = blindrss_core::rangecache::RangeCacheConfig::from_app_config(&cfg);
    assert!(rc.debug);
}
