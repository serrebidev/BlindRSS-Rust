use blindrss_core::forum_sources::{
    native_feed_url, parse_google_group, parse_groups_search, subscription_url,
};

#[test]
fn normalizes_forum_subscriptions_without_losing_search() {
    assert_eq!(
        subscription_url("mailto:access@googlegroups.com").as_deref(),
        Some("https://groups.google.com/g/access")
    );
    assert_eq!(
        subscription_url("https://groups.google.com/forum/#!forum/access").as_deref(),
        Some("https://groups.google.com/g/access")
    );
    assert_eq!(
        subscription_url("https://groups.google.com/g/access/c/thread"),
        None
    );
    assert_eq!(
        subscription_url("https://example.groups.io/g/access/search?q=rust%20reader").as_deref(),
        Some("https://example.groups.io/g/access/search?q=rust%20reader")
    );
    assert_eq!(
        native_feed_url("https://groups.io/g/access/topics").as_deref(),
        Some("https://groups.io/g/access/rss")
    );
    assert_eq!(
        subscription_url("https://groups.google.com.evil.test/g/access"),
        None
    );
    assert_eq!(
        native_feed_url("https://discuss.google.dev/c/cloud/7").as_deref(),
        Some("https://discuss.google.dev/c/cloud/7.rss")
    );
}

#[test]
fn google_rows_keep_subject_author_snippet_and_bounds() {
    let html = r#"<title>Access - Google Groups</title><div data-rowid="abc"><a href="./g/access/c/abc"><span class="o1DPKc">Rust &amp; RSS</span></a><span class="z0zUgf">Alice</span><span class="z0zUgf">Bob</span><span class="WzoK">First message</span></div><div data-rowid="abc"><a href="./g/access/c/abc"><span class="o1DPKc">Duplicate</span></a></div>"#;
    let feed = parse_google_group(html, "https://groups.google.com/g/access", 30).unwrap();
    assert_eq!(feed.title, "Access");
    assert_eq!(feed.entries.len(), 1);
    assert_eq!(feed.entries[0].title, "Rust & RSS");
    assert_eq!(feed.entries[0].author.as_deref(), Some("Alice, Bob"));
    assert_eq!(feed.entries[0].summary, "First message");
    assert_eq!(
        feed.entries[0].url,
        "https://groups.google.com/g/access/c/abc"
    );
    assert!(
        parse_google_group(
            "You don't have permission to access this content",
            "https://groups.google.com/g/access",
            30
        )
        .unwrap_err()
        .contains("sign-in")
    );
}

#[test]
fn groups_search_keeps_query_metadata_and_same_group_pagination() {
    let html = r#"<table><tr><td id="one"><span class="subject"><a href="/g/access/topic/thread/123">Reader news</a></span><span class="user-chip-name">Alice</span><span class="truncate-two-lines">Details</span><script>DisplayShortTime(1700000000000000000)</script></td></tr></table><a rel="next" href="?q=rust&page=2">Next</a>"#;
    let (feed, next) =
        parse_groups_search(html, "https://groups.io/g/access/search?q=rust", 100).unwrap();
    assert_eq!(feed.title, "access search: rust");
    assert_eq!(feed.entries[0].url, "https://groups.io/g/access/topic/123");
    assert_eq!(feed.entries[0].author.as_deref(), Some("Alice"));
    assert_eq!(
        feed.entries[0].date.as_deref(),
        Some("2023-11-14T22:13:20Z")
    );
    assert_eq!(
        next.as_deref(),
        Some("https://groups.io/g/access/search?q=rust&page=2")
    );
    let (_, next) = parse_groups_search(
        "<a rel='next' href='https://evil.test/'>Next</a>",
        "https://groups.io/g/access/search?q=rust",
        100,
    )
    .unwrap();
    assert!(next.is_none());
}
