//! Audit 8 Task 28 (C01): Caster media preparation — probing,
//! native-HLS validation, the Chromecast load ladder, the
//! live-stall watchdog, load-settle policy, and the HLS relay
//! lifecycle, all against fixtures (loopback HTTP, injected
//! transports, generated TS bytes).

use blindrss_core::castmedia::{
    ChromecastLoadChain, LiveWatchdog, ProbeResponse, ProbeResult, WatchdogAction, classify_head,
    looks_like_mpegts, native_hls_url_with, prepare_roku_target, prepare_upnp_target, probe_media,
    probe_media_with,
};
use blindrss_core::castsession::{MediaStatus, SettleOutcome, await_playing_from, load_payload};
use std::cell::RefCell;
use std::collections::VecDeque;

/// Four aligned MPEG-TS packets (sync byte every 188).
fn ts_bytes() -> Vec<u8> {
    let mut out = vec![0x55u8; 4 * 188];
    for i in 0..4 {
        out[i * 188] = 0x47;
    }
    out
}

fn scripted_fetch(
    responses: Vec<ProbeResponse>,
) -> (
    impl Fn(&str) -> Result<ProbeResponse, String>,
    std::rc::Rc<RefCell<usize>>,
) {
    let calls = std::rc::Rc::new(RefCell::new(0));
    let counted = calls.clone();
    let queue = RefCell::new(VecDeque::from(responses));
    let fetch = move |_: &str| -> Result<ProbeResponse, String> {
        *counted.borrow_mut() += 1;
        queue
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| "no more scripted responses".to_string())
    };
    (fetch, calls)
}

fn no_sleep(_: std::time::Duration) {}

#[test]
fn ts_detection_aligned_mid_packet_and_false_positive() {
    let ts = ts_bytes();
    assert!(looks_like_mpegts(&ts));
    // A capture starting mid-packet: sync lands at offset 151
    // and repeats at the stride four times.
    let mut stream = ts_bytes();
    stream.extend(ts_bytes());
    assert!(looks_like_mpegts(&stream[37..]));
    // A lone 0x47 is not a transport stream.
    let mut fake = vec![0x47u8];
    fake.extend(vec![0x11u8; 800]);
    assert!(!looks_like_mpegts(&fake));
    assert!(!looks_like_mpegts(b""));
    assert!(!looks_like_mpegts(b"ID3xxxxxxxx"));
}

#[test]
fn probe_magic_bytes_beat_a_lying_content_type() {
    // An IPTV server claiming audio/mpeg while serving TS:
    // the bytes win, and TS means live.
    let (fetch, calls) = scripted_fetch(vec![ProbeResponse {
        content_type: Some("audio/mpeg".into()),
        body: ts_bytes(),
    }]);
    let probe = probe_media_with("http://srv.test/channel", &fetch, &no_sleep);
    assert_eq!(probe.mime, "video/mp2t");
    assert_eq!(probe.is_audio, Some(false));
    assert!(probe.is_live);
    assert_eq!(*calls.borrow(), 1); // enough bytes: no retry
}

#[test]
fn probe_id3_beats_an_html_content_type() {
    let mut body = b"ID3".to_vec();
    body.extend(vec![0u8; 300]);
    let (fetch, _calls) = scripted_fetch(vec![ProbeResponse {
        content_type: Some("text/html".into()),
        body,
    }]);
    let probe = probe_media_with("http://srv.test/ep", &fetch, &no_sleep);
    assert_eq!(probe.mime, "audio/mpeg");
    assert_eq!(probe.is_audio, Some(true));
    assert!(!probe.is_live);
}

#[test]
fn probe_playlist_content_type_is_live_hls_at_once() {
    let (fetch, calls) = scripted_fetch(vec![ProbeResponse {
        content_type: Some("application/vnd.apple.mpegurl".into()),
        body: b"#EXTM3U\n".to_vec(),
    }]);
    let probe = probe_media_with("http://srv.test/live", &fetch, &no_sleep);
    assert_eq!(probe.mime, "application/vnd.apple.mpegurl");
    assert_eq!(probe.is_audio, Some(false));
    assert!(probe.is_live);
    assert_eq!(*calls.borrow(), 1);
}

