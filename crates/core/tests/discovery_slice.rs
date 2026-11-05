//! Discovery tests: social URL transforms (mirroring the Python
//! test_discovery_bluesky_piefed_search expectations) and HTML
//! alternate-link extraction.

use blindrss_core::discovery::{discover_in_html, social_feed_url};

#[test]
fn social_transforms() {
    assert_eq!(
        social_feed_url("https://bsky.app/profile/python.org").as_deref(),
        Some("https://bsky.app/profile/python.org/rss")
    );
    assert_eq!(
        social_feed_url("https://piefed.social/u/rimu").as_deref(),
        Some("https://piefed.social/u/rimu/feed")
    );
    assert_eq!(
        social_feed_url("https://piefed.social/c/flask").as_deref(),
        Some("https://piefed.social/community/flask/feed")
    );
    assert_eq!(
        social_feed_url("https://mastodon.social/@alice").as_deref(),
        Some("https://mastodon.social/@alice.rss")
    );
    assert_eq!(
        social_feed_url("https://mastodon.social/tags/rust").as_deref(),
        Some("https://mastodon.social/tags/rust.rss")
    );
    assert_eq!(
        social_feed_url("https://www.reddit.com/r/rust").as_deref(),
        Some("https://www.reddit.com/r/rust/.rss")
    );
    assert_eq!(
        social_feed_url("https://lemmy.world/c/rust").as_deref(),
        Some("https://lemmy.world/feeds/c/rust.xml")
    );
    assert_eq!(
        social_feed_url("https://lemmy.world/u/bob").as_deref(),
        Some("https://lemmy.world/feeds/u/bob.xml")
    );
    assert_eq!(
        social_feed_url("https://www.youtube.com/channel/UCabcdefghij").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?channel_id=UCabcdefghij")
    );
    assert_eq!(
        social_feed_url("https://www.youtube.com/playlist?list=PL123abc").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?playlist_id=PL123abc")
    );
    assert_eq!(social_feed_url("https://example.com/blog"), None);
    assert_eq!(social_feed_url("not a url"), None);
}

#[test]
fn html_alternate_discovery() {
    let html = r#"<html><head>
      <link rel="stylesheet" href="/style.css">
      <link rel="alternate" type="application/rss+xml" title="RSS" href="/feed.xml">
      <link rel="alternate" type="application/atom+xml" href="https://example.com/atom.xml">
      <link rel="alternate" type="application/json" href="/feed.json">
    </head><body>hi</body></html>"#;
    let found = discover_in_html(html, "https://example.com/blog/post");
    assert_eq!(
        found,
        vec![
            "https://example.com/feed.xml".to_string(),
            "https://example.com/atom.xml".to_string()
        ]
    );
}

#[test]
fn youtube_user_and_playlist_links_resolve_without_page_fetch() {
    assert_eq!(
        social_feed_url("https://www.youtube.com/user/TestUser").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?user=TestUser")
    );
    assert_eq!(
        social_feed_url("https://www.youtube.com/watch?v=abc&list=PL123abc").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?playlist_id=PL123abc")
    );
    assert_eq!(
        social_feed_url("https://notyoutube.com/channel/UC123"),
        None
    );
    assert_eq!(social_feed_url("https://notbsky.app/profile/alice"), None);
}

#[test]
fn alternate_links_honor_attribute_boundaries_and_url_resolution() {
    let html = r#"İ<LINK REL = 'alternate' TYPE = 'application/rss+xml' HREF = '../rss?x=1&amp;y=2'>
      <link rel='alternate' type='application/atom+xml' data-href='/wrong' href='?feed=atom'>
      <link rel='alternate' type='application/rss+xml' href='javascript:alert(1)'>
      <link rel='notalternate' type='application/rss+xml' href='/also-wrong'>
      <linkage rel='alternate' type='application/rss+xml' href='/fake-tag'>"#;
    // Attribute boundaries and URL resolution as before;
    // the ORDER is Audit 8 F08's ranked enumeration: the
    // candidate whose path shares the page's tokens
    // ("blog"/"article") outranks the bare /rss, exactly
    // like Python's `_alternate_feed_candidates`.
    assert_eq!(
        discover_in_html(html, "https://example.com/blog/article"),
        vec![
            "https://example.com/blog/article?feed=atom",
            "https://example.com/rss?x=1&y=2"
        ]
    );
}

