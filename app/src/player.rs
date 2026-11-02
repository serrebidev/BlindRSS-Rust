//! Player window (slice 7 + batch 3): wx MediaCtrl over the
//! article's media URL (direct or yt-dlp-resolved), with resume
//! via the Python playback_state table, chapter navigation with
//! announcements, playback-speed control, and segment skipping
//! (SponsorBlock for YouTube, ffmpeg silence detection).
//!
//! Position is saved every second by the window timer, so closing
//! the window at any point loses at most a second of progress.

use crate::media_backend::MediaBackend;
use blindrss_core::db::Chapter;
use blindrss_core::playback::{
    LoadGate, LoadToken, ResumePolicy, resolve_cancellable, saved_state,
};
use blindrss_core::{Article, Db, PlaybackState};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use wxdragon::prelude::*;
use wxdragon::widgets::media_ctrl::SeekMode;

type PlayerCommand = Rc<dyn Fn(&str) -> bool>;
type ResolvedMedia = (String, Vec<(String, String)>);
type QueueResult = (usize, Result<ResolvedMedia, String>);

thread_local! {
    static PLAYERS: RefCell<Vec<(usize, PlayerCommand)>> = const { RefCell::new(Vec::new()) };
}

/// Route main-window shortcuts to the most recently opened player.
pub fn dispatch_command(id: &str) -> bool {
    let action = PLAYERS.with(|players| players.borrow().last().map(|(_, action)| action.clone()));
    action.is_some_and(|action| action(id))
}

/// A range the player skips over, with its announcement label.
#[derive(Clone)]
struct SkipRange {
    start_ms: i64,
    end_ms: i64,
    label: String,
}

/// Worker results on the skip channel. SponsorBlock ranges
/// accumulate (each segment is reported once); silence ranges
/// REPLACE the previous set — the incremental scan publishes
/// its merged ranges-so-far as it goes, exactly like Python's
/// `_publish` rebinding `_silence_ranges` on every progress
/// callback, never appending to them.
enum SkipMsg {
    Sponsor(Vec<SkipRange>),
    Silence(Vec<SkipRange>),
}

struct PlayerState {
    article_id: String,
    title: String,
    db_path: PathBuf,
    article_url: String,
    resume: ResumePolicy,
    last_save: std::time::Instant,
    seeked: bool,
    saved_position_ms: i64,
    /// Skip ranges delivered by the SponsorBlock / silence
    /// workers on `skip_rx`, drained by the tick.
    skips: Vec<SkipRange>,
    /// The latest published set from the incremental silence
    /// scan (replaced wholesale on each progress publish).
    silence_skips: Vec<SkipRange>,
    skip_rx: std::sync::mpsc::Receiver<SkipMsg>,
    /// Load-cancellation gate (Python's `_active_load_seq` +
    /// abort event): beginning a load supersedes the previous
    /// load's workers; stop/close cancel the current one.
    load_gate: LoadGate,
    /// Fast InnerTube chapter results for this load, drained
    /// by the tick (empty when chapters were already known).
    chapter_rx: std::sync::mpsc::Receiver<Vec<Chapter>>,
    /// Range starts already skipped (Python's already-skipped
    /// guard: never skip the same range twice in a session).
    skipped: HashSet<i64>,
    /// Chapter index at the last tick, for crossing announcements.
    last_chapter: Option<usize>,
    /// True while the tick sets the seek slider programmatically,
    /// so the slider's change event doesn't seek in a loop.
    slider_guard: bool,
    /// Play-queue context when this player was opened from the
    /// queue: media-finished advances through the entries.
    queue: Option<QueueCtx>,
    /// Last play state the Play/Pause button label was synced
    /// to (the tick reconciles them).
    last_playing: Option<bool>,
    /// Autoplay watchdog: a wx MediaCtrl load can report
    /// success before the DirectShow graph is ready, and the
    /// immediate play() then silently no-ops — the episode
    /// sits stopped until played by hand (Brandon's log,
    /// 2026-10-31: player opened, state fell back to stopped
    /// within a second, a manual Space played fine). While
    /// active, the tick re-plays and, if the stream keeps
    /// dying, escalates direct -> proxy -> ffmpeg.
    autoplay_active: bool,
    autoplay_ticks: u8,
    /// Current fallback level for `stream` (0 direct,
    /// 1 proxy /stream, 2 ffmpeg /play).
    autoplay_level: u8,
    /// Raw stream URL of the current load (for escalation).
    stream: String,
    headers: Vec<(String, String)>,
    /// Last text written to the position readout.
    last_pos_text: String,
    /// Position at the last playback-state save (saves are
    /// throttled to real movement; opening the DB every tick
    /// was needless UI-thread I/O).
    last_saved_pos: i64,
}

