//! Playback resume policy and Python-compatible state identifiers.
//!
//! Also the player's load-cancellation gate and the fast
//! InnerTube chapter fetch (gap 7): Python's player tags every
//! load with `_active_load_seq`, cancels the silence scan's
//! abort event on stop/switch/close, and drops worker results
//! whose sequence went stale; `LoadGate`/`LoadToken` are that
//! pair. Python's chapter lookup tries one InnerTube
//! watch-next request (~0.7 s) before falling back to the
//! yt-dlp CLI (3-4 s); `fast_youtube_chapters` is that path.

use crate::{Db, PlaybackState, config::Config};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub fn playback_id(article_id: &str) -> String {
    format!("article:{article_id}")
}

pub fn saved_state(
    db: &Db,
    article_id: &str,
    url: &str,
) -> rusqlite::Result<Option<PlaybackState>> {
    for id in [
        playback_id(article_id),
        article_id.to_string(),
        url.to_string(),
    ] {
        if !id.is_empty()
            && let Some(state) = db.playback_state(&id)?
        {
            return Ok(Some(state));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy)]
pub struct ResumePolicy {
    pub enabled: bool,
    pub min_ms: i64,
    pub back_ms: i64,
    pub complete_threshold_ms: i64,
    pub save_interval: std::time::Duration,
}

impl ResumePolicy {
    pub fn from_config(cfg: &Config) -> Self {
        let interval = cfg
            .raw()
            .get("resume_save_interval_s")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(15.0);
        Self {
            enabled: cfg.get_bool("resume_playback", true),
            min_ms: cfg.get_i64("resume_min_ms", 0).max(0),
            back_ms: cfg.get_i64("resume_back_ms", 10_000).max(0),
            complete_threshold_ms: cfg.get_i64("resume_complete_threshold_ms", 60_000).max(0),
            save_interval: std::time::Duration::from_secs_f64(if interval.is_finite() {
                interval.clamp(2.0, 86_400.0)
            } else {
                15.0
            }),
        }
    }

    pub fn completed(&self, position_ms: i64, duration_ms: i64) -> bool {
        duration_ms > 0
            && position_ms > 0
            && duration_ms.saturating_sub(position_ms) <= self.complete_threshold_ms
    }

    pub fn resume_target(&self, state: &PlaybackState, url: &str) -> i64 {
        let seekable_file =
            crate::media::is_media_file_url(url) && !url.to_ascii_lowercase().contains(".m3u8");
        if !self.enabled
            || state.completed
            || state.position_ms < self.min_ms
            || state.seek_supported == Some(false) && !seekable_file
            || self.completed(state.position_ms, state.duration_ms.unwrap_or(0))
        {
            return 0;
        }
        if state.position_ms > self.back_ms {
            state.position_ms - self.back_ms
        } else {
            state.position_ms.max(0)
        }
    }

    pub fn save_state(
        &self,
        db: &Db,
        article_id: &str,
        url: &str,
        state: &PlaybackState,
    ) -> rusqlite::Result<bool> {
        if !self.enabled {
            return Ok(false);
        }
        let mut state = state.clone();
        state.completed |= self.completed(state.position_ms, state.duration_ms.unwrap_or(0));
        // Backends can briefly report zero during stop/reconnect. Keep both
        // meaningful progress and the completed marker instead of erasing them.
        if !state.completed
            && state.position_ms < 2_000
            && saved_state(db, article_id, url)?
                .is_some_and(|old| old.completed || old.position_ms >= 2_000)
        {
            return Ok(false);
        }
        if state.completed {
            state.position_ms = 0;
        }
        db.upsert_playback_state(&playback_id(article_id), &state)?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------
// Load cancellation (Python `_active_load_seq` + the silence
// scan's abort event, unified).
// ---------------------------------------------------------------------

struct LoadGateInner {
    seq: AtomicU64,
    cancelled_seq: AtomicU64,
}

/// The player-side load gate: one per player window. `begin`
/// starts a new load (queue advance, fresh open) and
/// supersedes every earlier token; `cancel_current` aborts the
/// in-flight load without starting another (stop, close).
#[derive(Clone, Default)]
pub struct LoadGate {
    inner: Arc<LoadGateInner>,
}

impl Default for LoadGateInner {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            cancelled_seq: AtomicU64::new(0),
        }
    }
}

impl LoadGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a load; the returned token identifies it. Every
    /// previously issued token becomes stale (cancelled).
    pub fn begin(&self) -> LoadToken {
        let seq = self.inner.seq.fetch_add(1, Ordering::SeqCst) + 1;
        LoadToken {
            inner: self.inner.clone(),
            seq,
        }
    }

    /// Cancel the load currently in flight (Python's stop /
    /// switch / close setting the scan's abort event). Tokens
    /// issued afterwards are unaffected.
    pub fn cancel_current(&self) {
        self.inner
            .cancelled_seq
            .store(self.inner.seq.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// A token for the current load without starting a new one.
    pub fn current(&self) -> LoadToken {
        LoadToken {
            inner: self.inner.clone(),
            seq: self.inner.seq.load(Ordering::SeqCst),
        }
    }
}

/// One load's identity, cloned into worker threads. A token is
/// cancelled when its load was explicitly cancelled OR when a
/// newer load superseded it — Python checks both (the abort
/// event, and `load_seq != _active_load_seq` in `_publish` and
/// `_do_play`).
#[derive(Clone)]
pub struct LoadToken {
    inner: Arc<LoadGateInner>,
    seq: u64,
}

impl LoadToken {
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled_seq.load(Ordering::SeqCst) == self.seq
            || self.inner.seq.load(Ordering::SeqCst) != self.seq
    }

    pub fn is_current(&self) -> bool {
        !self.is_cancelled()
    }
}

/// Run `work` on a worker thread and wait for it in 25 ms
/// slices, returning `None` as soon as the token is cancelled.
/// The work itself is cooperative-blind (a yt-dlp subprocess
/// inside `resolve_article_for_play` cannot be killed from
/// here), so a cancelled call detaches the worker and discards
/// its result — exactly Python's stale-sequence drop: the
/// resolution finishes in the background and nobody acts on
/// it. `None` also means the worker panicked.
pub fn resolve_cancellable<T: Send + 'static>(
    token: &LoadToken,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let done_worker = done.clone();
    std::thread::spawn(move || {
        let out = work();
        let _ = tx.send(out);
        done_worker.store(true, Ordering::SeqCst);
    });
    loop {
        if token.is_cancelled() {
            return None;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(value) => return Some(value),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

// ---------------------------------------------------------------------
// Fast InnerTube chapter fetch (Python
// youtube_fulltext.extract_chapters / _chapters_from_watch_next /
// _innertube). One watch-next POST settles most videos; the
// description-timestamp fallback validates against the length
// from one extra `player` POST. `None` from
// `fast_youtube_chapters` means "InnerTube could not settle
// it" — the caller falls back to the yt-dlp info path, as
// Python does. An empty vec is a settled answer (the video
// has no chapters), also Python's distinction.
// ---------------------------------------------------------------------

const INNERTUBE_URL: &str = "https://www.youtube.com/youtubei/v1/{}?prettyPrint=false";
// ponytail: pinned WEB client version, copied from Python; YouTube
// accepts old versions for years. Bump it if watch-next starts
// refusing requests (the yt-dlp fallback covers it).
const INNERTUBE_CLIENT_VERSION: &str = "2.20251010.01.00";

fn innertube(endpoint: &str, video_id: &str, timeout_s: u64) -> Option<serde_json::Value> {
    let body = serde_json::json!({
        "context": {"client": {
            "clientName": "WEB",
            "clientVersion": INNERTUBE_CLIENT_VERSION,
            "hl": "en",
        }},
        "videoId": video_id,
    });
    let payload = serde_json::to_string(&body).ok()?;
    // Python clamps the call timeout to 3..=15 s; build a
    // sibling of the shared agent with that bound (the shared
    // agent's own finite timeout is the feed timeout).
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(timeout_s.clamp(3, 15))))
        .user_agent(crate::http::user_agent())
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent
        .post(&INNERTUBE_URL.replace("{}", endpoint))
        .header("Origin", "https://www.youtube.com")
        .header("Content-Type", "application/json")
        .send(payload.as_str())
        .ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.is_object().then_some(value)
}

/// Every value stored under `key` anywhere in the JSON tree
/// (Python `_find_values`), in document order.
fn find_values<'a>(node: &'a serde_json::Value, key: &str, out: &mut Vec<&'a serde_json::Value>) {
    match node {
        serde_json::Value::Object(map) => {
            for (name, value) in map {
                if name == key {
                    out.push(value);
                } else {
                    find_values(value, key, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for value in items {
                find_values(value, key, out);
            }
        }
        _ => {}
    }
}

/// Python `_runs_text`: simpleText, else the joined runs.
fn runs_text(node: &serde_json::Value) -> String {
    if let Some(s) = node.get("simpleText").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    node.get("runs")
        .and_then(|v| v.as_array())
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(|v| v.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

/// Python `_parse_clock`: "12:34" / "1:02:03" to seconds.
fn parse_clock(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0f64;
    for part in parts {
        seconds = seconds * 60.0 + part.parse::<i64>().ok()? as f64;
    }
    Some(seconds)
}

/// Python `_valid_chapters`: keep ordered starts that do not
/// pass the video's duration (when known).
fn valid_chapters(pairs: Vec<(Option<f64>, String)>, duration: Option<f64>) -> Vec<(f64, String)> {
    let mut out = Vec::new();
    let mut last = 0.0f64;
    for (start, title) in pairs {
        let Some(start) = start else { continue };
        if last <= start && duration.is_none_or(|d| start <= d) {
            out.push((start, title));
            last = start;
        }
    }
    out
}

fn chapter_rows(pairs: Vec<(f64, String)>, video_id: &str) -> Vec<crate::db::Chapter> {
    let base = format!("https://www.youtube.com/watch?v={video_id}");
    let mut pairs = pairs;
    if let Some(first) = pairs.first()
        && first.0 > 0.0
    {
        // yt-dlp fills the gap before a late first chapter.
        pairs.insert(0, (0.0, "<Untitled Chapter 1>".to_string()));
    }
    pairs
        .into_iter()
        .map(|(start, title)| {
            let title = title.trim().to_string();
            crate::db::Chapter {
                start: start.max(0.0),
                title: Some(if title.is_empty() {
                    "Chapter".to_string()
                } else {
                    title
                }),
                href: Some(format!("{base}&t={}s", start.max(0.0) as i64)),
            }
        })
        .collect()
}

/// Chapters from a watch-next response (Python's first two
/// sources: player-bar chapter markers, then the "key
/// moments" engagement panel). Returns `Some` when either
/// source produced chapters, `None` when the caller should
/// try the description fallback. Pure; fixture-tested.
pub fn chapters_from_watch_next(
    data: &serde_json::Value,
    video_id: &str,
) -> Option<Vec<crate::db::Chapter>> {
    data.get("contents")?;
    let overlays = data.get("playerOverlays").cloned().unwrap_or_default();
    let mut lists = Vec::new();
    find_values(&overlays, "chapteredPlayerBarRenderer", &mut lists);
    let mut maps = Vec::new();
    find_values(&overlays, "markersMap", &mut maps);
    let mut by_key: Vec<(&str, &serde_json::Value)> = Vec::new();
    for markers_map in &maps {
        if let Some(entries) = markers_map.as_array() {
            for entry in entries {
                if let Some(value) = entry.get("value")
                    && value.get("chapters").is_some()
                {
                    let key = entry.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    if !by_key.iter().any(|(k, _)| *k == key) {
                        by_key.push((key, value));
                    }
                }
            }
        }
    }
    let mut candidates: Vec<&serde_json::Value> = lists;
    for wanted in ["DESCRIPTION_CHAPTERS", "AUTO_CHAPTERS"] {
        if let Some((_, value)) = by_key.iter().find(|(k, _)| *k == wanted) {
            candidates.push(value);
        }
    }
    for (_, value) in &by_key {
        candidates.push(value);
    }
    for candidate in candidates {
        let mut pairs = Vec::new();
        if let Some(chapters) = candidate.get("chapters").and_then(|v| v.as_array()) {
            for chapter in chapters {
                let Some(renderer) = chapter.get("chapterRenderer") else {
                    continue;
                };
                let start = renderer
                    .get("timeRangeStartMillis")
                    .and_then(|v| v.as_f64())
                    .map(|ms| ms / 1000.0);
                pairs.push((
                    start,
                    runs_text(renderer.get("title").unwrap_or(&serde_json::Value::Null)),
                ));
            }
        }
        let chapters = valid_chapters(pairs, None);
        if !chapters.is_empty() {
            return Some(chapter_rows(chapters, video_id));
        }
    }
    // "Key moments" panel.
    let panels = data
        .get("engagementPanels")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut marker_lists = Vec::new();
    find_values(&panels, "macroMarkersListRenderer", &mut marker_lists);
    for contents in marker_lists {
        let contents = contents
            .get("contents")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let mut items = Vec::new();
        find_values(&contents, "macroMarkersListItemRenderer", &mut items);
        let pairs: Vec<(Option<f64>, String)> = items
            .iter()
            .map(|item| {
                let clock = runs_text(
                    item.get("timeDescription")
                        .unwrap_or(&serde_json::Value::Null),
                );
                (
                    parse_clock(&clock),
                    runs_text(item.get("title").unwrap_or(&serde_json::Value::Null)),
                )
            })
            .collect();
        let chapters = valid_chapters(pairs, None);
        if !chapters.is_empty() {
            return Some(chapter_rows(chapters, video_id));
        }
    }
    None
}

/// A clock token in Python's description grammar
/// (`(?:\d+:)?\d{1,2}:\d{2}`): two or three colon-separated
/// groups, the last exactly two digits, the others one or two.
fn is_clock_token(token: &str) -> bool {
    let parts: Vec<&str> = token.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
        && parts.last().is_some_and(|p| p.len() == 2)
}

/// One description line in either order (Python's
/// `_DESCRIPTION_LINE_RE` pair): a leading or trailing clock
/// with a title on the other side. Returns (seconds, title,
/// time_first).
fn description_line(line: &str) -> Option<(f64, String, bool)> {
    let line = line.trim();
    // Time-first: "0:00 Intro" / "1:02:03 - Middle". The
    // clock token may carry trailing punctuation ("0:00)"),
    // which Python's \W* separator absorbs.
    if let Some((head, title)) = line.split_once(|c: char| c.is_whitespace()) {
        let clock = head.trim_end_matches(|c: char| !c.is_ascii_digit() && c != ':');
        if is_clock_token(clock)
            && head[clock.len()..]
                .chars()
                .all(|c| !c.is_alphanumeric() && c != '_')
            && let Some(secs) = parse_clock(clock)
        {
            let title = title.trim().trim_start_matches(|c: char| {
                !c.is_alphanumeric() && c != '_' && !c.is_whitespace()
            });
            let title = title.trim();
            if !title.is_empty() {
                return Some((secs, title.to_string(), true));
            }
        }
    }
    // Title-first: "Intro 0:00".
    if let Some((title, last)) = line.rsplit_once(|c: char| c.is_whitespace())
        && is_clock_token(last)
        && let Some(secs) = parse_clock(last)
    {
        let title = title.trim();
        if !title.is_empty() {
            return Some((secs, title.to_string(), false));
        }
    }
    None
}

/// Chapters from description timestamps (Python's third
/// source), validated against the video duration, time-first
/// lines before title-first. Empty when neither order yields
/// a valid ordered set.
pub fn chapters_from_description(text: &str, duration: f64) -> Vec<(f64, String)> {
    if duration <= 0.0 {
        return Vec::new();
    }
    let mut time_first: Vec<(f64, String)> = Vec::new();
    let mut title_first: Vec<(f64, String)> = Vec::new();
    for line in text.lines() {
        if let Some((secs, title, is_time_first)) = description_line(line) {
            if is_time_first {
                time_first.push((secs, title));
            } else {
                title_first.push((secs, title));
            }
        }
    }
    for mut found in [time_first, title_first] {
        found.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let pairs: Vec<(Option<f64>, String)> =
            found.into_iter().map(|(s, t)| (Some(s), t)).collect();
        let chapters = valid_chapters(pairs, Some(duration));
        if !chapters.is_empty() {
            return chapters;
        }
    }
    Vec::new()
}

/// The fast chapter fetch (Python `extract_chapters` minus
/// its memo and the yt-dlp fallback, which stay with the
/// caller): watch-next first, description timestamps second.
/// `None` = InnerTube could not settle it (fall back to
/// yt-dlp); `Some(vec![])` = settled, no chapters.
pub fn fast_youtube_chapters(url: &str, timeout_s: u64) -> Option<Vec<crate::db::Chapter>> {
    let video_id = crate::ytfull::video_id_from_url(url)?;
    let data = innertube("next", &video_id, timeout_s)?;
    data.get("contents")?;
    if let Some(chapters) = chapters_from_watch_next(&data, &video_id) {
        return Some(chapters);
    }
    // Description fallback: timestamps from the watch-next
    // response, validated against the length from one extra
    // `player` request — and only when timestamps exist at
    // all (Python returns a settled empty list otherwise).
    let mut descriptions = Vec::new();
    find_values(&data, "attributedDescription", &mut descriptions);
    let text: Vec<String> = descriptions
        .iter()
        .filter_map(|d| {
            d.get("content")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();
    let text = text.join("\n");
    let has_timestamps = text.lines().any(|l| description_line(l).is_some());
    if !has_timestamps {
        return Some(Vec::new());
    }
    let player = innertube("player", &video_id, timeout_s)?;
    let details = player.get("videoDetails")?;
    let duration: f64 = details
        .get("lengthSeconds")
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0.0);
    if duration <= 0.0 {
        return None;
    }
    let description = details
        .get("shortDescription")
        .and_then(|v| v.as_str())
        .unwrap_or(&text);
    let pairs = chapters_from_description(description, duration);
    Some(chapter_rows(pairs, &video_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(position_ms: i64) -> PlaybackState {
        PlaybackState {
            position_ms,
            duration_ms: Some(600_000),
            ..PlaybackState::default()
        }
    }

    #[test]
    fn resumes_python_article_ids_then_legacy_ids_and_url() {
        let db = Db::open_or_create(std::path::Path::new(":memory:")).unwrap();
        db.upsert_playback_state("https://example.test/media.mp3", &state(60_000))
            .unwrap();
        assert_eq!(
            saved_state(&db, "42", "https://example.test/media.mp3").unwrap(),
            Some(state(60_000))
        );
        db.upsert_playback_state("42", &state(90_000)).unwrap();
        assert_eq!(
            saved_state(&db, "42", "https://example.test/media.mp3").unwrap(),
            Some(state(90_000))
        );
        db.upsert_playback_state("article:42", &state(120_000))
            .unwrap();
        assert_eq!(
            saved_state(&db, "42", "https://example.test/media.mp3").unwrap(),
            Some(state(120_000))
        );
        assert_eq!(playback_id("42"), "article:42");
        assert_eq!(db.playback_state("42").unwrap(), Some(state(90_000)));
    }

    #[test]
    fn resumes_with_python_defaults_and_configured_limits() {
        let mut cfg = Config::default();
        let policy = ResumePolicy::from_config(&cfg);
        assert_eq!(
            policy.resume_target(&state(120_000), "https://example.test/media.mp3"),
            110_000
        );
        assert_eq!(
            policy.resume_target(&state(5_000), "https://example.test/media.mp3"),
            5_000
        );
        assert_eq!(
            policy.resume_target(&state(550_000), "https://example.test/media.mp3"),
            0
        );
        assert!(!policy.completed(0, 3_000));
        assert!(policy.completed(550_000, 600_000));
        assert_eq!(policy.save_interval, std::time::Duration::from_secs(15));
        cfg.set_path("resume_min_ms", serde_json::json!(30_000));
        cfg.set_path("resume_back_ms", serde_json::json!(5_000));
        cfg.set_path("resume_complete_threshold_ms", serde_json::json!(1_000));
        cfg.set_path("resume_save_interval_s", serde_json::json!(0.5));
        let policy = ResumePolicy::from_config(&cfg);
        assert_eq!(
            policy.resume_target(&state(20_000), "https://example.test/media.mp3"),
            0
        );
        assert_eq!(
            policy.resume_target(&state(550_000), "https://example.test/media.mp3"),
            545_000
        );
        assert_eq!(policy.save_interval, std::time::Duration::from_secs(2));
        cfg.set_path("resume_playback", serde_json::json!(false));
        assert_eq!(
            ResumePolicy::from_config(&cfg)
                .resume_target(&state(120_000), "https://example.test/media.mp3"),
            0
        );
    }

    #[test]
    fn completed_and_unseekable_streams_do_not_resume() {
        let policy = ResumePolicy::from_config(&Config::default());
        let mut saved = state(120_000);
        saved.completed = true;
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/media.mp3"),
            0
        );
        saved.completed = false;
        saved.seek_supported = Some(false);
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/live.m3u8"),
            0
        );
        assert_eq!(
            policy.resume_target(&saved, "https://example.test/media.mp3?token=1"),
            110_000
        );
    }

    #[test]
    fn saves_canonical_ids_and_preserves_progress_on_backend_reset() {
        let db = Db::open_or_create(std::path::Path::new(":memory:")).unwrap();
        let policy = ResumePolicy::from_config(&Config::default());
        db.upsert_playback_state("42", &state(90_000)).unwrap();
        assert!(!policy.save_state(&db, "42", "", &state(0)).unwrap());
        assert!(policy.save_state(&db, "42", "", &state(120_000)).unwrap());
        assert_eq!(
            db.playback_state("article:42").unwrap(),
            Some(state(120_000))
        );
        assert_eq!(db.playback_state("42").unwrap(), Some(state(90_000)));
        assert!(!policy.save_state(&db, "42", "", &state(0)).unwrap());
        assert_eq!(
            db.playback_state("article:42").unwrap(),
            Some(state(120_000))
        );
        assert!(policy.save_state(&db, "42", "", &state(550_000)).unwrap());
        let done = db.playback_state("article:42").unwrap().unwrap();
        assert!(done.completed);
        assert_eq!(done.position_ms, 0);
        assert!(!policy.save_state(&db, "42", "", &state(0)).unwrap());
        assert!(db.playback_state("article:42").unwrap().unwrap().completed);
    }

    #[test]
    fn load_gate_supersede_and_cancel_match_python_seq_semantics() {
        let gate = LoadGate::new();
        let first = gate.begin();
        assert!(first.is_current());
        // A newer load supersedes the old token (stale seq).
        let second = gate.begin();
        assert!(first.is_cancelled());
        assert!(second.is_current());
        // Explicit cancel hits only the current load.
        gate.cancel_current();
        assert!(second.is_cancelled());
        let third = gate.begin();
        assert!(third.is_current());
        assert_eq!(third.seq(), 3);
    }

    #[test]
    fn resolve_cancellable_returns_value_and_drops_cancelled_work() {
        let gate = LoadGate::new();
        let token = gate.begin();
        assert_eq!(resolve_cancellable(&token, || 42), Some(42));
        let token2 = gate.begin();
        let gate2 = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            gate2.cancel_current();
        });
        let started = std::time::Instant::now();
        let out = resolve_cancellable(&token2, || {
            std::thread::sleep(std::time::Duration::from_secs(30));
            7
        });
        assert_eq!(out, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn watch_next_player_bar_chapters_parse() {
        let data = serde_json::json!({
            "contents": {},
            "playerOverlays": {
                "playerOverlayRenderer": {
                    "decoratedPlayerBarRenderer": {
                        "decoratedPlayerBarRenderer": {
                            "playerBar": {
                                "chapteredPlayerBarRenderer": {
                                    "chapters": [
                                        {"chapterRenderer": {"timeRangeStartMillis": 0, "title": {"simpleText": "Intro"}}},
                                        {"chapterRenderer": {"timeRangeStartMillis": 90500, "title": {"runs": [{"text": "Middle"}]}}},
                                        {"chapterRenderer": {"timeRangeStartMillis": 180000, "title": {"simpleText": "End"}}}
                                    ]
                                }
                            }
                        }
                    }
                }
            }
        });
        let chapters = chapters_from_watch_next(&data, "abc123XYZ-_").unwrap();
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].start, 0.0);
        assert_eq!(chapters[1].start, 90.5);
        assert_eq!(chapters[1].title.as_deref(), Some("Middle"));
        assert_eq!(
            chapters[1].href.as_deref(),
            Some("https://www.youtube.com/watch?v=abc123XYZ-_&t=90s")
        );
        // No contents -> InnerTube could not settle it.
        assert!(chapters_from_watch_next(&serde_json::json!({}), "abc123XYZ-_").is_none());
    }

    #[test]
    fn watch_next_key_moments_and_late_first_chapter() {
        let data = serde_json::json!({
            "contents": {},
            "engagementPanels": [{
                "engagementPanelSectionListRenderer": {
                    "content": {
                        "macroMarkersListRenderer": {
                            "contents": [
                                {"macroMarkersListItemRenderer": {"title": {"simpleText": "Part One"}, "timeDescription": {"simpleText": "2:00"}}},
                                {"macroMarkersListItemRenderer": {"title": {"simpleText": "Part Two"}, "timeDescription": {"simpleText": "5:30"}}}
                            ]
                        }
                    }
                }
            }]
        });
        let chapters = chapters_from_watch_next(&data, "vid").unwrap();
        // The gap before the 2:00 first chapter is filled at 0.
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0].start, 0.0);
        assert_eq!(chapters[0].title.as_deref(), Some("<Untitled Chapter 1>"));
        assert_eq!(chapters[1].start, 120.0);
        assert_eq!(chapters[2].start, 330.0);
    }

    #[test]
    fn description_chapters_validate_against_duration() {
        let text = "0:00 Intro\n2:30 Middle\n9:59 Outro\n12:00 Beyond the end\nNot a chapter line";
        let chapters = chapters_from_description(text, 600.0);
        assert_eq!(
            chapters,
            vec![
                (0.0, "Intro".to_string()),
                (150.0, "Middle".to_string()),
                (599.0, "Outro".to_string())
            ]
        );
        // Title-first lines work too, and a zero duration
        // settles nothing.
        let titled = chapters_from_description("Intro 0:00\nMiddle 2:30", 600.0);
        assert_eq!(titled.len(), 2);
        assert!(chapters_from_description(text, 0.0).is_empty());
        // Out-of-order lines are sorted before validation.
        let shuffled = chapters_from_description("2:30 Middle\n0:00 Intro", 600.0);
        assert_eq!(shuffled[0], (0.0, "Intro".to_string()));
    }

    #[test]
    fn disabled_resume_never_changes_saved_progress() {
        let db = Db::open_or_create(std::path::Path::new(":memory:")).unwrap();
        let mut cfg = Config::default();
        cfg.set_path("resume_playback", serde_json::json!(false));
        let policy = ResumePolicy::from_config(&cfg);
        assert!(!policy.save_state(&db, "42", "", &state(120_000)).unwrap());
        assert_eq!(db.playback_state("article:42").unwrap(), None);
    }
}
