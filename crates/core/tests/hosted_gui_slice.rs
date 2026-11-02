//! Gap 2 (hosted GUI mutation completeness): the remote calls
//! the GUI category/favorite paths rely on — GReader label
//! rename/disable (category rename/delete for the GReader
//! family) and the Miniflux category creation the New Category
//! action mirrors. Fixture HTTP servers only; no live accounts.

use blindrss_core::providers::{GReaderClient, GReaderService, MinifluxClient};
use std::io::{Read, Write};

fn server(replies: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Read the full request: headers, then exactly
            // Content-Length body bytes (a single read can
            // return the headers alone).
            let mut data = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let head_end = data
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4);
                if let Some(end) = head_end {
                    let head = String::from_utf8_lossy(&data[..end]).into_owned();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= end + len {
                        break;
                    }
                }
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => data.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            requests.push(String::from_utf8_lossy(&data).into_owned());
            write!(stream, "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (url, worker)
}

fn body_of(request: &str) -> &str {
    request.split("\r\n\r\n").nth(1).unwrap_or("")
}

#[test]
fn greader_label_rename_and_disable_hit_python_endpoints() {
    let (url, server) = server(vec![(200, "OK"), (200, "OK")]);
    let mut client = GReaderClient::with_oauth(GReaderService::Inoreader, "test-token");
    client.api_base = url.clone();
    client.rename_label_remote("Tech", "Tech News").unwrap();
    client.disable_label_remote("Tech News").unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].starts_with("POST /rename-tag "),
        "{}",
        requests[0]
    );
    let rename_body = body_of(&requests[0]);
    assert!(
        rename_body.contains("s=user%2F-%2Flabel%2FTech"),
        "{rename_body}"
    );
    assert!(
        rename_body.contains("dest=user%2F-%2Flabel%2FTech+News"),
        "{rename_body}"
    );
    assert!(
        requests[1].starts_with("POST /disable-tag "),
        "{}",
        requests[1]
    );
    let disable_body = body_of(&requests[1]);
    assert!(
        disable_body.contains("s=user%2F-%2Flabel%2FTech+News"),
        "{disable_body}"
    );
    // The OAuth bearer reaches the server; the token itself is
    // never part of the form body.
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer test-token")
    );
    assert!(!disable_body.contains("test-token"));
}

#[test]
fn greader_label_ops_surface_http_failure() {
    let (url, server) = server(vec![(500, "{}")]);
    let mut client = GReaderClient::with_oauth(GReaderService::Inoreader, "test-token");
    client.api_base = url;
    assert!(client.rename_label_remote("Tech", "Tech News").is_err());
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn miniflux_category_create_posts_the_leaf_title() {
    let (url, server) = server(vec![(201, r#"{"id": 7, "title": "Podcasts"}"#)]);
    let client = MinifluxClient::new(&url, "test-key");
    let id = client.create_category_remote("Podcasts").unwrap();
    assert_eq!(id, 7);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("POST /v1/categories "),
        "{}",
        requests[0]
    );
    assert!(body_of(&requests[0]).contains(r#""title":"Podcasts""#));
}