#[test]
fn probe_retries_until_bytes_arrive_and_keeps_the_longest() {
    let (fetch, calls) = scripted_fetch(vec![
        ProbeResponse {
            content_type: None,
            body: Vec::new(),
        },
        ProbeResponse {
            content_type: Some("audio/mpeg".into()),
            body: ts_bytes(),
        },
    ]);
    let probe = probe_media_with("http://srv.test/slow", &fetch, &no_sleep);
    assert_eq!(probe.mime, "video/mp2t");
    assert!(probe.is_live);
    assert_eq!(*calls.borrow(), 2);
}

#[test]
fn probe_with_no_evidence_at_all_is_live() {
    // Every attempt failed: Caster refuses to call the stream
    // VOD on no evidence.
    let (fetch, calls) = scripted_fetch(vec![]);
    let probe = probe_media_with("http://srv.test/radio", &fetch, &no_sleep);
    assert_eq!(*calls.borrow(), 5);
    assert!(probe.is_live);
    assert_eq!(probe.is_audio, None);
    assert_eq!(probe.mime, "audio/mpeg"); // the URL guess
}

#[test]
fn probe_local_file_is_never_live() {
    let dir = std::env::temp_dir().join(format!(
        "blindrss-t28-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let ts_path = dir.join("clip.ts");
    std::fs::write(&ts_path, ts_bytes()).unwrap();
    let probe = probe_media(ts_path.to_str().unwrap());
    assert_eq!(probe.mime, "video/mp2t");
    assert!(!probe.is_live);
    let mp3_path = dir.join("ep.mp3");
    let mut body = b"ID3".to_vec();
    body.extend(vec![0u8; 64]);
    std::fs::write(&mp3_path, body).unwrap();
    let probe = probe_media(mp3_path.to_str().unwrap());
    assert_eq!(probe.mime, "audio/mpeg");
    assert_eq!(probe.is_audio, Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn classify_uses_content_type_tables_then_guess() {
    let (mime, is_audio) = classify_head(&[0u8; 16], Some("video/webm"), "http://x.test/v");
    assert_eq!(mime, "video/webm");
    assert_eq!(is_audio, Some(false));
    let (mime, is_audio) = classify_head(&[0u8; 16], Some("audio/flac"), "http://x.test/a");
    assert_eq!(mime, "audio/flac");
    assert_eq!(is_audio, Some(true));
    let (mime, is_audio) = classify_head(&[0u8; 16], None, "http://x.test/song.flac");
    assert_eq!(mime, "audio/flac");
    assert_eq!(is_audio, Some(true));
}

const LIVE_PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n\
#EXTINF:6.0,\nseg100.ts\n#EXTINF:5.5,\nseg101.ts\n";

#[test]
fn native_hls_needs_a_live_short_segment_playlist() {
    let url = "http://srv.test/chan/seg/seg/12345.ts";
    let fetch = |_: &str| Ok(LIVE_PLAYLIST.to_string());
    assert_eq!(
        native_hls_url_with(url, &fetch),
        Some("http://srv.test/chan/seg/seg/12345.m3u8".to_string())
    );
    // A finished (VOD) playlist is not the live original.
    let vod = LIVE_PLAYLIST.replace(
        "#EXTINF:5.5,\nseg101.ts\n",
        "#EXTINF:5.5,\nseg101.ts\n#EXT-X-ENDLIST\n",
    );
    let fetch = |_: &str| Ok(vod.clone());
    assert_eq!(native_hls_url_with(url, &fetch), None);
    // Segments longer than the limit mean the sibling is not
    // the low-latency original either.
    let long = LIVE_PLAYLIST.replace("#EXTINF:6.0,", "#EXTINF:30.0,");
    let fetch = |_: &str| Ok(long.clone());
    assert_eq!(native_hls_url_with(url, &fetch), None);
    // Not a segment-shaped URL: no fetch is even attempted.
    let fetch =
        |_: &str| -> Result<String, String> { panic!("must not fetch for a non-segment URL") };
    assert_eq!(
        native_hls_url_with("http://srv.test/movie.mp4", &fetch),
        None
    );
}

/// The fake relay maker plus its log of `live` flags, in call order.
type FakeRelay = (
    Box<dyn FnMut(&str, bool) -> Result<String, String>>,
    std::rc::Rc<RefCell<Vec<bool>>>,
);

fn fake_relay() -> FakeRelay {
    let calls = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen = calls.clone();
    (
        Box::new(move |_source: &str, live: bool| {
            seen.borrow_mut().push(live);
            Ok("http://relay.test/pl.m3u8".to_string())
        }),
        calls,
    )
}

fn probe(mime: &str, is_audio: Option<bool>, is_live: bool) -> ProbeResult {
    ProbeResult {
        mime: mime.to_string(),
        is_audio,
        is_live,
    }
}

#[test]
fn chain_vod_direct_flips_then_converts_then_gives_up() {
    let (relay, relay_calls) = fake_relay();
    let mut chain = ChromecastLoadChain::new(
        "http://x.test/movie.mp4",
        probe("video/mp4", Some(false), false),
        None,
        relay,
    );
    let a1 = chain.first().unwrap();
    assert_eq!(a1.url, "http://x.test/movie.mp4");
    assert_eq!(a1.stream_type, "BUFFERED");
    assert!(a1.relay_source.is_none());
    let a2 = chain.advance().unwrap();
    assert_eq!(a2.url, a1.url);
    assert_eq!(a2.stream_type, "LIVE"); // the flip
    let a3 = chain.advance().unwrap();
    assert_eq!(a3.url, "http://relay.test/pl.m3u8");
    assert_eq!(a3.mime, "application/vnd.apple.mpegurl");
    assert_eq!(a3.stream_type, "BUFFERED");
    assert_eq!(a3.relay_source.as_deref(), Some("http://x.test/movie.mp4"));
    assert!(chain.advance().is_none()); // bounded: 3 loads total
    assert_eq!(*relay_calls.borrow(), vec![false]);
}

#[test]
fn chain_native_falls_back_to_relay_then_flips() {
    let (relay, relay_calls) = fake_relay();
    let mut chain = ChromecastLoadChain::new(
        "http://x.test/chan/seg/seg/9.ts",
        probe("video/mp2t", Some(false), true),
        Some("http://x.test/chan/seg/seg/9.m3u8".to_string()),
        relay,
    );
    let a1 = chain.first().unwrap();
    assert_eq!(a1.url, "http://x.test/chan/seg/seg/9.m3u8");
    assert_eq!(a1.stream_type, "LIVE");
    let a2 = chain.advance().unwrap();
    assert_eq!(a2.url, "http://relay.test/pl.m3u8");
    assert_eq!(a2.stream_type, "LIVE");
    let a3 = chain.advance().unwrap();
    assert_eq!(a3.url, a2.url);
    assert_eq!(a3.stream_type, "BUFFERED"); // the flip
    assert!(chain.advance().is_none());
    assert_eq!(*relay_calls.borrow(), vec![true]);
}

#[test]
fn chain_ts_live_without_native_starts_at_the_relay() {
    let (relay, _calls) = fake_relay();
    let mut chain = ChromecastLoadChain::new(
        "http://x.test/channel",
        probe("video/mp2t", Some(false), true),
        None,
        relay,
    );
    let a1 = chain.first().unwrap();
    assert_eq!(a1.url, "http://relay.test/pl.m3u8");
    assert_eq!(a1.stream_type, "LIVE");
    let a2 = chain.advance().unwrap();
    assert_eq!(a2.stream_type, "BUFFERED");
    assert!(chain.advance().is_none()); // no conversion: relay already used
}

#[test]
fn chain_ts_vod_relays_buffered() {
    let (relay, relay_calls) = fake_relay();
    let mut chain = ChromecastLoadChain::new(
        "http://x.test/clip.ts",
        probe("video/mp2t", Some(false), false),
        None,
        relay,
    );
    let a1 = chain.first().unwrap();
    assert_eq!(a1.url, "http://relay.test/pl.m3u8");
    assert_eq!(a1.stream_type, "BUFFERED");
    assert_eq!(*relay_calls.borrow(), vec![false]);
}

#[test]
fn upnp_and_roku_targets_follow_the_probe() {
    let relay = |_: &str, _: bool| Ok("http://relay.test/pl.m3u8".to_string());
    // UPnP: TS relays; plain HTTP plays with the probed mime.
    let (url, mime, relayed) = prepare_upnp_target(
        "http://x.test/ch",
        &probe("video/mp2t", Some(false), true),
        &relay,
    )
    .unwrap();
    assert_eq!(url, "http://relay.test/pl.m3u8");
    assert_eq!(mime, "application/vnd.apple.mpegurl");
    assert_eq!(relayed.as_deref(), Some("http://x.test/ch"));
    let (url, mime, relayed) = prepare_upnp_target(
        "http://x.test/ep.mp3",
        &probe("audio/mpeg", Some(true), false),
        &relay,
    )
    .unwrap();
    assert_eq!(url, "http://x.test/ep.mp3");
    assert_eq!(mime, "audio/mpeg");
    assert!(relayed.is_none());
    // Roku: TS relays; an mp3 keeps its URL-derived type.
    let (url, mime, _) = prepare_roku_target(
        "http://x.test/ch",
        &probe("video/mp2t", Some(false), true),
        &relay,
    )
    .unwrap();
    assert_eq!(url, "http://relay.test/pl.m3u8");
    assert_eq!(mime, "application/vnd.apple.mpegurl");
    let (url, mime, relayed) = prepare_roku_target(
        "http://x.test/ep.mp3",
        &probe("audio/mpeg", Some(true), false),
        &relay,
    )
    .unwrap();
    assert_eq!(url, "http://x.test/ep.mp3");
    assert_eq!(mime, "audio/mpeg");
    assert!(relayed.is_none());
}

#[test]
fn load_payload_carries_stream_type_and_casters_current_time_rule() {
    let p = load_payload("http://x/v.mp4", "video/mp4", "T", "BUFFERED", 30.0, false);
    assert_eq!(p["media"]["streamType"], "BUFFERED");
    assert_eq!(p["currentTime"], 30.0);
    let p = load_payload("http://x/live", "video/mp2t", "T", "LIVE", 30.0, false);
    assert_eq!(p["media"]["streamType"], "LIVE");
    assert!(p.get("currentTime").is_none());
    // A live relay's buffered start is 0, never the resume point.
    let p = load_payload(
        "http://relay/pl.m3u8",
        "application/vnd.apple.mpegurl",
        "T",
        "BUFFERED",
        30.0,
        true,
    );
    assert_eq!(p["currentTime"], 0.0);
}

fn media_status(state: &str, session: i64, reason: Option<&str>) -> MediaStatus {
    MediaStatus {
        position: 0.0,
        duration: 0.0,
        state: state.to_string(),
        idle_reason: reason.map(|r| r.to_string()),
        media_session_id: Some(session),
    }
}

#[test]
fn settle_policy_matches_await_playing() {
    // PLAYING on the new session settles.
    let mut statuses = VecDeque::from(vec![media_status("PLAYING", 2, None)]);
    let mut source = || statuses.pop_front();
    assert_eq!(
        await_playing_from(
            &mut source,
            Some(1),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(5)
        ),
        SettleOutcome::Settled
    );
    // PAUSED settles too.
    let mut statuses = VecDeque::from(vec![media_status("PAUSED", 2, None)]);
    let mut source = || statuses.pop_front();
    assert_eq!(
        await_playing_from(
            &mut source,
            Some(1),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(5)
        ),
        SettleOutcome::Settled
    );
    // IDLE with a real reason rejects at once, with the reason.
    let mut statuses = VecDeque::from(vec![media_status("IDLE", 2, Some("ERROR"))]);
    let mut source = || statuses.pop_front();
    assert_eq!(
        await_playing_from(
            &mut source,
            Some(1),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(5)
        ),
        SettleOutcome::Failed(Some("ERROR".to_string()))
    );
    // A stale session's report is ignored, then the new one settles.
    let mut statuses = VecDeque::from(vec![
        media_status("IDLE", 1, Some("ERROR")),
        media_status("PLAYING", 2, None),
    ]);
    let mut source = || statuses.pop_front();
    assert_eq!(
        await_playing_from(
            &mut source,
            Some(1),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(5)
        ),
        SettleOutcome::Settled
    );
    // An interrupted old session on the NEW session id is not a
    // rejection either; the deadline decides, reason kept.
    let mut statuses = VecDeque::from(vec![media_status("IDLE", 2, Some("INTERRUPTED"))]);
    let mut source = || statuses.pop_front();
    assert_eq!(
        await_playing_from(
            &mut source,
            Some(1),
            std::time::Duration::from_millis(60),
            std::time::Duration::from_millis(5)
        ),
        SettleOutcome::Failed(Some("INTERRUPTED".to_string()))
    );
}

#[test]
fn watchdog_recovers_stalls_and_idles_with_bounds() {
    let mut wd = LiveWatchdog::new();
    // Not armed (a VOD load): nothing is ever recovered.
    assert_eq!(
        wd.note(Some("IDLE"), Some(1), Some(0.0), 100.0),
        WatchdogAction::None
    );
    wd.arm(true);
    // Progress at t=0; the same position at t=10 is fine, at
    // t=21 it is a stall (20 s rule).
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(10.0), 0.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(10.0), 10.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(10.0), 21.0),
        WatchdogAction::Reload
    );
    // Recovery is rate-limited to one per 30 s.
    assert_eq!(
        wd.note(Some("IDLE"), Some(1), Some(0.0), 22.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("IDLE"), Some(1), Some(0.0), 52.0),
        WatchdogAction::Reload
    );
    // A pause is the user's: progress clears, no recovery, and
    // the stall clock restarts from the next play observation.
    assert_eq!(
        wd.note(Some("PAUSED"), Some(1), Some(30.0), 60.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(30.0), 61.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(30.0), 80.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(30.0), 82.0),
        WatchdogAction::Reload
    );
    // Position movement is progress.
    let mut wd = LiveWatchdog::new();
    wd.arm(true);
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(0.0), 0.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(3.0), 19.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(3.0), 38.0),
        WatchdogAction::None
    );
    assert_eq!(
        wd.note(Some("PLAYING"), Some(1), Some(3.0), 40.0),
        WatchdogAction::Reload
    );
}

