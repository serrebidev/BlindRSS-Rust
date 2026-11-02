//! Casting depth (gap 8): cross-protocol sessions against
//! fake receivers — a UPnP renderer, a Kodi JSON-RPC endpoint
//! and a Roku ECP endpoint on loopback — plus the pure session
//! machinery (clock, queue advance, HMS) every protocol's
//! unified control surface shares.

use blindrss_core::cast::{
    CastClock, CastDevice, CastQueue, CastQueueItem, CastStatus, QueueAdvance, format_hms,
    parse_hms, protocol_label, supports_seek, supports_volume,
};
use blindrss_core::castupnp::{self, KodiAuth};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

struct Recorded {
    path: String,
    body: String,
}

/// A one-shot-per-connection HTTP server: records (path, body)
/// and answers through `respond(path, body) -> String`.
fn fake_server(
    respond: impl Fn(&str, &str) -> String + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    let respond = Arc::new(respond);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .ok();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 8192];
            let header_end = loop {
                match stream.read(&mut buf) {
                    Ok(0) => break None,
                    Ok(n) => {
                        bytes.extend_from_slice(&buf[..n]);
                        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            break Some(pos + 4);
                        }
                    }
                    Err(_) => break None,
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
            let content_length: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())?
                })
                .unwrap_or(0);
            let mut body_bytes = bytes[header_end..].to_vec();
            while body_bytes.len() < content_length {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => body_bytes.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
            }
            let request_line = head.lines().next().unwrap_or("").to_string();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            let body = String::from_utf8_lossy(&body_bytes).to_string();
            log2.lock().unwrap().push(Recorded {
                path: path.clone(),
                body: body.clone(),
            });
            let reply = respond(&path, &body);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=\"utf-8\"\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
        }
    });
    (base, log)
}

fn device(kind: &str, endpoint: String) -> CastDevice {
    CastDevice {
        name: "Fake".into(),
        host: "127.0.0.1".into(),
        port: 0,
        kind: kind.into(),
        model: String::new(),
        endpoint,
    }
}

fn soap_ok() -> String {
    "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body/></s:Envelope>"
        .into()
}

#[test]
fn upnp_session_pause_resume_seek_volume_status_stop() {
    let state = Arc::new(Mutex::new(("PLAYING".to_string(), "0:07:05".to_string())));
    let state2 = state.clone();
    let (base, log) = fake_server(move |_path, body| {
        if body.contains("GetTransportInfo") {
            let (s, _) = state2.lock().unwrap().clone();
            format!(
                "<s:Envelope><s:Body><u:GetTransportInfoResponse>\
                 <CurrentTransportState>{s}</CurrentTransportState>\
                 </u:GetTransportInfoResponse></s:Body></s:Envelope>"
            )
        } else if body.contains("GetPositionInfo") {
            let (_, p) = state2.lock().unwrap().clone();
            format!(
                "<s:Envelope><s:Body><u:GetPositionInfoResponse>\
                 <TrackDuration>0:42:10</TrackDuration><RelTime>{p}</RelTime>\
                 </u:GetPositionInfoResponse></s:Body></s:Envelope>"
            )
        } else {
            soap_ok()
        }
    });
    let dev = device("dlna", format!("{base}/AVTransport/control"));
    let auth = KodiAuth::default();

    castupnp::play_at(
        &dev,
        "http://media.test/ep1.mp3",
        "Ep 1",
        "audio/mpeg",
        0.0,
        &auth,
    )
    .unwrap();
    castupnp::pause(&dev, &auth, None).unwrap();
    castupnp::resume(&dev, &auth, None).unwrap();
    castupnp::seek(&dev, 425.0, &auth).unwrap();
    castupnp::set_volume(&dev, 0.4, &auth).unwrap();
    let status = castupnp::status(&dev, &auth).unwrap();
    assert_eq!(status.position_seconds, Some(425.0));
    assert_eq!(status.duration_seconds, Some(2530.0));
    assert_eq!(status.player_state.as_deref(), Some("PLAYING"));
    assert!(!status.supports_session_detection);
    castupnp::stop(&dev, &auth).unwrap();

    let log = log.lock().unwrap();
    let bodies: Vec<&str> = log.iter().map(|r| r.body.as_str()).collect();
    assert!(bodies.iter().any(|b| b.contains("SetAVTransportURI")));
    assert!(bodies.iter().any(|b| b.contains("<u:Pause")));
    assert!(bodies.iter().any(|b| b.contains("<u:Play")));
    assert!(
        bodies
            .iter()
            .any(|b| { b.contains("<u:Seek") && b.contains("<Target>0:07:05</Target>") })
    );
    assert!(bodies.iter().any(|b| b.contains("<u:Stop")));
    // Volume went to the RenderingControl path with 40%.
    let volume = log
        .iter()
        .find(|r| r.body.contains("SetVolume"))
        .expect("SetVolume sent");
    assert!(volume.path.contains("RenderingControl"), "{}", volume.path);
    assert!(volume.body.contains("<DesiredVolume>40</DesiredVolume>"));
}

