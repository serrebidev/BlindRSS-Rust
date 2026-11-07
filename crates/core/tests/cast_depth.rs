//! Casting depth (gap 8): cross-protocol sessions against
//! fake receivers — a UPnP renderer, a Kodi JSON-RPC endpoint
//! and a Roku ECP endpoint on loopback — plus the pure session
//! machinery (clock, queue advance, HMS) every protocol's
//! unified control surface shares.

use blindrss_core::cast::{
    CastClock, CastDevice, CastQueue, CastQueueItem, CastStatus, QueueAdvance, format_hms,
    merge_discovery_by_name, parse_hms, protocol_label, supports_seek, supports_volume,
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
        sinks: Vec::new(),
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
    // The seek-after-playing mechanics are protocol-generic;
    // a plain DLNA renderer keeps this test off the Sonos
    // coordinator path (covered by its own fixtures below).
    let dev = device("dlna", format!("{base}/AVTransport/control"));
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
    // C03: Caster's exact ECP launch — the Roku Media Player
    // channel (2213) with t/u/audioName/audioFormat.
    assert!(
        paths.contains(
            &"/launch/2213?t=a&u=http%3A%2F%2Fmedia.test%2Fep.mp3&audioName=Ep&audioFormat=mp3"
        ),
        "{paths:?}"
    );
    assert_eq!(paths.iter().filter(|p| **p == "/keypress/Play").count(), 2);
    assert_eq!(paths.last().copied(), Some("/keypress/Home"));
}

