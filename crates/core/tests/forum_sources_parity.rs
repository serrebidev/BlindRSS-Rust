use blindrss_core::forum_sources::{
    native_feed_url, parse_google_group, parse_groups_search, subscription_url,
};
use serde_json::json;

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

// ---------------------------------------------------------------------------
// Audit 8 Task 15 (R02–R04): Reddit/Lemmy threads are ONE semantic
// document; the plain reader (html_to_text) and the rich reader
// (clean_article_html, the sanitizer path) must expose the
// identical complete post/reply set.
// ---------------------------------------------------------------------------

fn heading_identities(text: &str) -> Vec<String> {
    // Every thread block heading starts with "#<n> Posted by" or
    // "#<n> Comment by"; collect the identity prefix of each.
    let mut out = Vec::new();
    for fragment in text.split('#').skip(1) {
        let mut parts = fragment.splitn(2, ' ');
        let Some(number) = parts.next() else { continue };
        let Some(rest) = parts.next() else { continue };
        if !number.chars().all(|c| c.is_ascii_digit()) || number.is_empty() {
            continue;
        }
        if rest.starts_with("Posted by") || rest.starts_with("Comment by") {
            let identity: String = rest
                .chars()
                .take_while(|c| *c != '—' && *c != '<')
                .collect();
            out.push(format!("#{number} {}", identity.trim()));
        }
    }
    out
}

#[test]
fn reddit_document_is_identical_in_both_readers() {
    let payload = json!([
        {"kind": "Listing", "data": {"children": [
            {"kind": "t3", "data": {
                "id": "art1", "name": "t3_art1", "title": "Reader parity thread",
                "author": "op", "created_utc": 1767225600.0,
                "selftext": "Opening post body for the reader parity thread.",
                "url": "https://www.reddit.com/r/rust/comments/art1/x/",
                "permalink": "/r/rust/comments/art1/x/"
            }}
        ]}},
        {"kind": "Listing", "data": {"children": [
            {"kind": "t1", "data": {
                "id": "a1", "name": "t1_a1", "author": "amy",
                "body": "A first substantive reply about the new reader.",
                "parent_id": "t3_art1", "created_utc": 1767225900.0, "replies": ""
            }},
            {"kind": "more", "data": {"id": "m1", "parent_id": "t3_art1", "children": ["z9"], "count": 1}}
        ]}}
    ]);
    let (_title, doc) = blindrss_core::threads::reddit_thread_html_from_payload(
        &payload,
        "https://www.reddit.com/r/rust/comments/art1/x/",
        &mut |url| {
            assert!(url.contains("morechildren"));
            Some(json!({"json": {"data": {"things": [
                {"kind": "t1", "data": {
                    "id": "z9", "name": "t1_z9", "author": "zoe",
                    "body": "A hidden reply loaded through morechildren expansion.",
                    "parent_id": "t3_art1", "created_utc": 1767226000.0, "replies": ""
                }}
            ]}}}))
        },
    )
    .unwrap();
    let url = "https://www.reddit.com/r/rust/comments/art1/x/";
    let plain = blindrss_core::extract::html_to_text(&doc);
    let rich = blindrss_core::article_html::clean_article_html(&doc, url, false);
    let plain_ids = heading_identities(&plain);
    let rich_ids = heading_identities(&rich);
    assert_eq!(plain_ids.len(), 3, "plain: {plain}");
    assert_eq!(plain_ids, rich_ids, "rich: {rich}");
    for body in [
        "Opening post body for the reader parity thread.",
        "A first substantive reply about the new reader.",
        "A hidden reply loaded through morechildren expansion.",
    ] {
        assert!(plain.contains(body), "plain missing {body}");
        assert!(rich.contains(body), "rich missing {body}");
    }
}

#[test]
fn lemmy_document_is_identical_in_both_readers() {
    let post_view = json!({
        "post": {
            "id": 42, "name": "Reader parity thread",
            "body": "Opening Lemmy post body for the reader parity thread.",
            "published": "2026-02-01T10:00:00",
            "ap_id": "https://lemmy.example/post/42"
        },
        "creator": {"name": "op"}
    });
    let comment = |id: i64, author: &str, content: &str| {
        json!({
            "comment": {
                "id": id, "content": content, "path": format!("0.{id}"),
                "published": "2026-02-01T11:00:00",
                "ap_id": format!("https://lemmy.example/comment/{id}")
            },
            "creator": {"name": author}
        })
    };
    let (_title, doc) =
        blindrss_core::threads::lemmy_thread_html_from("https://lemmy.example", 42, &mut |url| {
            if url.contains("/api/v3/post?") {
                return Some(json!({"post_view": post_view.clone()}));
            }
            if url.contains("/api/v3/comment/list") {
                return Some(json!({"comments": [
                    comment(1, "amy", "A first Lemmy reply rendered from Markdown."),
                    comment(2, "bob", "A second Lemmy reply with **emphasis** included."),
                ]}));
            }
            None
        })
        .unwrap();
    let url = "https://lemmy.example/post/42";
    let plain = blindrss_core::extract::html_to_text(&doc);
    let rich = blindrss_core::article_html::clean_article_html(&doc, url, false);
    let plain_ids = heading_identities(&plain);
    let rich_ids = heading_identities(&rich);
    assert_eq!(plain_ids.len(), 3, "plain: {plain}");
    assert_eq!(plain_ids, rich_ids, "rich: {rich}");
    for body in [
        "Opening Lemmy post body for the reader parity thread.",
        "A first Lemmy reply rendered from Markdown.",
        "A second Lemmy reply with",
    ] {
        assert!(plain.contains(body), "plain missing {body}");
        assert!(rich.contains(body), "rich missing {body}");
    }
}
