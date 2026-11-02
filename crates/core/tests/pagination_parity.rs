use blindrss_core::article_html::render_rich_article;
use blindrss_core::extract::{ExtractOutcome, fetch_full_text, find_next_page};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[test]
fn next_page_requires_article_continuation_on_same_origin() {
    let base = "https://example.com/review/page1";
    assert_eq!(
        find_next_page(
            "<link rel='alternate next' href='../page2?x=1&amp;y=2'>",
            base
        ),
        Some("https://example.com/page2?x=1&y=2".into())
    );
    assert_eq!(
        find_next_page("<a href='/page2' aria-label='Next page'>Continue</a>", base),
        Some("https://example.com/page2".into())
    );
    for html in [
        "<a href='/other' class='next-story'>Next</a>",
        "<a href='/other' data-track='next-article'>Next</a>",
        "<a rel='next' href='https://other.example/page2'>Next</a>",
        "<a rel='next' href='javascript:alert(1)'>Next</a>",
        "<a rel='next' href='#comments'>Next</a>",
    ] {
        assert_eq!(find_next_page(html, base), None, "{html}");
    }
    assert_eq!(
        find_next_page(
            "<link rel='next' href='/other'>",
            "https://www.neowin.net/story"
        ),
        None
    );
}

fn server(loop_back: bool, metered: bool) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut bytes = [0u8; 4096];
            let n = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]);
            let path = request.split_whitespace().nth(1).unwrap().to_string();
            seen.lock().unwrap().push(path.clone());
            let number = path
                .trim_start_matches("/page")
                .parse::<usize>()
                .unwrap_or(1);
            let next = if loop_back && number == 2 {
                1
            } else {
                number + 1
            };
            let prose = format!("This is the distinctive content of page {number}. We report on the detailed findings and explain the evidence for readers. The researchers have confirmed these findings in an independent investigation, with careful measurements and clear supporting facts. ").repeat(8);
            let meter = if metered {
                "<aside>To read as many articles as you like, subscribe.</aside>"
            } else {
                ""
            };
            let body = format!(
                "<html><head><title>Review</title><link rel='next' href='/page{next}'></head><body><article><p>{prose}</p></article>{meter}</body></html>"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{address}/page1"), requests)
}

#[test]
fn text_follows_six_pages_and_preserves_first_metadata() {
    let (url, requests) = server(false, false);
    let (outcome, first_html) = fetch_full_text(&url, "", None, "");
    let ExtractOutcome::Article { text, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(text.contains("distinctive content of page 6"), "{text}");
    assert!(!text.contains("distinctive content of page 7"));
    assert!(
        first_html
            .unwrap()
            .contains("distinctive content of page 1")
    );
    assert_eq!(requests.lock().unwrap().len(), 6);
}

#[test]
fn rich_reader_stops_at_pagination_loop() {
    let (url, requests) = server(true, false);
    let html = render_rich_article(&url, "", "Review", "", "", "", "", "", "en").unwrap();
    assert!(html.contains("distinctive content of page 2"), "{html}");
    assert_eq!(requests.lock().unwrap().len(), 2);
}

#[test]
fn rich_metered_notice_disappears_when_full_feed_content_wins() {
    let (url, _) = server(true, true);
    let html = render_rich_article(&url, "", "Review", "", "", "", "", "", "en").unwrap();
    assert!(html.contains("Only the free preview"), "{html}");
    let complete = format!(
        "<p>{}</p>",
        "Complete syndication with all details and independent reporting. ".repeat(300)
    );
    let html = render_rich_article(&url, &complete, "Review", "", "", "", "", "", "en").unwrap();
    assert!(html.contains("Complete syndication"));
    assert!(!html.contains("Only the free preview"));
}

#[test]
fn full_text_has_one_metered_notice_after_merging_pages() {
    let (url, _) = server(true, true);
    let (outcome, _) = fetch_full_text(&url, "", None, "");
    let ExtractOutcome::Article { text, .. } = outcome else {
        panic!("{outcome:?}")
    };
    let notice = blindrss_core::extract::metered_preview_notice();
    assert!(text.contains("distinctive content of page 2"), "{text}");
    assert_eq!(text.matches(notice).count(), 1, "{text}");
    assert!(text.ends_with(notice), "{text}");
}
