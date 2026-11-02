use blindrss_core::feedsearch::{SearchProvider, parse_results, request_url};

#[test]
fn directory_requests_encode_user_queries() {
    let url = request_url(SearchProvider::ApplePodcasts, "science & music", 20).unwrap();
    let url = url::Url::parse(&url).unwrap();
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "term" && v == "science & music")
    );
    assert!(
        url.query_pairs()
            .any(|(k, v)| k == "media" && v == "podcast")
    );
    let url = request_url(SearchProvider::Feedsearch, "Example", 20).unwrap();
    assert!(
        url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .any(|(k, v)| k == "url" && v == "example.com")
    );
    assert!(request_url(SearchProvider::NewsBlur, "   ", 20).is_err());
}

#[test]
fn apple_results_require_real_feed_urls_and_deduplicate() {
    let json = r#"{"results":[{"collectionName":"Science","artistName":"Alice","feedUrl":"https://example.com/feed"},{"collectionName":"Duplicate","feedUrl":"https://example.com/feed"},{"collectionName":"No feed"},{"collectionName":"Unsafe","feedUrl":"file:///secret"}]}"#;
    let results = parse_results(SearchProvider::ApplePodcasts, json, 20).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Science");
    assert_eq!(results[0].description, "Alice");
    assert_eq!(results[0].url, "https://example.com/feed");
}

#[test]
fn directory_response_shapes_match_python() {
    for (provider, json, title) in [
        (
            SearchProvider::GPodder,
            r#"[{"title":"G","author":"Host","url":"https://example.com/g"}]"#,
            "G",
        ),
        (
            SearchProvider::Fyyd,
            r#"{"data":[{"title":"F","xmlURL":"https://example.com/f"}]}"#,
            "F",
        ),
        (
            SearchProvider::Podverse,
            r#"[[{"title":"P","feedUrls":[{"url":"https://example.com/p"}]}],1]"#,
            "P",
        ),
        (
            SearchProvider::Feedly,
            r#"{"results":[{"title":"L","feedId":"feed/https://example.com/l"}]}"#,
            "L",
        ),
        (
            SearchProvider::NewsBlur,
            r#"{"feeds":[{"label":"N","value":"https://example.com/n","tagline":"News","num_subscribers":3},{"id":123,"value":"123"}]}"#,
            "N",
        ),
        (
            SearchProvider::NewsBlur,
            r#"[{"label":"Old","address":"https://example.com/old"}]"#,
            "Old",
        ),
        (
            SearchProvider::Feedsearch,
            r#"[{"title":"S","url":"https://example.com/s","site_name":"Site"}]"#,
            "S",
        ),
    ] {
        let results = parse_results(provider, json, 10).unwrap();
        assert_eq!(results.len(), 1, "{provider:?}");
        assert_eq!(results[0].title, title);
    }
}

