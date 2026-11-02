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
    assert_eq!(
        discover_in_html(html, "https://example.com/blog/article"),
        vec![
            "https://example.com/rss?x=1&y=2",
            "https://example.com/blog/article?feed=atom"
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