#[test]
fn hls_relay_starts_serves_and_stops() {
    // C01 relay lifecycle against a real ffmpeg and a loopback
    // TS origin: the relay produces a live playlist, and stop
    // tears it down (process and directory gone).
    let Some(ffmpeg) = blindrss_core::silence::find_ffmpeg() else {
        return;
    };
    let unique = format!(
        "blindrss-t28-relay-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(&unique);
    std::fs::create_dir_all(&dir).unwrap();
    let ts_path = dir.join("origin.ts");
    let status = std::process::Command::new(&ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=120:size=320x240:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=120",
            "-c:v",
            "mpeg2video",
            "-c:a",
            "mp2",
            "-f",
            "mpegts",
            ts_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let ts = std::fs::read(&ts_path).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            use std::io::Write;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: video/mp2t\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                ts.len()
            );
            // A live origin does not hand over two minutes of
            // stream instantly: throttle the delivery so the
            // relay is genuinely mid-stream while the test runs
            // (an instant origin lets ffmpeg finish and exit
            // before the assertions look).
            for chunk in ts.chunks(8192) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    });
    let source = format!("http://127.0.0.1:{port}/channel.ts");
    let hls_root = std::env::temp_dir().join("blindrss-hls");
    let dirs_before: std::collections::HashSet<String> = std::fs::read_dir(&hls_root)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let playlist_url = blindrss_core::proxy::start_hls_relay(&source, true).expect("relay starts");
    assert!(playlist_url.contains("mode=live"), "{playlist_url}");
    assert!(blindrss_core::proxy::hls_relay_active(&source));
    // The playlist is served through the proxy with segments.
    // Fetch it the way the other proxy fixtures do: a raw
    // socket to 127.0.0.1 on the proxy's port (the returned URL
    // carries the LAN address, and HTTP clients in this
    // environment detour through an egress proxy for it).
    let port: u16 = playlist_url
        .rsplit(':')
        .next()
        .and_then(|p| p.split('/').next())
        .and_then(|p| p.parse().ok())
        .expect("proxy port");
    let path = playlist_url
        .split_once(&format!(":{port}"))
        .map(|x| x.1)
        .expect("proxy path")
        .to_string();
    // HLS clients poll the playlist while it is being written;
    // fetch until it answers with the header (bounded).
    let mut text = String::new();
    for _ in 0..25 {
        use std::io::{Read, Write};
        let Ok(mut stream) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) else {
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        if write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .is_err()
        {
            continue;
        }
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        if raw.contains("#EXTM3U") {
            text = raw;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(text.contains("200 OK"), "{text}");
    assert!(text.contains("#EXTM3U"), "{text}");
    assert!(text.contains("seg"), "{text}");
    let new_dirs: Vec<String> = std::fs::read_dir(&hls_root)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .filter(|n| !dirs_before.contains(n))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(new_dirs.len(), 1, "{new_dirs:?}");
    blindrss_core::proxy::stop_hls_relay(&source);
    assert!(!blindrss_core::proxy::hls_relay_active(&source));
    assert!(
        !hls_root.join(&new_dirs[0]).exists(),
        "relay directory removed on stop"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
