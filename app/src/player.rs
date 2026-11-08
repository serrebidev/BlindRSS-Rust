//! Player window (slice 7 + batch 3): wx MediaCtrl over the
//! article's media URL (direct or yt-dlp-resolved), with resume
//! via the Python playback_state table, chapter navigation with
//! announcements, playback-speed control, and segment skipping
//! (SponsorBlock for YouTube; Skip Silence via core's
//! incremental, cancellable PCM scan — Audit 8 Task 21).
//!
//! Position is saved every second by the window timer, so closing
//! the window at any point loses at most a second of progress.
//!
//! There is exactly ONE player session (Audit 8 M01/M10/M11,
//! Python's `_ensure_player_window`): `begin_play` opens the
//! window in its loading state immediately — before any
//! resolution or metadata lookup runs — and the resolved stream
//! is delivered into it under a generation stamp
//! (`blindrss_core::player_session`). Activating the media the
//! session already holds toggles play/pause; activating anything
//! else stops the old playback and loads into the same window.
//! A result stamped with an older generation is stale and never
//! starts audio.

use crate::media_backend::MediaBackend;
use blindrss_core::announce::HiddenAnnouncer;
use blindrss_core::db::Chapter;
use blindrss_core::playback::{
    PendingPlaybackWrites, PlaybackSnapshot, ResumePolicy, SeekKind, SeekVerifier,
    chapter_commit_allowed, chapter_index_for_position, chapter_link_index, chapter_next_target,
    chapter_prev_target, playback_speeds, saved_state, speed_index, speed_step,
};
use blindrss_core::player_session::{Activation, ChapterGate, PlayerSession, SessionIdentity};
use blindrss_core::silence::{SilenceScanSettings, SilenceSkipPolicy, SkipContext};
use blindrss_core::{Article, Db, PlaybackState};
use patois::t;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use wxdragon::prelude::*;
use wxdragon::widgets::media_ctrl::SeekMode;

type PlayerCommand = Rc<dyn Fn(&str) -> bool>;
type ResolvedMedia = (String, Vec<(String, String)>);
type QueueResult = (usize, Result<ResolvedMedia, String>);

/// Everything the main window (and this module's own tick and
/// command closures) needs to drive the one live session.
struct SessionControl {
    frame: Frame,
    media: MediaBackend,
    state: Rc<RefCell<PlayerState>>,
    chapters_live: Rc<RefCell<(Vec<Chapter>, Vec<i64>)>>,
    chapter_list: ListBox,
    play_btn: Button,
    /// The speed picker (Audit 8 M15): its selection mirrors
    /// the saved speed whenever the speed is a ladder rung.
    speed_combo: ComboBox,
    /// The main-window command surface (play/pause, stop,
    /// seeks, volume, show/hide, chapters, equalizer, speed).
    command: PlayerCommand,
}

struct LiveSession {
    /// Frame handle, so the close handler only clears the
    /// session it belongs to.
    key: usize,
    control: Rc<SessionControl>,
    command: PlayerCommand,
}

thread_local! {
    static SESSION: RefCell<Option<LiveSession>> = const { RefCell::new(None) };
    /// Identity/generation policy for the session. Kept outside
    /// the window so generations keep moving across window
    /// lifetimes: a result stamped before a close can never
    /// match a later session.
    static SESSION_POLICY: RefCell<PlayerSession> = RefCell::new(PlayerSession::new());
}

fn session_control() -> Option<Rc<SessionControl>> {
    SESSION.with(|s| s.borrow().as_ref().map(|live| live.control.clone()))
}

/// True while `generation` is the live session's generation:
/// a worker result stamped with it may still act.
pub fn is_current(generation: u64) -> bool {
    SESSION.with(|s| s.borrow().is_some())
        && SESSION_POLICY.with(|p| p.borrow().is_current(generation))
}

/// Route main-window shortcuts to the one player session.
pub fn dispatch_command(id: &str) -> bool {
    let action = SESSION.with(|s| s.borrow().as_ref().map(|live| live.command.clone()));
    action.is_some_and(|action| action(id))
}

/// The session identity of an article (Python's
/// `is_current_media` inputs): its id, plus the playback target
/// known before resolution — the enclosure URL when there is
/// one, else the article URL.
fn identity_of(article: &Article) -> SessionIdentity {
    let target = article
        .media_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .unwrap_or(&article.url);
    SessionIdentity::new(&article.id, target)
}

/// Speak a load-state text the way Python's
/// `_announce_when_hidden` does: once per load, and only while
/// the player window is hidden (its status line cannot be read
/// then). The text is recorded either way, so showing and later
/// hiding the window never replays it.
fn hidden_announce(state: &mut PlayerState, frame: &Frame, text: &str) {
    if state.hidden.should_speak(text, frame.is_shown()) {
        crate::announce("general", text);
    }
}

/// Record a load-state text as already announced without
/// speaking it here — for paths that just announced it through
/// the unconditional channel (an explicit Play press), so the
/// hidden-window mechanism does not say it a second time.
fn note_announced(state: &mut PlayerState, text: &str) {
    state.hidden.should_speak(text, true);
}

/// (Re)arm the debounced landed-seek summary (M10): every seek
/// press pushes the announcement 400 ms out, so a burst of
/// presses speaks once, with the position actually landed on —
/// Python's `wx.CallLater(400, ...)` with Restart.
fn arm_seek_summary(state: &Rc<RefCell<PlayerState>>) {
    state.borrow_mut().seek_announce_at =
        Some(std::time::Instant::now() + std::time::Duration::from_millis(400));
}

/// The volume step for one button press, read from the config
/// at use time (percent → fraction, clamped like Python's).
fn volume_step() -> f64 {
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    (cfg.get_i64("volume_step", 5) as f64 / 100.0).clamp(0.01, 0.5)
}
thread_local! {
    /// Main-frame shortcut dispatch shared with this window
    /// (Audit 8 M14): Python's player delegates every modified
    /// key to the main frame's `dispatch_shortcut` with the text
    /// guard off, so the whole registry works while the player
    /// has focus. Installed by the main window when it is built;
    /// the delegate reads the live keymap, so shortcut edits
    /// apply without reopening the player.
    static SHORTCUT_DELEGATE: RefCell<Option<PlayerCommand>> = const { RefCell::new(None) };
}

/// Install the main frame's shortcut dispatch (accel -> handled).
pub fn set_shortcut_delegate(delegate: PlayerCommand) {
    SHORTCUT_DELEGATE.with(|d| *d.borrow_mut() = Some(delegate));
}

fn dispatch_shortcut_accel(accel: &str) -> bool {
    SHORTCUT_DELEGATE.with(|d| d.borrow().as_ref().is_some_and(|f| f(accel)))
}

/// A SponsorBlock range the player skips over, with its
/// announcement label. Skipped segments are remembered by
/// UUID, as Python does, never by start time. (Silence
/// ranges live in the Task 21 policy state, not here.)
#[derive(Clone)]
struct SkipRange {
    start_ms: i64,
    end_ms: i64,
    label: String,
    uuid: String,
}

/// The already-skipped key for a range: its SponsorBlock
/// UUID (the start-time fallback is defensive only now).
fn skip_key(range: &SkipRange) -> String {
    if range.uuid.is_empty() {
        format!("start:{}", range.start_ms)
    } else {
        range.uuid.clone()
    }
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
    /// Verified seek state (Audit 8 M05): armed by a resume
    /// restore or a user seek, driven by the tick, cancelled by
    /// a superseding seek, a new load or a stop. While a Resume
    /// verifier is armed, `seeked` stays false and position
    /// saves are held, exactly Python's restore-inflight rule.
    seek_verify: Option<SeekVerifier>,
    /// Stop then Play resumes (Audit 8 M08): set by Stop,
    /// consumed (and always cleared) by the next Play, which
    /// re-runs the resume restore against the saved row —
    /// Python's `_stopped_needs_resume`.
    stopped_needs_resume: bool,
    /// Position most recently handed to the persistence
    /// worker. Save bookkeeping (`last_saved_pos`/`last_save`)
    /// advances only when the worker confirms the write
    /// landed (M07) — never when it was merely queued.
    last_queued_pos: i64,
    /// Persistence worker channel (M07): position saves and
    /// learned seek-support updates leave the UI thread on
    /// this; confirmations of landed saves return on
    /// `write_confirm_rx`. None only when the worker could
    /// not be started, in which case saves fall back to the
    /// old synchronous path rather than being lost.
    write_tx: Option<std::sync::mpsc::Sender<WriteMsg>>,
    write_confirm_rx: std::sync::mpsc::Receiver<Vec<(String, PlaybackState)>>,
    /// Skip ranges delivered by the SponsorBlock / silence
    /// workers on `skip_rx`, drained by the tick.
    skips: Vec<SkipRange>,
    skip_rx: std::sync::mpsc::Receiver<Vec<SkipRange>>,
    /// Ranges already skipped (Python's already-skipped
    /// guard: never skip the same range twice in a session).
    /// SponsorBlock ranges key by segment UUID (see
    /// `skip_key`).
    skipped: HashSet<String>,
    /// Skip Silence (Audit 8 Task 21, M09 — Python's silence
    /// regions of gui/player.py): the guarded skip policy, the
    /// published (padded) ranges, and the scan lifecycle. The
    /// scan is QUEUED at load, started by the tick once
    /// playback is under way past the configured delay, and
    /// cancelled (ffmpeg killed and reaped) on every new
    /// load, stop and close.
    silence: SilenceSkipPolicy,
    silence_ranges: Vec<(i64, i64)>,
    silence_ready: bool,
    /// Remote-media flag for the current load (Python's
    /// startswith-http minus loopback test): selects the
    /// conservative scan settings and skip windows.
    silence_remote: bool,
    silence_pending: Option<SilencePending>,
    silence_scan: Option<SilenceScan>,
    silence_rx: std::sync::mpsc::Receiver<SilenceScanMsg>,
    /// Publish tunables resolved when the scan was queued.
    silence_pad_ms: i64,
    silence_gap_ms: i64,
    silence_resume_backoff: i64,
    silence_retrigger: i64,
    /// Desired player-tick cadence: Python runs its player
    /// timer at 280 ms while Skip Silence is on ("so jumps
    /// feel snappier"); this player's tick is 1000 ms
    /// otherwise. Resolved from config at every load in
    /// `queue_silence_scan`; the tick applies a change to
    /// the wx timer as it notices it (Audit 10).
    tick_fast: bool,
    tick_fast_applied: bool,
    /// Seek-slider drag state for the policy's drag guard:
    /// set by user slider events, cleared by the tick after
    /// a quiet interval (wxDragon exposes no drag-end event;
    /// see PARITY Task 21).
    silence_dragging: bool,
    silence_drag_at: Option<std::time::Instant>,
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
    /// Range-cache context of the current load (Audit 8
    /// Task 22, M20): when resolution followed redirects from
    /// the article's original URL to a different final URL,
    /// the ORIGINAL is the cache identity the gate registers,
    /// so an expired signed redirect refreshes through it and
    /// reuses the cached segments. Set at deliver time;
    /// toggle-reloads reuse it.
    range_cache_resolved: bool,
    range_cache_original: Option<String>,
    /// Last text written to the position readout.
    last_pos_text: String,
    /// Position at the last playback-state save (saves are
    /// throttled to real movement; opening the DB every tick
    /// was needless UI-thread I/O).
    last_saved_pos: i64,
    /// Per-load hidden-window load-state announcements
    /// (Audit 8 M10): reset at every load; the tick speaks
    /// "Playing" through it, load failures speak "Failed to
    /// load media", queue advances pre-suppress the routine
    /// states. UI thread only, like the rest of this state.
    hidden: HiddenAnnouncer,
    /// Debounced landed-seek summary (M10): armed by every
    /// user seek (buttons, keys, slider, Player-menu dispatch)
    /// at now + 400 ms and re-armed by the next press, exactly
    /// Python's `wx.CallLater(400)` pattern; the tick speaks
    /// the position summary when it fires.
    seek_announce_at: Option<std::time::Instant>,
    /// Chapter list selection awaiting commit (Audit 8 M13,
    /// Python's `_chapter_pending_idx`): selecting a chapter
    /// records it; Enter (or double-click) commits the seek.
    chapter_pending: Option<usize>,
    /// Last committed chapter and when, in milliseconds on the
    /// session's monotonic clock (Python's 350 ms same-chapter
    /// commit debounce, `_commit_chapter_selection`).
    chapter_commit: Option<(usize, u64)>,
    /// Chapter-fetch staleness gate (Audit 8 M12): each
    /// asynchronous chapter fetch for the live item is stamped
    /// here; results apply only while their stamp is latest.
    chapter_gate: ChapterGate,
    /// Chapter-fetch results returning from worker threads,
    /// drained by the tick: (generation, article id, stamp,
    /// chapters).
    chapter_tx: std::sync::mpsc::Sender<(u64, String, u64, Vec<Chapter>)>,
    chapter_rx: std::sync::mpsc::Receiver<(u64, String, u64, Vec<Chapter>)>,
    /// The playback target the main window's live status and
    /// row annotations key on: the article's enclosure URL
    /// when it has one, else its article URL (the session
    /// identity target — NOT the resolved stream URL).
    media_target: String,
    /// Live playback snapshot for the main window (Audit 8
    /// M15, Python's `playback_progress_snapshot`): the tick
    /// refreshes it; the main window polls it. Plain data.
    snapshot: PlaybackSnapshot,
    /// True while the tick sets the volume slider
    /// programmatically, so the slider's own event handler
    /// ignores the change (the seek slider's `slider_guard`
    /// pattern).
    volume_guard: bool,
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

// Used by the Windows seek-slider accessible value text;
// on other platforms nothing reads it.
#[allow(dead_code)]
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

/// Player-window menu ids (the Chapters menu bar and the
/// Chapters button's popup). Private to this frame — the main
/// window's menu ids live in main.rs and never reach here.
const PCM_SHOW_CHAPTERS: i32 = 2960;
const PCM_OPEN_LINK: i32 = 2961;
const PCM_PREV_CHAPTER: i32 = 2962;
const PCM_NEXT_CHAPTER: i32 = 2963;
const PCM_POPUP_OPEN_LINK: i32 = 2964;
const PCM_POPUP_NONE: i32 = 2965;
/// Popup chapter-jump items: base + chapter index.
const PCM_CHAPTER_BASE: i32 = 3000;

/// End-of-media test for the saved playback row: a position within
/// the configured end threshold counts as completed. A clip shorter than
/// that threshold is NOT complete at position zero — the older
/// `pos >= len - 5000` form compared against a negative bound and
/// marked every short clip finished the moment it opened, so it
/// never resumed where the user left it.
fn reached_end(pos: i64, len: i64, threshold: i64) -> bool {
    len > 0 && pos > 0 && len.saturating_sub(pos) <= threshold
}

/// One message for the persistence worker (M07).
enum WriteMsg {
    Save {
        article_id: String,
        url: String,
        state: PlaybackState,
        policy: ResumePolicy,
    },
    SeekSupported {
        playback_id: String,
        supported: bool,
    },
}

fn apply_write_msg(queue: &mut PendingPlaybackWrites, policy: &mut ResumePolicy, msg: WriteMsg) {
    match msg {
        WriteMsg::Save {
            article_id,
            url,
            state,
            policy: save_policy,
        } => {
            *policy = save_policy;
            queue.queue_save(&article_id, &url, state);
        }
        WriteMsg::SeekSupported {
            playback_id,
            supported,
        } => queue.queue_seek_supported(&playback_id, supported),
    }
}

/// Spawn the playback-persistence worker (Audit 8 M07): the
/// player's position writes used to open/schema-init the
/// database synchronously on the UI thread and silently drop
/// failures. The worker owns its own connection with Python's
/// 500 ms busy timeout, coalesces queued saves latest-wins,
/// flushes on Python's cadence (first attempt promptly, locked
/// writes retried — never dropped), and reports each landed
/// save back so the tick advances save bookkeeping only past
/// written state.
fn spawn_write_worker(
    db_path: PathBuf,
) -> (
    std::sync::mpsc::Sender<WriteMsg>,
    std::sync::mpsc::Receiver<Vec<(String, PlaybackState)>>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<WriteMsg>();
    let (confirm_tx, confirm_rx) = std::sync::mpsc::channel::<Vec<(String, PlaybackState)>>();
    std::thread::spawn(move || {
        let Ok(db) = Db::open_or_create(&db_path) else {
            return;
        };
        // Python's playback writes fail fast on a locked
        // database and retry from the pending queue.
        let _ = db.set_busy_timeout_ms(500);
        let mut queue = PendingPlaybackWrites::new();
        let mut policy = ResumePolicy::from_config(
            &blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default(),
        );
        let mut last_flush: Option<std::time::Instant> = None;
        loop {
            let disconnected = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(msg) => {
                    apply_write_msg(&mut queue, &mut policy, msg);
                    while let Ok(msg) = rx.try_recv() {
                        apply_write_msg(&mut queue, &mut policy, msg);
                    }
                    false
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => true,
            };
            if !queue.is_empty()
                && last_flush.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(450))
            {
                last_flush = Some(std::time::Instant::now());
                let written = queue.flush(&db, &policy);
                if !written.is_empty() {
                    let _ = confirm_tx.send(written);
                }
            }
            if disconnected {
                break;
            }
        }
    });
    (tx, confirm_rx)
}

