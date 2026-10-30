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
