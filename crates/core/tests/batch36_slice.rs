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
    let _guard = singleton_guard();
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
    let _guard = singleton_guard();
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
                // Read through the end of the request headers:
                // a single read() can return a split request
                // under parallel-test load, losing the Range
                // line and corrupting the response shape.
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
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

/// The range-cache proxy is a process-wide singleton and
/// every entry point used below (`retune_from_config`,
/// `proxify`, `maybe_range_cache_url`) replaces or reads its
/// global config. Tests that touch it must therefore run one
/// at a time: a concurrent test's retune can land between
/// this test's retune and its entry creation, handing the
/// entry another test's cache directory and background
/// settings — observed on CI as a vanished cache chunk
/// (Windows) and as an extra background origin hit racing
/// the hit-count assertions (macOS).
static RANGE_CACHE_SINGLETON_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn singleton_guard() -> std::sync::MutexGuard<'static, ()> {
    RANGE_CACHE_SINGLETON_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let _guard = singleton_guard();
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

    // Wait until the fetched window is committed to the disk
    // cache before asserting the repeat read is a pure cache
    // serve. The chunk file's name is deterministic
    // (chunk_path's zero-padded start-end), and its rename into
    // place is what registers the segment. (The CI failures
    // that motivated this — hits 2 -> 3 on macOS, chunk never
    // in this test's dir on Windows — were traced to parallel
    // tests retuning the shared proxy singleton; the
    // singleton_guard above is the fix, this poll is the
    // settle check.) The bounded poll keeps the test honest: a
    // cache that never writes the chunk still fails here.
    let chunk_name = "000000001000-000000001999.bin";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut chunk_on_disk = false;
    while std::time::Instant::now() < deadline {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for sub in rd.flatten() {
                let sub_path = sub.path();
                if sub_path.is_dir() && sub_path.join(chunk_name).is_file() {
                    chunk_on_disk = true;
                }
            }
        }
        if chunk_on_disk {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        chunk_on_disk,
        "fetched window was committed to the disk cache"
    );
    // Fresh baseline after the settle: any late first-request
    // origin traffic has landed by now.
    let hits_after_first = *origin.hits.lock().unwrap();

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

// ---------------------------------------------------------------------
// Audit 8 Task 22 (M03, M04): refused probes recover; background
// requests are capped at Python's 10 MiB.

#[test]
fn background_request_target_is_capped_at_ten_mib() {
    // Python _BG_MAX_REQUEST_BYTES: the bootstrap target and the
    // steady-state chunk both clamp to 10 MiB per origin request,
    // however large the configured burst/chunk sizes are.
    let cap = 10 * 1024 * 1024;
    assert_eq!(
        background_request_target(true, 64 * 1024 * 1024, 8 * 1024 * 1024),
        cap
    );
    assert_eq!(
        background_request_target(false, 64 * 1024 * 1024, 32 * 1024 * 1024),
        cap
    );
    // Smaller configured sizes pass through untouched.
    assert_eq!(
        background_request_target(true, 4 * 1024 * 1024, 8 * 1024 * 1024),
        4 * 1024 * 1024
    );
    assert_eq!(
        background_request_target(false, 64 * 1024 * 1024, 1024 * 1024),
        1024 * 1024
    );
}

/// Origin with a controllable probe-refusal switch (M03):
/// probe requests (Range bytes=0-0) can be refused with 403 a
/// chosen number of times; data ranges always serve.
struct GateOrigin {
    url: String,
    probe_hits: std::sync::Arc<std::sync::Mutex<usize>>,
    data_hits: std::sync::Arc<std::sync::Mutex<usize>>,
    refusals_left: std::sync::Arc<std::sync::Mutex<usize>>,
}

