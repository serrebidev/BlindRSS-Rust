//! B16 loopback tests: the impersonated fetch crosses a
//! real socket with the emulation profile's identity, the
//! caller's cookie and Python's Accept headers, and only a
//! 200..400 status yields a body. Loopback only — live
//! JA3-wall acceptance is a manual check, not a test (the
//! sandbox's egress is refused by the very endpoints this
//! stage exists for).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Serve one request, capture its head, reply with the
/// given status and body.
fn serve_once(status: u16, body: &str) -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let sink = captured.clone();
    let body = body.to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        *sink.lock().unwrap() = String::from_utf8_lossy(&buf).to_string();
        let reason = if status == 200 { "OK" } else { "Forbidden" };
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    (format!("http://{addr}/article"), captured)
}

#[test]
fn impersonated_fetch_sends_the_profile_identity_and_cookie() {
    let (url, captured) = serve_once(200, "<html><body>full story</body></html>");
    let body = blindrss_core::impersonate::fetch(&url, "", "session=abc", Duration::from_secs(15));
    assert_eq!(
        body.as_deref(),
        Some("<html><body>full story</body></html>")
    );
    // Header names arrive lowercased (hyper's HTTP/1.1
    // serialization); compare case-insensitively.
    let head = captured.lock().unwrap().clone().to_lowercase();
    // The profile owns the identity set (Python's
    // _impersonated_headers division of labor): the wire
    // User-Agent is the emulated Chrome's, not a Rust one.
    assert!(head.contains("chrome/149"), "request head: {head}");
    assert!(head.contains("cookie: session=abc"), "request head: {head}");
    assert!(
        head.contains("accept-language: en-us,en;q=0.9"),
        "request head: {head}"
    );
    assert!(
        head.contains("accept: text/html,application/xhtml+xml"),
        "request head: {head}"
    );
}

#[test]
fn impersonated_fetch_rejects_error_statuses() {
    let (url, _captured) = serve_once(403, "challenge");
    let body = blindrss_core::impersonate::fetch(&url, "", "", Duration::from_secs(15));
    assert_eq!(body, None);
}

#[test]
fn impersonated_fetch_bytes_returns_the_raw_body() {
    let (url, captured) = serve_once(200, RSS_BODY);
    let bytes =
        blindrss_core::impersonate::fetch_bytes(&url, "chrome", "", Duration::from_secs(15));
    assert_eq!(bytes.as_deref(), Some(RSS_BODY.as_bytes()));
    let head = captured.lock().unwrap().to_lowercase();
    assert!(
        head.contains("accept: application/rss+xml"),
        "the feed Accept header goes out, head: {head}"
    );
}

const RSS_BODY: &str = "<rss version='2.0'><channel><title>Bytes Feed</title></channel></rss>";