#[test]
fn website_scan_probes_common_feed_paths_at_page_and_site_root() {
    let candidates =
        blindrss_core::discovery::common_feed_candidates("https://example.com/blog?sort=new#top");
    assert!(candidates.contains(&"https://example.com/blog/backend.xml".to_string()));
    assert!(candidates.contains(&"https://example.com/backend.xml".to_string()));
    assert!(candidates.contains(&"https://example.com/blog?feed=rss2".to_string()));
    assert!(
        !candidates
            .iter()
            .any(|url| url.contains("sort=new") || url.contains('#'))
    );
    assert_eq!(
        candidates
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        candidates.len()
    );
    assert!(blindrss_core::discovery::common_feed_candidates("file:///tmp/test").is_empty());
}

// ---------------------------------------------------------------------------
// Audit 8 Task 26 — discovery/search regressions
// ---------------------------------------------------------------------------

use blindrss_core::discovery::{
    anchor_feed_urls, body_looks_like_feed, detect_page_feeds, looks_like_feed_url,
    openrss_feed_url, ytdlp_feed_url,
};

#[test]
fn ranked_alternates_put_page_feed_before_comment_feed() {
    // F08: the comment feed is FIRST in the document, but
    // Python's ranking demotes it below the page's own feed.
    let html = r#"<html><head>
      <link rel="alternate" type="application/rss+xml" title="Comments Feed" href="/tech-news/comments/feed/">
      <link rel="alternate" type="application/rss+xml" title="Tech News Feed" href="/tech-news/feed/">
    </head><body></body></html>"#;
    let found = discover_in_html(html, "https://example.com/tech-news/");
    assert_eq!(
        found,
        vec![
            "https://example.com/tech-news/feed/",
            "https://example.com/tech-news/comments/feed/"
        ]
    );
}

#[test]
fn alternates_resolve_against_the_effective_url() {
    // F09: relative links resolve against the URL the
    // fetch ENDED at (post-redirect), here a different
    // directory; absolute cross-host links pass through.
    let html = r#"<html><head>
      <link rel="alternate" type="application/rss+xml" title="RSS" href="../feed.xml">
      <link rel="alternate" type="application/atom+xml" title="Atom" href="https://cdn.other.test/atom.xml">
    </head></html>"#;
    let found = discover_in_html(html, "https://example.com/new/dir/page");
    assert!(found.contains(&"https://example.com/new/feed.xml".to_string()));
    assert!(found.contains(&"https://cdn.other.test/atom.xml".to_string()));
}

#[test]
fn anchor_candidates_cover_feed_hrefs() {
    // F08: plain anchors are a discovery source too.
    let html = r#"<html><body>
      <a href="/podcast.xml">Podcast feed</a>
      <a href="https://example.com/about">About</a>
      <a href="/blog/rss">RSS</a>
    </body></html>"#;
    let found = anchor_feed_urls(html, "https://example.com/blog/post");
    assert_eq!(
        found,
        vec![
            "https://example.com/podcast.xml",
            "https://example.com/blog/rss"
        ]
    );
}

#[test]
fn feed_url_and_body_shape_predicates() {
    assert!(looks_like_feed_url("https://example.com/blog/feed/"));
    assert!(looks_like_feed_url("https://example.com/?feed=rss2"));
    assert!(looks_like_feed_url("https://example.com/show?format=atom"));
    assert!(!looks_like_feed_url("https://example.com/blog/post"));
    assert!(!looks_like_feed_url("ftp://example.com/feed.xml"));

    assert!(body_looks_like_feed(
        r#"{"version": "https://jsonfeed.org/version/1.1", "title": "T", "items": []}"#,
        "application/json"
    ));
    assert!(body_looks_like_feed(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>T</title></channel></rss>"#,
        "application/rss+xml"
    ));
    assert!(body_looks_like_feed(
        r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title></feed>"#,
        ""
    ));
    assert!(!body_looks_like_feed(
        "<html><body>post</body></html>",
        "text/html"
    ));
    assert!(!body_looks_like_feed("{}", "application/json"));
}

