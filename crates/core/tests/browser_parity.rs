use blindrss_core::browser::{feed_text_from_page_source, is_challenge_page};

#[test]
fn browser_feed_response_requires_a_structural_feed() {
    let rss = "<rss version='2.0'><channel><title>News</title></channel></rss>";
    assert_eq!(feed_text_from_page_source(rss).as_deref(), Some(rss));
    let wrapped = "<html><body><pre>&lt;rss version='2.0'&gt;&lt;channel&gt;&lt;title&gt;News&lt;/title&gt;&lt;/channel&gt;&lt;/rss&gt;</pre></body></html>";
    assert!(
        feed_text_from_page_source(wrapped)
            .unwrap()
            .starts_with("<rss")
    );
    assert!(feed_text_from_page_source("<html><body>Not a feed</body></html>").is_none());
    assert!(
        feed_text_from_page_source(
            "{\"version\":\"https://jsonfeed.org/version/1.1\",\"items\":[]}"
        )
        .is_some()
    );
    assert!(
        feed_text_from_page_source(
            "{\"version\":\"https://jsonfeed.org/version/1.1\",\"items\":42}"
        )
        .is_none()
    );
}

#[test]
fn browser_challenges_cannot_be_accepted_as_article_content() {
    for marker in [
        "Just a moment...",
        "_cf_chl_opt",
        "Verify you are human",
        "Performing security verification",
    ] {
        assert!(is_challenge_page(&format!(
            "<html><body>{marker}</body></html>"
        )));
    }
    assert!(!is_challenge_page(
        "<article>A published story about the day's news.</article>"
    ));
}

#[test]
#[ignore = "requires installed Chromium; opens only an isolated managed headless profile"]
fn installed_browser_fetches_javascript_page_and_harvests_own_cookie() {
    use std::io::{Read, Write};
    use std::time::Duration;
    let directory =
        std::env::temp_dir().join(format!("blindrss-browser-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let mut config = blindrss_core::config::Config::default();
    config.set_path("browser_download_if_missing", serde_json::json!(false));
    blindrss_core::browser::configure(&config, &directory.join("config.json"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let body = format!(
                "<html><head><title>Browser fixture</title><link rel='alternate' type='application/rss+xml' href='/feed.xml'></head><body><article>{}</article><script>document.body.setAttribute('data-hydrated','yes');</script></body></html>",
                "Actual story. ".repeat(40)
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nSet-Cookie: cf_clearance=fixture; Path=/\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let response = blindrss_core::browser::fetch_page(
        &format!("http://{address}/story"),
        Duration::from_secs(20),
        None,
    )
    .unwrap();
    assert!(response.html.contains("data-hydrated=\"yes\""));
    assert!(
        std::fs::read_to_string(directory.join("site_cookies.txt"))
            .unwrap()
            .contains("cf_clearance\tfixture")
    );
    blindrss_core::browser::shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn browser_feed_validation_rejects_malformed_xml_and_html_feed_mentions() {
    for body in [
        "<rss><channel></rss>",
        "<rss><title>&unknown;</title></rss>",
        "<![CDATA[bad]]><rss/>",
        "<rss 42/>",
        "<rss/ ><rss/>",
        "<html><body><rss/></body></html>",
        "{\"version\":\"wrong\",\"items\":[]}",
    ] {
        assert!(feed_text_from_page_source(body).is_none(), "{body}");
    }
    for body in [
        "<feed xmlns='http://www.w3.org/2005/Atom'/>",
        "<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'/>",
        "<channel/>",
    ] {
        assert_eq!(feed_text_from_page_source(body).as_deref(), Some(body));
    }
}

#[test]
fn browser_rejects_invalid_targets_and_cancelled_requests_before_launch() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    assert!(
        blindrss_core::browser::fetch_page("file:///private", Duration::from_secs(1), None)
            .is_err()
    );
    assert!(blindrss_core::browser::fetch_page("http://127.0.0.1/", Duration::ZERO, None).is_err());
    let cancelled = AtomicBool::new(true);
    assert!(
        blindrss_core::browser::fetch_page(
            "http://127.0.0.1/",
            Duration::from_secs(1),
            Some(&cancelled)
        )
        .is_err()
    );
}