/// Custom accessibility for the seek slider: a VALUE TEXT in
/// time units. NVDA reads a stock wx slider's value as a raw
/// 0-1000 number; the accessible value is what makes the
/// slider usable ("12:34 of 45:00"). Everything not
/// overridden falls back to wx's built-in behaviour
/// (wxdragon's AccessibleImpl defaults do exactly that).
/// Windows only — wxdragon's accessible module is MSAA-bound;
/// elsewhere the slider's window name (set at creation) is
/// what the platform accessibility layer exposes.
///
/// The value comes from a lock-free snapshot the UI-thread
/// tick refreshes every second — NEVER from the media
/// control itself. MSAA callbacks run on NVDA's RPC thread,
/// not the UI thread, and wx/DirectShow calls from there
/// race the UI thread's own calls on the same graph; that
/// cross-thread access is the prime suspect for the freeze
/// Brandon hit on v0.1.29 ("it eventually freezes").
#[cfg(windows)]
struct SeekAccessible {
    pos: std::sync::Arc<std::sync::atomic::AtomicI64>,
    len: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

#[cfg(windows)]
impl wxdragon::accessible::AccessibleImpl for SeekAccessible {
    fn get_value(&self, _child_id: i32) -> (wxdragon::accessible::AccStatus, Option<String>) {
        let len = self.len.load(std::sync::atomic::Ordering::Relaxed);
        if len <= 0 {
            return (wxdragon::accessible::AccStatus::NotImplemented, None);
        }
        let pos = self.pos.load(std::sync::atomic::Ordering::Relaxed);
        (
            wxdragon::accessible::AccStatus::Ok,
            Some(format!("{} of {}", fmt_ms(pos), fmt_ms(len))),
        )
    }
}

/// The play queue inside a player window (config "play_queue"
/// entries as article stubs). The main window mirrors the
/// current index in config "play_queue_index" for its own
/// Next/Previous commands.
struct QueueCtx {
    entries: Vec<Article>,
    index: usize,
    rx: std::sync::mpsc::Receiver<QueueResult>,
    tx: std::sync::mpsc::Sender<QueueResult>,
    /// A resolve for the next entry is in flight.
    loading: bool,
}

/// Mirror the queue position for the main window (its
/// queue.next / queue.prev commands start from it).
fn set_queue_index(index: i64) {
    let path = crate::config_path();
    let mut cfg = blindrss_core::config::Config::load(&path).unwrap_or_default();
    cfg.set_path("play_queue_index", serde_json::json!(index));
    let _ = cfg.save(&path);
}

fn fmt_ms(ms: i64) -> String {
    let total = (ms.max(0)) / 1000;
    format!(
        "{}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

fn fmt_speed(rate: f64) -> String {
    let s = format!("{rate:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

const SPEEDS: [f64; 9] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// End-of-media test for the saved playback row: a position within
/// the configured end threshold counts as completed. A clip shorter than
/// that threshold is NOT complete at position zero — the older
/// `pos >= len - 5000` form compared against a negative bound and
/// marked every short clip finished the moment it opened, so it
/// never resumed where the user left it.
fn reached_end(pos: i64, len: i64, threshold: i64) -> bool {
    len > 0 && pos > 0 && len.saturating_sub(pos) <= threshold
}

fn save_position(state: &PlayerState, media: &MediaBackend, force_completed: bool) {
    if !state.resume.enabled || !state.seeked && !force_completed {
        return;
    }
    let Ok(db) = Db::open(&state.db_path) else {
        return;
    };
    let pos = media.tell();
    let len = media.length();
    let completed = force_completed || reached_end(pos, len, state.resume.complete_threshold_ms);
    let _ = state.resume.save_state(
        &db,
        &state.article_id,
        &state.article_url,
        &PlaybackState {
            position_ms: pos,
            duration_ms: if len > 0 { Some(len) } else { None },
            completed,
            seek_supported: None,
            title: Some(state.title.clone()),
        },
    );
}

fn cfg_f64(cfg: &blindrss_core::config::Config, key: &str, default: f64) -> f64 {
    cfg.raw()
        .get(key)
        .and_then(|v| v.as_f64())
        .unwrap_or(default)
}

fn chapter_index(starts: &[i64], pos_ms: i64) -> Option<usize> {
    starts.iter().rposition(|s| *s <= pos_ms)
}

fn chapter_label(chapters: &[Chapter], idx: usize) -> String {
    chapters
        .get(idx)
        .and_then(|c| c.title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Chapter {}", idx + 1))
}

/// Spawn the fast InnerTube chapter worker for a YouTube
/// article whose chapters are not already known, returning
/// the channel its result arrives on. Python's player gets
/// YouTube chapters through `extract_chapters`, which tries
/// one watch-next request (~0.7 s) before the yt-dlp CLI
/// (3-4 s); core's `fast_youtube_chapters` is that fast path.
/// When it cannot settle the chapters it returns None and the
/// player simply keeps having none — the slow path already
/// ran before the player opened (the main window's info
/// fetch) and is not repeated here. A cancelled/superseded
/// load drops its result.
fn spawn_chapter_worker(
    article: &Article,
    token: LoadToken,
) -> std::sync::mpsc::Receiver<Vec<Chapter>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<Chapter>>();
    let target = if blindrss_core::ytfull::is_youtube_video_url(&article.url) {
        Some(article.url.clone())
    } else {
        article
            .media_url
            .clone()
            .filter(|u| blindrss_core::ytfull::is_youtube_video_url(u))
    };
    if let Some(target) = target {
        std::thread::spawn(move || {
            if token.is_cancelled() {
                return;
            }
            if let Some(chapters) = blindrss_core::playback::fast_youtube_chapters(&target, 20)
                && !chapters.is_empty()
                && token.is_current()
            {
                let _ = tx.send(chapters);
            }
        });
    }
    rx
}

/// An idle chapter channel (chapters were already known, or
/// the article is not a YouTube video).
fn no_chapter_worker() -> std::sync::mpsc::Receiver<Vec<Chapter>> {
    let (_tx, rx) = std::sync::mpsc::channel::<Vec<Chapter>>();
    rx
}

/// Spawn the SponsorBlock and silence workers for one article
/// and return the channel their skip ranges arrive on. Called
/// at open and again on every queue advance. Both workers
/// carry the load's token: a stop, switch or close cancels
/// them (Python sets the silence scan's abort event and drops
/// stale-sequence results the same way).
fn spawn_skip_workers(
    cfg: &blindrss_core::config::Config,
    article: &Article,
    stream_url: &str,
    headers: &[(String, String)],
    token: LoadToken,
    position: std::sync::Arc<std::sync::atomic::AtomicI64>,
) -> std::sync::mpsc::Receiver<SkipMsg> {
    let (skip_tx, skip_rx) = std::sync::mpsc::channel::<SkipMsg>();
    if cfg.get_bool("sponsorblock_enabled", true) {
        let cats: Vec<String> = cfg
            .raw()
            .get("sponsorblock_categories")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| {
                ["sponsor", "selfpromo", "interaction"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            });
        let vid = blindrss_core::sponsorblock::youtube_video_id(&article.url).or_else(|| {
            article
                .media_url
                .as_deref()
                .and_then(blindrss_core::sponsorblock::youtube_video_id)
        });
        if let Some(vid) = vid {
            let tx = skip_tx.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                if let Ok(segs) = blindrss_core::sponsorblock::fetch_segments(&vid, &cats)
                    && token.is_current()
                {
                    let ranges = segs
                        .iter()
                        .map(|s| SkipRange {
                            start_ms: (s.start * 1000.0) as i64,
                            end_ms: (s.end * 1000.0) as i64,
                            label: format!("Skipped {}", s.category),
                        })
                        .collect();
                    let _ = tx.send(SkipMsg::Sponsor(ranges));
                }
            });
        }
    }
    if cfg.get_bool("skip_silence", true)
        && let Some(ffmpeg) = blindrss_core::silence::find_ffmpeg()
    {
        // Incremental scan (Python's player silence worker):
        // paced to stay `silence_scan_lead_seconds` ahead of
        // the listener, publishing ranges as it goes. The
        // position comes from the tick's lock-free snapshot —
        // never from the media control on a worker thread.
        let pad_ms = cfg.get_i64("silence_skip_padding_ms", 60);
        let mut options = blindrss_core::silence::SilenceScanOptions {
            window_ms: cfg.get_i64("silence_skip_window_ms", 25),
            min_silence_ms: cfg.get_i64("silence_skip_min_ms", 700),
            threshold_db: cfg_f64(cfg, "silence_skip_threshold_db", -38.0),
            merge_gap_ms: cfg.get_i64("silence_skip_merge_gap_ms", 260),
            lead_ms: cfg.get_i64("silence_scan_lead_seconds", 300) * 1000,
            ..Default::default()
        };
        options = options.for_remote(blindrss_core::silence::is_remote_source(stream_url));
        let merge_gap_ms = options.merge_gap_ms;
        let url = stream_url.to_string();
        let headers_vec: Vec<(String, String)> = headers.to_vec();
        let tx = skip_tx.clone();
        std::thread::spawn(move || {
            let publish = |ranges_ms: Vec<(i64, i64)>| -> Vec<SkipRange> {
                blindrss_core::silence::publish_ranges_ms(&ranges_ms, pad_ms, merge_gap_ms)
                    .iter()
                    .map(|(s, e)| SkipRange {
                        start_ms: *s,
                        end_ms: *e,
                        label: "Skipped silence".to_string(),
                    })
                    .collect()
            };
            let progress_tx = tx.clone();
            let progress = move |ranges_ms: Vec<(i64, i64)>| {
                let _ = progress_tx.send(SkipMsg::Silence(publish(ranges_ms)));
            };
            // A failed or cancelled scan keeps whatever its
            // progress callbacks already published (Python
            // keeps the published ranges on a mid-scan drop);
            // only a clean finish replaces them with the
            // final set.
            if let Ok(ranges) = blindrss_core::silence::scan_silence_incremental(
                &url,
                &ffmpeg,
                &options,
                &headers_vec,
                &token,
                Some(&position),
                &progress,
            ) {
                let _ = tx.send(SkipMsg::Silence(publish(ranges)));
            }
        });
    }

    skip_rx
}

pub fn open_player(
    _parent: &Frame,
    article: Article,
    stream_url: String,
    chapters: Vec<Chapter>,
    headers: Vec<(String, String)>,
) {
    open_player_impl(_parent, article, stream_url, chapters, None, headers);
}

/// Open the player on one queue entry, advancing through the
/// rest of the queue as each entry finishes.
pub fn open_player_queue(
    _parent: &Frame,
    article: Article,
    stream_url: String,
    entries: Vec<Article>,
    index: usize,
    headers: Vec<(String, String)>,
) {
    set_queue_index(index as i64);
    let chapters = Db::open(&crate::db_path())
        .ok()
        .and_then(|db| db.chapters(&article.id).ok())
        .unwrap_or_default();
    open_player_impl(
        _parent,
        article,
        stream_url,
        chapters,
        Some((entries, index)),
        headers,
    );
}

/// Persist the player volume (Python config `volume`: int
/// percent 0-100, default 100, saved whenever it changes).
fn persist_volume(volume: f64) {
    let mut cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    cfg.set_path("volume", serde_json::json!((volume * 100.0).round() as i64));
    let _ = cfg.save(&crate::config_path());
}

/// When the equalizer is enabled with a non-flat shape, route
/// playback through the local EQ stream (MediaCtrl exposes no
/// EQ; the stream is an ffmpeg transcode with the filter
/// chain applied — seeking inside it is limited). Otherwise
/// the URL passes through untouched.
fn eq_wrap(url: &str, cfg: &blindrss_core::config::Config, headers: &[(String, String)]) -> String {
    let eq = blindrss_core::equalizer::EqConfig::from_config(cfg);
    if eq.enabled && (eq.bands.iter().any(|g| *g != 0.0) || eq.effective_preamp_db() != 0.0) {
        let chain = blindrss_core::equalizer::ffmpeg_filter_chain(&eq);
        if !chain.is_empty() {
            return blindrss_core::proxy::eq_url(
                &blindrss_core::http_headers::protected_media_url(url, headers),
                &chain,
            );
        }
    }
    url.to_string()
}

/// Load `url` into the media control, falling back when the
/// native backend cannot open it. wx MediaCtrl's Windows
/// backend (DirectShow) routinely refuses podcast CDN URLs —
/// redirect chains, modern TLS, odd content types — that
/// libVLC (Python's engine) opens without trouble. Fallback
/// order: the raw URL, then the same bytes fetched by the
/// app's own HTTP stack and streamed over the local proxy,
/// then a live ffmpeg transcode of the URL. Returns which
/// path loaded: "" direct, "proxy", "ffmpeg", or "failed".
fn load_with_fallbacks(
    media: &MediaBackend,
    url: &str,
    headers: &[(String, String)],
) -> &'static str {
    // The primary load goes through the range cache when the
    // Python gate says so (Batch 36): it serves a truthful
    // Content-Length and real 206 ranges from a disk cache, so
    // the backend neither clamps seeks to a wrong duration nor
    // re-streams bytes it already fetched.
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    let primary = blindrss_core::rangecache::maybe_range_cache_url(url, headers, false, None, &cfg);
    let primary_headers = if primary == url { headers } else { &[] };
    if media.load_uri_with_headers(&primary, primary_headers) {
        return "";
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let protected = blindrss_core::http_headers::protected_media_url(url, headers);
        if media.load_uri(&blindrss_core::proxy::stream_url(&protected)) {
            return "proxy";
        }
        if blindrss_core::silence::find_ffmpeg().is_some()
            && media.load_uri(&blindrss_core::proxy::play_url(&protected))
        {
            return "ffmpeg";
        }
    }
    "failed"
}

fn open_player_impl(
    _parent: &Frame,
    article: Article,
    stream_url: String,
    chapters: Vec<Chapter>,
    queue: Option<(Vec<Article>, usize)>,
    headers: Vec<(String, String)>,
) {
    let db_path = crate::db_path();
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    let resume = ResumePolicy::from_config(&cfg);
    let saved = Db::open(&db_path)
        .ok()
        .and_then(|db| saved_state(&db, &article.id, &article.url).ok().flatten())
        .map(|s| resume.resume_target(&s, &stream_url))
        .unwrap_or(0);

    // Player knobs from config (Python keys/defaults).
    let seek_back_ms = cfg.get_i64("seek_back_ms", 10_000);
    let seek_fwd_ms = cfg.get_i64("seek_forward_ms", seek_back_ms);
    let volume_step = (cfg.get_i64("volume_step", 5) as f64 / 100.0).clamp(0.01, 0.5);
    // Persisted volume (Python `volume`: int percent, default 100).
    let start_volume = (cfg.get_i64("volume", 100) as f64 / 100.0).clamp(0.0, 1.0);
    let start_speed = cfg_f64(&cfg, "playback_speed", 1.0);
    let show_player = cfg.get_bool("show_player_on_play", true);

    let mut chapters = chapters;
    chapters.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let starts: Vec<i64> = chapters.iter().map(|c| (c.start * 1000.0) as i64).collect();
    // Shared with every handler so a queue switch (which clears
    // the chapters, like Python's load_media([])) is seen by the
    // navigation buttons and the tick alike.
    let chapters_live = Rc::new(RefCell::new((chapters.clone(), starts.clone())));

    let frame = Frame::builder()
        .with_title(&format!("Playing: {}", article.title))
        .with_size(Size::new(560, 460))
        .build();
    let media = MediaBackend::new(&frame, &cfg);
    let play_btn = Button::builder(&frame)
        .with_label(&crate::tr("Play"))
        .build();
    let stop_btn = Button::builder(&frame)
        .with_label(&crate::tr("Stop"))
        .build();
    let back_btn = Button::builder(&frame)
        .with_label(&format!("Back {}s", seek_back_ms / 1000))
        .build();
    let fwd_btn = Button::builder(&frame)
        .with_label(&format!("Forward {}s", seek_fwd_ms / 1000))
        .build();
    let vdown_btn = Button::builder(&frame)
        .with_label(&crate::tr("Volume Down"))
        .build();
    let vup_btn = Button::builder(&frame)
        .with_label(&crate::tr("Volume Up"))
        .build();
    let slower_btn = Button::builder(&frame)
        .with_label(&crate::tr("Slower"))
        .build();
    let faster_btn = Button::builder(&frame)
        .with_label(&crate::tr("Faster"))
        .build();
    let prev_ch_btn = Button::builder(&frame)
        .with_label(&crate::tr("Previous Chapter"))
        .build();
    let next_ch_btn = Button::builder(&frame)
        .with_label(&crate::tr("Next Chapter"))
        .build();
    // Position readout as a read-only text control, not a
    // static text: it is focusable, so NVDA can land on it and
    // read the position on demand.
    let pos_label = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::ReadOnly)
        .build();
    pos_label.set_value(&crate::tr("0:00:00 / 0:00:00"));
    // Position snapshot for the seek slider's accessible
    // value: the tick (UI thread) stores, the MSAA callback
    // (NVDA's thread) only loads. See SeekAccessible.
    let seek_pos = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let seek_len = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let seek_slider = Slider::builder(&frame).build();
    seek_slider.set_range(0, 1000);
    #[cfg(windows)]
    seek_slider.set_accessible(wxdragon::accessible::Accessible::new(
        &seek_slider,
        SeekAccessible {
            pos: seek_pos.clone(),
            len: seek_len.clone(),
        },
    ));
    let chapter_list = ListBox::builder(&frame).build();
    // Accessible names: NVDA reads a bare slider as just
    // "slider" and a bare list box as just "list" — the names
    // are what a screen reader announces for them (Python's
    // player labels every control the same way).
    crate::msaa_names::set_name(&seek_slider, &crate::tr("Seek"));
    crate::msaa_names::set_name(&chapter_list, &crate::tr("Chapters"));
    crate::msaa_names::set_name(&pos_label, &crate::tr("Position"));
    crate::msaa_names::set_name(
        &back_btn,
        &format!("{} {}s", crate::tr("Rewind"), seek_back_ms / 1000),
    );
    crate::msaa_names::set_name(
        &fwd_btn,
        &format!("{} {}s", crate::tr("Fast Forward"), seek_fwd_ms / 1000),
    );
    let seek_caption = StaticText::builder(&frame)
        .with_label(&crate::tr("Seek"))
        .build();
    let chapters_caption = StaticText::builder(&frame)
        .with_label(&crate::tr("Chapters"))
        .build();
    for ch in &chapters {
        let t = ch.title.as_deref().unwrap_or("Chapter");
        chapter_list.append(&format!("{} - {}", fmt_ms((ch.start * 1000.0) as i64), t));
    }

    let row = BoxSizer::builder(Orientation::Horizontal).build();
    row.add(&play_btn, 0, SizerFlag::All, 4);
    row.add(&stop_btn, 0, SizerFlag::All, 4);
    row.add(&back_btn, 0, SizerFlag::All, 4);
    row.add(&fwd_btn, 0, SizerFlag::All, 4);
    row.add(&vdown_btn, 0, SizerFlag::All, 4);
    row.add(&vup_btn, 0, SizerFlag::All, 4);
    let row2 = BoxSizer::builder(Orientation::Horizontal).build();
    row2.add(&slower_btn, 0, SizerFlag::All, 4);
    row2.add(&faster_btn, 0, SizerFlag::All, 4);
    row2.add(&prev_ch_btn, 0, SizerFlag::All, 4);
    row2.add(&next_ch_btn, 0, SizerFlag::All, 4);
    let col = BoxSizer::builder(Orientation::Vertical).build();
    col.add(&media.window(), 1, SizerFlag::Expand | SizerFlag::All, 4);
    col.add_sizer(&row, 0, SizerFlag::Expand, 0);
    col.add_sizer(&row2, 0, SizerFlag::Expand, 0);
    col.add(&pos_label, 0, SizerFlag::All, 4);
    col.add(&seek_caption, 0, SizerFlag::All, 4);
    col.add(&seek_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    col.add(&chapters_caption, 0, SizerFlag::All, 4);
    col.add(&chapter_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    frame.set_sizer(col, true);
    frame.create_status_bar(1, 0, -1, "player status");
    frame.set_status_text(&article.title, 0);

    // Skip-range workers: SponsorBlock (YouTube) and the
    // incremental ffmpeg silence scan report ranges over one
    // channel; the tick drains and applies them. This open is
    // the window's first load: its token governs every worker
    // until a queue advance supersedes it or stop/close
    // cancels it.
    let load_gate = LoadGate::new();
    let load_token = load_gate.begin();
    let skip_rx = spawn_skip_workers(
        &cfg,
        &article,
        &stream_url,
        &headers,
        load_token.clone(),
        seek_pos.clone(),
    );
    let chapter_rx = if chapters.is_empty() {
        spawn_chapter_worker(&article, load_token)
    } else {
        no_chapter_worker()
    };

    let state = Rc::new(RefCell::new(PlayerState {
        article_id: article.id.clone(),
        title: article.title.clone(),
        db_path,
        article_url: article.url.clone(),
        resume,
        last_save: std::time::Instant::now(),
        seeked: saved <= 0,
        saved_position_ms: saved,
        skips: Vec::new(),
        silence_skips: Vec::new(),
        skip_rx,
        load_gate,
        chapter_rx,
        skipped: HashSet::new(),
        last_chapter: None,
        slider_guard: false,
        last_playing: None,
        autoplay_active: false,
        autoplay_ticks: 0,
        autoplay_level: 0,
        stream: String::new(),
        headers: headers.clone(),
        last_pos_text: String::new(),
        last_saved_pos: -1,
        queue: queue.map(|(entries, index)| {
            let (tx, rx) = std::sync::mpsc::channel();
            QueueCtx {
                entries,
                index,
                rx,
                tx,
                loading: false,
            }
        }),
    }));

    let wrapped = if media.uses_vlc() {
        stream_url.clone()
    } else {
        eq_wrap(&stream_url, &cfg, &headers)
    };
    // An EQ-wrapped URL is already a local ffmpeg stream (its
    // own network path is ffmpeg's), so it loads directly;
    // anything else goes through the fallback chain.
    let via = if wrapped != stream_url {
        if media.load_uri(&wrapped) {
            ""
        } else {
            "failed"
        }
    } else {
        load_with_fallbacks(&media, &stream_url, &headers)
    };
    if via != "failed" {
        media.set_volume(start_volume);
        media.play();
        play_btn.set_label(&crate::tr("Pause"));
        if (start_speed - 1.0).abs() > 0.001 {
            media.set_playback_rate(start_speed);
        }
        frame.set_status_text(
            match via {
                "proxy" => "Playing (via local proxy)",
                "ffmpeg" => "Playing (via ffmpeg)",
                _ => "Playing",
            },
            0,
        );
        // Arm the autoplay watchdog: load success is not
        // playback (see PlayerState::autoplay_active).
        let mut s = state.borrow_mut();
        s.stream = stream_url.clone();
        s.autoplay_level = match via {
            "proxy" => 1,
            "ffmpeg" => 2,
            _ if wrapped != stream_url => 2,
            _ => 0,
        };
        s.autoplay_ticks = 0;
        s.autoplay_active = true;
    } else {
        frame.set_status_text("Could not load media", 0);
        crate::announce("general", "Could not load media");
    }

    // Queue advance: media finished -> resolve the next queue
    // entry on a worker; the tick performs the switch.
    {
        let st = state.clone();
        media.on_finished(move |_| {
            let mut s = st.borrow_mut();
            save_position(&s, &media, true);
            let Some(q) = s.queue.as_mut() else { return };
            if q.index + 1 >= q.entries.len() {
                set_queue_index(-1);
                return;
            }
            if q.loading {
                return;
            }
            q.loading = true;
            let next = q.index + 1;
            let entry = q.entries[next].clone();
            let tx = q.tx.clone();
            let path = s.db_path.clone();
            // The advance is a new load: it supersedes the
            // finished entry's workers, and a stop/close (or a
            // newer advance) cancels this resolve — its result
            // is then dropped, like Python's stale load seq.
            let token = s.load_gate.begin();
            drop(s);
            std::thread::spawn(move || {
                let result = resolve_cancellable(&token, move || {
                    let cfg = blindrss_core::config::Config::load(&crate::config_path())
                        .unwrap_or_default();
                    blindrss_core::media::resolve_article_for_play(&cfg, &entry).map(|r| {
                        let headers = crate::playback_headers_for(&entry, &r.url, &path);
                        (
                            crate::maybe_cache_youtube(&entry, &r.url, r.via_ytdlp),
                            headers,
                        )
                    })
                });
                let result = match result {
                    Some(result) => result,
                    None => Err("cancelled".to_string()),
                };
                let _ = tx.send((next, result));
            });
        });
    }

    let m = media;
    let st_pp = state.clone();
    let play_pause: Rc<dyn Fn()> = Rc::new(move || {
        if m.get_state() == MediaState::Playing {
            m.pause();
            save_position(&st_pp.borrow(), &m, false);
            // A deliberate pause/stop cancels the autoplay
            // watchdog — it must never fight the user.
            st_pp.borrow_mut().autoplay_active = false;
            crate::announce("general", "Paused");
        } else {
            m.play();
            crate::announce("general", "Playing");
        }
    });
    let pp = play_pause.clone();
    play_btn.on_click(move |_| pp());
    let m = media;
    let st = state.clone();
    let stop: Rc<dyn Fn()> = Rc::new(move || {
        save_position(&st.borrow(), &m, false);
        m.stop();
        // Stop cancels the in-flight load's workers (the
        // silence scan's ffmpeg, a pending queue resolve) and
        // drops their ranges, like Python's on_stop calling
        // _cancel_silence_scan. A later queue advance begins
        // a fresh load on the gate.
        let mut s = st.borrow_mut();
        s.autoplay_active = false;
        s.load_gate.cancel_current();
        s.skips.clear();
        s.silence_skips.clear();
        s.skipped.clear();
        if let Some(q) = s.queue.as_mut() {
            q.loading = false;
        }
    });
    let stop_click = stop.clone();
    stop_btn.on_click(move |_| stop_click());
    // Transport closures shared by the buttons and the
    // main-window/tray dispatch (`player.rewind` and friends,
    // Batch 41a addendum): one implementation, Python's steps
    // from config (seek_back_ms / seek_forward_ms /
    // volume_step, read at the top of this function).
    let rewind: Rc<dyn Fn()> = {
        let m = media;
        Rc::new(move || {
            m.seek(-seek_back_ms, SeekMode::FromCurrent);
        })
    };
    let forward: Rc<dyn Fn()> = {
        let m = media;
        Rc::new(move || {
            m.seek(seek_fwd_ms, SeekMode::FromCurrent);
        })
    };
    let volume_down: Rc<dyn Fn()> = {
        let m = media;
        Rc::new(move || {
            let v = (m.get_volume() - volume_step).max(0.0);
            m.set_volume(v);
            persist_volume(v);
            crate::announce(
                "general",
                &format!("Volume {}%", (v * 100.0).round() as i64),
            );
        })
    };
    let volume_up: Rc<dyn Fn()> = {
        let m = media;
        Rc::new(move || {
            let v = (m.get_volume() + volume_step).min(1.0);
            m.set_volume(v);
            persist_volume(v);
            crate::announce(
                "general",
                &format!("Volume {}%", (v * 100.0).round() as i64),
            );
        })
    };
    {
        let action = rewind.clone();
        back_btn.on_click(move |_| action());
    }
    {
        let action = forward.clone();
        fwd_btn.on_click(move |_| action());
    }
    {
        let action = volume_down.clone();
        vdown_btn.on_click(move |_| action());
    }
    {
        let action = volume_up.clone();
        vup_btn.on_click(move |_| action());
    }
    // Speed stepping through the standard ladder, announced.
    let change_speed: Rc<dyn Fn(i32)> = {
        let m = media;
        let frame_sp = frame;
        Rc::new(move |dir| {
            let cur = m.get_playback_rate();
            let idx = SPEEDS
                .iter()
                .position(|s| (*s - cur).abs() < 0.01)
                .unwrap_or(2);
            let next = (idx as i32 + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize;
            let rate = if dir == 0 { 1.0 } else { SPEEDS[next] };
            if (rate - cur).abs() > 0.001 {
                m.set_playback_rate(rate);
                let mut cfg =
                    blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
                cfg.set_path("playback_speed", serde_json::json!(rate));
                let _ = cfg.save(&crate::config_path());
                let label = format!("Speed {}x", fmt_speed(rate));
                frame_sp.set_status_text(&label, 0);
                crate::announce("playback_speed", &label);
            }
        })
    };
    for (btn, dir) in [(slower_btn, -1i32), (faster_btn, 1i32)] {
        let speed = change_speed.clone();
        btn.on_click(move |_| speed(dir));
    }
    // Chapter navigation, announced via media_navigation.
    {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        prev_ch_btn.on_click(move |_| {
            let (chapters_nav, starts_nav) = chapters_live_c.borrow().clone();
            let pos = m.tell();
            let Some(idx) = chapter_index(&starts_nav, pos) else {
                frame_ch.set_status_text("No chapters", 0);
                return;
            };
            // Deeper than 2s into a chapter, Prev restarts it;
            // within the first 2s, Prev goes to the chapter
            // before (or restarts chapter 0).
            let target = if pos - starts_nav[idx] > 2_000 {
                idx
            } else {
                idx.saturating_sub(1)
            };
            m.seek(starts_nav[target], SeekMode::FromStart);
            m.play();
            let label = chapter_label(&chapters_nav, target);
            frame_ch.set_status_text(&label, 0);
            crate::announce("media_navigation", &label);
        });
    }
    {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        next_ch_btn.on_click(move |_| {
            let (chapters_nav, starts_nav) = chapters_live_c.borrow().clone();
            let pos = m.tell();
            let next = chapter_index(&starts_nav, pos).map(|i| i + 1).unwrap_or(0);
            if next >= starts_nav.len() {
                frame_ch.set_status_text("No next chapter", 0);
                return;
            }
            m.seek(starts_nav[next], SeekMode::FromStart);
            m.play();
            let label = chapter_label(&chapters_nav, next);
            frame_ch.set_status_text(&label, 0);
            crate::announce("media_navigation", &label);
        });
    }
    // Seek to the chapter selected in the list — shared by
    // double-click and by Enter in the list (a screen reader
    // user cannot double-click; the list must be fully
    // keyboard-operable).
    let chapter_seek: Rc<dyn Fn()> = {
        let m = media;
        let chapters_live_dl = chapters_live.clone();
        Rc::new(move || {
            let (chapters_dl, starts_dl) = chapters_live_dl.borrow().clone();
            if let Some(sel) = chapter_list.get_selection()
                && let Some(ms) = starts_dl.get(sel as usize)
            {
                m.seek(*ms, SeekMode::FromStart);
                m.play();
                crate::announce(
                    "media_navigation",
                    &chapter_label(&chapters_dl, sel as usize),
                );
            }
        })
    };
    {
        let cs = chapter_seek.clone();
        chapter_list.on_item_double_clicked(move |_| cs());
    }

    // Player keyboard control (the window had none: every
    // action needed Tab + Space on a button). Keys are left
    // native where a control already owns them — Space/Enter
    // on buttons, arrows on the seek slider and chapter list.
    {
        let frame_k = frame;
        let m = media;
        let st_k = state.clone();
        let btn_handles: Vec<usize> = [
            play_btn,
            stop_btn,
            back_btn,
            fwd_btn,
            vdown_btn,
            vup_btn,
            slower_btn,
            faster_btn,
            prev_ch_btn,
            next_ch_btn,
        ]
        .iter()
        .map(|b| b.get_handle() as usize)
        .collect();
        let slider_h = seek_slider.get_handle() as usize;
        let list_h = chapter_list.get_handle() as usize;
        frame.bind_internal(EventType::CHAR_HOOK, move |ev| {
            if ev.control_down() || ev.cmd_down() || ev.alt_down() {
                ev.skip(true);
                return;
            }
            let code = ev.get_key_code().unwrap_or(0);
            let focused = ev
                .get_event_object()
                .map(|w| w.get_handle() as usize)
                .unwrap_or(0);
            let on_button = btn_handles.contains(&focused);
            let on_slider = focused == slider_h;
            let on_list = focused == list_h;
            match code {
                // Escape closes the player.
                27 => {
                    frame_k.close(false);
                    ev.skip(false);
                }
                // Enter in the chapter list seeks to the chapter.
                13 if on_list => {
                    chapter_seek();
                    ev.skip(false);
                }
                // Space toggles play/pause anywhere a button
                // does not already own it.
                32 if !on_button => {
                    if m.get_state() == MediaState::Playing {
                        m.pause();
                        st_k.borrow_mut().autoplay_active = false;
                        crate::announce("general", "Paused");
                    } else {
                        m.play();
                        crate::announce("general", "Playing");
                    }
                    ev.skip(false);
                }
                // Left/Right seek, Up/Down volume — outside the
                // controls that use arrows themselves.
                314 if !on_slider && !on_list => {
                    m.seek(-seek_back_ms, SeekMode::FromCurrent);
                    ev.skip(false);
                }
                316 if !on_slider && !on_list => {
                    m.seek(seek_fwd_ms, SeekMode::FromCurrent);
                    ev.skip(false);
                }
                315 if !on_slider && !on_list => {
                    let v = (m.get_volume() + volume_step).min(1.0);
                    m.set_volume(v);
                    persist_volume(v);
                    crate::announce(
                        "general",
                        &format!("Volume {}%", (v * 100.0).round() as i64),
                    );
                    ev.skip(false);
                }
                317 if !on_slider && !on_list => {
                    let v = (m.get_volume() - volume_step).max(0.0);
                    m.set_volume(v);
                    persist_volume(v);
                    crate::announce(
                        "general",
                        &format!("Volume {}%", (v * 100.0).round() as i64),
                    );
                    ev.skip(false);
                }
                _ => ev.skip(true),
            }
        });
    }

    // Seek slider: user changes seek proportionally; the tick's
    // own updates are guarded out of this handler.
    {
        let m = media;
        let st = state.clone();
        seek_slider.on_slider(move |ev| {
            if st.borrow().slider_guard {
                return;
            }
            let len = m.length();
            if len > 0 {
                let target = len * ev.get_value() as i64 / 1000;
                m.seek(target, SeekMode::FromStart);
            }
        });
    }

    // 1-second tick: drain skip ranges, resume seek once length
    // is known, apply skips, announce chapter crossings, update
    // the position label, and save playback state.
    let timer = Box::leak(Box::new(Timer::new(&frame)));
    let m = media;
    let st = state.clone();
    let frame_tick = frame;
    let chapters_tick = chapters_live.clone();
    let seek_pos_t = seek_pos.clone();
    let seek_len_t = seek_len.clone();
    timer.on_tick(move |_| {
        m.tick();
        let len = m.length();
        let pos = m.tell();
        // Refresh the seek slider's accessible-value snapshot
        // (the only writer; MSAA callbacks only read it).
        seek_pos_t.store(pos, std::sync::atomic::Ordering::Relaxed);
        seek_len_t.store(len, std::sync::atomic::Ordering::Relaxed);
        let mut seek_to: Option<(i64, String)> = None;
        let mut queue_load = None;
        let mut queue_error: Option<String> = None;
        let mut chapter_apply = None;
        {
            let mut s = st.borrow_mut();
            // Queue advance: a resolved next entry switches the
            // loaded media (frame title + chapters follow). The
            // queue is taken out of the state while processed so
            // the rest of the state stays mutable.
            if let Some(mut q) = s.queue.take() {
                let mut msgs = Vec::new();
                while let Ok(msg) = q.rx.try_recv() {
                    msgs.push(msg);
                }
                for (idx, res) in msgs {
                    if idx != q.index + 1 || !q.loading {
                        continue;
                    }
                    q.loading = false;
                    match res {
                        Ok((stream, headers)) => {
                            q.index = idx;
                            let entry = q.entries[idx].clone();
                            s.article_id = entry.id.clone();
                            s.article_url = entry.url.clone();
                            s.title = entry.title.clone();
                            s.skips.clear();
                            s.silence_skips.clear();
                            s.skipped.clear();
                            s.last_chapter = None;
                            let cfg2 = blindrss_core::config::Config::load(&crate::config_path())
                                .unwrap_or_default();
                            s.resume = ResumePolicy::from_config(&cfg2);
                            s.last_save = std::time::Instant::now();
                            s.last_saved_pos = -1;
                            // The resolve's token is still the
                            // current load (a stop would have
                            // cancelled the result away), so the
                            // new entry's workers inherit it.
                            let token = s.load_gate.current();
                            s.skip_rx = spawn_skip_workers(
                                &cfg2,
                                &entry,
                                &stream,
                                &headers,
                                token.clone(),
                                seek_pos_t.clone(),
                            );
                            s.saved_position_ms = Db::open(&s.db_path)
                                .ok()
                                .and_then(|db| {
                                    saved_state(&db, &entry.id, &entry.url).ok().flatten()
                                })
                                .map(|saved| s.resume.resume_target(&saved, &stream))
                                .unwrap_or(0);
                            s.seeked = s.saved_position_ms <= 0;
                            let mut chapters = Db::open(&s.db_path)
                                .ok()
                                .and_then(|db| db.chapters(&entry.id).ok())
                                .unwrap_or_default();
                            chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
                            let starts =
                                chapters.iter().map(|c| (c.start * 1000.0) as i64).collect();
                            chapter_list.clear();
                            for chapter in &chapters {
                                chapter_list.append(&format!(
                                    "{} - {}",
                                    fmt_ms((chapter.start * 1000.0) as i64),
                                    chapter.title.as_deref().unwrap_or("Chapter")
                                ));
                            }
                            let entry_has_chapters = !chapters.is_empty();
                            *chapters_tick.borrow_mut() = (chapters, starts);
                            s.chapter_rx = if entry_has_chapters {
                                no_chapter_worker()
                            } else {
                                spawn_chapter_worker(&entry, token)
                            };
                            set_queue_index(idx as i64);
                            let wrapped = if m.uses_vlc() {
                                stream.clone()
                            } else {
                                eq_wrap(&stream, &cfg2, &headers)
                            };
                            queue_load = Some((wrapped, stream, entry.title.clone(), headers));
                        }
                        // A cancelled resolve is silent: the
                        // user stopped or switched on purpose.
                        Err(e) if e == "cancelled" => {}
                        Err(e) => queue_error = Some(e),
                    }
                }
                s.queue = Some(q);
            }
            while let Ok(msg) = s.skip_rx.try_recv() {
                match msg {
                    SkipMsg::Sponsor(ranges) => s.skips.extend(ranges),
                    SkipMsg::Silence(ranges) => s.silence_skips = ranges,
                }
            }
            // Fast InnerTube chapters: applied only while the
            // load still has none (stored/yt-dlp chapters win),
            // then persisted to the chapters table like the
            // main window's info path does.
            while let Ok(chapters) = s.chapter_rx.try_recv() {
                if !chapters.is_empty() && chapters_tick.borrow().0.is_empty() {
                    chapter_apply = Some((s.article_id.clone(), s.db_path.clone(), chapters));
                }
            }
            if !s.seeked && len > 0 && s.saved_position_ms > 0 {
                m.seek(s.saved_position_ms, SeekMode::FromStart);
                s.seeked = true;
            }
            let hit = s
                .skips
                .iter()
                .chain(s.silence_skips.iter())
                .find(|r| {
                    pos >= r.start_ms && pos < r.end_ms - 250 && !s.skipped.contains(&r.start_ms)
                })
                .map(|r| (r.start_ms, r.end_ms, r.label.clone()));
            if let Some((start, end, label)) = hit {
                s.skipped.insert(start);
                seek_to = Some((end, label));
            }
            // Chapter crossing announcements while playing.
            let (chapters_now, starts_now) = chapters_tick.borrow().clone();
            if m.get_state() == MediaState::Playing
                && let Some(cur) = chapter_index(&starts_now, pos)
                && s.last_chapter != Some(cur)
            {
                if s.last_chapter.is_some() {
                    crate::announce("media_navigation", &chapter_label(&chapters_now, cur));
                }
                s.last_chapter = Some(cur);
            }
        }
        if let Some((end, label)) = seek_to {
            m.seek(end, SeekMode::FromStart);
            frame_tick.set_status_text(&label, 0);
            crate::announce("sponsor_skip", &label);
        }
        if let Some((wrapped, raw, title, headers)) = queue_load {
            let via = if wrapped != raw {
                if m.load_uri(&wrapped) { "" } else { "failed" }
            } else {
                load_with_fallbacks(&m, &raw, &headers)
            };
            if via != "failed" {
                m.play();
                {
                    let mut s = st.borrow_mut();
                    s.stream = raw.clone();
                    s.headers = headers;
                    s.autoplay_level = match via {
                        "proxy" => 1,
                        "ffmpeg" => 2,
                        _ if wrapped != raw => 2,
                        _ => 0,
                    };
                    s.autoplay_ticks = 0;
                    s.autoplay_active = true;
                }
                // A fresh load can reset the backend volume; the
                // persisted level is the one in the config.
                let cfg_q =
                    blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
                m.set_volume((cfg_q.get_i64("volume", 100) as f64 / 100.0).clamp(0.0, 1.0));
                frame_tick.set_title(&format!("Playing: {title}"));
                frame_tick.set_status_text(&format!("Queue: {title}"), 0);
                crate::announce("general", &format!("Now playing: {title}"));
            }
        }
        if let Some(e) = queue_error {
            frame_tick.set_status_text(&format!("Queue: {e}"), 0);
            crate::announce("general", &format!("Queue: {e}"));
        }
        if let Some((article_id, db_path, mut chapters)) = chapter_apply {
            chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
            if let Ok(db) = Db::open(&db_path) {
                let _ = db.replace_chapters(&article_id, &chapters);
            }
            let starts: Vec<i64> = chapters.iter().map(|c| (c.start * 1000.0) as i64).collect();
            chapter_list.clear();
            for chapter in &chapters {
                chapter_list.append(&format!(
                    "{} - {}",
                    fmt_ms((chapter.start * 1000.0) as i64),
                    chapter.title.as_deref().unwrap_or("Chapter")
                ));
            }
            *chapters_tick.borrow_mut() = (chapters, starts);
        }
        let playing_now = m.get_state() == MediaState::Playing;
        // Autoplay watchdog (see PlayerState): re-play a load
        // the backend accepted but never started; escalate the
        // fallback level if it keeps dying; give up loudly at
        // ~14 s instead of sitting silent.
        {
            let mut s = st.borrow_mut();
            if s.autoplay_active {
                if playing_now {
                    s.autoplay_active = false;
                } else {
                    s.autoplay_ticks = s.autoplay_ticks.saturating_add(1);
                    let t = s.autoplay_ticks;
                    if (t == 3 || t == 6) && len > 0 {
                        m.play();
                    } else if t == 9 && s.autoplay_level < 2 {
                        s.autoplay_level += 1;
                        let protected =
                            blindrss_core::http_headers::protected_media_url(&s.stream, &s.headers);
                        let next = if s.autoplay_level == 1 {
                            blindrss_core::proxy::stream_url(&protected)
                        } else {
                            blindrss_core::proxy::play_url(&protected)
                        };
                        if m.load_uri(&next) {
                            m.play();
                        }
                    } else if t >= 14 {
                        s.autoplay_active = false;
                        frame_tick.set_status_text("Could not play this media", 0);
                        crate::announce("general", "Could not play this media");
                    }
                }
            }
        }
        // Keep the Play/Pause button's label honest about the
        // state — the label is its accessible name, so NVDA
        // users hear "Pause" while playing and "Play" while
        // paused instead of a static "Play/Pause".
        {
            let mut s = st.borrow_mut();
            if s.last_playing != Some(playing_now) {
                s.last_playing = Some(playing_now);
                let label = if playing_now {
                    crate::tr("Pause")
                } else {
                    crate::tr("Play")
                };
                play_btn.set_label(&label);
            }
        }
        {
            let text = format!("{} / {}", fmt_ms(pos), fmt_ms(len));
            let mut s = st.borrow_mut();
            if s.last_pos_text != text {
                s.last_pos_text = text.clone();
                drop(s);
                pos_label.set_value(&text);
            }
        }
        if len > 0 {
            let mut s = st.borrow_mut();
            s.slider_guard = true;
            seek_slider.set_value((pos * 1000 / len) as i32);
            s.slider_guard = false;
        }
        {
            let mut s = st.borrow_mut();
            if s.resume.enabled
                && s.seeked
                && pos >= 1_000
                && s.last_save.elapsed() >= s.resume.save_interval
                && pos != s.last_saved_pos
            {
                s.last_saved_pos = pos;
                s.last_save = std::time::Instant::now();
                save_position(&s, &m, false);
            }
        }
    });
    timer.start(1000, false);

    let player_key = frame.get_handle() as usize;
    PLAYERS.with(|players| {
        players.borrow_mut().push((
            player_key,
            Rc::new(move |id| {
                match id {
                    "player.play_pause" => play_pause(),
                    "player.stop" => stop(),
                    // Batch 41a addendum: the rebuilt Player
                    // menu and tray dispatch these four names;
                    // they run the same transport closures as
                    // the player's own buttons.
                    "player.rewind" => rewind(),
                    "player.forward" => forward(),
                    "player.volume_up" => volume_up(),
                    "player.volume_down" => volume_down(),
                    "player.show_hide" => {
                        let show = !frame.is_shown();
                        frame.show(show);
                        if show {
                            frame.raise();
                            play_btn.set_focus();
                        }
                    }
                    "player.chapters" => {
                        frame.show(true);
                        frame.raise();
                        chapter_list.set_focus();
                    }
                    "player.equalizer" => {
                        crate::dialogs::show_equalizer(&frame, &crate::config_path());
                        let cfg = blindrss_core::config::Config::load(&crate::config_path())
                            .unwrap_or_default();
                        media.apply_equalizer(&blindrss_core::equalizer::EqConfig::from_config(
                            &cfg,
                        ));
                    }
                    "speed.up" => change_speed(1),
                    "speed.down" => change_speed(-1),
                    "speed.reset" => change_speed(0),
                    _ => return false,
                }
                true
            }),
        ))
    });
    let closing_state = state.clone();
    frame.on_close(move |event| {
        timer.stop();
        save_position(&closing_state.borrow(), &media, false);
        // Closing cancels the load's workers: the silence
        // scan's ffmpeg is killed and a pending queue resolve
        // or chapter fetch drops its result (Python's
        // shutdown path cancels the scan the same way).
        closing_state.borrow().load_gate.cancel_current();
        media.close();
        PLAYERS.with(|players| players.borrow_mut().retain(|(key, _)| *key != player_key));
        event.skip(true);
    });

    frame.show(true);
    // Land keyboard focus on Play/Pause, not the media pane:
    // the pane is a native video surface NVDA cannot read, and
    // it is created first so it would otherwise take focus.
    play_btn.set_focus();
    // show_player_on_play: raise the player over the main window
    // when playback starts (Python default in its settings UI).
    if show_player {
        frame.raise();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_media_is_not_complete_at_zero() {
        // 3-second clip: opening it must not be "finished".
        assert!(!reached_end(0, 3_000, 60_000));
        // Nor is the very start of a long episode.
        assert!(!reached_end(0, 600_000, 60_000));
        assert!(!reached_end(0, 0, 60_000));
    }

    #[test]
    fn near_the_end_is_complete() {
        assert!(reached_end(596_000, 600_000, 60_000));
        assert!(reached_end(600_000, 600_000, 60_000));
        // A short clip is complete once it is played past the start.
        assert!(reached_end(2_900, 3_000, 60_000));
        assert!(!reached_end(0, 4_999, 60_000));
    }
}