#[test]
fn directory_results_enforce_limit_and_response_validation() {
    let json = r#"[{"url":"https://example.com/1"},{"url":"https://example.com/2"}]"#;
    assert_eq!(
        parse_results(SearchProvider::GPodder, json, 1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        parse_results(SearchProvider::GPodder, json, 0)
            .unwrap()
            .is_empty()
    );
    assert!(parse_results(SearchProvider::GPodder, "not json", 10).is_err());
    assert!(parse_results(SearchProvider::NewsBlur, r#"{"error":"blocked"}"#, 10).is_err());
}

#[test]
fn social_directory_requests_and_results_follow_python() {
    for (provider, json, expected) in [
        (
            SearchProvider::Reddit,
            r#"{"data":{"children":[{"data":{"display_name":"rust","public_description":"Rust news"}}]}}"#,
            "https://www.reddit.com/r/rust/.rss",
        ),
        (
            SearchProvider::Lemmy,
            r#"{"communities":[{"community":{"name":"rust","title":"Rust","actor_id":"https://lemmy.world/c/rust"}}]}"#,
            "https://lemmy.world/feeds/c/rust.xml",
        ),
        (
            SearchProvider::Mastodon,
            r#"{"accounts":[{"url":"https://mastodon.social/@alice","display_name":"Alice"}],"hashtags":[]}"#,
            "https://mastodon.social/@alice.rss",
        ),
        (
            SearchProvider::Bluesky,
            r#"{"actors":[{"handle":"alice.bsky.social","displayName":"Alice"}]}"#,
            "https://bsky.app/profile/alice.bsky.social/rss",
        ),
        (
            SearchProvider::PieFed,
            r#"{"communities":[{"community":{"name":"rust","actor_id":"https://piefed.social/c/rust"}}]}"#,
            "https://piefed.social/community/rust/feed",
        ),
        (
            SearchProvider::Mixcloud,
            r#"{"data":[{"name":"DJ","url":"https://www.mixcloud.com/dj/"}]}"#,
            "https://www.mixcloud.com/dj/",
        ),
    ] {
        let request = request_url(provider, "science & music", 500).unwrap();
        assert!(
            url::Url::parse(&request)
                .unwrap()
                .query_pairs()
                .any(|(_, value)| value == "science & music")
        );
        let rows = parse_results(provider, json, 20).unwrap();
        assert_eq!(rows.len(), 1, "{provider:?}");
        assert_eq!(rows[0].url, expected, "{provider:?}");
    }
}

#[test]
fn grouped_sources_preserve_python_selection_and_site_gates() {
    use blindrss_core::feedsearch::{SearchGroup, selected_providers, site_target};
    assert_eq!(
        site_target("TechSpot").as_deref(),
        Some("https://techspot.com")
    );
    assert_eq!(site_target("two words"), None);
    let all = selected_providers(SearchGroup::All, "two words");
    assert!(!all.contains(&SearchProvider::Feedly));
    assert!(!all.contains(&SearchProvider::Feedsearch));
    assert!(!all.contains(&SearchProvider::Website));
    assert!(selected_providers(SearchGroup::Rss, "techspot").contains(&SearchProvider::Website));
    assert!(
        selected_providers(SearchGroup::All, "example.org").contains(&SearchProvider::Feedsearch)
    );
    assert!(
        !selected_providers(SearchGroup::Podcasts, "music").contains(&SearchProvider::NewsBlur)
    );
}

#[test]
fn social_response_entries_cannot_inject_urls_or_paths() {
    assert!(
        parse_results(
            SearchProvider::Reddit,
            r#"{"data":{"children":[{"data":{"display_name":"../private?x"}}]}}"#,
            20
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        parse_results(
            SearchProvider::Mastodon,
            r#"{"accounts":[{"url":"file:///tmp/x"}],"hashtags":[]}"#,
            20
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        parse_results(
            SearchProvider::Bluesky,
            r#"{"actors":[{"handle":"evil/path?x"}]}"#,
            20
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn html_directories_catalogue_and_localized_news_use_real_addresses() {
    use blindrss_core::config::Config;
    use blindrss_core::feedsearch::{google_news_results, parse_catalogue, parse_html_results};
    let rows = parse_html_results(SearchProvider::GroupsIo, r#"<table><tr><td><a href="/g/rust">Rust group</a></td></tr><tr><a href="https://evil.test/g/other">Bad</a></tr></table>"#, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].url, "https://groups.io/g/rust/rss");
    let rows = parse_html_results(SearchProvider::Feedspot, r#"<div class="feed_heading"><a href="https://site.test">Site</a></div><a class="ext extdomain" href="https://site.test">website</a><a class="ext" href="https://site.test/feed">RSS</a><a class="ext" href="https://feedspot.com/private">Private</a>"#, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Site");
    let rows = parse_catalogue(r#"<opml version="2.0"><head/><body><outline text="Tech"><outline text="Rust" xmlUrl="https://example.com/feed"/></outline></body></opml>"#, "Canada").unwrap();
    assert!(rows[0].description.contains("Canada Tech"));
    let mut cfg = Config::default();
    cfg.set_path("discovery_country", serde_json::json!("GB"));
    cfg.set_path("discovery_language", serde_json::json!("en"));
    cfg.set_path("discovery_city", serde_json::json!("London"));
    let rows = google_news_results("science", 20, &cfg).unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|row| {
        url::Url::parse(&row.url)
            .unwrap()
            .query_pairs()
            .any(|(k, v)| k == "gl" && v == "GB")
    }));
}

#[test]
fn youtube_results_are_channel_or_playlist_subscriptions() {
    use blindrss_core::feedsearch::parse_youtube_results;
    let rows = parse_youtube_results(r#"{"entries":[{"title":"Video","channel":"Channel","channel_id":"UC123","view_count":10},{"channel_id":"UC123"},{"url":"https://www.youtube.com/playlist?list=PL123","title":"Playlist","id":"PL123"}]}"#, false, 20).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Channel");
    let rows = parse_youtube_results(r#"{"entries":[{"url":"https://www.youtube.com/playlist?list=PL123","title":"Playlist","id":"PL123"}]}"#, true, 20).unwrap();
    assert!(rows[0].url.ends_with("playlist_id=PL123"));
}

#[test]
fn piefed_user_results_and_keyless_newsapis_fail_safely() {
    let rows = parse_results(SearchProvider::PieFed, r#"{"communities":[],"users":[{"person":{"user_name":"alice","actor_id":"https://lemmy.world/u/alice"}}]}"#, 20).unwrap();
    assert_eq!(rows[0].url, "https://lemmy.world/feeds/u/alice.xml");
    let error =
        blindrss_core::feedsearch::search(SearchProvider::NewsApis, "science", 10).unwrap_err();
    assert!(error.contains("Configure"));
}