#[test]
fn roku_launch_queries_are_casters_exact_ecp_parameters() {
    // C03: audio, video and HLS each carry their own name/format
    // pair; the query is asserted byte-for-byte.
    assert_eq!(
        castupnp::roku_launch_path("http://x.test/a b.mp3", "My Song", "audio/mpeg"),
        "/launch/2213?t=a&u=http%3A%2F%2Fx.test%2Fa+b.mp3&audioName=My+Song&audioFormat=mp3"
    );
    assert_eq!(
        castupnp::roku_launch_path("http://x.test/v.mkv", "Film", "video/x-matroska"),
        "/launch/2213?t=v&u=http%3A%2F%2Fx.test%2Fv.mkv&videoName=Film&videoFormat=mp4"
    );
    assert_eq!(
        castupnp::roku_launch_path(
            "http://x.test/live.m3u8",
            "Channel",
            "application/vnd.apple.mpegurl"
        ),
        "/launch/2213?t=v&u=http%3A%2F%2Fx.test%2Flive.m3u8&videoName=Channel&videoFormat=hls"
    );
    assert_eq!(
        castupnp::roku_launch_path("http://x.test/w.wav", "W", "audio/wav"),
        "/launch/2213?t=a&u=http%3A%2F%2Fx.test%2Fw.wav&audioName=W&audioFormat=wav"
    );
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

/// fake_server with an explicit bind address (the Sonos and
/// MusicCast fixtures need well-known hosts/ports).
fn fake_server_on(
    addr: &str,
    respond: impl Fn(&str, &str) -> String + Send + Sync + 'static,
) -> Option<(String, Arc<Mutex<Vec<Recorded>>>)> {
    let listener = std::net::TcpListener::bind(addr).ok()?;
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
    Some((base, log))
}

fn yxc_responder(path: &str, _body: &str) -> Option<String> {
    if path.contains("main/getStatus") {
        Some(r#"{"response_code":0,"power":"on","input":"hdmi1"}"#.into())
    } else if path.contains("system/getFeatures") {
        Some(
            r#"{"response_code":0,"zone":[{"id":"main","func_list":["prepare_input_change","volume"]}]}"#
                .into(),
        )
    } else if path.contains("prepareInputChange") || path.contains("setInput") {
        Some(r#"{"response_code":0}"#.into())
    } else {
        None
    }
}

#[test]
fn musiccast_input_is_prepared_before_the_transport_push() {
    // C06: a Yamaha renderer's input is switched to server
    // (prepareInputChange first, as its feature list requires)
    // BEFORE SetAVTransportURI is pushed.
    let (base, log) = fake_server(|path, body| {
        if let Some(reply) = yxc_responder(path, body) {
            return reply;
        }
        soap_ok()
    });
    let hostport = base.trim_start_matches("http://").to_string();
    assert!(castupnp::yxc_available(&hostport));
    castupnp::musiccast_prepare_host(&hostport);
    let dev = device("dlna", format!("{base}/AVTransport/control"));
    castupnp::play_upnp(&dev, "http://media.test/ep.mp3", "Ep", "audio/mpeg").unwrap();
    let log = log.lock().unwrap();
    let order: Vec<String> = log
        .iter()
        .map(|r| {
            if r.path.contains("getStatus") {
                "getStatus"
            } else if r.path.contains("getFeatures") {
                "getFeatures"
            } else if r.path.contains("prepareInputChange") {
                "prepare"
            } else if r.path.contains("setInput") {
                "setInput"
            } else if r.body.contains("SetAVTransportURI") {
                "push"
            } else if r.body.contains("<u:Play") {
                "play"
            } else {
                "?"
            }
            .to_string()
        })
        .collect();
    let pos = |name: &str| order.iter().position(|o| o == name).expect(name);
    assert!(pos("getStatus") < pos("getFeatures"), "{order:?}");
    assert!(pos("getFeatures") < pos("prepare"), "{order:?}");
    assert!(pos("prepare") < pos("setInput"), "{order:?}");
    assert!(pos("setInput") < pos("push"), "{order:?}");
    assert!(pos("push") < pos("play"), "{order:?}");
}

#[test]
fn musiccast_preparation_wires_through_play_on_port_80() {
    // The full wiring: play_upnp itself prepares the input.
    // YXC listens on the plain HTTP port, so this binds :80 and
    // skips where that is not permitted.
    let Some((base, log)) = fake_server_on("127.0.0.1:80", |path, body| {
        if let Some(reply) = yxc_responder(path, body) {
            return reply;
        }
        soap_ok()
    }) else {
        return;
    };
    let dev = device("dlna", format!("{base}/AVTransport/control"));
    castupnp::play_upnp(&dev, "http://media.test/ep.mp3", "Ep", "audio/mpeg").unwrap();
    let log = log.lock().unwrap();
    // Ordering only: another test's renderer probe may also
    // knock on port 80 while this server is bound.
    let first_set_input = log
        .iter()
        .position(|r| r.path.contains("setInput"))
        .expect("setInput sent");
    let first_push = log
        .iter()
        .position(|r| r.body.contains("SetAVTransportURI"))
        .expect("push sent");
    assert!(first_set_input < first_push, "input prepared before push");
}

#[test]
fn non_musiccast_renderer_skips_preparation() {
    // A renderer that does not answer YXC is not one: the probe
    // says unavailable and preparation is a no-op (the cast
    // proceeds regardless).
    assert!(!castupnp::yxc_available("127.0.0.1:1"));
    castupnp::musiccast_prepare_host("127.0.0.1:1");
}

fn sonos_description(avt_control: &str, zgt_control: Option<&str>) -> String {
    let zgt = zgt_control
        .map(|c| {
            format!(
                "<service><serviceType>urn:schemas-upnp-org:service:ZoneGroupTopology:1</serviceType>\
                 <controlURL>{c}</controlURL></service>"
            )
        })
        .unwrap_or_default();
    format!(
        "<root><device><friendlyName>Sonos</friendlyName>\
         <manufacturer>Sonos, Inc.</manufacturer>\
         <serviceList>\
         <service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>\
         <controlURL>{avt_control}</controlURL></service>{zgt}\
         </serviceList></device></root>"
    )
}

fn zone_group_state_reply() -> String {
    // One household on one loopback host (the sandbox's proxy
    // bypass list covers 127.0.0.1 only): the coordinator's
    // description lives at a distinct path, the member's at the
    // canonical one — members match by exact Location.
    let inner = "<ZoneGroupState><ZoneGroups>\
        <ZoneGroup Coordinator=\"RINCON_COORD\" ID=\"RINCON_COORD:57\">\
        <ZoneGroupMember UUID=\"RINCON_COORD\" Location=\"http://127.0.0.1:1400/coord/device_description.xml\" ZoneName=\"Living Room\"/>\
        <ZoneGroupMember UUID=\"RINCON_MEMBER\" Location=\"http://127.0.0.1:1400/xml/device_description.xml\" ZoneName=\"Kitchen\"/>\
        </ZoneGroup></ZoneGroups></ZoneGroupState>";
    let escaped = inner
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        "<s:Envelope><s:Body><u:GetZoneGroupStateResponse>\
         <ZoneGroupState>{escaped}</ZoneGroupState>\
         </u:GetZoneGroupStateResponse></s:Body></s:Envelope>"
    )
}

#[test]
fn sonos_group_state_parsing_finds_the_coordinator() {
    // C04 parse level: the member's group coordinator is the
    // other member; when this device coordinates, there is
    // nothing to reroute.
    let reply = zone_group_state_reply();
    assert_eq!(
        castupnp::parse_zone_group_coordinator(
            &reply,
            "http://127.0.0.1:1400/xml/device_description.xml"
        ),
        Some("http://127.0.0.1:1400/coord/device_description.xml".to_string())
    );
    assert_eq!(
        castupnp::parse_zone_group_coordinator(
            &reply,
            "http://127.0.0.1:1400/coord/device_description.xml"
        ),
        None
    );
    assert_eq!(
        castupnp::parse_zone_group_coordinator(
            &reply,
            "http://192.0.2.9:1400/xml/device_description.xml"
        ),
        None
    );
}

#[test]
fn sonos_play_and_stop_route_to_the_group_coordinator() {
    // C04: play and stop addressed to a group MEMBER land on
    // the coordinator's AVTransport instead. One server plays
    // the whole household, routed by path: the member answers
    // description + group state, the coordinator (a different
    // description path) takes the transport calls.
    let Some((_base, log)) = fake_server_on("127.0.0.1:1400", |path, body| {
        if path == "/xml/device_description.xml" {
            sonos_description(
                "/member/AVTransport/Control",
                Some("/ZoneGroupTopology/Control"),
            )
        } else if path == "/coord/device_description.xml" {
            sonos_description("/coord/AVTransport/Control", None)
        } else if body.contains("GetZoneGroupState") {
            zone_group_state_reply()
        } else {
            soap_ok()
        }
    }) else {
        return; // cannot bind the Sonos fixture port here
    };
    let dev = CastDevice {
        name: "Kitchen".into(),
        host: "127.0.0.1".into(),
        port: 1400,
        kind: "sonos".into(),
        model: String::new(),
        endpoint: "http://127.0.0.1:1400/member/AVTransport/Control".into(),
        sinks: Vec::new(),
    };
    castupnp::play_upnp(&dev, "http://media.test/ep.mp3", "Ep", "audio/mpeg").unwrap();
    castupnp::stop_upnp(&dev).unwrap();
    let log = log.lock().unwrap();
    assert!(
        log.iter().any(|r| r.body.contains("GetZoneGroupState")),
        "group state consulted"
    );
    assert!(
        !log.iter()
            .any(|r| r.path.contains("/member/") && r.body.contains("SetAVTransportURI")),
        "member must not receive the push"
    );
    assert!(
        log.iter()
            .any(|r| r.path.contains("/coord/") && r.body.contains("SetAVTransportURI")),
        "coordinator receives the push: {:?}",
        log.iter().map(|r| r.path.clone()).collect::<Vec<_>>()
    );
    assert!(
        log.iter()
            .any(|r| r.path.contains("/coord/") && r.body.contains("<u:Stop")),
        "coordinator receives the stop"
    );
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

// ---------------------------------------------------------------------------
// Cross-protocol name merge (Caster `_discover_sync`): one box
// answering two protocols under one name must appear once, and
// the entry kept is the one that can do more.

fn named(kind: &str, name: &str, sinks: &[&str]) -> CastDevice {
    CastDevice {
        name: name.into(),
        host: format!("host-{name}-{kind}"),
        port: 0,
        kind: kind.into(),
        model: String::new(),
        endpoint: String::new(),
        sinks: sinks.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn merge_keeps_the_video_capable_answer_for_a_tv_seen_twice() {
    // AirPlay first (RAOP: audio only, no sinks -> not a video
    // kind), then DLNA publishing video sinks: DLNA replaces,
    // in the AirPlay entry's position.
    let merged = merge_discovery_by_name(vec![
        named("airplay", "Lounge TV", &[]),
        named("kodi", "Study", &[]),
        named("dlna", "Lounge TV", &["video/mp4", "audio/mpeg"]),
    ]);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].name, "Lounge TV");
    assert_eq!(merged[0].kind, "dlna");
    assert_eq!(merged[1].name, "Study");
}

#[test]
fn merge_keeps_first_answer_when_the_challenger_proves_nothing_more() {
    // An amplifier: DLNA sinks are audio-only, so the later
    // DLNA answer must NOT displace the AirPlay one (Caster:
    // order decides every tie; AirPlay is the better path).
    let merged = merge_discovery_by_name(vec![
        named("airplay", "Amp", &[]),
        named("dlna", "Amp", &["audio/mpeg"]),
    ]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].kind, "airplay");
    // And a challenger with no published sinks never replaces,
    // even a same-name entry that cannot show video either.
    let merged2 = merge_discovery_by_name(vec![
        named("airplay", "Amp", &[]),
        named("dlna", "Amp", &[]),
    ]);
    assert_eq!(merged2.len(), 1);
    assert_eq!(merged2[0].kind, "airplay");
}

#[test]
fn supports_video_believes_sinks_then_falls_back_to_kind() {
    assert!(named("dlna", "TV", &["video/mp4"]).supports_video());
    assert!(!named("dlna", "Amp", &["audio/mpeg"]).supports_video());
    assert!(named("dlna", "Silent renderer", &[]).supports_video());
    assert!(!named("airplay", "Speaker", &[]).supports_video());
    assert!(named("chromecast", "Cast TV", &[]).supports_video());
}
