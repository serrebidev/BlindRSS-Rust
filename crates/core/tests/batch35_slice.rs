//! Batch 35: source-feed family — classifiers, subscription
//! normalization, listing HTML parsers, uuid5 id shaping, and the
//! new Db helpers (article counts, YouTube history state, hosted
//! article media). Network fetchers are not exercised (offline suite).

use blindrss_core::discovery::social_feed_url;
use blindrss_core::sources::*;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", std::process::id(), nanos, n)
}

fn temp_db() -> (std::path::PathBuf, blindrss_core::db::Db) {
    let dir = std::env::temp_dir().join(format!("blindrss-b35-{}", unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rss.db");
    let db = blindrss_core::db::Db::open_or_create(&path).unwrap();
    (path, db)
}

#[test]
fn classify_listing_sources() {
    assert_eq!(
        classify("https://odysee.com/@SomeChannel:5"),
        Some(SourceKind::Odysee)
    );
    // A native Odysee RSS export stays a plain feed.
    assert_eq!(classify("https://odysee.com/rss.xml"), None);
    assert_eq!(
        classify("https://rumble.com/c/SomeChannel"),
        Some(SourceKind::Rumble)
    );
    assert_eq!(classify("https://rumble.com/feed.xml"), None);
    assert_eq!(
        classify("https://www.youtube.com/results?search_query=lofi"),
        Some(SourceKind::YoutubeSearch)
    );
    assert_eq!(
        classify("https://soundcloud.com/someartist"),
        Some(SourceKind::SoundCloud)
    );
    assert_eq!(
        classify("https://soundcloud.com/someartist/sets/mix"),
        Some(SourceKind::SoundCloud)
    );
    // A single SoundCloud track is not a listing.
    assert_eq!(classify("https://soundcloud.com/someartist/a-track"), None);
    assert_eq!(
        classify("https://www.mixcloud.com/someuser/"),
        Some(SourceKind::Mixcloud)
    );
    assert_eq!(
        classify("https://groups.google.com/g/nvda-devel"),
        Some(SourceKind::GoogleGroup)
    );
    // A thread URL is not a subscription.
    assert_eq!(
        classify("https://groups.google.com/g/nvda-devel/c/abc123"),
        None
    );
    assert_eq!(
        classify("https://groups.io/g/testgroup/search?q=hello"),
        Some(SourceKind::GroupsIoSearch)
    );
    // A Groups.io group landing is native RSS, not the search source.
    assert_eq!(classify("https://groups.io/g/testgroup"), None);
    assert_eq!(classify("https://example.com/feed.xml"), None);
}

#[test]
fn subscription_normalization() {
    // Rumble channel pages take the /videos listing view.
    assert_eq!(
        normalize_rumble_feed_url("https://rumble.com/c/Foo"),
        "https://rumble.com/c/Foo/videos"
    );
    assert_eq!(
        normalize_rumble_feed_url("https://rumble.com/search/video?q=x"),
        "https://rumble.com/search/video?q=x&sort=date"
    );
    // Odysee tracking parameters are stripped.
    assert_eq!(
        normalize_odysee_url("https://odysee.com/@chan:1/video:2?r=abc#t=3"),
        "https://odysee.com/@chan:1/video:2"
    );
    // Groups.io landing -> native RSS; search stays synthetic.
    assert_eq!(
        social_feed_url("https://groups.io/g/testgroup").as_deref(),
        Some("https://groups.io/g/testgroup/rss")
    );
    assert_eq!(
        social_feed_url("https://groups.io/g/testgroup/search?q=x").as_deref(),
        Some("https://groups.io/g/testgroup/search?q=x")
    );
    // Google Groups address + page forms.
    assert_eq!(
        google_group_subscription_url("nvda-devel@googlegroups.com").as_deref(),
        Some("https://groups.google.com/g/nvda-devel")
    );
    assert_eq!(
        social_feed_url("https://groups.google.com/g/nvda-devel").as_deref(),
        Some("https://groups.google.com/g/nvda-devel")
    );
    // YouTube search subscribes as-is.
    assert_eq!(
        social_feed_url("https://www.youtube.com/results?search_query=jazz").as_deref(),
        Some("https://www.youtube.com/results?search_query=jazz")
    );
    // Channel id extraction for the RSS fallback / history backfill.
    assert_eq!(
        youtube_channel_id(
            "https://www.youtube.com/feeds/videos.xml?channel_id=UC1234567890abcdefghijkl"
        )
        .as_deref(),
        Some("UC1234567890abcdefghijkl")
    );
    assert_eq!(
        youtube_channel_id("https://www.youtube.com/feeds/videos.xml?channel_id=XX123"),
        None
    );
}

#[test]
fn uuid5_matches_python() {
    // Computed with CPython uuid.uuid5(NAMESPACE_URL, seed).
    assert_eq!(
        uuid5_url("blindrss:youtube-search:feed1:https://www.youtube.com/watch?v=abc123"),
        "f756a2e4-a3bf-588c-b024-7acf37f0dd0f"
    );
    assert_eq!(
        uuid5_url("blindrss:groups-io-search:f2:https://groups.io/g/test/topic/123"),
        "20a7fbac-e6a1-559d-a962-55a5fe030b7c"
    );
}

#[test]
fn rumble_listing_parse() {
    let html = r#"<html><head><title>Chan Videos</title></head><body>
      <div class="videostream thumbnail__grid--item">
        <a href="/v123abc-title.html"><h3 class="thumbnail__title">First Video</h3></a>
        <time datetime="2026-10-01T12:00:00Z"></time>
      </div>
      <div class="videostream">
        <a href="/v456def-other.html"><h3 class="thumbnail__title">Second</h3></a>
      </div>
      <a href="/videos">not a video</a>
    </body></html>"#;
    let items = parse_rumble_listing(html, "https://rumble.com");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].url, "https://rumble.com/v123abc-title.html");
    assert_eq!(items[0].title, "First Video");
    assert_eq!(items[0].published.as_deref(), Some("2026-10-01T12:00:00Z"));
    assert_eq!(items[1].title, "Second");
}

