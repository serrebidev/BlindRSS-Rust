//! Chromecast controls. The worker owns the TLS session for the window lifetime.

use blindrss_core::cast::CastDevice;
use blindrss_core::castsession::CastSession;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use wxdragon::prelude::*;

enum Command {
    Play,
    Pause,
    Seek(f64),
    Volume(f64),
    Stop,
}

pub fn open(device: CastDevice, title: String, url: String, mime: String) {
    let frame = Frame::builder()
        .with_title(&format!("Casting: {title}"))
        .with_size(Size::new(560, 320))
        .build();
    let layout = BoxSizer::builder(Orientation::Vertical).build();
    let caption = StaticText::builder(&frame)
        .with_label(&format!("{title} on {}", device.name))
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
    for button in [&play, &pause, &stop] {
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
    let (commands, rx) = mpsc::channel();
    let (updates, results) = mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let run = || -> Result<(), String> {
            let mut session = CastSession::connect(&device.host, device.port)?;
            session.launch_default_receiver()?;
            session.load_media(&url, &mime, &title)?;
            let _ = updates.send(Ok("Playing".into()));
            let mut poll = Instant::now();
            loop {
                let result = match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Command::Play) => session.media_control("PLAY"),
                    Ok(Command::Pause) => session.media_control("PAUSE"),
                    Ok(Command::Seek(seconds)) => session.seek(seconds),
                    Ok(Command::Volume(level)) => session.set_volume(level),
                    Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => {
                        let _ = session.media_control("STOP");
                        return Ok(());
                    }
                    Err(RecvTimeoutError::Timeout) => Ok(()),
                };
                if let Err(error) = result {
                    let _ = updates.send(Err(error));
                }
                if poll.elapsed() >= Duration::from_secs(3) {
                    let result = session.media_status().map(|s| {
                        format!(
                            "{}: {:.0} of {:.0} seconds",
                            s.state, s.position, s.duration
                        )
                    });
                    if updates.send(result).is_err() {
                        let _ = session.media_control("STOP");
                        return Ok(());
                    }
                    session.ping()?;
                    poll = Instant::now();
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