#[test]
fn upnp_resume_play_seeks_once_playing() {
    let (base, log) = fake_server(|_path, body| {
        if body.contains("GetTransportInfo") {
            "<CurrentTransportState>PLAYING</CurrentTransportState>".into()
        } else if body.contains("GetPositionInfo") {
            "<TrackDuration>0:42:10</TrackDuration><RelTime>0:00:00</RelTime>".into()
        } else {
            soap_ok()
        }
    });
    let dev = device("sonos", format!("{base}/AVTransport/control"));
    castupnp::play_upnp_at(&dev, "http://media.test/ep.mp3", "Ep", "audio/mpeg", 65.0).unwrap();
    let log = log.lock().unwrap();
    let actions: Vec<String> = log
        .iter()
        .map(|r| {
            ["SetAVTransportURI", "Play", "Seek", "GetTransportInfo"]
                .iter()
                .find(|a| r.body.contains(**a))
                .unwrap_or(&"?")
                .to_string()
        })
        .collect();
    let seek_at = actions
        .iter()
        .position(|a| a == "Seek")
        .expect("resume seek");
    let play_at = actions.iter().position(|a| a == "Play").expect("play");
    assert!(play_at < seek_at, "{actions:?}");
    assert!(log[seek_at].body.contains("<Target>0:01:05</Target>"));
}

#[test]
fn kodi_session_play_pause_seek_volume_status_stop() {
    let (base, log) = fake_server(|_path, body| {
        if body.contains("Player.GetActivePlayers") {
            r#"{"jsonrpc":"2.0","id":1,"result":[{"playerid":1,"type":"audio"}]}"#.into()
        } else if body.contains("Player.GetProperties") {
            r#"{"jsonrpc":"2.0","id":1,"result":{"time":{"hours":0,"minutes":7,"seconds":5,"milliseconds":0},"totaltime":{"hours":0,"minutes":42,"seconds":10,"milliseconds":0},"speed":0}}"#.into()
        } else {
            r#"{"jsonrpc":"2.0","id":1,"result":"OK"}"#.into()
        }
    });
    let mut dev = device("kodi", String::new());
    // Kodi's base is host:port when no endpoint was resolved.
    let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
    dev.port = port;
    let auth = KodiAuth::default();

    castupnp::play_at(
        &dev,
        "http://media.test/ep.mp3",
        "Ep",
        "audio/mpeg",
        0.0,
        &auth,
    )
    .unwrap();
    castupnp::pause(&dev, &auth, None).unwrap();
    castupnp::resume(&dev, &auth, None).unwrap();
    castupnp::seek(&dev, 425.0, &auth).unwrap();
    castupnp::set_volume(&dev, 0.4, &auth).unwrap();
    let status = castupnp::status(&dev, &auth).unwrap();
    assert_eq!(status.position_seconds, Some(425.0));
    assert_eq!(status.player_state.as_deref(), Some("PAUSED"));
    castupnp::stop(&dev, &auth).unwrap();

    let log = log.lock().unwrap();
    let methods: Vec<&str> = [
        "Player.Open",
        "Player.PlayPause",
        "Player.Seek",
        "Application.SetVolume",
        "Player.GetProperties",
        "Player.Stop",
    ]
    .iter()
    .filter(|m| log.iter().any(|r| r.body.contains(**m)))
    .copied()
    .collect();
    assert_eq!(methods.len(), 6, "{methods:?}");
    let seek = log.iter().find(|r| r.body.contains("Player.Seek")).unwrap();
    assert!(seek.body.contains("\"minutes\":7"), "{}", seek.body);
    assert!(seek.body.contains("\"seconds\":5"), "{}", seek.body);
    let volume = log
        .iter()
        .find(|r| r.body.contains("Application.SetVolume"))
        .unwrap();
    assert!(volume.body.contains("\"volume\":40"), "{}", volume.body);
}

#[test]
fn roku_toggle_is_gated_by_clock_state_and_seek_is_refused() {
    let (base, log) = fake_server(|_path, _body| String::new());
    let dev = device("roku", base);
    let auth = KodiAuth::default();

    castupnp::play_at(
        &dev,
        "http://media.test/ep.mp3",
        "Ep",
        "audio/mpeg",
        0.0,
        &auth,
    )
    .unwrap();
    // Python sends the Play keypress only when its clock says
    // playing (for pause) / paused (for resume).
    castupnp::pause(&dev, &auth, Some("paused")).unwrap();
    castupnp::pause(&dev, &auth, Some("playing")).unwrap();
    castupnp::resume(&dev, &auth, Some("paused")).unwrap();
    castupnp::resume(&dev, &auth, Some("playing")).unwrap();
    assert!(castupnp::seek(&dev, 10.0, &auth).is_err());
    assert!(castupnp::set_volume(&dev, 0.5, &auth).is_err());
    castupnp::stop(&dev, &auth).unwrap();

    let log = log.lock().unwrap();
    let paths: Vec<&str> = log.iter().map(|r| r.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.starts_with("/launch/15985")),
        "{paths:?}"
    );
    assert_eq!(paths.iter().filter(|p| **p == "/keypress/Play").count(), 2);
    assert_eq!(paths.last().copied(), Some("/keypress/Home"));
}

