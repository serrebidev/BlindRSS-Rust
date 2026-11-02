use blindrss_core::source_feeds::{
    parse_listing, parse_rumble_listing, subscription_url, youtube_uploads_url,
};
use serde_json::json;

#[test]
fn listings_do_not_consume_tracks_or_ordinary_podcast_feeds() {
    assert_eq!(
        subscription_url("https://soundcloud.com/artist").as_deref(),
        Some("https://soundcloud.com/artist")
    );
    assert!(subscription_url("https://soundcloud.com/artist/track").is_none());
    assert!(subscription_url("https://feeds.soundcloud.com/users/a/sounds.rss").is_none());
    assert_eq!(
        subscription_url("https://rumble.com/c/example?tracking=x").as_deref(),
        Some("https://rumble.com/c/example/videos")
    );
    assert!(subscription_url("https://rumble.com/v123-story.html").is_none());
    assert!(subscription_url("https://rumble.com.evil.test/c/test").is_none());
    assert!(subscription_url("https://mixcloud.com/person/show/").is_none());
    assert!(subscription_url("https://mixcloud.com/person/playlists/favorites/").is_some());
    assert_eq!(
        youtube_uploads_url("https://www.youtube.com/feeds/videos.xml?channel_id=UCabcdefghijk")
            .as_deref(),
        Some("https://www.youtube.com/playlist?list=UUabcdefghijk")
    );
}

#[test]
fn listing_dates_metadata_and_ids_survive_normalization() {
    let value = json!({"title":"Artist", "entries":[{"url":"abc123", "ie_key":"Youtube", "title":"First", "timestamp":1700000000,"channel":"Creator"},{"url":"https://www.youtube.com/watch?v=abc123", "title":"Duplicate"}]});
    let feed = parse_listing(&value, "https://www.youtube.com/playlist?list=UUtest", 60);
    assert_eq!(feed.entries.len(), 1);
    assert_eq!(
        feed.entries[0].date.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(feed.entries[0].media_url, Some(feed.entries[0].url.clone()));
    let sc = parse_listing(
        &json!({"collection":[{"kind":"track","permalink_url":"https://soundcloud.com/a/one","title":"One","created_at":"2026-10-31T01:02:03Z","description":"Notes","user":{"username":"Alice"}}]}),
        "https://soundcloud.com/a",
        20,
    );
    assert_eq!(sc.entries[0].author.as_deref(), Some("Alice"));
    assert_eq!(sc.entries[0].date.as_deref(), Some("2026-10-31T01:02:03Z"));
    assert_eq!(sc.entries[0].summary, "Notes");
}

#[test]
fn rumble_cards_keep_titles_dates_and_normalize_tracking() {
    let html = r#"<title>Channel - Rumble</title><article class="videostream"><a href="/v123-story.html?foo=1">image</a><h3 class="thumbnail__title">The story</h3><time datetime="2026-10-31T01:02:03Z"></time></article><a href="/v123-story.html">duplicate</a><a href="https://evil.test/v321.html">wrong host</a>"#;
    let feed = parse_rumble_listing(html, "https://rumble.com/c/channel/videos", 30);
    assert_eq!(feed.entries.len(), 1);
    assert_eq!(feed.entries[0].title, "The story");
    assert_eq!(feed.entries[0].url, "https://rumble.com/v123-story.html");
    assert_eq!(
        feed.entries[0].date.as_deref(),
        Some("2026-10-31T01:02:03Z")
    );
}
