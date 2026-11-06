//! Unified cast controls: one persistent window for every
//! protocol (Python's CastingManager presented as the
//! player's cast session). The worker thread owns the
//! protocol engine — a CASTv2 session for Chromecast, SOAP /
//! ECP / JSON-RPC calls for DLNA/Sonos/Roku/Kodi, a RAOP
//! stream for AirPlay — and drives the shared session logic
//! from core: the clock position for receivers that cannot
//! report one, resume positions persisted in playback_state,
//! and queue advancement when the receiver reports the
//! current item finished.

use blindrss_core::cast::{
    CastClock, CastDevice, CastQueue, CastQueueItem, CastStatus, QueueAdvance,
};
use blindrss_core::castmedia::{
    ChromecastLoadChain, LiveWatchdog, WatchdogAction, prepare_roku_target, prepare_upnp_target,
    probe_media,
};
use blindrss_core::castsession::CastSession;
use blindrss_core::castupnp::{self, KodiAuth};
use blindrss_core::config::Config;
use blindrss_core::playback::{ResumePolicy, saved_state};
use blindrss_core::raop::RaopHandle;
use blindrss_core::{Db, PlaybackState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use wxdragon::prelude::*;

enum Command {
    Play,
    Pause,
    Seek(f64),
    Volume(f64),
    Next,
    Stop,
}

/// The resume key for one cast item: the stream URL with any
/// proxy prefix stripped back to its encoded target, so the
/// same article resumes across cast sessions even though the
/// proxy's LAN address changes between runs.
fn resume_key(url: &str) -> String {
    match url.split_once("?url=") {
        Some((_, target)) => format!("cast:{target}"),
        None => format!("cast:{url}"),
    }
}

/// Where this item left off, per the shared resume policy.
fn resume_seconds(url: &str) -> f64 {
    let cfg = Config::load(&crate::config_path()).unwrap_or_default();
    let policy = ResumePolicy::from_config(&cfg);
    if !policy.enabled {
        return 0.0;
    }
    let key = resume_key(url);
    let Ok(db) = Db::open(&crate::db_path()) else {
        return 0.0;
    };
    saved_state(&db, &key, &key)
        .ok()
        .flatten()
        .map(|s| policy.resume_target(&s, url) as f64 / 1000.0)
        .unwrap_or(0.0)
}

/// Persist a cast position (the local player's save rules:
/// completed items store as completed, backend resets never
/// erase real progress).
fn save_position(url: &str, title: &str, pos: f64, duration: f64, completed: bool) {
    let cfg = Config::load(&crate::config_path()).unwrap_or_default();
    let policy = ResumePolicy::from_config(&cfg);
    if !policy.enabled {
        return;
    }
    let Ok(db) = Db::open(&crate::db_path()) else {
        return;
    };
    let key = resume_key(url);
    let _ = policy.save_state(
        &db,
        &key,
        &key,
        &PlaybackState {
            position_ms: (pos * 1000.0) as i64,
            duration_ms: (duration > 0.0).then_some((duration * 1000.0) as i64),
            completed,
            seek_supported: None,
            title: Some(title.to_string()),
        },
    );
}

/// One running AirPlay stream under its shared handle.
struct RaopRun {
    handle: Arc<RaopHandle>,
    done: Arc<AtomicBool>,
}

/// Start (or restart, for a seek) the RAOP stream of `item`
/// decoded from `start` seconds: ffmpeg decodes to PCM, the
/// RAOP sender streams it (the app's AirPlay path, moved
/// under the unified controls).
fn start_raop(
    device: &CastDevice,
    item: &CastQueueItem,
    start: f64,
    volume: f64,
    updates: &mpsc::Sender<Result<String, String>>,
) -> Result<RaopRun, String> {
    let ffmpeg = blindrss_core::silence::find_ffmpeg()
        .ok_or_else(|| "AirPlay casting needs ffmpeg".to_string())?;
    let cfg = Config::load(&crate::config_path()).unwrap_or_default();
    let eq = blindrss_core::equalizer::EqConfig::from_config(&cfg);
    let handle = RaopHandle::new((volume * 100.0) as u8);
    let done = Arc::new(AtomicBool::new(false));
    let (host, port) = (device.host.clone(), device.port);
    let url = item.url.clone();
    let run_handle = handle.clone();
    let run_done = done.clone();
    let updates = updates.clone();
    let title = item.title.clone();
    std::thread::spawn(move || {
        let mut args: Vec<String> = vec!["-v".into(), "error".into()];
        if start > 0.0 {
            args.push("-ss".into());
            args.push(format!("{start:.3}"));
        }
        args.extend([
            "-i".into(),
            url,
            "-f".into(),
            "s16le".into(),
            "-ar".into(),
            "44100".into(),
            "-ac".into(),
            "2".into(),
            "-".into(),
        ]);
        let spawned = std::process::Command::new(&ffmpeg)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let result = match spawned {
            Ok(mut child) => {
                let mut pcm = child.stdout.take();
                let result = match pcm.as_mut() {
                    Some(p) => blindrss_core::raop::stream_pcm_with_handle(
                        &host,
                        port,
                        p,
                        Some(&eq),
                        &run_handle,
                        start,
                    )
                    .map(|_| ()),
                    None => Err("ffmpeg gave no output".to_string()),
                };
                let _ = child.kill();
                let _ = child.wait();
                result
            }
            Err(e) => Err(format!("AirPlay cast failed to start ffmpeg: {e}")),
        };
        if let Err(e) = &result
            && !run_handle.stop.load(Ordering::Relaxed)
        {
            let _ = updates.send(Err(format!("AirPlay cast of {title} ended: {e}")));
        }
        run_done.store(true, Ordering::Relaxed);
    });
    Ok(RaopRun { handle, done })
}

/// The protocol engine the worker owns for one item.
/// Chromecast needs no variant: its session lives in
/// `Worker::cast` across items (connect once, load per item).
enum Engine {
    Upnp,
    Raop(RaopRun),
}

struct Worker {
    device: CastDevice,
    auth: KodiAuth,
    cast: Option<CastSession>,
    /// The source URL whose HLS relay the current item plays
    /// through, when it plays through one (C01): the relay is
    /// stopped when the item is replaced or the cast ends.
    relay_source: Option<String>,
    /// Live-stall watchdog state (C01) and the live load it
    /// would reload, Chromecast only.
    watchdog: LiveWatchdog,
    live_load: Option<(String, String, &'static str, String)>,
}

/// Percent-decode a proxy target the way `proxy::url_decode`
/// does (`+` is a space, `%XX` a byte).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| -> Option<u8> {
                    match b {
                        b'0'..=b'9' => Some(b - b'0'),
                        b'a'..=b'f' => Some(b - b'a' + 10),
                        b'A'..=b'F' => Some(b - b'A' + 10),
                        _ => None,
                    }
                };
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h * 16 + l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The original target behind a proxy URL (`/stream?url=`,
/// `/file?path=`), when this item came through the local proxy.
fn proxy_target(url: &str) -> Option<String> {
    for key in ["?url=", "?path="] {
        if let Some((_, rest)) = url.split_once(key) {
            let encoded = rest.split('&').next().unwrap_or(rest);
            return Some(url_decode(encoded));
        }
    }
    None
}

/// Start the HLS relay for a cast load (Caster's `_start_relay`
/// through the proxy's relay surface).
fn start_relay(source: &str, live: bool) -> Result<String, String> {
    blindrss_core::proxy::start_hls_relay(source, live)
}

impl Worker {
    /// Stop the current item's HLS relay, if it has one.
    fn stop_relay(&mut self) {
        if let Some(source) = self.relay_source.take() {
            blindrss_core::proxy::stop_hls_relay(&source);
        }
    }

    /// Load `item` from `start` on the device, returning the
    /// engine that now owns it. Every protocol that Caster
    /// probes is probed here first (C01): the item's claimed
    /// MIME is never trusted — bytes, playlist headers and
    /// liveness decide the load, TS goes through the HLS relay,
    /// and a Chromecast load the receiver rejects climbs
    /// Caster's retry ladder before it may fail.
    fn start_item(
        &mut self,
        item: &CastQueueItem,
        start: f64,
        volume: f64,
        updates: &mpsc::Sender<Result<String, String>>,
    ) -> Result<Engine, String> {
        self.stop_relay();
        self.watchdog.disarm();
        self.live_load = None;
        match self.device.kind.as_str() {
            "chromecast" => {
                if self.cast.is_none() {
                    let mut session = CastSession::connect(&self.device.host, self.device.port)?;
                    session.launch_default_receiver()?;
                    self.cast = Some(session);
                }
                let probe = probe_media(&item.url);
                let native = if probe.is_ts() && probe.is_live {
                    blindrss_core::castmedia::native_hls_url(&item.url).or_else(|| {
                        proxy_target(&item.url)
                            .and_then(|t| blindrss_core::castmedia::native_hls_url(&t))
                    })
                } else {
                    None
                };
                let mut chain =
                    ChromecastLoadChain::new(&item.url, probe.clone(), native, start_relay);
                let session = self.cast.as_mut().expect("connected above");
                let mut attempt = chain.first().ok_or_else(|| {
                    format!("{} did not start playing (no answer)", self.device.name)
                })?;
                loop {
                    let previous = session.media_session_id;
                    let live_relay =
                        attempt.relay_source.is_some() && attempt.stream_type == "LIVE";
                    session.load_media_prepared(
                        &attempt.url,
                        &attempt.mime,
                        &item.title,
                        attempt.stream_type,
                        start,
                        live_relay,
                    )?;
                    match session.await_playing(previous) {
                        blindrss_core::castsession::SettleOutcome::Settled => {
                            if attempt.stream_type == "BUFFERED" {
                                session.correct_resume_seek(start);
                            }
                            self.relay_source = attempt.relay_source.clone();
                            if probe.is_live {
                                self.live_load = Some((
                                    attempt.url.clone(),
                                    attempt.mime.clone(),
                                    attempt.stream_type,
                                    item.title.clone(),
                                ));
                                self.watchdog.arm(true);
                            }
                            return Ok(Engine::Upnp);
                        }
                        blindrss_core::castsession::SettleOutcome::Failed(reason) => {
                            match chain.advance() {
                                Some(next) => attempt = next,
                                None => {
                                    return Err(format!(
                                        "{} did not start playing ({})",
                                        self.device.name,
                                        reason.as_deref().unwrap_or("no answer")
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            "airplay" => Ok(Engine::Raop(start_raop(
                &self.device,
                item,
                start,
                volume,
                updates,
            )?)),
            "dlna" => {
                let probe = probe_media(&item.url);
                let (play_url, mime, relay) = prepare_upnp_target(&item.url, &probe, &start_relay)?;
                castupnp::play_at(
                    &self.device,
                    &play_url,
                    &item.title,
                    &mime,
                    start,
                    &self.auth,
                )?;
                self.relay_source = relay;
                Ok(Engine::Upnp)
            }
            "roku" => {
                let probe = probe_media(&item.url);
                let (play_url, mime, relay) = prepare_roku_target(&item.url, &probe, &start_relay)?;
                castupnp::play_at(
                    &self.device,
                    &play_url,
                    &item.title,
                    &mime,
                    start,
                    &self.auth,
                )?;
                self.relay_source = relay;
                Ok(Engine::Upnp)
            }
            "sonos" => {
                // Python `_play_sonos`: the probed type when the
                // probe says audio, else audio/mpeg.
                let probe = probe_media(&item.url);
                let mime = if probe.is_audio == Some(true) {
                    probe.mime.clone()
                } else {
                    "audio/mpeg".to_string()
                };
                castupnp::play_at(
                    &self.device,
                    &item.url,
                    &item.title,
                    &mime,
                    start,
                    &self.auth,
                )?;
                Ok(Engine::Upnp)
            }
            _ => {
                castupnp::play_at(
                    &self.device,
                    &item.url,
                    &item.title,
                    &item.mime,
                    start,
                    &self.auth,
                )?;
                Ok(Engine::Upnp)
            }
        }
    }

    fn is_chromecast(&self) -> bool {
        self.device.kind == "chromecast"
    }

    /// One live-watchdog tick (C01): when the armed watchdog
    /// calls for a recovery, reload the recorded live load the
    /// way `_recover_live_cast` does — same URL, MIME and
    /// stream type, settle-watched, rate-limited by the policy.
    fn watchdog_tick(&mut self, now: f64) {
        if !self.watchdog.is_armed() {
            return;
        }
        let Some((url, mime, stream_type, title)) = self.live_load.clone() else {
            return;
        };
        let Some(session) = self.cast.as_mut() else {
            return;
        };
        let Ok(status) = session.media_status() else {
            return;
        };
        if self.watchdog.note(
            Some(&status.state),
            status.media_session_id,
            Some(status.position),
            now,
        ) == WatchdogAction::Reload
        {
            let previous = session.media_session_id;
            let _ = session.load_media_prepared(&url, &mime, &title, stream_type, 0.0, true);
            let _ = session.await_playing(previous);
        }
    }
}

pub fn open(device: CastDevice, title: String, url: String, mime: String) {
    // The persistent controls also carry the play queue: the
    // current item first, then the configured queue entries
    // (their media URLs proxied like any cast stream).
    let mut items = vec![CastQueueItem {
        url,
        title: title.clone(),
        mime,
    }];
    for entry in crate::dialogs::load_queue(&crate::config_path()) {
        let entry_title = entry
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let Some(media) = entry.get("media_url").and_then(|v| v.as_str()) else {
            continue;
        };
        if entry_title == title {
            continue;
        }
        let reachable = if media.starts_with("http://") || media.starts_with("https://") {
            blindrss_core::proxy::stream_url(media)
        } else if media.contains("://") {
            media.to_string()
        } else {
            blindrss_core::proxy::file_url(std::path::Path::new(media))
        };
        items.push(CastQueueItem {
            url: reachable,
            title: entry_title,
            mime: entry
                .get("media_type")
                .and_then(|v| v.as_str())
                .filter(|t| t.contains('/'))
                .unwrap_or("audio/mpeg")
                .to_string(),
        });
    }
    open_queue(device, items);
}

pub fn open_queue(device: CastDevice, items: Vec<CastQueueItem>) {
    let first_title = items
        .first()
        .map(|i| i.title.clone())
        .unwrap_or_else(|| "Cast".to_string());
    let frame = Frame::builder()
        .with_title(&format!("Casting: {first_title}"))
        .with_size(Size::new(560, 320))
        .build();
    let layout = BoxSizer::builder(Orientation::Vertical).build();
    let caption = StaticText::builder(&frame)
        .with_label(&format!("{first_title} on {}", device.name))
        .build();
    layout.add(&caption, 0, SizerFlag::All, 4);
    let status = TextCtrl::builder(&frame)
        .with_style(TextCtrlStyle::ReadOnly)
        .build();
    status.set_name("Cast playback status");
    status.set_value("Connecting...");
    layout.add(&status, 0, SizerFlag::Expand | SizerFlag::All, 4);
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let play = Button::builder(&frame).with_label("&Play").build();
    let pause = Button::builder(&frame).with_label("Pa&use").build();
    let stop = Button::builder(&frame).with_label("&Stop casting").build();
    let next = Button::builder(&frame).with_label("&Next").build();
    for button in [&play, &pause, &stop, &next] {
        buttons.add(button, 0, SizerFlag::All, 4);
    }
    layout.add_sizer(&buttons, 0, SizerFlag::All, 0);
    let seek_label = StaticText::builder(&frame)
        .with_label("Seek position in seconds:")
        .build();
    layout.add(&seek_label, 0, SizerFlag::All, 4);
    let seek_value = TextCtrl::builder(&frame).build();
    seek_value.set_name("Seek position in seconds");
    seek_value.set_value("0");
    layout.add(&seek_value, 0, SizerFlag::Expand | SizerFlag::All, 4);
    let seek = Button::builder(&frame).with_label("&Seek").build();
    layout.add(&seek, 0, SizerFlag::All, 4);
    let volume_label = StaticText::builder(&frame)
        .with_label("Volume (percent):")
        .build();
    layout.add(&volume_label, 0, SizerFlag::All, 4);
    let volume = Slider::builder(&frame).build();
    volume.set_name("Cast volume percent");
    volume.set_range(0, 100);
    volume.set_value(100);
    layout.add(&volume, 0, SizerFlag::Expand | SizerFlag::All, 4);
    frame.set_sizer(layout, true);
    // Escape closes the cast window (the article window and
    // player patterns; a modeless frame gets no Esc for
    // free). It goes through the same close path as the
    // window's close button, which stops the cast.
    frame.bind_internal(EventType::CHAR_HOOK, move |event| {
        if event.get_key_code().unwrap_or(0) == 27 {
            frame.close(false);
            event.skip(false);
        } else {
            event.skip(true);
        }
    });
    let (commands, rx) = mpsc::channel();
    let (updates, results) = mpsc::channel::<Result<String, String>>();
    let worker_device = device.clone();
    std::thread::spawn(move || {
        let run = || -> Result<(), String> {
            let cfg = Config::load(&crate::config_path()).unwrap_or_default();
            let mut worker = Worker {
                device: worker_device,
                auth: KodiAuth {
                    user: cfg.get_str("cast_kodi_user", "").to_string(),
                    password: cfg.get_str("cast_kodi_password", "").to_string(),
                },
                cast: None,
                relay_source: None,
                watchdog: LiveWatchdog::new(),
                live_load: None,
            };
            let run_started = Instant::now();
            let mut queue = CastQueue::new(items);
            let Some(first) = queue.start().cloned() else {
                return Err("Nothing to cast".into());
            };
            let mut volume_level = 1.0f64;
            let mut clock = CastClock::new();
            let first_start = resume_seconds(&first.url);
            let mut engine = worker.start_item(&first, first_start, volume_level, &updates)?;
            clock.set("playing", Some(first_start));
            // A just-started item is live by construction;
            // priming lets a later idle report count as its
            // finish (the queue's end-of-media event).
            let _ = queue.note_status(&CastStatus {
                player_state: Some("PLAYING".into()),
                connected: true,
                ..Default::default()
            });
            let _ = updates.send(Ok(format!("Playing: {}", first.title)));
            let mut current = first;
            let mut saved_pos = -1.0f64;
            let mut finished_all = false;
            let mut poll = Instant::now();
            loop {
                let result = match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Command::Play) => {
                        let r = command_play(&mut worker, &mut engine);
                        if r.is_ok() {
                            clock.set("playing", None);
                        }
                        r
                    }
                    Ok(Command::Pause) => {
                        let r = command_pause(&mut worker, &mut engine, clock.state());
                        if r.is_ok() {
                            clock.set("paused", None);
                        }
                        r
                    }
                    Ok(Command::Seek(seconds)) => {
                        let r = command_seek(
                            &mut worker,
                            &mut engine,
                            &current,
                            seconds,
                            volume_level,
                            &updates,
                        );
                        if r.is_ok() {
                            let state = clock.state().unwrap_or("playing").to_string();
                            clock.set(&state, Some(seconds));
                        }
                        r
                    }
                    Ok(Command::Volume(level)) => {
                        volume_level = level;
                        command_volume(&mut worker, &engine, level)
                    }
                    Ok(Command::Next) => {
                        save_position(
                            &current.url,
                            &current.title,
                            clock.position().unwrap_or(0.0),
                            0.0,
                            true,
                        );
                        match queue.skip_next().cloned() {
                            Some(item) => {
                                stop_engine(&mut worker, &mut engine);
                                let start = resume_seconds(&item.url);
                                match worker.start_item(&item, start, volume_level, &updates) {
                                    Ok(new_engine) => {
                                        engine = new_engine;
                                        clock.set("playing", Some(start));
                                        let _ = queue.note_status(&CastStatus {
                                            player_state: Some("PLAYING".into()),
                                            connected: true,
                                            ..Default::default()
                                        });
                                        let _ =
                                            updates.send(Ok(format!("Playing: {}", item.title)));
                                        current = item;
                                        saved_pos = -1.0;
                                        Ok(())
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            None => {
                                stop_engine(&mut worker, &mut engine);
                                let _ = updates.send(Ok("Queue finished".into()));
                                finished_all = true;
                                Ok(())
                            }
                        }
                    }
                    Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => {
                        if let Some(pos) = clock.position() {
                            save_position(&current.url, &current.title, pos, 0.0, false);
                        }
                        stop_engine(&mut worker, &mut engine);
                        return Ok(());
                    }
                    Err(RecvTimeoutError::Timeout) => Ok(()),
                };
                if let Err(error) = result {
                    let _ = updates.send(Err(error));
                }
                if finished_all || poll.elapsed() < Duration::from_secs(2) {
                    continue;
                }
                poll = Instant::now();
                // A stalled live Chromecast load recovers
                // itself (bounded by the watchdog policy).
                if worker.is_chromecast() {
                    worker.watchdog_tick(run_started.elapsed().as_secs_f64());
                }
                // Poll the receiver: position for the resume
                // save, and the queue's end-of-media event.
                let mut status = engine_status(&mut worker, &engine);
                if status.position_seconds.is_none() {
                    status.position_seconds = clock.position();
                }
                if status.player_state.is_none() {
                    status.player_state = clock.state().map(|s| s.to_uppercase());
                }
                if let Some(pos) = status.position_seconds {
                    clock.set(
                        if status.player_state.as_deref() == Some("PAUSED") {
                            "paused"
                        } else {
                            "playing"
                        },
                        Some(pos),
                    );
                    if (pos - saved_pos).abs() >= 5.0 {
                        save_position(
                            &current.url,
                            &current.title,
                            pos,
                            status.duration_seconds.unwrap_or(0.0),
                            false,
                        );
                        saved_pos = pos;
                    }
                }
                match queue.note_status(&status) {
                    QueueAdvance::Next(item) => {
                        save_position(
                            &current.url,
                            &current.title,
                            status.duration_seconds.unwrap_or(0.0),
                            status.duration_seconds.unwrap_or(0.0),
                            true,
                        );
                        let start = resume_seconds(&item.url);
                        match worker.start_item(&item, start, volume_level, &updates) {
                            Ok(new_engine) => {
                                engine = new_engine;
                                clock.set("playing", Some(start));
                                let _ = queue.note_status(&CastStatus {
                                    player_state: Some("PLAYING".into()),
                                    connected: true,
                                    ..Default::default()
                                });
                                let _ = updates.send(Ok(format!("Playing: {}", item.title)));
                                current = item;
                                saved_pos = -1.0;
                            }
                            Err(e) => {
                                let _ = updates.send(Err(e));
                            }
                        }
                    }
                    QueueAdvance::Finished => {
                        save_position(
                            &current.url,
                            &current.title,
                            status.duration_seconds.unwrap_or(0.0),
                            status.duration_seconds.unwrap_or(0.0),
                            true,
                        );
                        let _ = updates.send(Ok("Queue finished".into()));
                        finished_all = true;
                    }
                    QueueAdvance::None => {}
                }
                if let Ok(text) = status_text(&current, &status)
                    && updates.send(Ok(text)).is_err()
                {
                    stop_engine(&mut worker, &mut engine);
                    return Ok(());
                }
            }
        };
        if let Err(error) = run() {
            let _ = updates.send(Err(error));
        }
    });
    let send = commands.clone();
    play.on_click(move |_| {
        let _ = send.send(Command::Play);
    });
    let send = commands.clone();
    pause.on_click(move |_| {
        let _ = send.send(Command::Pause);
    });
    let send = commands.clone();
    seek.on_click(
        move |_| match seek_value.get_value().trim().parse::<f64>() {
            Ok(seconds) if seconds.is_finite() && seconds >= 0.0 => {
                let _ = send.send(Command::Seek(seconds));
            }
            _ => status.set_value("Enter a nonnegative seek position in seconds."),
        },
    );
    let send = commands.clone();
    volume.on_slider(move |_| {
        let _ = send.send(Command::Volume(volume.get_value() as f64 / 100.0));
    });
    let send = commands.clone();
    next.on_click(move |_| {
        let _ = send.send(Command::Next);
    });
    stop.on_click(move |_| {
        frame.close(false);
    });
    let timer = Box::leak(Box::new(Timer::new(&frame)));
    timer.on_tick(move |_| {
        while let Ok(result) = results.try_recv() {
            let text = result.unwrap_or_else(|e| format!("Cast failed: {e}"));
            if status.get_value() != text {
                status.set_value(&text);
            }
        }
    });
    timer.start(250, false);
    frame.on_close(move |event| {
        timer.stop();
        let _ = commands.send(Command::Stop);
        event.skip(true);
    });
    frame.show(true);
    play.set_focus();
}

fn status_text(current: &CastQueueItem, status: &CastStatus) -> Result<String, String> {
    let state = status.player_state.as_deref().unwrap_or("IDLE");
    Ok(match (status.position_seconds, status.duration_seconds) {
        (Some(pos), Some(dur)) if dur > 0.0 => {
            format!(
                "{}: {}: {:.0} of {:.0} seconds",
                current.title, state, pos, dur
            )
        }
        (Some(pos), _) => format!("{}: {}: {:.0} seconds", current.title, state, pos),
        _ => format!("{}: {}", current.title, state),
    })
}

fn command_play(worker: &mut Worker, engine: &mut Engine) -> Result<(), String> {
    match engine {
        Engine::Raop(run) => {
            run.handle.resume();
            Ok(())
        }
        _ if worker.is_chromecast() => worker
            .cast
            .as_mut()
            .ok_or("no cast session")?
            .media_control("PLAY"),
        _ => castupnp::resume(&worker.device, &worker.auth, Some("paused")),
    }
}

fn command_pause(
    worker: &mut Worker,
    engine: &mut Engine,
    clock_state: Option<&str>,
) -> Result<(), String> {
    match engine {
        Engine::Raop(run) => {
            run.handle.pause();
            Ok(())
        }
        _ if worker.is_chromecast() => worker
            .cast
            .as_mut()
            .ok_or("no cast session")?
            .media_control("PAUSE"),
        _ => castupnp::pause(&worker.device, &worker.auth, clock_state),
    }
}

fn command_seek(
    worker: &mut Worker,
    engine: &mut Engine,
    current: &CastQueueItem,
    seconds: f64,
    volume: f64,
    updates: &mpsc::Sender<Result<String, String>>,
) -> Result<(), String> {
    match engine {
        Engine::Raop(run) => {
            // A RAOP seek reopens the source at the target
            // (Python's pending seek): stop this stream and
            // start the item again from there.
            run.handle.request_stop();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !run.done.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            *engine = Engine::Raop(start_raop(
                &worker.device,
                current,
                seconds,
                volume,
                updates,
            )?);
            Ok(())
        }
        _ if worker.is_chromecast() => worker.cast.as_mut().ok_or("no cast session")?.seek(seconds),
        _ => castupnp::seek(&worker.device, seconds, &worker.auth),
    }
}

fn command_volume(worker: &mut Worker, engine: &Engine, level: f64) -> Result<(), String> {
    match engine {
        Engine::Raop(run) => {
            run.handle.set_volume((level * 100.0) as u8);
            Ok(())
        }
        _ if worker.is_chromecast() => match worker.cast.as_mut() {
            Some(session) => session.set_volume(level),
            None => Err("no cast session".into()),
        },
        _ => castupnp::set_volume(&worker.device, level, &worker.auth),
    }
}

fn engine_status(worker: &mut Worker, engine: &Engine) -> CastStatus {
    match engine {
        Engine::Raop(run) => CastStatus {
            position_seconds: Some(run.handle.position_seconds()),
            player_state: Some(
                if run.handle.is_finished() {
                    "IDLE"
                } else if run.handle.is_paused() {
                    "PAUSED"
                } else {
                    "PLAYING"
                }
                .into(),
            ),
            connected: true,
            ..Default::default()
        },
        _ if worker.is_chromecast() => match worker.cast.as_mut() {
            Some(session) => {
                let status = session.media_status().map(|s| s.to_unified());
                let _ = session.ping();
                status.unwrap_or_default()
            }
            None => CastStatus::default(),
        },
        _ => castupnp::status(&worker.device, &worker.auth).unwrap_or_default(),
    }
}

fn stop_engine(worker: &mut Worker, engine: &mut Engine) {
    // Every stop path tears the item's relay down too: nothing
    // keeps transcoding for a cast that has moved on (C01).
    worker.watchdog.disarm();
    worker.live_load = None;
    worker.stop_relay();
    match engine {
        Engine::Raop(run) => run.handle.request_stop(),
        _ if worker.is_chromecast() => {
            if let Some(session) = worker.cast.as_mut() {
                let _ = session.media_control("STOP");
            }
        }
        _ => {
            let _ = castupnp::stop(&worker.device, &worker.auth);
        }
    }
}
