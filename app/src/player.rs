//! Player window (slice 7 + batch 3): wx MediaCtrl over the
//! article's media URL (direct or yt-dlp-resolved), with resume
//! via the Python playback_state table, chapter navigation with
//! announcements, playback-speed control, and segment skipping
//! (SponsorBlock for YouTube, ffmpeg silence detection).
//!
//! Position is saved every second by the window timer, so closing
//! the window at any point loses at most a second of progress.

use blindrss_core::db::Chapter;
use blindrss_core::{Article, Db, PlaybackState};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use wxdragon::prelude::*;
use wxdragon::widgets::media_ctrl::SeekMode;

/// A range the player skips over, with its announcement label.
#[derive(Clone)]
struct SkipRange {
    start_ms: i64,
    end_ms: i64,
    label: String,
}

struct PlayerState {
    article_id: String,
    title: String,
    db_path: PathBuf,
    seeked: bool,
    saved_position_ms: i64,
    /// Skip ranges delivered by the SponsorBlock / silence
    /// workers on `skip_rx`, drained by the tick.
    skips: Vec<SkipRange>,
    skip_rx: std::sync::mpsc::Receiver<Vec<SkipRange>>,
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
}

/// The play queue inside a player window (config "play_queue"
/// entries as article stubs). The main window mirrors the
/// current index in config "play_queue_index" for its own
/// Next/Previous commands.
struct QueueCtx {
    entries: Vec<Article>,
    index: usize,
    rx: std::sync::mpsc::Receiver<(usize, Result<String, String>)>,
    tx: std::sync::mpsc::Sender<(usize, Result<String, String>)>,
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

fn save_position(state: &PlayerState, media: &MediaCtrl, force_completed: bool) {
    let Ok(db) = Db::open(&state.db_path) else {
        return;
    };
    let pos = media.tell();
    let len = media.length();
    let completed = force_completed || (len > 0 && pos >= len - 5_000);
    let _ = db.upsert_playback_state(
        &state.article_id,
        &PlaybackState {
            position_ms: pos,
            duration_ms: if len > 0 { Some(len) } else { None },
            completed,
            seek_supported: Some(true),
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

/// Spawn the SponsorBlock and silence workers for one article
/// and return the channel their skip ranges arrive on. Called
/// at open and again on every queue advance.
fn spawn_skip_workers(
    cfg: &blindrss_core::config::Config,
    article: &Article,
    stream_url: &str,
) -> std::sync::mpsc::Receiver<Vec<SkipRange>> {
    let (skip_tx, skip_rx) = std::sync::mpsc::channel::<Vec<SkipRange>>();
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
            std::thread::spawn(move || {
                if let Ok(segs) = blindrss_core::sponsorblock::fetch_segments(&vid, &cats) {
                    let ranges = segs
                        .iter()
                        .map(|s| SkipRange {
                            start_ms: (s.start * 1000.0) as i64,
                            end_ms: (s.end * 1000.0) as i64,
                            label: format!("Skipped {}", s.category),
                        })
                        .collect();
                    let _ = tx.send(ranges);
                }
            });
        }
    }
    if cfg.get_bool("skip_silence", true) && blindrss_core::silence::find_ffmpeg().is_some() {
        let threshold = cfg_f64(cfg, "silence_skip_threshold_db", -38.0);
        let min_silence = cfg.get_i64("silence_skip_min_ms", 700) as f64 / 1000.0;
        let url = stream_url.to_string();
        let tx = skip_tx.clone();
        std::thread::spawn(move || {
            if let Ok(ranges) = blindrss_core::silence::detect_silence(&url, threshold, min_silence)
            {
                let ranges = ranges
                    .iter()
                    .map(|(s, e)| SkipRange {
                        start_ms: (s * 1000.0) as i64,
                        end_ms: (e * 1000.0) as i64,
                        label: "Skipped silence".to_string(),
                    })
                    .collect();
                let _ = tx.send(ranges);
            }
        });
    }

    skip_rx
}

pub fn open_player(_parent: &Frame, article: Article, stream_url: String, chapters: Vec<Chapter>) {
    open_player_impl(_parent, article, stream_url, chapters, None);
}

/// Open the player on one queue entry, advancing through the
/// rest of the queue as each entry finishes.
pub fn open_player_queue(
    _parent: &Frame,
    article: Article,
    stream_url: String,
    entries: Vec<Article>,
    index: usize,
) {
    set_queue_index(index as i64);
    open_player_impl(
        _parent,
        article,
        stream_url,
        Vec::new(),
        Some((entries, index)),
    );
}

/// When the equalizer is enabled with a non-flat shape, route
/// playback through the local EQ stream (MediaCtrl exposes no
/// EQ; the stream is an ffmpeg transcode with the filter
/// chain applied — seeking inside it is limited). Otherwise
/// the URL passes through untouched.
fn eq_wrap(url: &str, cfg: &blindrss_core::config::Config) -> String {
    let eq = blindrss_core::equalizer::EqConfig::from_config(cfg);
    if eq.enabled && (eq.bands.iter().any(|g| *g != 0.0) || eq.effective_preamp_db() != 0.0) {
        let chain = blindrss_core::equalizer::ffmpeg_filter_chain(&eq);
        if !chain.is_empty() {
            return blindrss_core::proxy::eq_url(url, &chain);
        }
    }
    url.to_string()
}

fn open_player_impl(
    _parent: &Frame,
    article: Article,
    stream_url: String,
    chapters: Vec<Chapter>,
    queue: Option<(Vec<Article>, usize)>,
) {
    let db_path = crate::db_path();
    let cfg = blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
    let saved = Db::open(&db_path)
        .ok()
        .and_then(|db| db.playback_state(&article.id).ok().flatten())
        .filter(|s| !s.completed)
        .map(|s| s.position_ms)
        .unwrap_or(0);

    // Player knobs from config (Python keys/defaults).
    let seek_back_ms = cfg.get_i64("seek_back_ms", 10_000);
    let seek_fwd_ms = cfg.get_i64("seek_forward_ms", 10_000);
    let volume_step = (cfg.get_i64("volume_step", 5) as f64 / 100.0).clamp(0.01, 0.5);
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
    let media = MediaCtrl::builder(&frame).build();
    let play_btn = Button::builder(&frame)
        .with_label(&crate::tr("Play/Pause"))
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
    let pos_label = StaticText::builder(&frame)
        .with_label(&crate::tr("0:00:00 / 0:00:00"))
        .build();
    let seek_slider = Slider::builder(&frame).build();
    seek_slider.set_range(0, 1000);
    let chapter_list = ListBox::builder(&frame).build();
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
    col.add(&media, 1, SizerFlag::Expand | SizerFlag::All, 4);
    col.add_sizer(&row, 0, SizerFlag::Expand, 0);
    col.add_sizer(&row2, 0, SizerFlag::Expand, 0);
    col.add(&pos_label, 0, SizerFlag::All, 4);
    col.add(&seek_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    col.add(&chapter_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    frame.set_sizer(col, true);
    frame.create_status_bar(1, 0, -1, "player status");
    frame.set_status_text(&article.title, 0);

    // Skip-range workers: SponsorBlock (YouTube) and ffmpeg
    // silence detection report ranges over one channel; the tick
    // drains and applies them.
    let skip_rx = spawn_skip_workers(&cfg, &article, &stream_url);

    let state = Rc::new(RefCell::new(PlayerState {
        article_id: article.id.clone(),
        title: article.title.clone(),
        db_path,
        seeked: saved <= 0,
        saved_position_ms: saved,
        skips: Vec::new(),
        skip_rx,
        skipped: HashSet::new(),
        last_chapter: None,
        slider_guard: false,
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

    let stream_url = eq_wrap(&stream_url, &cfg);
    let loaded = media.load_uri(&stream_url);
    if loaded {
        media.play();
        if (start_speed - 1.0).abs() > 0.001 {
            media.set_playback_rate(start_speed);
        }
        frame.set_status_text("Playing", 0);
    } else {
        frame.set_status_text("Could not load media", 0);
    }

    // Queue advance: media finished -> resolve the next queue
    // entry on a worker; the tick performs the switch.
    {
        let st = state.clone();
        media.on_finished(move |_| {
            let mut s = st.borrow_mut();
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
            drop(s);
            std::thread::spawn(move || {
                let result = blindrss_core::media::resolve_for_play(
                    entry.media_url.as_deref(),
                    entry.media_type.as_deref(),
                    &entry.url,
                    entry.content.as_deref(),
                )
                .map(|r| crate::maybe_cache_youtube(&entry, &r.url, r.via_ytdlp));
                let _ = tx.send((next, result));
            });
        });
    }

    let m = media;
    play_btn.on_click(move |_| {
        if m.get_state() == MediaState::Playing {
            m.pause();
        } else {
            m.play();
        }
    });
    let m = media;
    let st = state.clone();
    stop_btn.on_click(move |_| {
        m.stop();
        save_position(&st.borrow(), &m, false);
    });
    let m = media;
    back_btn.on_click(move |_| {
        m.seek(-seek_back_ms, SeekMode::FromCurrent);
    });
    let m = media;
    fwd_btn.on_click(move |_| {
        m.seek(seek_fwd_ms, SeekMode::FromCurrent);
    });
    let m = media;
    vdown_btn.on_click(move |_| {
        let v = (m.get_volume() - volume_step).max(0.0);
        m.set_volume(v);
    });
    let m = media;
    vup_btn.on_click(move |_| {
        let v = (m.get_volume() + volume_step).min(1.0);
        m.set_volume(v);
    });
    // Speed stepping through the standard ladder, announced.
    for (btn, dir) in [(slower_btn, -1i32), (faster_btn, 1i32)] {
        let m = media;
        let frame_sp = frame;
        btn.on_click(move |_| {
            let cur = m.get_playback_rate();
            let idx = SPEEDS
                .iter()
                .position(|s| (*s - cur).abs() < 0.01)
                .unwrap_or(2);
            let next = (idx as i32 + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize;
            let rate = SPEEDS[next];
            if (rate - cur).abs() > 0.001 {
                m.set_playback_rate(rate);
                let label = format!("Speed {}x", fmt_speed(rate));
                frame_sp.set_status_text(&label, 0);
                crate::announce("playback_speed", &label);
            }
        });
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
    let m = media;
    let chapters_live_dl = chapters_live.clone();
    chapter_list.on_item_double_clicked(move |_| {
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
    });

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
    timer.on_tick(move |_| {
        let len = m.length();
        let pos = m.tell();
        let mut seek_to: Option<(i64, String)> = None;
        let mut queue_load: Option<(String, String)> = None;
        let mut queue_error: Option<String> = None;
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
                        Ok(stream) => {
                            q.index = idx;
                            let entry = q.entries[idx].clone();
                            s.article_id = entry.id.clone();
                            s.title = entry.title.clone();
                            s.skips.clear();
                            s.skipped.clear();
                            s.last_chapter = None;
                            let cfg2 = blindrss_core::config::Config::load(&crate::config_path())
                                .unwrap_or_default();
                            s.skip_rx = spawn_skip_workers(&cfg2, &entry, &stream);
                            s.saved_position_ms = Db::open(&s.db_path)
                                .ok()
                                .and_then(|db| db.playback_state(&entry.id).ok().flatten())
                                .filter(|saved| !saved.completed)
                                .map(|saved| saved.position_ms)
                                .unwrap_or(0);
                            s.seeked = s.saved_position_ms <= 0;
                            *chapters_tick.borrow_mut() = (Vec::new(), Vec::new());
                            chapter_list.clear();
                            set_queue_index(idx as i64);
                            queue_load = Some((eq_wrap(&stream, &cfg2), entry.title.clone()));
                        }
                        Err(e) => queue_error = Some(e),
                    }
                }
                s.queue = Some(q);
            }
            while let Ok(ranges) = s.skip_rx.try_recv() {
                s.skips.extend(ranges);
            }
            if !s.seeked && len > 0 && s.saved_position_ms > 0 {
                m.seek(s.saved_position_ms, SeekMode::FromStart);
                s.seeked = true;
            }
            let hit = s
                .skips
                .iter()
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
        if let Some((stream, title)) = queue_load
            && m.load_uri(&stream)
        {
            m.play();
            frame_tick.set_title(&format!("Playing: {title}"));
            frame_tick.set_status_text(&format!("Queue: {title}"), 0);
            crate::announce("general", &format!("Now playing: {title}"));
        }
        if let Some(e) = queue_error {
            frame_tick.set_status_text(&format!("Queue: {e}"), 0);
        }
        pos_label.set_label(&format!("{} / {}", fmt_ms(m.tell()), fmt_ms(len)));
        if len > 0 {
            let mut s = st.borrow_mut();
            s.slider_guard = true;
            seek_slider.set_value((m.tell() * 1000 / len) as i32);
            s.slider_guard = false;
        }
        save_position(&st.borrow(), &m, false);
    });
    timer.start(1000, false);

    frame.show(true);
    // show_player_on_play: raise the player over the main window
    // when playback starts (Python default in its settings UI).
    if show_player {
        frame.raise();
    }
}