#[test]
fn hms_roundtrip_and_capabilities_match_python() {
    assert_eq!(format_hms(425.0), "0:07:05");
    assert_eq!(format_hms(3723.0), "1:02:03");
    assert_eq!(parse_hms("0:07:05"), Some(425.0));
    assert_eq!(parse_hms("1:02:03.5"), Some(3723.5));
    assert_eq!(parse_hms("07:05"), None);
    assert_eq!(protocol_label("chromecast"), "Chromecast");
    assert_eq!(protocol_label("dlna"), "DLNA");
    assert!(supports_seek("kodi") && !supports_seek("roku"));
    assert!(supports_volume("sonos") && !supports_volume("roku"));
    let dev = device("dlna", "http://h/ctl".into());
    assert_eq!(dev.display_name(), "Fake [DLNA]");
    assert_eq!(dev.unique_id(), "DLNA:http://h/ctl");
}

#[test]
fn clock_freezes_when_paused_and_resumes_from_base() {
    let mut clock = CastClock::new();
    assert_eq!(clock.position(), None);
    clock.set("playing", Some(100.0));
    std::thread::sleep(std::time::Duration::from_millis(30));
    let pos = clock.position().unwrap();
    assert!((100.0..101.0).contains(&pos), "{pos}");
    clock.set("paused", None);
    let frozen = clock.position().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(clock.position(), Some(frozen));
    clock.set("playing", None);
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(clock.position().unwrap() > frozen);
    // An explicit position wins over extrapolation (a seek).
    clock.set("playing", Some(500.0));
    let pos = clock.position().unwrap();
    assert!((500.0..501.0).contains(&pos), "{pos}");
}

fn item(n: u32) -> CastQueueItem {
    CastQueueItem {
        url: format!("http://media.test/ep{n}.mp3"),
        title: format!("Ep {n}"),
        mime: "audio/mpeg".into(),
    }
}

fn status(state: &str, position: Option<f64>) -> CastStatus {
    CastStatus {
        position_seconds: position,
        player_state: Some(state.into()),
        connected: true,
        ..Default::default()
    }
}

#[test]
fn queue_advances_only_after_a_live_item_finishes() {
    let mut queue = CastQueue::new(vec![item(1), item(2), item(3)]);
    assert_eq!(queue.start().unwrap().title, "Ep 1");
    // An idle report before anything played is not a finish.
    assert_eq!(queue.note_status(&status("IDLE", None)), QueueAdvance::None);
    assert_eq!(
        queue.note_status(&status("PLAYING", Some(3.0))),
        QueueAdvance::None
    );
    assert_eq!(
        queue.note_status(&status("PAUSED", Some(30.0))),
        QueueAdvance::None
    );
    // Finish: idle after playing advances to the next item.
    match queue.note_status(&status("IDLE", None)) {
        QueueAdvance::Next(next) => assert_eq!(next.title, "Ep 2"),
        other => panic!("expected Next, got {other:?}"),
    }
    // The same idle report must not advance twice.
    assert_eq!(queue.note_status(&status("IDLE", None)), QueueAdvance::None);
    assert_eq!(
        queue.note_status(&status("PLAYING", Some(1.0))),
        QueueAdvance::None
    );
    match queue.note_status(&status("STOPPED", None)) {
        QueueAdvance::Next(next) => assert_eq!(next.title, "Ep 3"),
        other => panic!("expected Next, got {other:?}"),
    }
    assert_eq!(
        queue.note_status(&status("PLAYING", Some(1.0))),
        QueueAdvance::None
    );
    assert_eq!(
        queue.note_status(&status("IDLE", None)),
        QueueAdvance::Finished
    );
    assert_eq!(queue.current(), None);
}

#[test]
fn raop_handle_tracks_pause_volume_and_position() {
    let handle = blindrss_core::raop::RaopHandle::new(80);
    assert_eq!(handle.volume(), 80);
    assert_eq!(handle.position_seconds(), 0.0);
    assert!(!handle.is_paused());
    assert!(!handle.is_finished());
    handle.pause();
    assert!(handle.is_paused());
    handle.resume();
    assert!(!handle.is_paused());
    handle.set_volume(140);
    assert_eq!(handle.volume(), 100);
    handle.request_stop();
    assert!(handle.stop.load(std::sync::atomic::Ordering::Relaxed));
}
