//! Audit 8 M02: SponsorBlock privacy and action semantics,
//! ported from Python core/sponsorblock.py and its test suite
//! (tests/test_sponsorblock.py). Lookups must use the
//! four-hex SHA-256 hash-prefix endpoint (never the bare
//! video id), empty categories must issue no request, only
//! "skip" segments of at least 0.5 s for THIS video survive
//! a collision response, and any lookup failure yields no
//! segments so playback carries on.

use blindrss_core::config::Config;
use blindrss_core::sponsorblock::{
    API_BASE, Segment, fetch_segments_with_base, hash_prefix, lookup_url, lookup_url_with_base,
    parse_segments, segment_at, skip_categories,
};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

const VIDEO_ID: &str = "dQw4w9WgXcQ";

fn seg(start: f64, end: f64, category: &str, uuid: &str) -> Segment {
    Segment {
        start,
        end,
        category: category.to_string(),
        uuid: uuid.to_string(),
    }
}

#[test]
fn lookup_url_exposes_only_the_hash_prefix() {
    let cats = vec!["sponsor".to_string()];
    let url = lookup_url(VIDEO_ID, &cats).expect("url");
    let prefix = hash_prefix(VIDEO_ID);
    assert_eq!(prefix, blindrss_core::rangecache::sha256_hex(VIDEO_ID)[..4]);
    assert!(url.starts_with(&format!("{API_BASE}/{prefix}?")), "{url}");
    assert!(!url.contains(VIDEO_ID), "bare video id leaked: {url}");
    assert!(url.contains("actionTypes=%5B%22skip%22%5D"), "{url}");
    assert!(url.contains("categories=%5B%22sponsor%22%5D"), "{url}");
    // No request may be built for an empty id or no categories.
    assert!(lookup_url("", &cats).is_none());
    assert!(lookup_url(VIDEO_ID, &[]).is_none());
}

#[test]
fn parse_keeps_only_this_video_and_skip_segments() {
    // Port of Python test_parse_keeps_only_this_video_and_skip_segments.
    let json = r#"[
      {"videoID": "other", "segments": [
        {"category": "sponsor", "segment": [1, 9], "UUID": "x"}
      ]},
      {"videoID": "abc123def45", "segments": [
        {"category": "outro", "actionType": "skip", "segment": [90, 100], "UUID": "b"},
        {"category": "sponsor", "actionType": "skip", "segment": [10, 40.5], "UUID": "a"},
        {"category": "poi_highlight", "actionType": "poi", "segment": [5, 5], "UUID": "c"},
        {"category": "sponsor", "actionType": "skip", "segment": [50, 50.2], "UUID": "tiny"},
        {"category": "sponsor", "actionType": "mute", "segment": [60, 70], "UUID": "m"},
        {"category": "sponsor", "segment": [20, 25], "UUID": "d"}
      ]}
    ]"#;
    let segs = parse_segments(json, "abc123def45");
    let uuids: Vec<&str> = segs.iter().map(|s| s.uuid.as_str()).collect();
    // "d" has no actionType: Python defaults it to "skip".
    assert_eq!(uuids, ["a", "d", "b"]);
    assert_eq!(segs[0], seg(10.0, 40.5, "sponsor", "a"));
    // A non-list payload and a payload for another prefix
    // collision set both yield nothing.
    assert!(parse_segments("{\"oops\": true}", "abc123def45").is_empty());
    assert!(parse_segments(json, "someoneelse").is_empty());
    assert!(parse_segments("not json", "abc123def45").is_empty());
}

#[test]
fn segment_at_skips_once_and_ignores_tail() {
    // Port of Python test_segment_at_skips_once_and_ignores_tail.
    let segs = vec![seg(10.0, 40.0, "sponsor", "a")];
    let done: HashSet<String> = HashSet::new();
    assert!(segment_at(&segs, 9.9, &done).is_none());
    assert_eq!(segment_at(&segs, 10.0, &done).unwrap().uuid, "a");
    assert!(segment_at(&segs, 39.8, &done).is_none()); // last half second
    let done: HashSet<String> = ["a".to_string()].into_iter().collect();
    assert!(segment_at(&segs, 20.0, &done).is_none());
}

fn cfg_with(pairs: serde_json::Value) -> Config {
    let mut cfg = Config::default();
    *cfg.raw_mut() = pairs;
    cfg
}

