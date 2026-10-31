//! Batch 29 (v0.1.28): playback fallback plumbing — the
//! /play transcode URL and the local proxy's progressive
//! /stream serving (it buffered whole files before this
//! batch, which stalled podcast playback start).

use blindrss_core::proxy::{play_url, stream_url};
use std::io::{Read, Write};

#[test]
fn play_url_targets_the_transcode_route() {
    let u = play_url("https://cdn.example.com/ep 1.mp3?x=1&y=2");
    assert!(u.contains("/play?url="), "{u}");
    assert!(
        u.contains("https%3A%2F%2Fcdn.example.com%2Fep%201.mp3%3Fx%3D1%26y%3D2"),
        "{u}"
    );
}

#[test]
fn stream_url_targets_the_stream_route() {
    let u = stream_url("https://cdn.example.com/ep.mp3");
    assert!(u.contains("/stream?url=https%3A%2F%2F"), "{u}");
}

/// Serve one fixed body on a loopback port, forever (one
/// response per accepted connection).
fn spawn_upstream(body: &'static [u8]) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
            let _ = s.flush();
        }
    });
    port
}

#[test]
fn stream_route_passes_body_and_length_through() {
    let port = spawn_upstream(b"hello podcast");
    let target = format!("http://127.0.0.1:{port}/ep.mp3");
    let proxy = stream_url(&target);
    // The proxy base uses the LAN IP; dial it via loopback
    // instead (this VM's egress setup hairpins its own LAN IP).
    let pport = proxy
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .to_string();
    let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{pport}")).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let path = &proxy[proxy.find("/stream").unwrap()..];
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).unwrap();
    let text = String::from_utf8_lossy(&resp);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("Content-Type: audio/mpeg"), "{text}");
    assert!(text.contains("Content-Length: 13"), "{text}");
    assert!(text.ends_with("hello podcast"), "{text}");
}
