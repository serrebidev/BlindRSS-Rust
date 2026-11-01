//! Batch 36 (range-cache proxy) tests: the pure segment math
//! and header parsers (direct ports of Python's helpers,
//! checked against Python's semantics), the player gate, and
//! an offline end-to-end pass against a local origin server
//! on 127.0.0.1 — ranges are served from the origin once and
//! from the disk cache afterwards.

use blindrss_core::rangecache::*;

#[test]
fn segment_math_matches_python() {
    // _merge_segments: overlapping and ADJACENT ranges merge.
    assert_eq!(merge_segments(vec![(10, 19), (0, 9)]), vec![(0, 19)]);
    assert_eq!(
        merge_segments(vec![(0, 5), (7, 9), (30, 40)]),
        vec![(0, 5), (7, 9), (30, 40)]
    );
    assert!(merge_segments(vec![]).is_empty());
    // _normalize_segments: no merging (files are 1:1), bad
    // ranges dropped, deduped + sorted.
    assert_eq!(
        normalize_segments(vec![(5, 3), (10, 12), (0, 4), (10, 12)]),
        vec![(0, 4), (10, 12)]
    );
    // _missing_segments.
    assert_eq!(
        missing_segments(&[(0, 99), (200, 299)], 50, 250),
        vec![(100, 199)]
    );
    assert_eq!(missing_segments(&[], 3, 7), vec![(3, 7)]);
    assert!(missing_segments(&[(0, 10)], 0, 10).is_empty());
    assert!(missing_segments(&[(0, 10)], 9, 5).is_empty());
}

#[test]
fn header_parsers_match_python() {
    assert_eq!(
        parse_content_range("bytes 0-0/12345"),
        Some((0, 0, Some(12345)))
    );
    assert_eq!(parse_content_range("bytes 10-20/*"), Some((10, 20, None)));
    assert_eq!(parse_content_range("items 0-0/5"), None);
    // _parse_range_header: open-ended leaves end unset.
    assert_eq!(
        parse_range_header("bytes=100-", Some(1000)),
        Some((100, None))
    );
    assert_eq!(
        parse_range_header("bytes=100-199", Some(1000)),
        Some((100, Some(199)))
    );
    // End clamps to the total; end < start becomes start.
    assert_eq!(
        parse_range_header("bytes=100-5000", Some(1000)),
        Some((100, Some(999)))
    );
    assert_eq!(
        parse_range_header("bytes=100-50", Some(1000)),
        Some((100, Some(100)))
    );
    // Suffix ranges and multi-ranges are unsupported in Python.
    assert_eq!(parse_range_header("bytes=-100", Some(1000)), None);
    assert_eq!(parse_range_header("bytes=0-1,5-6", Some(1000)), None);
    assert_eq!(parse_range_header("", Some(1000)), None);
}

#[test]
fn player_gate_rules() {
    let cfg = blindrss_core::config::Config::default();
    // Non-http and loopback pass through untouched.
    assert_eq!(
        maybe_range_cache_url("/local/file.mp3", &[], false, None, &cfg),
        "/local/file.mp3"
    );
    assert_eq!(
        maybe_range_cache_url("http://127.0.0.1:8000/x.mp3", &[], false, None, &cfg),
        "http://127.0.0.1:8000/x.mp3"
    );
    // HLS is never range-cached.
    assert_eq!(
        maybe_range_cache_url("https://cdn.example.com/live.m3u8", &[], false, None, &cfg),
        "https://cdn.example.com/live.m3u8"
    );
    // Host helpers.
    assert!(is_googlevideo_url(
        "https://rr1---sn-abc.googlevideo.com/videoplayback"
    ));
    assert!(!is_googlevideo_url(
        "https://example.com/googlevideo.com.mp3"
    ));
    assert!(is_podcast_tracking_url("https://pscrb.fm/rss/p/show.mp3"));
    assert!(is_podcast_tracking_url("https://www.podtrac.com/pts/x.mp3"));
    assert!(!is_podcast_tracking_url("https://example.com/show.mp3"));
}

