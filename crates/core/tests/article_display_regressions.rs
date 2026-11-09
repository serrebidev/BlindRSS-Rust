use blindrss_core::{Db, Feed};

#[test]
fn bluesky_missing_titles_use_post_text_for_new_and_cached_articles() {
    let path = std::env::temp_dir().join(format!("blindrss-bluesky-{}.db", uuid::Uuid::new_v4()));
    let db = Db::open_or_create(&path).unwrap();
    let feed = Feed {
        id: "bluesky".into(),
        url: "https://bsky.app/profile/example/rss".into(),
        title: "Bluesky".into(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    };
    db.insert_feed_if_missing(&feed).unwrap();
    let parsed = blindrss_core::feeds::parse_feed(br#"<rss version="2.0"><channel><title>Bluesky</title><item><guid>post</guid><link>https://bsky.app/profile/example/post/one</link><description><![CDATA[<p>Radical comes from a Latin word meaning &quot;root.&quot;</p><p>New paragraph.</p>]]></description></item></channel></rss>"#).unwrap();
    blindrss_core::refresh::store_parsed(&db, &feed, &parsed, "deleted").unwrap();
    assert_eq!(
        db.article_by_id("post").unwrap().unwrap().title,
        "Radical comes from a Latin word meaning \"root.\" New paragraph."
    );

    let stored: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT title FROM articles WHERE id = 'post'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        stored,
        "Radical comes from a Latin word meaning \"root.\" New paragraph."
    );

    rusqlite::Connection::open(&path).unwrap().execute("UPDATE articles SET title = 'No Title', is_read = 1, is_favorite = 1 WHERE id = 'post'", []).unwrap();
    let cached = db.article_by_id("post").unwrap().unwrap();
    assert_eq!(
        cached.title,
        "Radical comes from a Latin word meaning \"root.\" New paragraph."
    );
    assert!(cached.is_read && cached.is_favorite);
    let rows = db.articles_light(Some("bluesky"), 10).unwrap();
    assert_eq!(
        rows[0].title,
        "Radical comes from a Latin word meaning \"root.\" New paragraph."
    );
    assert!(rows[0].is_read && rows[0].is_favorite);
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn title_previews_preserve_real_titles_and_bound_unicode_text() {
    use blindrss_core::feeds::article_title;
    assert_eq!(
        article_title("Real episode title", "<a>Footer</a>"),
        "Real episode title"
    );
    assert_eq!(article_title("No Title", ""), "No Title");
    assert_eq!(article_title("", ""), "No Title");
    assert_eq!(article_title("No title", "<p>A &amp; B</p>"), "A & B");
    assert_eq!(article_title("", &"é".repeat(83)), "é".repeat(83));
    assert_eq!(
        article_title("", &"é".repeat(84)),
        format!("{}...", "é".repeat(80))
    );
}

#[test]
fn title_preview_html_keeps_text_nodes_without_reader_annotations() {
    use blindrss_core::feeds::article_title;
    assert_eq!(
        article_title("", "<span>Hello</span><span>world</span>"),
        "Hello world"
    );
    assert_eq!(
        article_title("", "<script>\u{130}\u{e9}</script><p>Hello</p>"),
        "Hello"
    );
    assert_eq!(
        article_title(
            "",
            "<h2>Heading</h2><blockquote>Words</blockquote><a href='https://example.com'>Link</a>"
        ),
        "Heading Words Link"
    );
}

#[test]
fn reader_html_skips_unicode_script_without_panicking() {
    assert_eq!(
        blindrss_core::extract::html_to_text("<script>\u{130}\u{e9}</script><p>Hello</p>"),
        "Hello"
    );
}