fn start_gate_origin(body: Vec<u8>, refusals: usize) -> GateOrigin {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let probe_hits = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let data_hits = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let refusals_left = std::sync::Arc::new(std::sync::Mutex::new(refusals));
    let (ph, dh, rl) = (probe_hits.clone(), data_hits.clone(), refusals_left.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let (ph, dh, rl) = (ph.clone(), dh.clone(), rl.clone());
            let body = body.clone();
            std::thread::spawn(move || {
                // Read through the end of the request headers:
                // a single read() can return a split request
                // under parallel-test load, losing the Range
                // line and corrupting the response shape.
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
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
                if range == "bytes=0-0" {
                    *ph.lock().unwrap() += 1;
                    let mut left = rl.lock().unwrap();
                    if *left > 0 {
                        *left -= 1;
                        drop(left);
                        let head = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.flush();
                        return;
                    }
                } else {
                    *dh.lock().unwrap() += 1;
                }
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
    GateOrigin {
        url: format!("http://127.0.0.1:{port}/ep.mp3"),
        probe_hits,
        data_hits,
        refusals_left,
    }
}

fn rc_test_cfg(tag: &str) -> blindrss_core::config::Config {
    let mut cfg = blindrss_core::config::Config::default();
    let dir = std::env::temp_dir().join(format!(
        "blindrss-rc-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    cfg.raw_mut()["range_cache_dir"] = serde_json::json!(dir.to_string_lossy());
    cfg.raw_mut()["range_cache_background_download"] = serde_json::json!(false);
    cfg
}

#[test]
fn probe_403_then_206_recovers() {
    let _guard = singleton_guard();
    // M03: googlevideo can refuse a freshly minted URL's first
    // probe with 403 and serve ranges a second later. The probe
    // retries once and the session must end up range-supported,
    // with later reads served from the disk cache.
    let body: Vec<u8> = (0..120_000u32).map(|i| (i % 251) as u8).collect();
    let origin = start_gate_origin(body.clone(), 1);
    let cfg = rc_test_cfg("m03a");
    retune_from_config(&cfg);
    let proxied = proxify(&origin.url, &[], false);
    assert!(proxied.contains("/media?id="), "got {proxied}");

    let (status, data, head) = http_get(&proxied, Some("bytes=1000-1999"));
    assert_eq!(status, 206);
    assert_eq!(data, body[1000..=1999]);
    assert!(head.contains("bytes 1000-1999/120000"), "head: {head}");
    assert_eq!(
        *origin.probe_hits.lock().unwrap(),
        2,
        "one refused probe + one retry"
    );
    let data_after_first = *origin.data_hits.lock().unwrap();
    assert_eq!(data_after_first, 1, "one origin fetch for the range");

    // Second read of the same range: pure cache, no new origin
    // traffic of either kind.
    let (status2, data2, _) = http_get(&proxied, Some("bytes=1000-1999"));
    assert_eq!(status2, 206);
    assert_eq!(data2, body[1000..=1999]);
    assert_eq!(*origin.probe_hits.lock().unwrap(), 2);
    assert_eq!(*origin.data_hits.lock().unwrap(), data_after_first);
}

#[test]
fn probe_refusal_never_permanently_disables_cache() {
    let _guard = singleton_guard();
    // M03: a probe round that is refused twice (initial +
    // retry) leaves range support UNKNOWN, not unsupported —
    // the next request probes again and can recover.
    let body: Vec<u8> = (0..120_000u32).map(|i| (i % 251) as u8).collect();
    // Refuse far more probes than one request round makes.
    let origin = start_gate_origin(body.clone(), usize::MAX);
    let cfg = rc_test_cfg("m03b");
    retune_from_config(&cfg);
    let proxied = proxify(&origin.url, &[], false);

    // While refused, the bytes still reach the client (the
    // passthrough relays the origin's own 206).
    let (status, data, _) = http_get(&proxied, Some("bytes=1000-1999"));
    assert_eq!(status, 206);
    assert_eq!(data, body[1000..=1999]);
    let probes_while_refused = *origin.probe_hits.lock().unwrap();
    assert!(
        probes_while_refused >= 2,
        "probe was retried, hits={probes_while_refused}"
    );

    // The origin starts answering probes: the very next read
    // must probe again (support was left unknown, not latched
    // off) and land on the cache path.
    *origin.refusals_left.lock().unwrap() = 0;
    let (status2, data2, head2) = http_get(&proxied, Some("bytes=2000-2999"));
    assert_eq!(status2, 206);
    assert_eq!(data2, body[2000..=2999]);
    assert!(head2.contains("bytes 2000-2999/120000"), "head: {head2}");
    assert!(
        *origin.probe_hits.lock().unwrap() > probes_while_refused,
        "a fresh probe ran after the refusal round"
    );
    let data_after_recovery = *origin.data_hits.lock().unwrap();

    // And once recovered, repeat reads are cache-served.
    let (status3, data3, _) = http_get(&proxied, Some("bytes=2000-2999"));
    assert_eq!(status3, 206);
    assert_eq!(data3, body[2000..=2999]);
    assert_eq!(*origin.data_hits.lock().unwrap(), data_after_recovery);
}

#[test]
fn gate_registers_original_url_as_cache_identity() {
    let _guard = singleton_guard();
    // M20: when the player hands the gate a resolved URL plus
    // its original, the persisted mapping is keyed by the
    // ORIGINAL (stable) URL — an expired signed redirect later
    // refreshes through the original and reuses the segments.
    let cfg = rc_test_cfg("m20");
    retune_from_config(&cfg);
    let original = "https://pod.example.com/show/episode-7";
    let resolved = "https://signed.example.com/ep.mp3?token=expires-soon";
    let local = maybe_range_cache_url(resolved, &[], true, Some(original), &cfg);
    assert!(local.contains("/media?id="), "got {local}");
    // The session id is the hash of the REGISTERED url plus
    // the request headers (proxify's identity rule): it must
    // be the original URL's id, not the signed URL's — the
    // signed URL is never the cache identity. (Asserted on
    // the id rather than the mapping file because the proxy
    // is a process-wide singleton shared with parallel tests.)
    let ua = blindrss_core::http::user_agent();
    let original_id = &sha256_hex(&format!("{original}\nuser-agent:{ua}"))[..24];
    let signed_id = &sha256_hex(&format!("{resolved}\nuser-agent:{ua}"))[..24];
    assert!(
        local.contains(original_id),
        "cache identity is the original URL: {local}"
    );
    assert!(
        !local.contains(signed_id),
        "signed URL must not be the identity: {local}"
    );
}
