use blindrss_core::{Db, feeds};
use std::io::{Read, Write};

#[test]
fn conditional_refresh_reuses_validated_body_and_manual_refresh_bypasses_cache() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for n in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let size = stream.read(&mut bytes).unwrap();
            requests.push(String::from_utf8_lossy(&bytes[..size]).to_lowercase());
            let body = "<rss version='2.0'><channel><title>Feed</title><item><guid>one</guid><title>Article</title></item></channel></rss>";
            let reply = if n == 1 {
                "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".into()
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nETag: \"version1\"\r\nLast-Modified: Wed, 21 Oct 2026 07:28:00 GMT\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            stream.write_all(reply.as_bytes()).unwrap();
        }
        requests
    });
    let path = std::env::temp_dir().join(format!("blindrss-cache-{}.db", uuid::Uuid::new_v4()));
    let db = Db::open_or_create(&path).unwrap();
    let url = format!("http://{address}/feed");
    let first = feeds::fetch_with_cache(&db, &url, false).unwrap();
    let cached = feeds::fetch_with_cache(&db, &url, false).unwrap();
    assert_eq!(first, cached);
    assert_eq!(feeds::fetch_with_cache(&db, &url, true).unwrap(), first);
    let requests = server.join().unwrap();
    assert!(requests[1].contains("if-none-match: \"version1\""));
    assert!(requests[1].contains("if-modified-since:"));
    assert!(!requests[2].contains("if-none-match:"));
    drop(db);
    std::fs::remove_file(path).unwrap();
}
