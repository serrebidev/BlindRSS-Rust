//! Local HTTP proxy for playback and casting (Python
//! `core/stream_proxy.py` + `core/range_cache_proxy.py`).
//!
//! Cast devices and some players need a plain HTTP URL on the
//! LAN for media that is local, behind a resolver, or in a
//! format they cannot take directly. One server per process,
//! started on demand:
//!
//!   GET /stream?url=<enc>   relay a remote URL (Range passed
//!                           through so devices can seek)
//!   GET /file?path=<enc>    serve a local file with Range
//!   GET /hls?url=<enc>      ffmpeg-transcoded HLS playlist +
//!                           segments for picky renderers
//!
//! The base URL uses the machine's LAN address so other
//! devices can reach it.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::OnceLock;

static BASE: OnceLock<String> = OnceLock::new();

/// The LAN IPv4 address other devices can reach us at (the
/// UDP-connect trick: no traffic is sent).
pub fn lan_ip() -> String {
    let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else {
        return "127.0.0.1".to_string();
    };
    if socket.connect("8.8.8.8:80").is_ok()
        && let Ok(addr) = socket.local_addr()
    {
        return addr.ip().to_string();
    }
    "127.0.0.1".to_string()
}

/// Start the proxy if needed and return its base URL
/// (http://<lan-ip>:<port>).
pub fn ensure_started() -> String {
    if let Some(base) = BASE.get() {
        return base.clone();
    }
    let listener = TcpListener::bind("0.0.0.0:0").expect("proxy bind");
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let base = format!("http://{}:{port}", lan_ip());
    let _ = BASE.set(base.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    std::thread::spawn(move || handle(stream));
                }
                Err(_) => break,
            }
        }
    });
    base
}

/// A proxy URL relaying `target` (for cast devices).
pub fn stream_url(target: &str) -> String {
    format!("{}/stream?url={}", ensure_started(), url_encode(target))
}

/// A proxy URL serving a local file.
pub fn file_url(path: &std::path::Path) -> String {
    format!(
        "{}/file?path={}",
        ensure_started(),
        url_encode(&path.to_string_lossy())
    )
}

/// A proxy URL for the HLS rendition of `target`.
pub fn hls_url(target: &str) -> String {
    format!("{}/hls?url={}", ensure_started(), url_encode(target))
}

/// A proxy URL that plays `target` through the ffmpeg audio
/// filter chain `af` (the equalizer path for the built-in
/// player: MediaCtrl has no EQ, so when the EQ is enabled the
/// player loads this stream instead — a live transcode, which
/// means seeking inside it is limited).
pub fn eq_url(target: &str, af: &str) -> String {
    format!(
        "{}/eq?url={}&af={}",
        ensure_started(),
        url_encode(target),
        url_encode(af)
    )
}

/// Serve `url` transcoded through the ffmpeg `-af` chain as a
/// live MP3 stream.
fn serve_eq(stream: &mut std::net::TcpStream, url: &str, af: &str) {
    use std::io::Read as _;
    let Some(ffmpeg) = crate::silence::find_ffmpeg() else {
        return respond(stream, "503 Unavailable", &[], b"ffmpeg not found");
    };
    let mut cmd = std::process::Command::new(&ffmpeg);
    cmd.args(["-v", "error", "-i", url, "-af", af, "-f", "mp3", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return respond(stream, "502 Bad Gateway", &[], b"ffmpeg failed to start");
    };
    let Some(mut out) = child.stdout.take() else {
        let _ = child.kill();
        return respond(stream, "502 Bad Gateway", &[], b"ffmpeg failed");
    };
    let head = "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        let _ = child.kill();
        return;
    }
    let mut buf = [0u8; 16384];
    loop {
        match out.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(url_decode(v));
        }
    }
    None
}

