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

pub fn open_player(_parent: &Frame, article: Article, stream_url: String, chapters: Vec<Chapter>) {
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

    let frame = Frame::builder()
        .with_title(&format!("Playing: {}", article.title))
        .with_size(Size::new(560, 460))
        .build();
    let media = MediaCtrl::builder(&frame).build();
    let play_btn = Button::builder(&frame).with_label("Play/Pause").build();
    let stop_btn = Button::builder(&frame).with_label("Stop").build();
    let back_btn = Button::builder(&frame)
        .with_label(&format!("Back {}s", seek_back_ms / 1000))
        .build();
    let fwd_btn = Button::builder(&frame)
        .with_label(&format!("Forward {}s", seek_fwd_ms / 1000))
        .build();
    let vdown_btn = Button::builder(&frame).with_label("Volume Down").build();
    let vup_btn = Button::builder(&frame).with_label("Volume Up").build();
    let slower_btn = Button::builder(&frame).with_label("Slower").build();
    let faster_btn = Button::builder(&frame).with_label("Faster").build();
    let prev_ch_btn = Button::builder(&frame)
        .with_label("Previous Chapter")
        .build();
    let next_ch_btn = Button::builder(&frame).with_label("Next Chapter").build();
    let pos_label = StaticText::builder(&frame)
        .with_label("0:00:00 / 0:00:00")
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
        let threshold = cfg_f64(&cfg, "silence_skip_threshold_db", -38.0);
        let min_silence = cfg.get_i64("silence_skip_min_ms", 700) as f64 / 1000.0;
        let url = stream_url.clone();
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
    }));

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
        let chapters_nav = chapters.clone();
        let starts_nav = starts.clone();
        prev_ch_btn.on_click(move |_| {
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
        let chapters_nav = chapters.clone();
        let starts_nav = starts.clone();
        next_ch_btn.on_click(move |_| {
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
    let chapters_dl = chapters.clone();
    let starts_dl = starts.clone();
    chapter_list.on_item_double_clicked(move |_| {
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
    let starts_tick = starts.clone();
    let chapters_tick = chapters.clone();
    timer.on_tick(move |_| {
        let len = m.length();
        let pos = m.tell();
        let mut seek_to: Option<(i64, String)> = None;
        {
            let mut s = st.borrow_mut();
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
            if m.get_state() == MediaState::Playing
                && let Some(cur) = chapter_index(&starts_tick, pos)
                && s.last_chapter != Some(cur)
            {
                if s.last_chapter.is_some() {
                    crate::announce("media_navigation", &chapter_label(&chapters_tick, cur));
                }
                s.last_chapter = Some(cur);
            }
        }
        if let Some((end, label)) = seek_to {
            m.seek(end, SeekMode::FromStart);
            frame_tick.set_status_text(&label, 0);
            crate::announce("sponsor_skip", &label);
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