#[test]
fn skip_categories_respects_switch_and_order() {
    // Port of Python test_skip_categories_respects_switch_and_order.
    let off = cfg_with(serde_json::json!({"sponsorblock_enabled": false}));
    assert!(skip_categories(&off).is_empty());
    let cfg = cfg_with(serde_json::json!({
        "sponsorblock_enabled": true,
        "sponsorblock_categories": ["intro", "sponsor", "bogus"]
    }));
    assert_eq!(skip_categories(&cfg), ["sponsor", "intro"]);
    // Missing categories fall back to Python's DEFAULT_SKIP,
    // and a non-list stored value does the same.
    let unset = cfg_with(serde_json::json!({"sponsorblock_enabled": true}));
    assert_eq!(
        skip_categories(&unset),
        ["sponsor", "selfpromo", "interaction"]
    );
    let wrong_type = cfg_with(serde_json::json!({
        "sponsorblock_enabled": true,
        "sponsorblock_categories": "sponsor"
    }));
    assert_eq!(
        skip_categories(&wrong_type),
        ["sponsor", "selfpromo", "interaction"]
    );
    // An explicitly empty list means no categories, hence
    // no request (verified against the fixture below).
    let empty = cfg_with(serde_json::json!({
        "sponsorblock_enabled": true,
        "sponsorblock_categories": []
    }));
    assert!(skip_categories(&empty).is_empty());
}

/// A one-shot-per-connection loopback HTTP fixture that
/// records request lines and User-Agent headers.
struct Fixture {
    base: String,
    hits: Arc<Mutex<usize>>,
    requests: Arc<Mutex<Vec<String>>>,
    user_agents: Arc<Mutex<Vec<String>>>,
}

fn start_fixture(status: u16, body: &'static str) -> Fixture {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(0usize));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let user_agents = Arc::new(Mutex::new(Vec::new()));
    let (h, r, u) = (hits.clone(), requests.clone(), user_agents.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (h, r, u) = (h.clone(), r.clone(), u.clone());
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                *h.lock().unwrap() += 1;
                if let Some(line) = text.lines().next() {
                    r.lock().unwrap().push(line.to_string());
                }
                for line in text.lines() {
                    if let Some((k, v)) = line.split_once(':')
                        && k.trim().eq_ignore_ascii_case("user-agent")
                    {
                        u.lock().unwrap().push(v.trim().to_string());
                    }
                }
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Internal Server Error",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            });
        }
    });
    Fixture {
        base: format!("http://127.0.0.1:{port}/api/skipSegments"),
        hits,
        requests,
        user_agents,
    }
}

const COLLISION_BODY: &str = r#"[
  {"videoID": "collision99", "segments": [
    {"category": "sponsor", "actionType": "skip", "segment": [1, 9], "UUID": "zz"}
  ]},
  {"videoID": "dQw4w9WgXcQ", "segments": [
    {"category": "sponsor", "actionType": "skip", "segment": [1, 5], "UUID": "u"}
  ]}
]"#;

#[test]
fn fetch_sends_only_hash_prefix_with_central_user_agent() {
    // Port of Python test_fetch_sends_only_hash_prefix, over
    // a real loopback HTTP round trip.
    let fx = start_fixture(200, COLLISION_BODY);
    let cats = vec!["sponsor".to_string()];
    let segs = fetch_segments_with_base(&fx.base, VIDEO_ID, &cats);
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].uuid, "u");
    let requests = fx.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let line = &requests[0];
    let prefix = hash_prefix(VIDEO_ID);
    assert!(
        line.starts_with(&format!("GET /api/skipSegments/{prefix}?")),
        "{line}"
    );
    assert!(!line.contains(VIDEO_ID), "bare video id leaked: {line}");
    // The User-Agent comes from core's central resolution,
    // never a hard-coded call-site string.
    let uas = fx.user_agents.lock().unwrap();
    assert_eq!(uas.len(), 1);
    assert_eq!(uas[0], blindrss_core::http::user_agent());
    assert!(!uas[0].contains("BlindRSS-Rust"), "{}", uas[0]);
    // lookup_url_with_base agrees with what was sent.
    let built = lookup_url_with_base(&fx.base, VIDEO_ID, &cats).unwrap();
    assert!(built.starts_with(&format!("{}/{prefix}?", fx.base)));
}

#[test]
fn empty_categories_issue_no_request() {
    let fx = start_fixture(200, COLLISION_BODY);
    let segs = fetch_segments_with_base(&fx.base, VIDEO_ID, &[]);
    assert!(segs.is_empty());
    assert_eq!(*fx.hits.lock().unwrap(), 0);
    // An empty video id is likewise a no-request no-op.
    let segs = fetch_segments_with_base(&fx.base, "", &["sponsor".to_string()]);
    assert!(segs.is_empty());
    assert_eq!(*fx.hits.lock().unwrap(), 0);
}

#[test]
fn lookup_failure_leaves_playback_usable() {
    // Port of Python test_fetch_failure_means_no_segments:
    // every failure mode returns no segments, never an error.
    let cats = vec!["sponsor".to_string()];
    let fx500 = start_fixture(500, "boom");
    assert!(fetch_segments_with_base(&fx500.base, VIDEO_ID, &cats).is_empty());
    let fx404 = start_fixture(404, "");
    assert!(fetch_segments_with_base(&fx404.base, VIDEO_ID, &cats).is_empty());
    // Connection refused: bind a port, then drop the listener.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = dead.local_addr().unwrap().port();
    drop(dead);
    let base = format!("http://127.0.0.1:{port}/api/skipSegments");
    assert!(fetch_segments_with_base(&base, VIDEO_ID, &cats).is_empty());
}