/// Queue a learned seek-support update for the worker (M05's
/// learning half): support facts persist through the same
/// never-drop queue as position saves.
fn queue_seek_supported(state: &PlayerState, supported: bool) {
    if state.article_id.is_empty() {
        return;
    }
    let playback_id = blindrss_core::playback::playback_id(&state.article_id);
    if let Some(tx) = &state.write_tx {
        let _ = tx.send(WriteMsg::SeekSupported {
            playback_id,
            supported,
        });
    } else if let Ok(db) = Db::open(&state.db_path) {
        let _ = db.set_seek_supported(&playback_id, supported);
    }
}

fn save_position(state: &PlayerState, media: &MediaBackend, force_completed: bool) {
    if !state.resume.enabled || !state.seeked && !force_completed {
        return;
    }
    let pos = media.tell();
    let len = media.length();
    let completed = force_completed || reached_end(pos, len, state.resume.complete_threshold_ms);
    let outgoing = PlaybackState {
        position_ms: pos,
        duration_ms: if len > 0 { Some(len) } else { None },
        completed,
        seek_supported: None,
        title: Some(state.title.clone()),
    };
    // M07: the write leaves the UI thread. Failures are the
    // worker's problem to retry, never a silent drop here.
    if let Some(tx) = &state.write_tx {
        let _ = tx.send(WriteMsg::Save {
            article_id: state.article_id.clone(),
            url: state.article_url.clone(),
            state: outgoing,
            policy: state.resume,
        });
        return;
    }
    // No worker (its spawn-time open failed): keep the old
    // synchronous path rather than lose persistence entirely.
    if let Ok(db) = Db::open(&state.db_path) {
        let _ = state
            .resume
            .save_state(&db, &state.article_id, &state.article_url, &outgoing);
    }
}

/// Build the resume-restore verifier for a saved target
/// (M05): remote inputs get the wider landing tolerance, and
/// seek support can only be learned against an input that
/// does not look seekable, mirroring `ResumePolicy`.
fn resume_verifier(target_ms: i64, stream: &str) -> SeekVerifier {
    let remote = stream.starts_with("http://") || stream.starts_with("https://");
    let seekable_looking = blindrss_core::media::is_media_file_url(stream)
        && !stream.to_ascii_lowercase().contains(".m3u8");
    SeekVerifier::for_resume(
        target_ms,
        remote,
        seekable_looking,
        std::time::Instant::now(),
    )
}

/// Python's `_note_user_seek`: a user seek cancels any pending
/// resume restore, clears the stopped-resume flag, marks the
/// stream seek-supported, and starts the seek guard on the
/// target actually requested (the backend's computed target,
/// not the raw delta).
fn begin_user_seek(state: &mut PlayerState, target_ms: i64) {
    state.stopped_needs_resume = false;
    state.seeked = true;
    // The silence policy hears about user seeks too (Python
    // `_note_user_seek`): skipping pauses, the position
    // floor resets, landing verification cancels.
    state
        .silence
        .note_user_seek(std::time::Instant::now(), state.silence_remote);
    let remote = state.stream.starts_with("http://") || state.stream.starts_with("https://");
    state.seek_verify = Some(SeekVerifier::for_user_seek(
        target_ms,
        remote,
        std::time::Instant::now(),
    ));
    queue_seek_supported(state, true);
}