#[test]
fn google_group_items_parse() {
    let html = r#"<html><head><title>My Group - Google Groups</title></head><body>
      <div data-rowid="conv1">
        <a href="./g/mygroup/c/conv1"><span class="o1DPKc">Hello World Thread</span></a>
        <span class="z0zUgf">Alice</span><span class="z0zUgf">Bob</span>
        <span class="tRlaM">Nov 1</span>
        <span class="WzoK">A snippet of the thread</span>
      </div>
      <div data-rowid="conv2">
        <a href="./g/mygroup/c/conv2"><span class="t17a0d">Second Thread</span></a>
        <span class="z0zUgf">Carol</span>
      </div>
    </body></html>"#;
    let items = parse_google_group_items(html, 30);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "conv1");
    assert_eq!(items[0].title, "Hello World Thread");
    assert_eq!(items[0].author.as_deref(), Some("Alice, Bob"));
    assert_eq!(items[0].url, "https://groups.google.com/g/mygroup/c/conv1");
    assert_eq!(items[0].content, "A snippet of the thread");
    assert_eq!(items[1].title, "Second Thread");
}

#[test]
fn groups_io_search_page_parse() {
    // DisplayShortTime takes Unix NANOSECONDS: 1762000000 s below.
    let html = r#"<html><body><table>
      <tr><td id="12345">
        <div class="subject"><a href="/g/testgroup/topic/release_notes/99887766">Release Notes Topic</a></div>
        <div class="truncate-two-lines">A short snippet</div>
        <span class="user-chip-name">Dana</span>
        <script>DisplayShortTime(1762000000000000000)</script>
      </td></tr>
      <tr><td id="12346">
        <div class="subject"><a href="/g/testgroup/topic/11223344">Plain Topic</a></div>
      </td></tr>
    </table></body></html>"#;
    let items = parse_groups_io_search_page(html, "https://groups.io/g/testgroup/search?q=x");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].url, "https://groups.io/g/testgroup/topic/99887766");
    assert_eq!(items[0].title, "Release Notes Topic");
    assert_eq!(items[0].author.as_deref(), Some("Dana"));
    assert_eq!(items[0].content, "A short snippet");
    assert_eq!(items[0].published.as_deref(), Some("2025-11-01T12:26:40Z"));
    assert_eq!(items[1].url, "https://groups.io/g/testgroup/topic/11223344");
}