#[test]
fn disabled_gate_returns_direct_url() {
    let mut cfg = blindrss_core::config::Config::default();
    cfg.raw_mut()["range_cache_enabled"] = serde_json::json!(false);
    assert_eq!(
        maybe_range_cache_url("https://cdn.example.com/ep.mp3", &[], false, None, &cfg),
        "https://cdn.example.com/ep.mp3"
    );
    // ... but a podcast tracker still forces the proxy (the
    // gate returns a loopback /media URL or, when delivery is
    // impossible, the original — never anything else).
    let out = maybe_range_cache_url("https://pscrb.fm/ep.mp3", &[], false, None, &cfg);
    assert!(out == "https://pscrb.fm/ep.mp3" || out.contains("/media?id="));
}

/// A range-capable origin on loopback: serves a fixed body,
/// honouring Range with 206 + Content-Range, and counts hits.
struct Origin {
    url: String,
    hits: std::sync::Arc<std::sync::Mutex<usize>>,
}

fn start_origin(body: Vec<u8>) -> Origin {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let hits2 = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let hits = hits2.clone();
            let body = body.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let Ok(n) = stream.read(&mut buf) else { return };
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                *hits.lock().unwrap() += 1;
                let range = text
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        if k.trim().eq_ignore_ascii_case("range") {
                            Some(v.trim().to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                let total = body.len();
                let (status, start, end) = match parse_range_header(&range, Some(total as u64)) {
                    Some((s, Some(e))) => ("206 Partial Content", s as usize, e as usize),
                    Some((s, None)) => ("206 Partial Content", s as usize, total - 1),
                    None => ("200 OK", 0, total - 1),
                };
                let slice = &body[start..=end];
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    slice.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(slice);
                let _ = stream.flush();
            });
        }
    });
    Origin {
        url: format!("http://127.0.0.1:{port}/ep.mp3"),
        hits,
    }
}

fn http_get(url: &str, range: Option<&str>) -> (u16, Vec<u8>, String) {
    use std::io::{Read, Write};
    let after = url.split("://").nth(1).unwrap();
    let (authority, path) = after.split_once('/').unwrap();
    let mut stream = std::net::TcpStream::connect(authority).unwrap();
    let mut req = format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    if let Some(r) = range {
        req.push_str(&format!("Range: {r}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let split = text.find("\r\n\r\n").unwrap();
    (status, raw[split + 4..].to_vec(), text[..split].to_string())
}

#[test]
fn end_to_end_range_cache_on_loopback() {
    // A 300 KB deterministic body.
    let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let origin = start_origin(body.clone());
    let mut cfg = blindrss_core::config::Config::default();
    let dir = std::env::temp_dir().join(format!(
        "blindrss-rc-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    cfg.raw_mut()["range_cache_dir"] = serde_json::json!(dir.to_string_lossy());
    cfg.raw_mut()["range_cache_background_download"] = serde_json::json!(false);

    // The player gate skips loopback origins by design, so the
    // end-to-end pass registers the URL directly after tuning
    // the singleton at the test config.
    retune_from_config(&cfg);
    let proxied = proxify(&origin.url, &[], false);
    assert!(proxied.contains("/media?id="), "got {proxied}");

    // A ranged read comes back 206 with the right bytes and
    // the real total in Content-Range.
    let (status, data, head) = http_get(&proxied, Some("bytes=1000-1999"));
    assert_eq!(status, 206);
    assert_eq!(data, body[1000..=1999]);
    assert!(head.contains("bytes 1000-1999/300000"), "head: {head}");
    let hits_after_first = *origin.hits.lock().unwrap();
    assert!(
        hits_after_first >= 2,
        "probe + fetch, hits={hits_after_first}"
    );

    // The same range again is served from the disk cache: no
    // new origin hit for the data (the probe is single-flight
    // and already done).
    let (status2, data2, _) = http_get(&proxied, Some("bytes=1000-1999"));
    assert_eq!(status2, 206);
    assert_eq!(data2, body[1000..=1999]);
    assert_eq!(*origin.hits.lock().unwrap(), hits_after_first);

    // An open-ended read clamps to the inline window (1 MB
    // default here exceeds the file, so the whole tail).
    let (status3, data3, head3) = http_get(&proxied, Some("bytes=299000-"));
    assert_eq!(status3, 206);
    assert_eq!(data3, body[299000..]);
    assert!(
        head3.contains("bytes 299000-299999/300000"),
        "head: {head3}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