/// Stop then Play (M08): re-run the resume restore against the
/// saved row — the saved position with the configured rewind —
/// and arm the verified restore. The stopped flag is cleared
/// whether or not a restore applies, as Python's `play()` does
/// in its finally block.
fn resume_after_stop(state: &Rc<RefCell<PlayerState>>) {
    let target = {
        let s = state.borrow();
        if !s.stopped_needs_resume {
            return;
        }
        Db::open(&s.db_path)
            .ok()
            .and_then(|db| {
                saved_state(&db, &s.article_id, &s.article_url)
                    .ok()
                    .flatten()
            })
            .map(|saved| s.resume.resume_target(&saved, &s.stream))
            .unwrap_or(0)
    };
    let mut s = state.borrow_mut();
    s.stopped_needs_resume = false;
    if target > 0 {
        s.saved_position_ms = target;
        s.seeked = false;
        let stream = s.stream.clone();
        s.seek_verify = Some(resume_verifier(target, &stream));
    }
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

/// A queued silence scan (Python's `_silence_scan_pending`):
/// the stream to scan and the load generation it belongs to.
struct SilencePending {
    url: String,
    generation: u64,
    queued_at: std::time::Instant,
}

/// A running silence scan: its cancellation flag (the worker
/// thread kills and reaps its ffmpeg child by it).
struct SilenceScan {
    abort: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Scan results returning from the worker thread, drained by
/// the tick: incremental progress sets and the final result.
enum SilenceScanMsg {
    Progress(u64, Vec<(i64, i64)>),
    Done(u64, Result<Vec<(i64, i64)>, String>),
}

/// Python's remote test for silence purposes: an http(s) URL
/// that is not loopback.
fn silence_remote(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !lower.contains("127.0.0.1")
        && !lower.contains("localhost")
}

/// Queue the silence scan for a fresh load (Python
/// `_queue_silence_scan`): only when Skip Silence is on and
/// the URL is non-empty. The publish tunables resolve here,
/// with the remote adjustments applied.
fn queue_silence_scan(s: &mut PlayerState, cfg: &blindrss_core::config::Config, url: &str) {
    // The tick cadence follows the same config switch
    // (Python restarts its player timer at the faster rate
    // whenever Skip Silence is on), so resolve it before
    // the early return below.
    s.tick_fast = cfg.get_bool("skip_silence", true);
    if !s.tick_fast || url.is_empty() {
        return;
    }
    s.silence_remote = silence_remote(url);
    let settings = SilenceScanSettings::from_config(cfg, s.silence_remote);
    s.silence_pad_ms = settings.padding_ms;
    s.silence_gap_ms = settings.scan.merge_gap_ms;
    s.silence_resume_backoff = settings.resume_backoff_ms;
    s.silence_retrigger = settings.retrigger_backoff_ms;
    let generation = SESSION_POLICY.with(|p| p.borrow().generation());
    s.silence_pending = Some(SilencePending {
        url: url.to_string(),
        generation,
        queued_at: std::time::Instant::now(),
    });
}

/// Cancel any running or queued silence scan and clear all
/// of its state (Python `_cancel_silence_scan`, which also
/// clears the skip policy's fields).
fn cancel_silence_scan(s: &mut PlayerState) {
    if let Some(scan) = &s.silence_scan {
        scan.abort.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    s.silence_scan = None;
    let (_, rx) = std::sync::mpsc::channel();
    s.silence_rx = rx;
    s.silence_pending = None;
    s.silence_ranges.clear();
    s.silence_ready = false;
    s.silence.reset();
    s.silence_dragging = false;
    s.silence_drag_at = None;
}

/// Start the queued scan on its worker thread (Python
/// `_start_silence_scan`): ffmpeg decodes the stream and
/// core's scanner paces behind the playback position,
/// publishing incremental results on the channel. Playback
/// request headers ride along, as Python passes them.
fn start_silence_scan(
    s: &mut PlayerState,
    cfg: &blindrss_core::config::Config,
    seek_pos: &std::sync::Arc<std::sync::atomic::AtomicI64>,
) {
    let Some(pending) = s.silence_pending.take() else {
        return;
    };
    let Some(ffmpeg) = blindrss_core::silence::find_ffmpeg() else {
        return;
    };
    let settings = SilenceScanSettings::from_config(cfg, s.silence_remote);
    let mut scan_cfg = settings.scan;
    scan_cfg.headers = s.headers.clone();
    scan_cfg.default_user_agent = blindrss_core::user_agents::resolve_from_config(cfg);
    let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    s.silence_rx = rx;
    s.silence_scan = Some(SilenceScan {
        abort: abort.clone(),
    });
    let generation = pending.generation;
    let url = pending.url;
    let pos = seek_pos.clone();
    std::thread::spawn(move || {
        let tx_progress = tx.clone();
        let mut progress = move |ranges: &[(i64, i64)], _scanned: i64| -> bool {
            tx_progress
                .send(SilenceScanMsg::Progress(generation, ranges.to_vec()))
                .is_ok()
        };
        let result = blindrss_core::silence::scan_silence(
            &ffmpeg,
            &url,
            &scan_cfg,
            &abort,
            &|| Some(pos.load(std::sync::atomic::Ordering::Relaxed)),
            &mut progress,
        );
        let msg = match result {
            Ok(ranges) => SilenceScanMsg::Done(generation, Ok(ranges)),
            Err(blindrss_core::silence::ScanError::Cancelled) => {
                SilenceScanMsg::Done(generation, Err("cancelled".to_string()))
            }
            Err(e) => SilenceScanMsg::Done(generation, Err(format!("{e:?}"))),
        };
        let _ = tx.send(msg);
    });
}

/// Spawn the SponsorBlock worker for one article and return
/// the channel its skip ranges arrive on. Called at open and
/// again on every queue advance. Silence has its own
/// lifecycle above (Task 21) — it no longer rides this
/// channel.
fn spawn_skip_workers(
    cfg: &blindrss_core::config::Config,
    article: &Article,
) -> std::sync::mpsc::Receiver<Vec<SkipRange>> {
    let (skip_tx, skip_rx) = std::sync::mpsc::channel::<Vec<SkipRange>>();
    // Python skip_categories: empty when SponsorBlock is
    // off or no categories are chosen — and then no lookup
    // is issued at all.
    let cats = blindrss_core::sponsorblock::skip_categories(cfg);
    if !cats.is_empty() {
        let vid = blindrss_core::sponsorblock::youtube_video_id(&article.url).or_else(|| {
            article
                .media_url
                .as_deref()
                .and_then(blindrss_core::sponsorblock::youtube_video_id)
        });
        if let Some(vid) = vid {
            let tx = skip_tx.clone();
            std::thread::spawn(move || {
                // fetch_segments never errors: any lookup
                // failure yields no segments and playback
                // carries on.
                let segs = blindrss_core::sponsorblock::fetch_segments(&vid, &cats);
                let ranges = segs
                    .iter()
                    .map(|s| {
                        let secs = (s.end - s.start).round() as i64;
                        SkipRange {
                            start_ms: (s.start * 1000.0) as i64,
                            end_ms: (s.end * 1000.0) as i64,
                            label: format!(
                                "Skipped {}, {} second{}",
                                blindrss_core::sponsorblock::category_label(&s.category),
                                secs,
                                if secs == 1 { "" } else { "s" }
                            ),
                            uuid: s.uuid.clone(),
                        }
                    })
                    .collect();
                let _ = tx.send(ranges);
            });
        }
    }
    skip_rx
}

/// Apply a speed to the backend and mirror it in the picker
/// (Python's `set_playback_speed`): an off-ladder rate leaves
/// the picker's selection alone (no rung claims it).
fn apply_speed_selection(media: &MediaBackend, combo: &ComboBox, rate: f64) {
    media.set_playback_rate(rate);
    if let Some(idx) = speed_index(&playback_speeds(), rate) {
        combo.set_selection(idx as u32);
    }
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
    url_is_resolved: bool,
    original_url: Option<&str>,
) -> &'static str {
    // The primary load goes through the range cache when the
    // Python gate says so (Batch 36): it serves a truthful
    // Content-Length and real 206 ranges from a disk cache, so
    // the backend neither clamps seeks to a wrong duration nor
    // re-streams bytes it already fetched. A redirect-resolved
    // load registers its ORIGINAL URL as the cache identity
    // (M20): the resolved signed URL expires, the original
    // does not.
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    let primary = blindrss_core::rangecache::maybe_range_cache_url(
        url,
        headers,
        url_is_resolved,
        original_url,
        &cfg,
    );
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

/// Load a resolved stream into the backend and start it,
/// applying the persisted volume and playback speed and arming
/// the autoplay watchdog. Shared by the delivery of a fresh
/// load, a toggle-reload of an ended stream, and a play-queue
/// switch. Returns false when every fallback failed.
fn start_loaded_stream(
    media: &MediaBackend,
    frame: &Frame,
    play_btn: &Button,
    state: &Rc<RefCell<PlayerState>>,
    stream: &str,
    headers: Vec<(String, String)>,
) -> bool {
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    let wrapped = if media.uses_vlc() {
        stream.to_string()
    } else {
        eq_wrap(stream, &cfg, &headers)
    };
    // An EQ-wrapped URL is already a local ffmpeg stream (its
    // own network path is ffmpeg's), so it loads directly;
    // anything else goes through the fallback chain.
    let (url_is_resolved, original_url) = {
        let s = state.borrow();
        (s.range_cache_resolved, s.range_cache_original.clone())
    };
    let via = if wrapped != stream {
        if media.load_uri(&wrapped) {
            ""
        } else {
            "failed"
        }
    } else {
        load_with_fallbacks(
            media,
            stream,
            &headers,
            url_is_resolved,
            original_url.as_deref(),
        )
    };
    if via == "failed" {
        return false;
    }
    media.set_volume((cfg.get_i64("volume", 100) as f64 / 100.0).clamp(0.0, 1.0));
    media.play();
    play_btn.set_label(&t("Pause"));
    let start_speed = cfg_f64(&cfg, "playback_speed", 1.0);
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
    // Arm the autoplay watchdog: load success is not playback
    // (see PlayerState::autoplay_active).
    let mut s = state.borrow_mut();
    s.stream = stream.to_string();
    s.headers = headers;
    s.autoplay_level = match via {
        "proxy" => 1,
        "ffmpeg" => 2,
        _ if wrapped != stream => 2,
        _ => 0,
    };
    s.autoplay_ticks = 0;
    s.autoplay_active = true;
    true
}

/// What one activation (Enter / Play) did with the session.
pub enum BeginOutcome {
    /// The session already held this media: play/pause was
    /// toggled and nothing is loading. The caller must not
    /// resolve or deliver anything.
    Toggled,
    /// The session is in its loading state for the item under
    /// this generation. The caller resolves on a worker and
    /// delivers (or fails) under the same generation; a newer
    /// activation makes the delivery stale and it is dropped.
    Loading(u64),
}

/// Begin playing `article` in the one player session, NOW —
/// before any resolution or metadata lookup (Audit 8 M11,
/// Python's `_open_article`): the window exists (or is reused)
/// in its loading state when this returns, so the user can
/// cancel by activating something else or closing it.
pub fn begin_play(_parent: &Frame, article: &Article) -> BeginOutcome {
    let alive = SESSION.with(|s| s.borrow().is_some());
    let (kind, generation) =
        SESSION_POLICY.with(|p| p.borrow_mut().activate(alive, identity_of(article)));
    match kind {
        Activation::Toggle => {
            if let Some(control) = session_control() {
                control.toggle_or_reload();
                // Python's toggle branch keeps the player's
                // chapters in step with the article's stored
                // set: chapters fetched since this item loaded
                // (or a different set entirely) replace the
                // player's when they differ (Audit 8 M12).
                let stored = Db::open(&crate::db_path())
                    .ok()
                    .and_then(|db| db.chapters(&article.id).ok())
                    .unwrap_or_default();
                let differs = {
                    let live = control.chapters_live.borrow();
                    live.0.len() != stored.len()
                        || live.0.iter().zip(stored.iter()).any(|(a, b)| {
                            a.start != b.start || a.title != b.title || a.href != b.href
                        })
                };
                if differs {
                    control.set_chapters(stored);
                }
            }
            BeginOutcome::Toggled
        }
        Activation::Create => {
            let control = create_session(article);
            SESSION.with(|s| {
                *s.borrow_mut() = Some(LiveSession {
                    key: control.frame.get_handle() as usize,
                    control: control.clone(),
                    command: control.command.clone(),
                });
            });
            control.begin_load(article, None);
            BeginOutcome::Loading(generation)
        }
        Activation::Reuse => {
            if let Some(control) = session_control() {
                control.begin_load(article, None);
            }
            BeginOutcome::Loading(generation)
        }
    }
}

/// Begin a play queue on `entries[index]`: like `begin_play`
/// but always a fresh load — Python's queue path calls
/// `load_media` directly and never takes the toggle branch,
/// even when the first entry is the media already loaded.
pub fn begin_queue_play(
    _parent: &Frame,
    article: &Article,
    entries: Vec<Article>,
    index: usize,
) -> BeginOutcome {
    let alive = SESSION.with(|s| s.borrow().is_some());
    let (kind, generation) = SESSION_POLICY.with(|p| {
        p.borrow_mut()
            .activate_new_load(alive, identity_of(article))
    });
    let queue = Some((entries, index));
    match kind {
        Activation::Create => {
            let control = create_session(article);
            SESSION.with(|s| {
                *s.borrow_mut() = Some(LiveSession {
                    key: control.frame.get_handle() as usize,
                    control: control.clone(),
                    command: control.command.clone(),
                });
            });
            control.begin_load(article, queue);
        }
        _ => {
            if let Some(control) = session_control() {
                control.begin_load(article, queue);
            }
        }
    }
    BeginOutcome::Loading(generation)
}

/// Deliver a resolved stream for the load stamped
/// `generation`. A stale generation (a newer activation, a
/// queue advance, a close) is dropped whole: it never starts
/// audio (M11).
pub fn deliver_media(
    generation: u64,
    article: &Article,
    stream_url: String,
    chapters: Vec<Chapter>,
    headers: Vec<(String, String)>,
) {
    if !is_current(generation) {
        return;
    }
    let Some(control) = session_control() else {
        return;
    };
    // The delivery must belong to the item the session is
    // loading (the generation is the guard; this is the belt
    // to its braces).
    if control.state.borrow().article_id != article.id {
        return;
    }
    control.deliver(article, stream_url, chapters, headers);
}

/// The resolution for the load stamped `generation` failed:
/// the session leaves its loading state with the failure on
/// the status line and, while hidden, spoken (M10).
pub fn fail_load(generation: u64) {
    if !is_current(generation) {
        return;
    }
    let Some(control) = session_control() else {
        return;
    };
    control.fail();
}

/// Apply a soundcard (output-device) change to the live
/// session (Audit 8 M21): the settings dialog's
/// `preferred_soundcard` change reaches the playing backend
/// immediately, without reloading the media — Python's
/// `mainframe` calls `apply_preferred_soundcard()` on the live
/// player window the same way. False when no session is live
/// or the backend cannot select a device (the choice still
/// applies at the next load, which reads the config).
pub fn apply_preferred_soundcard(device: &str) -> bool {
    session_control().is_some_and(|control| control.media.apply_soundcard(device))
}

/// Replace the session's chapters (metadata that arrived after
/// the audio started — M11 keeps it off the critical path).
/// Refused for a stale generation.
pub fn update_chapters(generation: u64, chapters: Vec<Chapter>) -> bool {
    if !is_current(generation) {
        return false;
    }
    let Some(control) = session_control() else {
        return false;
    };
    control.set_chapters(chapters);
    true
}

/// Stamp an asynchronous chapter fetch for the article the
/// session currently holds (Audit 8 M12): the stamp is what
/// the fetch's result must present to apply. 0 when the
/// session holds another article (or nothing) — the caller
/// then has nothing to fetch for.
pub fn begin_chapter_fetch(article_id: &str) -> u64 {
    let Some(control) = session_control() else {
        return 0;
    };
    let mut s = control.state.borrow_mut();
    if s.article_id != article_id {
        return 0;
    }
    s.chapter_gate.begin(article_id)
}

/// Apply chapters produced by an asynchronous fetch (Audit 8
/// M12): the load generation must still be current AND the
/// fetch stamp must still be the gate's latest for the article
/// the session holds. A stale queue result — the fetch of an
/// entry the queue has since left, or one superseded by a
/// re-fetch — is refused whole.
pub fn update_chapters_gated(
    generation: u64,
    article_id: &str,
    stamp: u64,
    chapters: Vec<Chapter>,
) -> bool {
    if !is_current(generation) {
        return false;
    }
    let Some(control) = session_control() else {
        return false;
    };
    {
        let s = control.state.borrow();
        if s.article_id != article_id || !s.chapter_gate.accepts(article_id, stamp) {
            return false;
        }
    }
    control.set_chapters(chapters);
    true
}

/// The live playback snapshot for the main window's status
/// field and row annotations (Audit 8 M15): a plain-data copy
/// the player tick refreshes; no media calls happen here.
pub fn progress_snapshot() -> PlaybackSnapshot {
    let Some(control) = session_control() else {
        return PlaybackSnapshot::default();
    };
    control.state.borrow().snapshot.clone()
}

/// Fill the chapter list and the live chapter cell from one
/// chapter set: sorted by start, labeled with Python's chapter
/// menu label (`_format_player_chapter_menu_label` — the same
/// text the chapter choice and menus carry). Shared by the
/// delivery path, late chapter upgrades and queue advances.
fn fill_chapter_list(
    list: &ListBox,
    live: &Rc<RefCell<(Vec<Chapter>, Vec<i64>)>>,
    chapters: Vec<Chapter>,
) {
    let mut chapters = chapters;
    chapters.sort_by(|a, b| a.start.total_cmp(&b.start));
    let starts: Vec<i64> = chapters.iter().map(|c| (c.start * 1000.0) as i64).collect();
    list.clear();
    for ch in &chapters {
        list.append(&blindrss_core::chapters::chapter_menu_label(ch));
    }
    *live.borrow_mut() = (chapters, starts);
}

/// Spawn an asynchronous chapter fetch for one article (Audit
/// 8 M12): the core pipeline (stored rows, Podcasting 2.0
/// chapter file, YouTube metadata, embedded ID3) runs on a
/// worker with its own database handle; the result returns on
/// the session's chapter channel stamped with the load
/// generation and the gate stamp taken when the fetch began,
/// and the tick applies it only while both still match.
fn spawn_chapter_fetch(
    db_path: &std::path::Path,
    tx: &std::sync::mpsc::Sender<(u64, String, u64, Vec<Chapter>)>,
    generation: u64,
    article_id: String,
    stamp: u64,
) {
    let tx = tx.clone();
    let db_path = db_path.to_path_buf();
    std::thread::spawn(move || {
        let chapters = Db::open_or_create(&db_path)
            .map(|db| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                blindrss_core::chapters::get_article_chapters(
                    &db,
                    &blindrss_core::chapters::HttpChapterTransport,
                    &article_id,
                    now,
                )
            })
            .unwrap_or_default();
        let _ = tx.send((generation, article_id, stamp, chapters));
    });
}

impl SessionControl {
    /// Put the session into the loading state for `article`
    /// (Python's `load_media` before its resolver runs): the
    /// outgoing item's position is saved, old playback stops,
    /// per-load state resets, visibility follows the
    /// `show_player_on_play` preference (default hidden, no
    /// focus steal) and the hidden load announcement is made.
    fn begin_load(&self, article: &Article, queue: Option<(Vec<Article>, usize)>) {
        let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
        if let Some((_, index)) = &queue {
            set_queue_index(*index as i64);
        }
        {
            let mut s = self.state.borrow_mut();
            // Persist the outgoing item's position (Python's
            // load_media does this for the previous media) —
            // but only when media actually loaded for it. A
            // virgin, still-loading or failed session has an
            // empty stream, and saving then would overwrite
            // the article's saved resume position with zero.
            if !s.stream.is_empty() {
                save_position(&s, &self.media, false);
            }
            self.media.stop();
            s.autoplay_active = false;
            s.article_id = article.id.clone();
            s.article_url = article.url.clone();
            s.title = article.title.clone();
            // Outstanding chapter fetches belong to the old
            // item: their results must never land on this one.
            s.chapter_gate.invalidate();
            s.chapter_pending = None;
            s.chapter_commit = None;
            s.media_target = article
                .media_url
                .as_deref()
                .filter(|u| !u.is_empty())
                .unwrap_or(&article.url)
                .to_string();
            s.snapshot = PlaybackSnapshot {
                has_media: true,
                playing: false,
                state: "Loading media...".to_string(),
                title: article.title.clone(),
                article_id: article.id.clone(),
                media_url: s.media_target.clone(),
                position_ms: 0,
                duration_ms: 0,
            };
            s.stream = String::new();
            s.headers = Vec::new();
            s.skips.clear();
            s.skipped.clear();
            // A new load retires the old item's silence scan
            // (Python cancels at load): kill its ffmpeg, drop
            // its published ranges, reset the skip policy.
            cancel_silence_scan(&mut s);
            s.last_chapter = None;
            let (_, rx) = std::sync::mpsc::channel();
            s.skip_rx = rx;
            s.saved_position_ms = 0;
            s.seeked = true;
            s.seek_verify = None;
            s.stopped_needs_resume = false;
            s.last_queued_pos = -1;
            s.last_saved_pos = -1;
            s.last_save = std::time::Instant::now();
            s.resume = ResumePolicy::from_config(&cfg);
            s.seek_announce_at = None;
            // Force the tick's play-state sync to re-observe:
            // without this, a load that reaches Playing before
            // the next tick (previous item was playing too)
            // would never register its readiness transition.
            s.last_playing = None;
            s.hidden.reset(&[]);
            s.queue = queue.map(|(entries, index)| {
                let (tx, rx) = std::sync::mpsc::channel();
                QueueCtx {
                    entries,
                    index,
                    rx,
                    tx,
                    loading: false,
                }
            });
        }
        *self.chapters_live.borrow_mut() = (Vec::new(), Vec::new());
        self.chapter_list.clear();
        self.frame.set_title(&article.title);
        self.frame.set_status_text("Loading media...", 0);
        // Visibility first, then the announcement: Python
        // defers its hidden announcement until after
        // `_open_article` has applied the preference.
        self.apply_visibility(&cfg);
        let mut s = self.state.borrow_mut();
        hidden_announce(&mut s, &self.frame, "Loading media...");
    }

    /// Honor `show_player_on_play` for a fresh load (M10):
    /// Python's merged-config default is hidden. Shown means
    /// show + raise + focus on Play/Pause; hidden means the
    /// window stays (or goes) out of sight and focus is NOT
    /// touched — the main window keeps it.
    fn apply_visibility(&self, cfg: &blindrss_core::config::Config) {
        if cfg.get_bool("show_player_on_play", false) {
            self.frame.show(true);
            self.frame.raise();
            self.play_btn.set_focus();
        } else {
            self.frame.show(false);
        }
    }

    /// Start the resolved stream for the loading item.
    fn deliver(
        &self,
        article: &Article,
        stream_url: String,
        chapters: Vec<Chapter>,
        headers: Vec<(String, String)>,
    ) {
        let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
        let skip_rx = spawn_skip_workers(&cfg, article);
        {
            let mut s = self.state.borrow_mut();
            s.skip_rx = skip_rx;
            // Range-cache context (M20): the URL resolution
            // started from is the article's enclosure (or its
            // page URL). A yt-dlp-supported page URL means the
            // stream came from yt-dlp and carries no context;
            // anything else that resolved to a DIFFERENT final
            // URL registers the original as the cache identity.
            let input_url = article
                .media_url
                .as_deref()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or(&article.url);
            let via_ytdlp = blindrss_core::media::is_ytdlp_supported(input_url)
                && !blindrss_core::media::is_media_file_url(input_url);
            let (resolved, original) =
                blindrss_core::media::range_cache_context(input_url, &stream_url, via_ytdlp, true);
            s.range_cache_resolved = resolved;
            s.range_cache_original = original;
            // Queue (do not start) this load's silence scan:
            // the tick starts it once playback is under way
            // (Python's queue-then-start scheduling, M09).
            queue_silence_scan(&mut s, &cfg, &stream_url);
            s.saved_position_ms = Db::open(&s.db_path)
                .ok()
                .and_then(|db| saved_state(&db, &article.id, &article.url).ok().flatten())
                .map(|saved| s.resume.resume_target(&saved, &stream_url))
                .unwrap_or(0);
            s.seeked = s.saved_position_ms <= 0;
            if s.saved_position_ms > 0 {
                // Verified restore (M05): the tick issues the
                // seek once the backend is ready and confirms
                // the landing — it is not assumed here.
                s.seek_verify = Some(resume_verifier(s.saved_position_ms, &stream_url));
            }
        }
        self.set_chapters(chapters);
        // The picker mirrors the saved speed when it is a
        // ladder rung (Python's set_playback_speed selection);
        // an off-ladder saved value leaves it unselected.
        let saved_speed = cfg_f64(&cfg, "playback_speed", 1.0);
        if let Some(idx) = speed_index(&playback_speeds(), saved_speed) {
            self.speed_combo.set_selection(idx as u32);
        }
        if start_loaded_stream(
            &self.media,
            &self.frame,
            &self.play_btn,
            &self.state,
            &stream_url,
            headers,
        ) {
            self.frame.set_title(&format!("Playing: {}", article.title));
        } else {
            self.frame.set_status_text("Could not load media", 0);
            let mut s = self.state.borrow_mut();
            hidden_announce(&mut s, &self.frame, "Failed to load media");
        }
    }

    /// Resolution failed for the loading item.
    fn fail(&self) {
        self.frame.set_status_text("Could not load media", 0);
        let mut s = self.state.borrow_mut();
        s.snapshot.state = "Failed to load media".to_string();
        hidden_announce(&mut s, &self.frame, "Failed to load media");
    }

    /// Repeated activation of the held media (Python's
    /// `is_current_media` branch): pause when playing, reload
    /// when the stream ended or stopped, else resume.
    fn toggle_or_reload(&self) {
        match self.media.get_state() {
            MediaState::Playing => {
                self.media.pause();
                let mut s = self.state.borrow_mut();
                save_position(&s, &self.media, false);
                // A deliberate pause cancels the autoplay
                // watchdog — it must never fight the user.
                s.autoplay_active = false;
                crate::announce("general", "Paused");
            }
            MediaState::Stopped => {
                let (stream, headers) = {
                    let s = self.state.borrow();
                    (s.stream.clone(), s.headers.clone())
                };
                if stream.is_empty() {
                    // Still loading: nothing to reload yet.
                    self.media.play();
                    return;
                }
                if start_loaded_stream(
                    &self.media,
                    &self.frame,
                    &self.play_btn,
                    &self.state,
                    &stream,
                    headers,
                ) {
                    // The reload restarts the stream; Play
                    // after Stop resumes from the saved
                    // position with the configured rewind
                    // (M08).
                    resume_after_stop(&self.state);
                    crate::announce("general", "Playing");
                    let mut s = self.state.borrow_mut();
                    note_announced(&mut s, "Playing");
                } else {
                    self.frame.set_status_text("Could not load media", 0);
                    let mut s = self.state.borrow_mut();
                    hidden_announce(&mut s, &self.frame, "Failed to load media");
                }
            }
            _ => {
                resume_after_stop(&self.state);
                self.media.play();
                crate::announce("general", "Playing");
                let mut s = self.state.borrow_mut();
                note_announced(&mut s, "Playing");
            }
        }
    }

    /// Replace the chapter set everywhere the window reads it
    /// (navigation buttons, the tick, the list box).
    fn set_chapters(&self, chapters: Vec<Chapter>) {
        fill_chapter_list(&self.chapter_list, &self.chapters_live, chapters);
    }
}

/// Build the session window in its (empty) loading state. No
/// media is loaded here: `begin_load` + `deliver` drive the
/// load, so the window opens immediately (M11).
fn create_session(article: &Article) -> Rc<SessionControl> {
    let db_path = crate::db_path();
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    let resume = ResumePolicy::from_config(&cfg);

    // Player knobs from config (Python keys/defaults). The
    // button labels are built from them once, here; the
    // handlers re-read the config when used, because the one
    // session outlives any single Settings change.
    let seek_back_ms = cfg.get_i64("seek_back_ms", 10_000);
    let seek_fwd_ms = cfg.get_i64("seek_forward_ms", seek_back_ms);

    // Shared with every handler so a queue switch (which clears
    // the chapters, like Python's load_media([])) is seen by the
    // navigation buttons and the tick alike.
    let chapters_live = Rc::new(RefCell::new((
        Vec::<blindrss_core::db::Chapter>::new(),
        Vec::<i64>::new(),
    )));

    let frame = Frame::builder()
        .with_title(&article.title)
        .with_size(Size::new(560, 460))
        .build();
    let media = MediaBackend::new(&frame, &cfg);
    let play_btn = Button::builder(&frame).with_label(&t("Play")).build();
    let stop_btn = Button::builder(&frame).with_label(&t("Stop")).build();
    let back_btn = Button::builder(&frame)
        .with_label(&format!("Back {}s", seek_back_ms / 1000))
        .build();
    let fwd_btn = Button::builder(&frame)
        .with_label(&format!("Forward {}s", seek_fwd_ms / 1000))
        .build();
    let vdown_btn = Button::builder(&frame)
        .with_label(&t("Volume Down"))
        .build();
    let vup_btn = Button::builder(&frame).with_label(&t("Volume Up")).build();
    let slower_btn = Button::builder(&frame).with_label(&t("Slower")).build();
    let faster_btn = Button::builder(&frame).with_label(&t("Faster")).build();
    let prev_ch_btn = Button::builder(&frame)
        .with_label(&t("Previous Chapter"))
        .build();
    let next_ch_btn = Button::builder(&frame)
        .with_label(&t("Next Chapter"))
        .build();
    // Speed picker (Audit 8 M15, Python's read-only speed
    // combo): the full 0.5–4.0 ladder in 0.1 steps. Appended
    // at the end of the second button row — no existing
    // control moves.
    let speed_combo = ComboBox::builder(&frame)
        .with_style(ComboBoxStyle::ReadOnly)
        .build();
    {
        let speeds = playback_speeds();
        for speed in &speeds {
            speed_combo.append(&format!("{}x", fmt_speed(*speed)));
        }
        if let Some(idx) = speed_index(&speeds, cfg_f64(&cfg, "playback_speed", 1.0)) {
            speed_combo.set_selection(idx as u32);
        }
    }
    // Chapters button (Python's "Chapters Menu"): opens the
    // dynamic chapter popup — open-link item with live
    // availability, then one jump item per chapter.
    let chapters_btn = Button::builder(&frame).with_label(&t("Chapters")).build();
    // Absolute volume (Audit 8 M15, Python's volume slider +
    // percent readout): a new row appended after the chapter
    // list, so the existing tab order is untouched.
    let volume_slider = Slider::builder(&frame).build();
    volume_slider.set_range(0, 100);
    let volume_pct = cfg.get_i64("volume", 100).clamp(0, 100) as i32;
    volume_slider.set_value(volume_pct);
    let volume_value = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::ReadOnly)
        .build();
    volume_value.set_value(&format!("{volume_pct}%"));
    // Position readout as a read-only text control, not a
    // static text: it is focusable, so NVDA can land on it and
    // read the position on demand.
    let pos_label = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::ReadOnly)
        .build();
    pos_label.set_value(&t("No media loaded"));
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
    crate::msaa_names::set_name(&seek_slider, &t("Seek"));
    crate::msaa_names::set_name(&chapter_list, &t("Chapters"));
    crate::msaa_names::set_name(&pos_label, &t("Playback Time"));
    crate::msaa_names::set_name(&speed_combo, &t("Playback Speed"));
    crate::msaa_names::set_name(&chapters_btn, &t("Chapters Menu"));
    crate::msaa_names::set_name(&volume_slider, &t("Volume"));
    crate::msaa_names::set_name(
        &volume_value,
        &format!("{} {volume_pct}%", t("Volume Level:")),
    );
    crate::msaa_names::set_name(
        &back_btn,
        &format!("{} {}s", t("Rewind"), seek_back_ms / 1000),
    );
    crate::msaa_names::set_name(
        &fwd_btn,
        &format!("{} {}s", t("Fast Forward"), seek_fwd_ms / 1000),
    );
    let seek_caption = StaticText::builder(&frame).with_label(&t("Seek")).build();
    let chapters_caption = StaticText::builder(&frame)
        .with_label(&t("Chapters"))
        .build();
    // The chapter list starts empty: chapters arrive with the
    // delivery (or after it, via update_chapters).

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
    row2.add(&speed_combo, 0, SizerFlag::All, 4);
    row2.add(&chapters_btn, 0, SizerFlag::All, 4);
    let volume_row = BoxSizer::builder(Orientation::Horizontal).build();
    volume_row.add(&volume_slider, 1, SizerFlag::Expand | SizerFlag::All, 4);
    volume_row.add(&volume_value, 0, SizerFlag::All, 4);
    let col = BoxSizer::builder(Orientation::Vertical).build();
    col.add(&media.window(), 1, SizerFlag::Expand | SizerFlag::All, 4);
    col.add_sizer(&row, 0, SizerFlag::Expand, 0);
    col.add_sizer(&row2, 0, SizerFlag::Expand, 0);
    col.add(&pos_label, 0, SizerFlag::All, 4);
    col.add(&seek_caption, 0, SizerFlag::All, 4);
    col.add(&seek_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    col.add(&chapters_caption, 0, SizerFlag::All, 4);
    col.add(&chapter_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    col.add_sizer(&volume_row, 0, SizerFlag::Expand, 0);
    frame.set_sizer(col, true);
    frame.create_status_bar(1, 0, -1, "player status");
    frame.set_status_text(&article.title, 0);
    // Player menu bar (Audit 8 M13/M15, Python's
    // _init_player_menu_bar): a Chapters menu whose items'
    // availability the tick keeps in step with the active
    // chapter (Python's `_refresh_chapter_controls_state`).
    // Python's Playback menu holds only the Cast item; in the
    // Rust port casting is main-window surface, so there is
    // no Playback menu here.
    let chapter_menu = Menu::builder()
        .append_item(PCM_SHOW_CHAPTERS, "Show &Chapters\tCtrl+M", "")
        .append_item(PCM_OPEN_LINK, "Open Chapter &Link\tCtrl+Shift+L", "")
        .append_separator()
        .append_item(PCM_PREV_CHAPTER, "&Previous Chapter\tCtrl+Shift+Left", "")
        .append_item(PCM_NEXT_CHAPTER, "&Next Chapter\tCtrl+Shift+Right", "")
        .build();
    let menu_bar = MenuBar::builder().append(chapter_menu, "&Chapters").build();
    frame.set_menu_bar(menu_bar);

    // The window opens in its loading state: skip workers,
    // the resume target and the stream itself all arrive with
    // the delivery (SessionControl::deliver), so nothing here
    // waits on resolution or metadata (M11). The skip channel
    // starts senderless; the tick's drain treats a disconnected
    // channel as "no ranges yet".
    let (_, skip_rx) = std::sync::mpsc::channel();
    let (_, silence_rx) = std::sync::mpsc::channel();
    let (chapter_tx, chapter_rx) = std::sync::mpsc::channel();
    let (write_tx, write_confirm_rx) = spawn_write_worker(db_path.clone());
    let state = Rc::new(RefCell::new(PlayerState {
        // Virgin identity: begin_load binds the first article
        // (and the empty stream marks "nothing loaded yet" for
        // the position saves).
        article_id: String::new(),
        title: String::new(),
        db_path,
        article_url: String::new(),
        resume,
        last_save: std::time::Instant::now(),
        seeked: true,
        saved_position_ms: 0,
        seek_verify: None,
        stopped_needs_resume: false,
        last_queued_pos: -1,
        write_tx: Some(write_tx),
        write_confirm_rx,
        skips: Vec::new(),
        skip_rx,
        skipped: HashSet::new(),
        silence: SilenceSkipPolicy::new(),
        silence_ranges: Vec::new(),
        silence_ready: false,
        silence_remote: false,
        silence_pending: None,
        silence_scan: None,
        silence_rx,
        silence_pad_ms: 60,
        silence_gap_ms: 260,
        silence_resume_backoff: 360,
        silence_retrigger: 1400,
        tick_fast: false,
        tick_fast_applied: false,
        silence_dragging: false,
        silence_drag_at: None,
        last_chapter: None,
        slider_guard: false,
        last_playing: None,
        autoplay_active: false,
        autoplay_ticks: 0,
        autoplay_level: 0,
        stream: String::new(),
        headers: Vec::new(),
        range_cache_resolved: false,
        range_cache_original: None,
        last_pos_text: String::new(),
        last_saved_pos: -1,
        hidden: HiddenAnnouncer::new(),
        seek_announce_at: None,
        chapter_pending: None,
        chapter_commit: None,
        chapter_gate: ChapterGate::new(),
        chapter_tx,
        chapter_rx,
        media_target: String::new(),
        snapshot: PlaybackSnapshot::default(),
        volume_guard: false,
        queue: None,
    }));

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
            drop(s);
            std::thread::spawn(move || {
                let cfg =
                    blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
                // A completed download on disk wins (M19
                // legacy lookup, as in the main window's
                // delivery path).
                let legacy = crate::downloaded_path_for(&cfg, &entry)
                    .and_then(|p| blindrss_core::media::local_media_url(&p).ok());
                let result = match legacy {
                    Some(file_url) => Ok((file_url, Vec::new())),
                    None => blindrss_core::media::resolve_article_for_play(&cfg, &entry).map(|r| {
                        let headers = crate::playback_headers_for(&entry, &r.url, &path);
                        (
                            crate::maybe_cache_youtube(&entry, &r.url, r.via_ytdlp),
                            headers,
                        )
                    }),
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
            // Play after Stop first re-runs the resume
            // restore against the saved row (M08).
            resume_after_stop(&st_pp);
            m.play();
            crate::announce("general", "Playing");
            // This press already said it: the tick's
            // hidden-window readiness announcement must not
            // repeat it when the state lands.
            note_announced(&mut st_pp.borrow_mut(), "Playing");
        }
    });
    let pp = play_pause.clone();
    play_btn.on_click(move |_| pp());
    let m = media;
    let st = state.clone();
    let stop: Rc<dyn Fn()> = Rc::new(move || {
        // Save only when media actually loaded (see
        // begin_load): a stop during loading must not zero
        // the article's saved resume position.
        let had_stream = {
            let s = st.borrow();
            if !s.stream.is_empty() {
                save_position(&s, &m, false);
                true
            } else {
                false
            }
        };
        m.stop();
        let mut s = st.borrow_mut();
        s.autoplay_active = false;
        // Stop cancels the silence scan too (Python's stop
        // path calls `_cancel_silence_scan`).
        cancel_silence_scan(&mut s);
        if had_stream {
            s.snapshot.state = "Stopped".to_string();
        }
        // Any pending seek verification ends with the stop;
        // Play re-runs the restore from the saved row (M08).
        s.seek_verify = None;
        if had_stream {
            s.stopped_needs_resume = true;
        }
    });
    let stop_click = stop.clone();
    stop_btn.on_click(move |_| stop_click());
    // Seek / volume controls, shared between the window's
    // buttons and the main window's Player menu (Batch 41a's
    // player.rewind / player.forward / player.volume_* dispatch
    // names land in the session command match below). Amounts
    // are re-read from the config at use time: the one session
    // outlives any single Settings change. Every seek (re)arms
    // the debounced landed-seek summary (M10).
    let rewind: Rc<dyn Fn()> = {
        let m = media;
        let st = state.clone();
        Rc::new(move || {
            let cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            let target = m.seek(-cfg.get_i64("seek_back_ms", 10_000), SeekMode::FromCurrent);
            begin_user_seek(&mut st.borrow_mut(), target);
            arm_seek_summary(&st);
        })
    };
    let forward: Rc<dyn Fn()> = {
        let m = media;
        let st = state.clone();
        Rc::new(move || {
            let cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            let back = cfg.get_i64("seek_back_ms", 10_000);
            let target = m.seek(cfg.get_i64("seek_forward_ms", back), SeekMode::FromCurrent);
            begin_user_seek(&mut st.borrow_mut(), target);
            arm_seek_summary(&st);
        })
    };
    let volume_down: Rc<dyn Fn()> = {
        let m = media;
        Rc::new(move || {
            let v = (m.get_volume() - volume_step()).max(0.0);
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
            let v = (m.get_volume() + volume_step()).min(1.0);
            m.set_volume(v);
            persist_volume(v);
            crate::announce(
                "general",
                &format!("Volume {}%", (v * 100.0).round() as i64),
            );
        })
    };
    let rw = rewind.clone();
    back_btn.on_click(move |_| rw());
    let fw = forward.clone();
    fwd_btn.on_click(move |_| fw());
    let vd = volume_down.clone();
    vdown_btn.on_click(move |_| vd());
    let vu = volume_up.clone();
    vup_btn.on_click(move |_| vu());
    // Speed stepping on Python's ladder (Audit 8 M15): the
    // current speed is the SAVED one (Python reads the config,
    // not the backend), snapped to the nearest rung and then
    // stepped; the picker follows every change.
    let change_speed: Rc<dyn Fn(i32)> = {
        let m = media;
        let frame_sp = frame;
        Rc::new(move |dir| {
            let mut cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            let current = cfg_f64(&cfg, "playback_speed", 1.0);
            let rate = speed_step(&playback_speeds(), current, dir);
            if (rate - current).abs() > 0.000_1 {
                apply_speed_selection(&m, &speed_combo, rate);
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
    // Picking a speed in the picker applies it at once
    // (Python's on_speed_select -> set_playback_speed).
    {
        let m = media;
        let frame_sc = frame;
        speed_combo.on_selection_changed(move |_| {
            let Some(sel) = speed_combo.get_selection() else {
                return;
            };
            let speeds = playback_speeds();
            let Some(&rate) = speeds.get(sel as usize) else {
                return;
            };
            m.set_playback_rate(rate);
            let mut cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            cfg.set_path("playback_speed", serde_json::json!(rate));
            let _ = cfg.save(&crate::config_path());
            let label = format!("Speed {}x", fmt_speed(rate));
            frame_sc.set_status_text(&label, 0);
            crate::announce("playback_speed", &label);
        });
    }
    // Chapter commands (Audit 8 M13, Python's player chapter
    // model): selecting a chapter in the list only RECORDS it
    // (Python's on_chapter_select); Enter / double-click
    // commits the seek through the debounced commit path
    // shared by every jump; Previous selects the previous
    // chapter outright (no restart-current rule); the chapter
    // link opens for the selection's — or the active —
    // chapter when it carries a valid href.
    let session_epoch = std::time::Instant::now();
    let commit_chapter: Rc<dyn Fn(usize)> = {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        let st = state.clone();
        Rc::new(move |idx| {
            let (chapters_nav, starts_nav) = chapters_live_c.borrow().clone();
            let Some(&target_ms) = starts_nav.get(idx) else {
                return;
            };
            let now_ms = session_epoch.elapsed().as_millis() as u64;
            {
                let mut s = st.borrow_mut();
                if !chapter_commit_allowed(s.chapter_commit, idx, now_ms) {
                    return;
                }
                s.chapter_commit = Some((idx, now_ms));
                s.chapter_pending = Some(idx);
            }
            chapter_list.set_selection(idx as u32, true);
            // Python clamps the target to the duration when
            // the duration is known.
            let len = m.length();
            let target = if len > 0 {
                target_ms.min(len)
            } else {
                target_ms
            };
            m.seek(target, SeekMode::FromStart);
            begin_user_seek(&mut st.borrow_mut(), target);
            m.play();
            let label = blindrss_core::chapters::chapter_announce_text(&chapters_nav[idx]);
            frame_ch.set_status_text(&label, 0);
            crate::announce("media_navigation", &label);
        })
    };
    let jump_prev_chapter: Rc<dyn Fn()> = {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        let commit = commit_chapter.clone();
        Rc::new(move || {
            let (_, starts_nav) = chapters_live_c.borrow().clone();
            if starts_nav.is_empty() {
                frame_ch.set_status_text("No chapters", 0);
                return;
            }
            let active = chapter_index_for_position(&starts_nav, m.tell());
            match chapter_prev_target(active) {
                Some(target) => commit(target),
                None => frame_ch.set_status_text("No previous chapter", 0),
            }
        })
    };
    let jump_next_chapter: Rc<dyn Fn()> = {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        let commit = commit_chapter.clone();
        Rc::new(move || {
            let (_, starts_nav) = chapters_live_c.borrow().clone();
            if starts_nav.is_empty() {
                frame_ch.set_status_text("No chapters", 0);
                return;
            }
            let active = chapter_index_for_position(&starts_nav, m.tell());
            match chapter_next_target(active, starts_nav.len()) {
                Some(target) => commit(target),
                None => frame_ch.set_status_text("No next chapter", 0),
            }
        })
    };
    {
        let jump = jump_prev_chapter.clone();
        prev_ch_btn.on_click(move |_| jump());
    }
    {
        let jump = jump_next_chapter.clone();
        next_ch_btn.on_click(move |_| jump());
    }
    // Open the chapter link for the link-action chapter
    // (Python's _open_chapter_link_at_index over
    // _chapter_link_action_index): the list selection when it
    // names a chapter, else the active chapter.
    let open_chapter_link: Rc<dyn Fn()> = {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        Rc::new(move || {
            let (chapters_nav, starts_nav) = chapters_live_c.borrow().clone();
            let active = chapter_index_for_position(&starts_nav, m.tell());
            let link_idx = chapter_link_index(
                chapter_list.get_selection().map(|s| s as usize),
                active,
                chapters_nav.len(),
            );
            let href = if link_idx >= 0 {
                chapters_nav
                    .get(link_idx as usize)
                    .and_then(|c| c.href.as_deref())
                    .and_then(blindrss_core::chapters::validated_chapter_href)
            } else {
                None
            };
            match href {
                Some(url) => crate::open_article_link(&frame_ch, &url),
                None => frame_ch.set_status_text("Chapter link unavailable", 0),
            }
        })
    };
    // Seek to the chapter selected in the list — shared by
    // double-click and by Enter in the list (a screen reader
    // user cannot double-click; the list must be fully
    // keyboard-operable). The commit goes to the pending
    // selection recorded by the selection handler, falling
    // back to the list's current selection.
    let chapter_seek: Rc<dyn Fn()> = {
        let st = state.clone();
        let commit = commit_chapter.clone();
        Rc::new(move || {
            let idx = st
                .borrow()
                .chapter_pending
                .or_else(|| chapter_list.get_selection().map(|s| s as usize));
            if let Some(idx) = idx {
                commit(idx);
            }
        })
    };
    {
        let cs = chapter_seek.clone();
        chapter_list.on_item_double_clicked(move |_| cs());
    }
    // Selecting a chapter records it as pending — no seek
    // (Python's on_chapter_select). The tick's own selection
    // sync records the same way, harmlessly: committing the
    // active chapter seeks to its start, as in Python.
    {
        let st = state.clone();
        chapter_list.on_selection_changed(move |_| {
            if let Some(sel) = chapter_list.get_selection() {
                st.borrow_mut().chapter_pending = Some(sel as usize);
            }
        });
    }
    // The Chapters button's popup (Python's
    // _show_chapters_popup_menu): the open-link item for the
    // link-action chapter with its availability, a separator,
    // then one jump item per chapter with the active chapter
    // marked [Current]. Built fresh on every click, so it
    // always reflects the live set — including chapters that
    // arrived after playback started (M12).
    {
        let m = media;
        let frame_ch = frame;
        let chapters_live_c = chapters_live.clone();
        chapters_btn.on_click(move |_| {
            let (chapters_nav, starts_nav) = chapters_live_c.borrow().clone();
            let mut menu = Menu::builder();
            if chapters_nav.is_empty() {
                menu = menu.append_item(PCM_POPUP_NONE, "No chapters available", "");
                let mut menu = menu.build();
                menu.enable_item(PCM_POPUP_NONE, false);
                frame_ch.popup_menu(&mut menu, None);
                return;
            }
            let active = chapter_index_for_position(&starts_nav, m.tell());
            let link_idx = chapter_link_index(
                chapter_list.get_selection().map(|s| s as usize),
                active,
                chapters_nav.len(),
            );
            let link_label = if link_idx >= 0 {
                format!(
                    "Open Link for {}",
                    blindrss_core::chapters::chapter_menu_label(&chapters_nav[link_idx as usize])
                )
            } else {
                "Open Link for Selected Chapter".to_string()
            };
            let link_ok = link_idx >= 0
                && chapters_nav
                    .get(link_idx as usize)
                    .and_then(|c| c.href.as_deref())
                    .and_then(blindrss_core::chapters::validated_chapter_href)
                    .is_some();
            menu = menu.append_item(PCM_POPUP_OPEN_LINK, &link_label, "");
            menu = menu.append_separator();
            for (i, ch) in chapters_nav.iter().enumerate() {
                let label = blindrss_core::chapters::chapter_menu_label(ch);
                let label = if i as i64 == active {
                    format!("[Current] {label}")
                } else {
                    label
                };
                menu = menu.append_item(PCM_CHAPTER_BASE + i as i32, &label, "");
            }
            let mut menu = menu.build();
            menu.enable_item(PCM_POPUP_OPEN_LINK, link_ok);
            frame_ch.popup_menu(&mut menu, None);
        });
    }
    // The menu bar's Chapters menu and the popup's items land
    // here (this frame's one menu handler).
    {
        let frame_m = frame;
        let commit = commit_chapter.clone();
        let prev = jump_prev_chapter.clone();
        let next = jump_next_chapter.clone();
        let open = open_chapter_link.clone();
        frame.on_menu(move |ev| {
            let id = ev.get_id();
            match id {
                PCM_SHOW_CHAPTERS => {
                    frame_m.show(true);
                    frame_m.raise();
                    chapter_list.set_focus();
                }
                PCM_OPEN_LINK | PCM_POPUP_OPEN_LINK => open(),
                PCM_PREV_CHAPTER => prev(),
                PCM_NEXT_CHAPTER => next(),
                _ if (PCM_CHAPTER_BASE..PCM_CHAPTER_BASE + 1000).contains(&id) => {
                    commit((id - PCM_CHAPTER_BASE) as usize);
                }
                _ => {}
            }
        });
    }

    // Player keyboard control (the window had none: every
    // action needed Tab + Space on a button). Keys are left
    // native where a control already owns them — Space/Enter
    // on buttons, arrows on the seek slider and chapter list.
    {
        let frame_k = frame;
        let m = media;
        let st_k = state.clone();
        let pp_k = play_pause.clone();
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
            chapters_btn,
        ]
        .iter()
        .map(|b| b.get_handle() as usize)
        .collect();
        let slider_h = seek_slider.get_handle() as usize;
        let list_h = chapter_list.get_handle() as usize;
        // Per-control help topics (Audit 8 M15, Python's
        // set_help_topic tags at the end of player UI
        // construction): the seek slider, the time readout and
        // the volume slider are "player-controls", the speed
        // picker is "playback-speed", the chapter controls are
        // "chapters", everything else falls back to the
        // frame's "player". (Python also tags its Cast button
        // "casting"; the Rust player has no cast control —
        // casting is main-window surface here.)
        let speed_h = speed_combo.get_handle() as usize;
        let volume_h = volume_slider.get_handle() as usize;
        let pos_h = pos_label.get_handle() as usize;
        let chapters_btn_h = chapters_btn.get_handle() as usize;
        frame.bind_internal(EventType::CHAR_HOOK, move |ev| {
            let code = ev.get_key_code().unwrap_or(0);
            let focused = ev
                .get_event_object()
                .map(|w| w.get_handle() as usize)
                .unwrap_or(0);
            let on_button = btn_handles.contains(&focused);
            let on_slider = focused == slider_h;
            let on_list = focused == list_h;
            // Enter in the chapter list commits the selection —
            // checked first, with any modifiers (Python
            // player.on_char_hook order).
            if code == 13 && on_list {
                chapter_seek();
                ev.skip(false);
                return;
            }
            // Configurable shortcuts (Audit 8 M14): the shared
            // bridge forms the accelerator. F1 follows the
            // binding-aware help rule; registry commands delegate
            // to the main frame with the text guard off, exactly
            // Python's player delegation; media arrows resolve
            // when no command claimed the chord. A modified key
            // nobody claims passes on to the focused control.
            let mods = blindrss_core::shortcuts::KeyModifiers {
                ctrl: ev.control_down(),
                alt: ev.alt_down(),
                shift: ev.shift_down(),
                cmd: ev.meta_down(),
            };
            if let Some(accel) = blindrss_core::shortcuts::event_accel(code, mods, true) {
                let cfg =
                    blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
                let map = blindrss_core::shortcuts::accel_map(
                    &blindrss_core::shortcuts::overrides_from_config(&cfg),
                );
                if blindrss_core::shortcuts::help_key_action(&accel, &map)
                    == blindrss_core::shortcuts::HelpKeyAction::ContextHelp
                {
                    let topic = if on_list || focused == chapters_btn_h {
                        "chapters"
                    } else if focused == speed_h {
                        "playback-speed"
                    } else if on_slider || focused == volume_h || focused == pos_h {
                        "player-controls"
                    } else {
                        "player"
                    };
                    crate::help::show_help_topic(&frame_k, &crate::ui_language(), Some(topic));
                    ev.skip(false);
                    return;
                }
                if dispatch_shortcut_accel(&accel) {
                    ev.skip(false);
                    return;
                }
                let media_handled = match blindrss_core::shortcuts::media_arrow_action(
                    cfg!(target_os = "macos"),
                    mods,
                    code,
                ) {
                    Some("seek_back") => {
                        let target = m.seek(-seek_back_ms, SeekMode::FromCurrent);
                        begin_user_seek(&mut st_k.borrow_mut(), target);
                        true
                    }
                    Some("seek_forward") => {
                        let target = m.seek(seek_fwd_ms, SeekMode::FromCurrent);
                        begin_user_seek(&mut st_k.borrow_mut(), target);
                        true
                    }
                    Some("volume_up") => {
                        let v = (m.get_volume() + volume_step()).min(1.0);
                        m.set_volume(v);
                        persist_volume(v);
                        crate::announce(
                            "general",
                            &format!("Volume {}%", (v * 100.0).round() as i64),
                        );
                        true
                    }
                    Some("volume_down") => {
                        let v = (m.get_volume() - volume_step()).max(0.0);
                        m.set_volume(v);
                        persist_volume(v);
                        crate::announce(
                            "general",
                            &format!("Volume {}%", (v * 100.0).round() as i64),
                        );
                        true
                    }
                    _ => false,
                };
                ev.skip(!media_handled);
                return;
            }
            match code {
                // Escape closes the player.
                27 => {
                    frame_k.close(false);
                    ev.skip(false);
                }
                // (Enter in the chapter list is handled above,
                // before shortcut delegation.)
                // Space toggles play/pause anywhere a button
                // does not already own it — the same closure
                // the Play/Pause button runs. The speed picker
                // owns Space too (it opens the dropdown).
                32 if !on_button && focused != speed_h => {
                    pp_k();
                    ev.skip(false);
                }
                // Left/Right seek, Up/Down volume — outside the
                // controls that use arrows themselves.
                314 if !on_slider && !on_list => {
                    let cfg = blindrss_core::config::Config::load(&crate::config_path())
                        .unwrap_or_default();
                    let target =
                        m.seek(-cfg.get_i64("seek_back_ms", 10_000), SeekMode::FromCurrent);
                    begin_user_seek(&mut st_k.borrow_mut(), target);
                    arm_seek_summary(&st_k);
                    ev.skip(false);
                }
                316 if !on_slider && !on_list => {
                    let cfg = blindrss_core::config::Config::load(&crate::config_path())
                        .unwrap_or_default();
                    let back = cfg.get_i64("seek_back_ms", 10_000);
                    let target =
                        m.seek(cfg.get_i64("seek_forward_ms", back), SeekMode::FromCurrent);
                    begin_user_seek(&mut st_k.borrow_mut(), target);
                    arm_seek_summary(&st_k);
                    ev.skip(false);
                }
                315 if !on_slider && !on_list => {
                    let v = (m.get_volume() + volume_step()).min(1.0);
                    m.set_volume(v);
                    persist_volume(v);
                    crate::announce(
                        "general",
                        &format!("Volume {}%", (v * 100.0).round() as i64),
                    );
                    ev.skip(false);
                }
                317 if !on_slider && !on_list => {
                    let v = (m.get_volume() - volume_step()).max(0.0);
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
            // A user slider event marks a drag in progress
            // for the silence policy (Python's
            // `_is_dragging_slider` guard); the tick clears
            // it after a quiet interval.
            {
                let mut s = st.borrow_mut();
                s.silence_dragging = true;
                s.silence_drag_at = Some(std::time::Instant::now());
            }
            let len = m.length();
            if len > 0 {
                let target = len * ev.get_value() as i64 / 1000;
                m.seek(target, SeekMode::FromStart);
                begin_user_seek(&mut st.borrow_mut(), target);
                arm_seek_summary(&st);
            }
        });
    }

    // Absolute volume slider (Audit 8 M15): a drag sets the
    // volume outright (Python's on_volume_slider ->
    // set_volume_percent). The backend volume is applied only
    // while playing — Python does the same because changing
    // VLC's volume while paused unpauses it (user-reported) —
    // but the value always persists and the readout always
    // follows. The tick's programmatic sync is guarded out.
    {
        let m = media;
        let st = state.clone();
        volume_slider.on_slider(move |_| {
            if st.borrow().volume_guard {
                return;
            }
            let percent = volume_slider.get_value().clamp(0, 100);
            if m.get_state() == MediaState::Playing {
                m.set_volume(percent as f64 / 100.0);
            }
            persist_volume(percent as f64 / 100.0);
            volume_value.set_value(&format!("{percent}%"));
            crate::msaa_names::set_name(
                &volume_value,
                &format!("{} {percent}%", t("Volume Level:")),
            );
        });
    }

    // Player tick: drain skip ranges, resume seek once length
    // is known, apply skips, announce chapter crossings, update
    // the position label, and save playback state. The tick
    // runs at 1 s, sped up to 280 ms while Skip Silence is on
    // (Python's cadence, applied inside the tick below).
    // The Chapters menu's availability as last refreshed by
    // the tick: (chapter count, active index, link available).
    let menu_state_last = Rc::new(std::cell::Cell::new((usize::MAX, i64::MIN, false)));
    // One shared handle, copied into each closure that
    // needs it (the tick, the close handler): Timer's
    // methods all take &self.
    let timer: &'static Timer<_> = Box::leak(Box::new(Timer::new(&frame)));
    let timer_cadence = timer;
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
        let mut reseek_to: Option<i64> = None;
        let mut queue_load = None;
        let mut queue_error: Option<String> = None;
        {
            let mut s = st.borrow_mut();
            // Persistence confirmations (M07): save bookkeeping
            // advances only for writes the worker reports
            // landed — a queued-but-unwritten position never
            // counts as saved.
            while let Ok(written) = s.write_confirm_rx.try_recv() {
                for (id, saved) in written {
                    if id == blindrss_core::playback::playback_id(&s.article_id) {
                        s.last_saved_pos = saved.position_ms;
                        s.last_save = std::time::Instant::now();
                    }
                }
            }
            // Chapter-fetch results (Audit 8 M12): apply only
            // while the load generation is current AND the
            // fetch stamp still belongs to the article the
            // session holds — a stale queue result is dropped
            // whole. Empty results never apply (Python's fetch
            // thread only delivers a non-empty set).
            while let Ok((cgen, article_id, stamp, chapters)) = s.chapter_rx.try_recv() {
                if SESSION_POLICY.with(|p| p.borrow().is_current(cgen))
                    && s.article_id == article_id
                    && s.chapter_gate.accepts(&article_id, stamp)
                    && !chapters.is_empty()
                {
                    fill_chapter_list(&chapter_list, &chapters_tick, chapters);
                }
            }
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
                            // The advance is a new load in the
                            // session policy: any main-window
                            // delivery still in flight for the
                            // previous item goes stale, and the
                            // routine load states are
                            // pre-suppressed — the queue
                            // advancing on its own says nothing
                            // unless it fails (Python's
                            // load_media(announce=False)).
                            SESSION_POLICY
                                .with(|p| p.borrow_mut().begin_internal_load(identity_of(&entry)));
                            s.hidden.reset(&["Loading media...", "Playing"]);
                            s.article_id = entry.id.clone();
                            s.article_url = entry.url.clone();
                            s.title = entry.title.clone();
                            // The identity switch retires the
                            // old entry's chapter fetches and
                            // pending chapter state, and the
                            // main window's live snapshot now
                            // names the new entry (M12/M15).
                            s.chapter_gate.invalidate();
                            s.chapter_pending = None;
                            s.chapter_commit = None;
                            s.media_target = entry
                                .media_url
                                .as_deref()
                                .filter(|u| !u.is_empty())
                                .unwrap_or(&entry.url)
                                .to_string();
                            s.snapshot = PlaybackSnapshot {
                                has_media: true,
                                playing: false,
                                state: "Loading media...".to_string(),
                                title: entry.title.clone(),
                                article_id: entry.id.clone(),
                                media_url: s.media_target.clone(),
                                position_ms: 0,
                                duration_ms: 0,
                            };
                            s.skips.clear();
                            s.skipped.clear();
                            // The advance retires the old
                            // entry's silence scan and queues
                            // the new entry's (Task 21).
                            cancel_silence_scan(&mut s);
                            s.last_chapter = None;
                            let cfg2 = blindrss_core::config::Config::load(&crate::config_path())
                                .unwrap_or_default();
                            s.resume = ResumePolicy::from_config(&cfg2);
                            s.last_save = std::time::Instant::now();
                            s.last_saved_pos = -1;
                            s.skip_rx = spawn_skip_workers(&cfg2, &entry);
                            queue_silence_scan(&mut s, &cfg2, &stream);
                            s.saved_position_ms = Db::open(&s.db_path)
                                .ok()
                                .and_then(|db| {
                                    saved_state(&db, &entry.id, &entry.url).ok().flatten()
                                })
                                .map(|saved| s.resume.resume_target(&saved, &stream))
                                .unwrap_or(0);
                            s.seeked = s.saved_position_ms <= 0;
                            s.seek_verify = None;
                            if s.saved_position_ms > 0 {
                                s.seek_verify = Some(resume_verifier(s.saved_position_ms, &stream));
                            }
                            let stored_chapters = Db::open(&s.db_path)
                                .ok()
                                .and_then(|db| db.chapters(&entry.id).ok())
                                .unwrap_or_default();
                            let had_chapters = !stored_chapters.is_empty();
                            fill_chapter_list(&chapter_list, &chapters_tick, stored_chapters);
                            // An entry with no stored chapters
                            // gets the same asynchronous chapter
                            // fetch a fresh load gets (M12):
                            // provider / embedded results
                            // upgrade the live player when they
                            // land, gate-stamped so a result for
                            // an entry the queue has since left
                            // never applies.
                            if !had_chapters {
                                let stamp = s.chapter_gate.begin(&entry.id);
                                let cgen = SESSION_POLICY.with(|p| p.borrow().generation());
                                spawn_chapter_fetch(
                                    &s.db_path,
                                    &s.chapter_tx,
                                    cgen,
                                    entry.id.clone(),
                                    stamp,
                                );
                            }
                            set_queue_index(idx as i64);
                            queue_load = Some((stream, entry.title.clone(), headers));
                        }
                        Err(e) => queue_error = Some(e),
                    }
                }
                s.queue = Some(q);
            }
            while let Ok(ranges) = s.skip_rx.try_recv() {
                s.skips.extend(ranges);
            }
            // Silence scan results (Task 21): progress sets
            // publish incrementally — padded and gap-merged
            // exactly like Python's `_publish` — and a set
            // arriving for a stale generation aborts the scan
            // instead of applying.
            while let Ok(msg) = s.silence_rx.try_recv() {
                match msg {
                    SilenceScanMsg::Progress(generation, raw) => {
                        if SESSION_POLICY.with(|p| p.borrow().is_current(generation)) {
                            s.silence_ranges = blindrss_core::silence::pad_and_merge_ms(
                                raw,
                                s.silence_pad_ms,
                                s.silence_gap_ms,
                            );
                            s.silence_ready = true;
                        } else if let Some(scan) = &s.silence_scan {
                            scan.abort.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    SilenceScanMsg::Done(generation, result) => {
                        s.silence_scan = None;
                        if SESSION_POLICY.with(|p| p.borrow().is_current(generation)) {
                            match result {
                                Ok(raw) => {
                                    s.silence_ranges = blindrss_core::silence::pad_and_merge_ms(
                                        raw,
                                        s.silence_pad_ms,
                                        s.silence_gap_ms,
                                    );
                                    s.silence_ready = true;
                                }
                                Err(e) => {
                                    if e != "cancelled" {
                                        eprintln!("[silence-scan] {e}");
                                    }
                                    // Python keeps any ranges
                                    // progress already
                                    // published; with none,
                                    // ready stays false.
                                }
                            }
                        }
                    }
                }
            }
            // Start the queued scan once playback is under
            // way and the configured delay since the load has
            // passed (Python `_maybe_start_pending_silence_
            // scan`); a queue entry whose load has gone stale
            // is dropped.
            if s.silence_pending.is_some() && s.silence_scan.is_none() {
                let pending = s.silence_pending.as_ref().unwrap();
                let (pending_gen, queued_at) = (pending.generation, pending.queued_at);
                if !SESSION_POLICY.with(|p| p.borrow().is_current(pending_gen)) {
                    s.silence_pending = None;
                } else if m.get_state() == MediaState::Playing {
                    let cfg_t = blindrss_core::config::Config::load(&crate::config_path())
                        .unwrap_or_default();
                    let settings = SilenceScanSettings::from_config(&cfg_t, s.silence_remote);
                    if queued_at.elapsed().as_secs_f64() >= settings.scan_delay_s {
                        start_silence_scan(&mut s, &cfg_t, &seek_pos_t);
                    }
                }
            }
            // Verified seek drive (M05): the armed verifier —
            // resume restore or user-seek guard — observes the
            // backend's real position once a second. It waits
            // through opening/buffering, issues and re-issues
            // the seek within its bounded budget, learns seek
            // support from the outcome (persisted through the
            // write queue), and reports a failed user seek
            // exactly once.
            if let Some(mut verifier) = s.seek_verify.take() {
                let step = verifier.observe(
                    std::time::Instant::now(),
                    m.stream_phase(),
                    pos,
                    m.is_seekable(),
                );
                if let Some(target) = step.seek_to {
                    m.seek(target, SeekMode::FromStart);
                    // The silence policy's floor hears about
                    // app seeks too (Python routes them all
                    // through `_apply_seek_time_ms`).
                    s.silence
                        .note_app_seek(target, pos, std::time::Instant::now());
                }
                if let Some(supported) = step.learn_seek_supported {
                    queue_seek_supported(&s, supported);
                }
                if step.announce_failure {
                    crate::announce(
                        "general",
                        &format!(
                            "Could not seek there. Position {}",
                            blindrss_core::playback::format_time_ms(pos)
                        ),
                    );
                }
                if step.active {
                    s.seek_verify = Some(verifier);
                } else if verifier.kind() == SeekKind::Resume {
                    // The restore ended (landed or given up):
                    // position saves may resume.
                    s.seeked = true;
                }
            }
            // Silence skip policy (Task 21, M09): verify the
            // last skip landed, ratchet the position floor,
            // then evaluate Python's guards (resume pending,
            // slider drag, unseekable, cooldown, floor). The
            // policy's seeks are its own: they do NOT enter
            // the user-seek guard above (Task 19's settled
            // split) — the policy verifies landings itself.
            {
                let now = std::time::Instant::now();
                let remote = s.silence_remote;
                // Cadence (Audit 10): Python restarts its
                // player timer at 280 ms while Skip Silence
                // is on, so a skip fires within a few hundred
                // ms of the position entering a silent span
                // instead of up to a second late. Apply a
                // resolved change as it is noticed.
                if s.tick_fast != s.tick_fast_applied {
                    s.tick_fast_applied = s.tick_fast;
                    timer_cadence.start(if s.tick_fast { 280 } else { 1000 }, false);
                }
                if let Some(target) = s.silence.verify_tick(pos, now, remote) {
                    s.silence.note_app_seek(target, pos, now);
                    reseek_to = Some(target);
                }
                s.silence.update_floor(pos, now);
                if s.silence_dragging
                    && s.silence_drag_at.is_some_and(|t| {
                        now.duration_since(t) > std::time::Duration::from_millis(600)
                    })
                {
                    s.silence_dragging = false;
                }
                if m.get_state() == MediaState::Playing {
                    let ranges = s.silence_ranges.clone();
                    let ctx = SkipContext {
                        ready: s.silence_ready,
                        seekable: m.is_seekable(),
                        resume_pending: s
                            .seek_verify
                            .as_ref()
                            .is_some_and(|v| v.kind() == SeekKind::Resume),
                        dragging: s.silence_dragging,
                        casting: false,
                        is_remote: s.silence_remote,
                        resume_backoff_ms: s.silence_resume_backoff,
                        retrigger_backoff_ms: s.silence_retrigger,
                    };
                    if let Some(target) = s.silence.evaluate(&ranges, pos, now, &ctx) {
                        s.silence.note_app_seek(target, pos, now);
                        seek_to = Some((target, "Skipped silence".to_string()));
                    }
                }
            }
            let hit = s
                .skips
                .iter()
                .find(|r| {
                    // SponsorBlock ranges follow Python's
                    // segment_at: the last half second does
                    // not count, and a segment is done by
                    // UUID.
                    let tail_ms = if r.uuid.is_empty() { 250 } else { 500 };
                    pos >= r.start_ms
                        && pos < r.end_ms - tail_ms
                        && !s.skipped.contains(&skip_key(r))
                })
                .map(|r| (skip_key(r), r.end_ms, r.label.clone()));
            if let Some((key, end, label)) = hit {
                s.skipped.insert(key);
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
            // Chapters menu availability (M13, Python's
            // _refresh_chapter_controls_state on its UI timer):
            // Previous needs an earlier chapter, Next a later
            // one, Open Link a valid href on the link-action
            // chapter (the list selection when it names one,
            // else the active chapter).
            let active_now = chapter_index_for_position(&starts_now, pos);
            let link_idx = chapter_link_index(
                chapter_list.get_selection().map(|sel| sel as usize),
                active_now,
                chapters_now.len(),
            );
            let link_ok = link_idx >= 0
                && chapters_now
                    .get(link_idx as usize)
                    .and_then(|c| c.href.as_deref())
                    .and_then(blindrss_core::chapters::validated_chapter_href)
                    .is_some();
            let menu_key = (chapters_now.len(), active_now, link_ok);
            if menu_state_last.get() != menu_key {
                menu_state_last.set(menu_key);
                // The menu bar owns the Chapters menu once
                // attached; item state goes through it.
                if let Some(menu_bar) = frame.get_menu_bar() {
                    menu_bar
                        .enable_item(PCM_PREV_CHAPTER, chapter_prev_target(active_now).is_some());
                    menu_bar.enable_item(
                        PCM_NEXT_CHAPTER,
                        chapter_next_target(active_now, chapters_now.len()).is_some(),
                    );
                    menu_bar.enable_item(PCM_OPEN_LINK, link_ok);
                }
            }
        }
        if let Some((end, label)) = seek_to {
            m.seek(end, SeekMode::FromStart);
            frame_tick.set_status_text(&label, 0);
            crate::announce("sponsor_skip", &label);
        }
        if let Some(target) = reseek_to {
            // The silence policy's landing re-seek: silent,
            // at most once per skip (Python's
            // silence_skip_verify seek).
            m.seek(target, SeekMode::FromStart);
        }
        if let Some((stream, title, headers)) = queue_load {
            // No "Now playing" announcement: the advance is a
            // load the user did not just ask for, and Python
            // stays quiet about it (load_media(announce=False));
            // the frame title and status still follow.
            if start_loaded_stream(&m, &frame_tick, &play_btn, &st, &stream, headers) {
                frame_tick.set_title(&format!("Playing: {title}"));
                frame_tick.set_status_text(&format!("Queue: {title}"), 0);
            } else {
                frame_tick.set_status_text("Could not load media", 0);
                let mut s = st.borrow_mut();
                hidden_announce(&mut s, &frame_tick, "Failed to load media");
            }
        }
        if let Some(e) = queue_error {
            frame_tick.set_status_text(&format!("Queue: {e}"), 0);
            crate::announce("general", &format!("Queue: {e}"));
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
                        // Python's failure speech is the
                        // hidden-window one: said once per load,
                        // and only when the status line cannot
                        // be read.
                        hidden_announce(&mut s, &frame_tick, "Failed to load media");
                    }
                }
            }
        }
        // Keep the Play/Pause button's label honest about the
        // state — the label is its accessible name, so NVDA
        // users hear "Pause" while playing and "Play" while
        // paused instead of a static "Play/Pause". The same
        // transition is the load's readiness moment: while the
        // window is hidden, "Playing" is spoken once per load
        // (M10).
        {
            let mut s = st.borrow_mut();
            if s.last_playing != Some(playing_now) {
                s.last_playing = Some(playing_now);
                if playing_now {
                    hidden_announce(&mut s, &frame_tick, "Playing");
                }
                let label = if playing_now { t("Pause") } else { t("Play") };
                play_btn.set_label(&label);
            }
        }
        // Debounced landed-seek summary (M10): a burst of seek
        // presses speaks once, 400 ms after the last one, with
        // the position actually landed on.
        {
            let due = {
                let mut s = st.borrow_mut();
                match s.seek_announce_at {
                    Some(at) if at <= std::time::Instant::now() => {
                        s.seek_announce_at = None;
                        true
                    }
                    _ => false,
                }
            };
            if due {
                crate::announce(
                    "media_navigation",
                    &blindrss_core::playback::seek_summary(m.tell(), m.length()),
                );
            }
        }
        {
            // Python's time-info text (M15, _compose_time_info):
            // elapsed / remaining / total, or elapsed only for
            // a live or unknown-length stream. Before any media
            // loads, the readout keeps its "No media loaded"
            // initial text, as Python's does.
            if !st.borrow().stream.is_empty() {
                let text = blindrss_core::playback::seek_summary(pos, len);
                let mut s = st.borrow_mut();
                if s.last_pos_text != text {
                    s.last_pos_text = text.clone();
                    drop(s);
                    pos_label.set_value(&text);
                }
            }
        }
        // Volume slider sync (M15): the slider and its readout
        // follow the persisted volume when it changes through
        // the buttons, the keys or a queue/load reset —
        // guarded, so the sync never re-enters the slider's
        // own handler. The persisted value is the truth every
        // write path already maintains.
        {
            let cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            let percent = cfg.get_i64("volume", 100).clamp(0, 100) as i32;
            if volume_slider.get_value() != percent {
                {
                    let mut s = st.borrow_mut();
                    s.volume_guard = true;
                    volume_slider.set_value(percent);
                    s.volume_guard = false;
                }
                volume_value.set_value(&format!("{percent}%"));
                crate::msaa_names::set_name(
                    &volume_value,
                    &format!("{} {percent}%", t("Volume Level:")),
                );
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
                && pos != s.last_queued_pos
            {
                // Queue the save on the worker (M07);
                // last_saved_pos/last_save advance only when
                // its confirmation comes back.
                s.last_queued_pos = pos;
                save_position(&s, &m, false);
            }
        }
        // Live snapshot for the main window (M15): refreshed
        // here, on the UI thread, from backend state — the
        // main window only ever copies this plain data, and
        // nothing here runs in an accessibility callback.
        {
            let mut s = st.borrow_mut();
            if s.snapshot.has_media {
                s.snapshot.playing = playing_now;
                s.snapshot.position_ms = pos;
                s.snapshot.duration_ms = len;
                if playing_now {
                    s.snapshot.state = "Playing".to_string();
                } else if m.get_state() == MediaState::Paused {
                    s.snapshot.state = "Paused".to_string();
                }
                s.snapshot.title = s.title.clone();
                s.snapshot.article_id = s.article_id.clone();
                s.snapshot.media_url = s.media_target.clone();
            }
        }
    });
    timer.start(1000, false);

    let player_key = frame.get_handle() as usize;
    let command: PlayerCommand = Rc::new(move |id| {
        match id {
            "player.play_pause" => play_pause(),
            "player.stop" => stop(),
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
                let cfg =
                    blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
                media.apply_equalizer(&blindrss_core::equalizer::EqConfig::from_config(&cfg));
            }
            "speed.up" => change_speed(1),
            "speed.down" => change_speed(-1),
            "speed.reset" => change_speed(0),
            _ => return false,
        }
        true
    });
    let closing_state = state.clone();
    frame.on_close(move |event| {
        timer.stop();
        // Closing mid-load is the cancel path (M11): nothing
        // played, so there is no position to save — and saving
        // would overwrite the article's resume position with
        // zero. Only a session with media loaded saves.
        {
            let mut s = closing_state.borrow_mut();
            if !s.stream.is_empty() {
                save_position(&s, &media, false);
            }
            // Shutdown cancels the silence scan (Python's
            // player shutdown does the same): no ffmpeg
            // outlives the window.
            cancel_silence_scan(&mut s);
            // The main window's live status ends with the
            // session: its field clears on the next poll.
            s.snapshot = PlaybackSnapshot::default();
        }
        media.close();
        // The session ends with its window: clear the live
        // registration (only if it is still this window) and
        // forget the policy identity — generations keep moving,
        // so nothing stamped before the close can act later.
        SESSION.with(|s| {
            let mut slot = s.borrow_mut();
            if slot.as_ref().is_some_and(|live| live.key == player_key) {
                *slot = None;
            }
        });
        SESSION_POLICY.with(|p| p.borrow_mut().close());
        event.skip(true);
    });

    // No show/focus here: begin_load applies the
    // show_player_on_play preference for the first load (and
    // every later one) — hidden by default, without stealing
    // focus (M10).
    Rc::new(SessionControl {
        frame,
        media,
        state,
        chapters_live,
        chapter_list,
        play_btn,
        speed_combo,
        command,
    })
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