#[test]
fn listing_entries_shape() {
    let items = vec![
        SourceItem {
            id: "x1".into(),
            url: "https://example.com/a".into(),
            title: "Dated".into(),
            author: Some("Ann".into()),
            published: Some("2026-10-05T00:00:00Z".into()),
            content: "body".into(),
            media_url: None,
            media_type: None,
        },
        SourceItem {
            id: "x2".into(),
            url: "https://example.com/b".into(),
            title: "Undated".into(),
            author: None,
            published: None,
            content: String::new(),
            media_url: None,
            media_type: None,
        },
        SourceItem {
            id: "x3".into(),
            url: "https://example.com/c".into(),
            title: "DisplayDate".into(),
            author: None,
            published: Some("Nov 1".into()),
            content: String::new(),
            media_url: None,
            media_type: None,
        },
    ];
    let entries = items_to_entries(items);
    assert!(entries.iter().all(|entry| entry.tags.is_empty()));
    assert_eq!(entries[0].date.as_deref(), Some("2026-10-05T00:00:00Z"));
    // Undated and display-string dates fall back to a current stamp,
    // never a year-0001 sort position.
    assert!(entries[1].date.as_deref().unwrap_or("").starts_with("20"));
    assert!(entries[2].date.as_deref().unwrap_or("").starts_with("20"));
}

#[test]
fn db_helpers_roundtrip() {
    let (_path, db) = temp_db();
    assert_eq!(db.article_count_for_feed("nope").unwrap(), 0);
    assert_eq!(db.youtube_history_state("f1").unwrap(), (0, 0));
    db.set_youtube_history_state("f1", 2, 12345).unwrap();
    assert_eq!(db.youtube_history_state("f1").unwrap(), (2, 12345));
    assert!(db.hosted_article_media("k").unwrap().is_none());
    db.set_hosted_article_media("k", Some("https://x/a.mp3"), Some("audio/mpeg"), 999)
        .unwrap();
    let (mu, mt, checked) = db.hosted_article_media("k").unwrap().unwrap();
    assert_eq!(mu.as_deref(), Some("https://x/a.mp3"));
    assert_eq!(mt.as_deref(), Some("audio/mpeg"));
    assert_eq!(checked, 999);
    // A recorded miss keeps its row with NULL media.
    db.set_hosted_article_media("miss", None, None, 5).unwrap();
    let (mu, _mt, checked) = db.hosted_article_media("miss").unwrap().unwrap();
    assert!(mu.is_none());
    assert_eq!(checked, 5);
}

#[test]
fn npr_and_sky_url_shapes() {
    assert!(is_npr_url(
        "https://www.npr.org/2026/11/01/nx-s1-12345/story"
    ));
    assert!(!is_npr_url("https://example.com/npr"));
    assert!(is_sky_news_url("https://news.sky.com/story/x-123"));
    assert!(!is_sky_news_url("https://sky.com/story/x-123"));
    assert_eq!(
        soundcloud_listing_kind("https://soundcloud.com/artist/sets/deep-mix"),
        "playlist"
    );
    assert_eq!(soundcloud_listing_kind("https://soundcloud.com/you"), "");
    let (kind, key) = mixcloud_listing("https://www.mixcloud.com/user/playlists/faves/").unwrap();
    assert_eq!(kind, "playlist");
    assert_eq!(key, "/user/playlists/faves/");
}
