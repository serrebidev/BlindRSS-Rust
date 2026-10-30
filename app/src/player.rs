//! Player window (slice 7): wx MediaCtrl over the article's media
//! URL (direct or yt-dlp-resolved), with resume via the Python
//! playback_state table and chapter navigation.
//!
//! Position is saved every second by the window timer, so closing
//! the window at any point loses at most a second of progress.

use blindrss_core::db::Chapter;
use blindrss_core::{Article, Db, PlaybackState};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use wxdragon::prelude::*;
use wxdragon::widgets::media_ctrl::SeekMode;

struct PlayerState {
    article_id: String,
    title: String,
    db_path: PathBuf,
    seeked: bool,
    saved_position_ms: i64,
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

pub fn open_player(_parent: &Frame, article: Article, stream_url: String, chapters: Vec<Chapter>) {
    let db_path = crate::db_path();
    let saved = Db::open(&db_path)
        .ok()
        .and_then(|db| db.playback_state(&article.id).ok().flatten())
        .filter(|s| !s.completed)
        .map(|s| s.position_ms)
        .unwrap_or(0);

    let frame = Frame::builder()
        .with_title(&format!("Playing: {}", article.title))
        .with_size(Size::new(560, 420))
        .build();
    let media = MediaCtrl::builder(&frame).build();
    let play_btn = Button::builder(&frame).with_label("Play/Pause").build();
    let stop_btn = Button::builder(&frame).with_label("Stop").build();
    let back_btn = Button::builder(&frame).with_label("Back 30s").build();
    let fwd_btn = Button::builder(&frame).with_label("Forward 30s").build();
    let vdown_btn = Button::builder(&frame).with_label("Volume Down").build();
    let vup_btn = Button::builder(&frame).with_label("Volume Up").build();
    let pos_label = StaticText::builder(&frame)
        .with_label("0:00:00 / 0:00:00")
        .build();
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
    let col = BoxSizer::builder(Orientation::Vertical).build();
    col.add(&media, 1, SizerFlag::Expand | SizerFlag::All, 4);
    col.add_sizer(&row, 0, SizerFlag::Expand, 0);
    col.add(&pos_label, 0, SizerFlag::All, 4);
    col.add(&chapter_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    frame.set_sizer(col, true);
    frame.create_status_bar(1, 0, -1, "player status");
    frame.set_status_text(&article.title, 0);

    let state = Rc::new(RefCell::new(PlayerState {
        article_id: article.id.clone(),
        title: article.title.clone(),
        db_path,
        seeked: saved <= 0,
        saved_position_ms: saved,
    }));

    let loaded = media.load_uri(&stream_url);
    if loaded {
        media.play();
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
        m.seek(-30_000, SeekMode::FromCurrent);
    });
    let m = media;
    fwd_btn.on_click(move |_| {
        m.seek(30_000, SeekMode::FromCurrent);
    });
    let m = media;
    vdown_btn.on_click(move |_| {
        let v = (m.get_volume() - 0.1).max(0.0);
        m.set_volume(v);
    });
    let m = media;
    vup_btn.on_click(move |_| {
        let v = (m.get_volume() + 0.1).min(1.0);
        m.set_volume(v);
    });
    let m = media;
    let starts: Vec<i64> = chapters.iter().map(|c| (c.start * 1000.0) as i64).collect();
    chapter_list.on_item_double_clicked(move |_| {
        if let Some(sel) = chapter_list.get_selection()
            && let Some(ms) = starts.get(sel as usize)
        {
            m.seek(*ms, SeekMode::FromStart);
            m.play();
        }
    });

    // 1-second tick: resume seek once length is known, position
    // label, and the playback-state save.
    let timer = Box::leak(Box::new(Timer::new(&frame)));
    let m = media;
    let st = state.clone();
    timer.on_tick(move |_| {
        let len = m.length();
        {
            let mut s = st.borrow_mut();
            if !s.seeked && len > 0 && s.saved_position_ms > 0 {
                m.seek(s.saved_position_ms, SeekMode::FromStart);
                s.seeked = true;
            }
        }
        pos_label.set_label(&format!("{} / {}", fmt_ms(m.tell()), fmt_ms(len)));
        save_position(&st.borrow(), &m, false);
    });
    timer.start(1000, false);

    frame.show(true);
    frame.raise();
}