#[test]
fn ytdlp_feed_url_direct_shapes_need_no_network() {
    // F07: the direct conversions are pure.
    assert_eq!(
        ytdlp_feed_url("https://www.youtube.com/channel/UCabcdefghij/videos").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?channel_id=UCabcdefghij")
    );
    assert_eq!(
        ytdlp_feed_url("https://www.youtube.com/user/SomeUser").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?user=SomeUser")
    );
    assert_eq!(
        ytdlp_feed_url("https://www.youtube.com/playlist?list=PL123abc").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?playlist_id=PL123abc")
    );
    assert_eq!(
        ytdlp_feed_url("https://www.youtube.com/feeds/videos.xml?channel_id=UCx").as_deref(),
        Some("https://www.youtube.com/feeds/videos.xml?channel_id=UCx")
    );
    // Search pages, non-YouTube pages and junk have no
    // native feed.
    assert_eq!(
        ytdlp_feed_url("https://www.youtube.com/results?search_query=rust"),
        None
    );
    assert_eq!(ytdlp_feed_url("https://vimeo.com/12345"), None);
    assert_eq!(ytdlp_feed_url(""), None);
}

#[test]
fn openrss_rejects_self_and_junk_without_network() {
    assert_eq!(
        openrss_feed_url("https://openrss.org/feed/example.com/blog"),
        None
    );
    assert_eq!(openrss_feed_url("not a url"), None);
    assert_eq!(openrss_feed_url("ftp://example.com/blog"), None);
}

// --- Detect Feeds on Page against a loopback server (F10) ---

/// One loopback route: (status, headers, body).
type LoopbackRoute = (u16, Vec<(String, String)>, String);

fn loopback_server(routes: std::collections::HashMap<String, LoopbackRoute>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            use std::io::{Read, Write};
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .split('?')
                .next()
                .unwrap_or("/")
                .to_string();
            let (status, headers, body) =
                routes
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, Vec::new(), "not found".to_string()));
            let mut response = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n", body.len());
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str("\r\n");
            response.push_str(&body);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn detect_page_feeds_follows_redirect_base_and_wide_types() {
    // F09 + F10: /old 301s to /new/dir/page; the JSON feed
    // link is relative to the EFFECTIVE URL, and plain
    // application/json counts in the detect type set.
    let mut routes = std::collections::HashMap::new();
    routes.insert(
        "/old".to_string(),
        (
            301,
            vec![("Location".to_string(), "/new/dir/page".to_string())],
            String::new(),
        ),
    );
    routes.insert(
        "/new/dir/page".to_string(),
        (
            200,
            vec![("Content-Type".to_string(), "text/html".to_string())],
            r#"<html><head><link rel="alternate" type="application/json" title="JSON Feed" href="../feed.json"></head><body></body></html>"#.to_string(),
        ),
    );
    let base = loopback_server(routes);
    let found = detect_page_feeds(&format!("{base}/old")).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].title, "JSON Feed");
    assert_eq!(found[0].url, format!("{base}/new/feed.json"));
}

#[test]
fn detect_page_feeds_finds_vbulletin_anchors() {
    // F08/F10: vBulletin external.php anchors prove the
    // shape; the all-forums feed comes first, then one per
    // visible forum.
    let mut routes = std::collections::HashMap::new();
    routes.insert(
        "/forum".to_string(),
        (
            200,
            vec![("Content-Type".to_string(), "text/html".to_string())],
            r#"<html><body>
              <a href="/external.php?type=RSS2">RSS</a>
              <a href="/forums/12-general-chat">General Chat</a>
              <a href="/forums/9-news">News</a>
            </body></html>"#
                .to_string(),
        ),
    );
    let base = loopback_server(routes);
    let found = detect_page_feeds(&format!("{base}/forum")).unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].title, "All forums");
    assert_eq!(found[0].url, format!("{base}/external.php?type=RSS2"));
    assert_eq!(found[1].title, "General Chat");
    assert_eq!(
        found[1].url,
        format!("{base}/external.php?type=RSS2&forumids=12")
    );
    assert_eq!(
        found[2].url,
        format!("{base}/external.php?type=RSS2&forumids=9")
    );
}

#[test]
fn detect_page_feeds_reports_http_errors() {
    let base = loopback_server(std::collections::HashMap::new());
    let error = detect_page_feeds(&format!("{base}/missing")).unwrap_err();
    assert!(!error.is_challenge);
    assert!(error.message.contains("404"), "message: {}", error.message);
}

#[test]
fn detect_page_feeds_returns_feed_documents_as_themselves() {
    let mut routes = std::collections::HashMap::new();
    routes.insert(
        "/feed.xml".to_string(),
        (
            200,
            vec![(
                "Content-Type".to_string(),
                "application/rss+xml".to_string(),
            )],
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>T</title></channel></rss>"#
                .to_string(),
        ),
    );
    let base = loopback_server(routes);
    let found = detect_page_feeds(&format!("{base}/feed.xml")).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].url, format!("{base}/feed.xml"));
    assert_eq!(found[0].title, "");
}