fn respond(
    stream: &mut std::net::TcpStream,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn handle(mut stream: std::net::TcpStream) {
    let mut buf = [0u8; 8192];
    let Ok(n) = stream.read(&mut buf) else { return };
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let Some(line) = request.lines().next() else {
        return;
    };
    let mut parts = line.split_whitespace();
    let _method = parts.next();
    let Some(target) = parts.next() else { return };
    let range = request
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
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    match path {
        "/file" => {
            let Some(p) = query_param(query, "path") else {
                return respond(&mut stream, "400 Bad Request", &[], b"missing path");
            };
            serve_file(&mut stream, std::path::Path::new(&p), &range);
        }
        "/stream" => {
            let Some(u) = query_param(query, "url") else {
                return respond(&mut stream, "400 Bad Request", &[], b"missing url");
            };
            serve_stream(&mut stream, &u, &range);
        }
        "/hls" => {
            let Some(u) = query_param(query, "url") else {
                return respond(&mut stream, "400 Bad Request", &[], b"missing url");
            };
            serve_hls(&mut stream, &u);
        }
        "/eq" => {
            let (Some(u), Some(af)) = (query_param(query, "url"), query_param(query, "af")) else {
                return respond(&mut stream, "400 Bad Request", &[], b"missing url/af");
            };
            serve_eq(&mut stream, &u, &af);
        }
        _ => {
            if let Some(rest) = path.strip_prefix("/hlsseg/") {
                serve_hls_segment(&mut stream, rest);
            } else {
                respond(&mut stream, "404 Not Found", &[], b"not found");
            }
        }
    }
}

fn serve_file(stream: &mut std::net::TcpStream, path: &std::path::Path, range: &str) {
    let Ok(data) = std::fs::read(path) else {
        return respond(stream, "404 Not Found", &[], b"no such file");
    };
    let mime = mime_for(path);
    serve_bytes(stream, &data, mime, range);
}

fn serve_bytes(stream: &mut std::net::TcpStream, data: &[u8], mime: &str, range: &str) {
    if let Some((start, end)) = parse_range(range, data.len()) {
        let slice = &data[start..=end];
        respond(
            stream,
            "206 Partial Content",
            &[
                ("Content-Type", mime.to_string()),
                (
                    "Content-Range",
                    format!("bytes {start}-{end}/{}", data.len()),
                ),
                ("Accept-Ranges", "bytes".to_string()),
            ],
            slice,
        );
    } else {
        respond(
            stream,
            "200 OK",
            &[
                ("Content-Type", mime.to_string()),
                ("Accept-Ranges", "bytes".to_string()),
            ],
            data,
        );
    }
}

/// Parse a single "bytes=start-end" Range header against a
/// body length. None = serve the whole body.
pub fn parse_range(range: &str, len: usize) -> Option<(usize, usize)> {
    let spec = range.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    if start.is_empty() {
        // Suffix range: last N bytes.
        let n: usize = end.parse().ok()?;
        if n == 0 || len == 0 {
            return None;
        }
        return Some((len.saturating_sub(n), len - 1));
    }
    let start: usize = start.parse().ok()?;
    if start >= len {
        return None;
    }
    let end: usize = if end.is_empty() {
        len - 1
    } else {
        end.parse::<usize>().ok()?.min(len - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

fn serve_stream(stream: &mut std::net::TcpStream, url: &str, range: &str) {
    let req = crate::http::agent().get(url);
    let req = if range.is_empty() {
        req
    } else {
        req.header("Range", range)
    };
    match req.call() {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let mime = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let content_range = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            match resp.body_mut().read_to_vec() {
                Ok(data) => {
                    let mut headers = vec![
                        ("Content-Type", mime),
                        ("Accept-Ranges", "bytes".to_string()),
                    ];
                    if !content_range.is_empty() {
                        headers.push(("Content-Range", content_range));
                    }
                    respond(
                        stream,
                        if status == 206 {
                            "206 Partial Content"
                        } else {
                            "200 OK"
                        },
                        &headers,
                        &data,
                    );
                }
                Err(e) => respond(
                    stream,
                    "502 Bad Gateway",
                    &[],
                    format!("upstream read: {e}").as_bytes(),
                ),
            }
        }
        Err(e) => respond(
            stream,
            "502 Bad Gateway",
            &[],
            format!("upstream: {e}").as_bytes(),
        ),
    }
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/aac",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "m3u8" => "application/vnd.apple.mpegurl",
        "ts" => "video/mp2t",
        _ => "application/octet-stream",
    }
}

/// Directory holding transcoded HLS renditions, one subdir
/// per source hash.
fn hls_dir(url: &str) -> std::path::PathBuf {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    std::env::temp_dir()
        .join("blindrss-hls")
        .join(format!("{hash:x}"))
}

fn serve_hls(stream: &mut std::net::TcpStream, url: &str) {
    let dir = hls_dir(url);
    let playlist = dir.join("index.m3u8");
    if !playlist.exists() {
        let Some(ffmpeg) = crate::silence::find_ffmpeg() else {
            return respond(stream, "503 Unavailable", &[], b"ffmpeg not found");
        };
        let _ = std::fs::create_dir_all(&dir);
        // Transcode (copy when possible) to HLS segments.
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-y",
                "-i",
                url,
                "-c",
                "copy",
                "-f",
                "hls",
                "-hls_time",
                "6",
                "-hls_segment_filename",
                &dir.join("seg%05d.ts").to_string_lossy(),
                &playlist.to_string_lossy(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => return respond(stream, "502 Bad Gateway", &[], b"ffmpeg failed"),
        }
    }
    match std::fs::read(&playlist) {
        Ok(mut text) => {
            // Rewrite segment references to our segment route.
            let base = BASE.get().cloned().unwrap_or_default();
            let body = String::from_utf8_lossy(&text).to_string();
            let mut out = String::new();
            for line in body.lines() {
                if line.starts_with("seg") {
                    out.push_str(&format!(
                        "{base}/hlsseg/{}/{line}\n",
                        hls_dir(url).file_name().unwrap().to_string_lossy()
                    ));
                } else {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            text = out.into_bytes();
            respond(
                stream,
                "200 OK",
                &[("Content-Type", "application/vnd.apple.mpegurl".to_string())],
                &text,
            );
        }
        Err(_) => respond(stream, "404 Not Found", &[], b"no playlist"),
    }
}

fn serve_hls_segment(stream: &mut std::net::TcpStream, rest: &str) {
    // rest = "<hash>/<file>"
    let Some((hash, file)) = rest.split_once('/') else {
        return respond(stream, "400 Bad Request", &[], b"bad segment path");
    };
    if file.contains("..") || hash.contains("..") {
        return respond(stream, "400 Bad Request", &[], b"bad segment path");
    }
    let path = std::env::temp_dir()
        .join("blindrss-hls")
        .join(hash)
        .join(file);
    match std::fs::read(&path) {
        Ok(data) => respond(
            stream,
            "200 OK",
            &[("Content-Type", "video/mp2t".to_string())],
            &data,
        ),
        Err(_) => respond(stream, "404 Not Found", &[], b"no segment"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=2000-3000", 1000), None);
        assert_eq!(parse_range("", 1000), None);
        assert_eq!(url_decode("a%20b+c"), "a b c");
    }
}
