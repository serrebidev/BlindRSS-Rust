//! Source-depth batch regression tests (PARITY-2026-11-01 gaps 3-4):
//! HTML forum thread linearization (Invision / vBulletin / FluxBB /
//! Drupal / XenForo), Google Groups conversation rebuild, Google
//! Support thread_view decoding, and the FeedSearchDialog detail
//! enrichments. Fixtures mirror Python's
//! tests/test_fulltext_forum_thread.py.

use blindrss_core::forum_sources::{
    forum_thread_html_for, forum_thread_text_for, google_groups_thread_document,
    google_support_thread_document, is_google_groups_thread_url, is_google_support_thread_url,
};

const NSANE_URL: &str = "https://nsaneforums.com/topic/488155-adguard-80/#comment-1908581";
const FST_URL: &str =
    "https://filesharingtalk.com/threads/481617-Last-one-to-post-wins-the-internets";
const FORUM_URL: &str =
    "https://forum.audiogames.net/topic/59794/what-apps-do-you-guys-use/new/posts/";
const APPLEVIS_URL: &str = "https://www.applevis.com/forum/macos-mac-apps/i-built-mac-browser";

fn invision_post(id: u64, author: &str, when: &str, body: &str) -> String {
    format!(
        "<article id=\"elComment_{id}\" class=\"cPost ipsBox ipsComment ipsComment_parent ipsClearfix\">\
         <aside class=\"ipsComment_author\"><h3 class=\"ipsType_sectionHead cAuthorPane_author\">{author}</h3></aside>\
         <div class=\"ipsComment_content\"><div class=\"ipsComment_meta\">\
         <time datetime=\"2026-08-23T00:00:00Z\">{when}</time></div>\
         <div data-role=\"commentContent\" class=\"ipsType_richText\"><p>{body}</p></div>\
         <ul class=\"ipsComment_controls\"><li>Quote</li><li>Share</li></ul>\
         </div></article>"
    )
}

#[test]
fn invision_thread_keeps_every_post_with_attribution() {
    let posts = [
        (
            1908581,
            "Matt",
            "Thursday at 02:55 PM",
            "Adguard is the world's most advanced Internet filter.",
        ),
        (
            1908582,
            "Matt",
            "Thursday at 03:00 PM",
            "Windows 8 and 8.1 support ends.",
        ),
        (
            1908816,
            "mray88",
            "9 hours ago",
            "version 8 is pretty buggy and a lot of things are broken.",
        ),
    ];
    let html = format!(
        "<html><head><title>Adguard 8.0 - Nsane Forums</title></head><body>\
         <section class=\"ipsRecommendedComments\"><h2>Recommended Posts</h2>\
         <div data-role=\"commentContent\">This duplicated recommendation must not appear.</div></section>\
         {}</body></html>",
        posts
            .iter()
            .map(|p| invision_post(p.0, p.1, p.2, p.3))
            .collect::<String>()
    );
    let text = forum_thread_text_for(NSANE_URL, &html);
    let headers: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(headers.len(), 3);
    for (i, (_, author, when, body)) in posts.iter().enumerate() {
        assert_eq!(headers[i], format!("#{} {} — {}", i + 1, author, when));
        assert!(text.contains(body), "missing body {body}");
    }
    assert!(!text.contains("duplicated recommendation"));
    assert!(!text.contains("Quote"));
    // Rich view: one <h2> per post.
    let rich = forum_thread_html_for(NSANE_URL, &html).unwrap();
    assert_eq!(rich.matches("<h2>").count(), 3);
    assert!(rich.contains("pretty buggy"));
}

fn vbulletin_post(
    number: u64,
    post_id: u64,
    author: &str,
    when: &str,
    body: &str,
    signature: &str,
) -> String {
    format!(
        "<li id=\"post_{post_id}\" class=\"postbitlegacy postcontainer old\">\
         <div class=\"posthead\"><span class=\"postdate\">{when}</span><a class=\"postcounter\">#{number}</a></div>\
         <div class=\"userinfo\"><a class=\"username\">{author}</a></div>\
         <div class=\"postbody\"><div id=\"post_message_{post_id}\">\
         <blockquote class=\"postcontent restore\"><p>{body}</p>{}</blockquote></div></div></li>",
        if signature.is_empty() {
            String::new()
        } else {
            format!("<div class=\"signature\">{signature}</div>")
        }
    )
}

#[test]
fn vbulletin_thread_keeps_every_post_and_drops_signatures() {
    let html = format!(
        "<html><head><title>Last one to post wins the internets</title></head><body><ol id=\"posts\">\
         {}{}</ol><a rel=\"next\" href=\"/threads/481617/page2\">Next</a>\
         <li class=\"postbitlegacy\"></li></body></html>",
        vbulletin_post(
            1,
            3894813,
            "tesco",
            "03-16-2008, 07:25 PM",
            "Last one to post gets twenty dollars.",
            ""
        ),
        vbulletin_post(
            2,
            3894199,
            "IdolEyes787",
            "01-02-2025, 08:27 AM",
            "The next reply remains part of the same conversation.",
            "A repeated signature that must be removed."
        ),
    );
    let text = forum_thread_text_for(FST_URL, &html);
    assert!(text.contains("#1 tesco — 03-16-2008, 07:25 PM"), "{text}");
    assert!(
        text.contains("#2 IdolEyes787 — 01-02-2025, 08:27 AM"),
        "{text}"
    );
    assert!(text.contains("Last one to post gets twenty dollars."));
    assert!(!text.contains("repeated signature"), "{text}");
    // A forum index page is not a thread.
    assert_eq!(
        forum_thread_text_for(
            "https://filesharingtalk.com/forum.php",
            "<html><body><main><a href=\"/threads/1-example\">Example</a></main></body></html>"
        ),
        ""
    );
}

fn fluxbb_post(num: u64, byline: &str, author: &str, when: &str, body: &str, sig: &str) -> String {
    format!(
        "<div class=\"post odd\"><div class=\"posthead\"><h3 class=\"hn post-ident\">\
         <span class=\"post-num\"><a class=\"permalink\" href=\"#p{num}\">#{num}</a></span>\
         <span class=\"post-byline\"><span>{byline} by</span> <a href=\"https://forum.audiogames.net/user/1/\">{author}</a></span>\
         <span class=\"post-link\">{when}</span></h3></div>\
         <div class=\"postbody\"><div class=\"post-entry\"><div class=\"entry-content\">\
         <p>{body}</p>{}</div></div></div></div>",
        if sig.is_empty() {
            String::new()
        } else {
            format!("<div class=\"sig-content\">{sig}</div>")
        }
    )
}

#[test]
fn fluxbb_thread_keeps_posts_drops_signatures_and_expands_truncated_links() {
    let amazon =
        "https://www.amazon.com/dp/B0CKB9WK3F?ref_=ppx_hzsearch_conn_dt_b_fed_asin_title_1";
    let html = format!(
        "<html><head><title>What apps do you guys use?</title></head><body><div id=\"brd-main\">\
         {}{}</div></body></html>",
        fluxbb_post(
            1,
            "Topic",
            "Zayed",
            "2026-07-19 13:12:20",
            "Which delivery apps do you use?",
            ""
        ),
        fluxbb_post(
            2,
            "Reply",
            "Cornettoking",
            "Yesterday 18:24:00",
            &format!(
                "It is called Lieferando here. <a href=\"{amazon}\">https://www.amazon.com/dp/B0CKB9WK3F?re … in_title_1</a>"
            ),
            "Visit my soundcloud page where I upload silly songs."
        ),
    );
    let text = forum_thread_text_for(FORUM_URL, &html);
    assert!(
        text.contains("#1 Topic by Zayed — 2026-07-19 13:12:20"),
        "{text}"
    );
    assert!(
        text.contains("#2 Reply by Cornettoking — Yesterday 18:24:00"),
        "{text}"
    );
    assert!(text.contains("Which delivery apps do you use?"));
    assert!(!text.contains("soundcloud"), "{text}");
    assert!(text.contains(amazon), "{text}");
    assert!(!text.contains('…'), "{text}");
}

#[test]
fn drupal_thread_keeps_lead_and_every_comment() {
    let comment = |cid: u64, subject: &str, author: &str, when: &str, body: &str| {
        format!(
            "<article class=\"comment js-comment\" id=\"comment-{cid}\" role=\"article\">\
             <div class=\"comment__title\"><h3><a class=\"permalink\" href=\"/comment/{cid}\">{subject}</a></h3></div>\
             <div class=\"comment__text-wrapper\"><footer class=\"comment__meta\">\
             <p class=\"comment__author\">By <span>{author}</span> on {when}</p></footer>\
             <div class=\"comment__title comment__content\">\
             <div class=\"text-content field field--name-comment-body comment__text-content\"><p>{body}</p></div>\
             <ul class=\"links inline comment__links\"><li>Reply</li></ul>\
             </div></div></article>"
        )
    };
    let html = format!(
        "<html><head><title>Mega thread | AppleVis</title></head><body>\
         <article class=\"node node--type-forum\">\
         <div class=\"node__meta\"><p><span>By <span><span>AppleVis</span></span>, 20  July,  2026</span></p>\
         <div class=\"node__content\">\
         <div class=\"field field--name-taxonomy-forums\"><div class=\"field__item\">Apple Beta Releases</div></div>\
         <div class=\"text-content field field--name-body field__item\">\
         <p>Please use this megathread for any and all discussion.</p></div>\
         </div></div></article>{}{}</body></html>",
        comment(
            1,
            "Speech quality",
            "tunmi13",
            "Monday, July 20, 2026 - 18:46",
            "Cannot tell if fixed."
        ),
        comment(
            2,
            "not fixed",
            "Jokyboy129",
            "Monday, July 20, 2026 - 20:02",
            "It is definitely not fixed."
        ),
    );
    let text = forum_thread_text_for(APPLEVIS_URL, &html);
    assert!(text.contains("#1 By AppleVis, 20 July, 2026"), "{text}");
    assert!(text.contains("Please use this megathread"));
    assert!(
        text.contains("#2 Speech quality — By tunmi13 on Monday, July 20, 2026 - 18:46"),
        "{text}"
    );
    assert!(
        text.contains("#3 not fixed — By Jokyboy129 on Monday, July 20, 2026 - 20:02"),
        "{text}"
    );
    assert!(text.contains("It is definitely not fixed."));
    assert!(!text.contains("Apple Beta Releases"), "{text}");
    // A blog entry (lead only, no comments) is not a thread.
    let blog = "<html><body><article class=\"node\"><div class=\"node__meta\"><p>By AppleVis</p>\
                <div class=\"node__content\"><div class=\"field--name-body\"><p>Solo entry.</p></div></div></article></body></html>";
    assert_eq!(forum_thread_text_for(APPLEVIS_URL, blog), "");
}

#[test]
fn xenforo_thread_keeps_every_post_with_attribution() {
    let post = |n: u64, author: &str, when: &str, body: &str| {
        format!(
            "<article class=\"message message--post js-post\" data-author=\"{author}\" data-content=\"post-{n}\">\
             <div class=\"message-inner\"><div class=\"message-cell message-cell--user\">\
             <h4 class=\"message-name\"><a href=\"/members/x\">{author}</a></h4></div>\
             <div class=\"message-cell message-cell--main\"><div class=\"message-main\">\
             <header class=\"message-attribution\"><ul class=\"message-attribution-main\">\
             <li><a href=\"/threads/t.1/post-{n}\"><time datetime=\"2026-08-23T00:00:00Z\">{when}</time></a></li></ul>\
             <ul class=\"message-attribution-opposite\"><li><a href=\"/threads/t.1/post-{n}\">#{n}</a></li></ul></header>\
             <div class=\"message-content\"><div class=\"message-userContent\">\
             <article class=\"message-body js-selectToQuote\"><div class=\"bbWrapper\"><p>{body}</p></div></article>\
             </div></div></div></div></div></article>"
        )
    };
    let html = format!(
        "<html><head><title>Thread</title></head><body><div class=\"p-body-content\">{}{}</div></body></html>",
        post(1, "alice", "Aug 23, 2026 at 2:55 PM", "Opening post body."),
        post(2, "bob", "Aug 23, 2026 at 3:01 PM", "A reply body."),
    );
    // XenForo keys on markup alone (no Python host list exists).
    let text = forum_thread_text_for("https://example-forum.test/threads/t.1/", &html);
    assert!(
        text.contains("#1 alice — Aug 23, 2026 at 2:55 PM"),
        "{text}"
    );
    assert!(text.contains("#2 bob — Aug 23, 2026 at 3:01 PM"), "{text}");
    assert!(text.contains("Opening post body."));
    assert!(text.contains("A reply body."));
}

#[test]
fn google_groups_conversation_rebuilds_all_messages() {
    assert!(is_google_groups_thread_url(
        "https://groups.google.com/g/access/c/AbC123"
    ));
    assert!(is_google_groups_thread_url(
        "https://groups.google.com/forum/#!topic/access/AbC123"
    ));
    assert!(!is_google_groups_thread_url(
        "https://groups.google.com/g/access"
    ));
    let message = |id: &str, author: &str, date: &str, body: &str| {
        format!(
            "<div class=\"eH2Xlc\"><h3>{author}</h3><span class=\"zX2W9c\">{date}</span>\
             <span data-message-id=\"{id}\"></span>\
             <div role=\"region\" aria-labelledby=\"h-{id}\"><p>{body}</p>\
             <div class=\"gmail_quote\">Quoted noise</div></div></div>"
        )
    };
    let html = format!(
        "<html><head><title>Rust readers - Google Groups</title></head><body>{}{}</body></html>",
        message("m1", "Alice", "Nov 1, 2026", "First message body."),
        message("m2", "Bob", "Nov 2, 2026", "Second message body."),
    );
    let doc = google_groups_thread_document(&html, "https://groups.google.com/g/access/c/AbC123");
    assert!(doc.contains("<title>Rust readers</title>"), "{doc}");
    assert!(doc.contains("#1 Posted by Alice — Nov 1, 2026"), "{doc}");
    assert!(doc.contains("#2 Message by Bob — Nov 2, 2026"), "{doc}");
    assert!(doc.contains("First message body."));
    assert!(doc.contains("Second message body."));
    assert!(!doc.contains("Quoted noise"), "{doc}");
    // The semantic document linearizes back into attributed posts.
    let text = forum_thread_text_for("https://groups.google.com/g/access/c/AbC123", &doc);
    assert!(text.contains("#1 Posted by Alice — Nov 1, 2026"), "{text}");
    assert!(text.contains("Second message body."));
    // Permission-denied pages rebuild nothing.
    assert_eq!(
        google_groups_thread_document(
            "You don't have permission to access this content",
            "https://groups.google.com/g/access/c/AbC123"
        ),
        ""
    );
}

#[test]
fn youtube_history_state_uses_pythons_columns_and_due_rules() {
    use blindrss_core::db::Db;
    let dir = std::env::temp_dir().join(format!(
        "blindrss-sd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open_or_create(&dir.join("rss.db")).unwrap();
    // The state table FK-references feeds, as in Python.
    db.insert_feed_if_missing(&blindrss_core::Feed {
        id: "feed-a".to_string(),
        url: "https://www.youtube.com/feeds/videos.xml?channel_id=UCx".to_string(),
        title: "Channel".to_string(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    })
    .unwrap();
    // A Python-shaped row: done=1 was the old Videos-tab listing,
    // which missed live streams — due again immediately.
    db.set_youtube_history_state("feed-a", 1, 100.0).unwrap();
    assert!(db.youtube_history_due("feed-a", 200.0));
    // done=2 is final.
    db.set_youtube_history_state("feed-a", 2, 100.0).unwrap();
    assert!(!db.youtube_history_due("feed-a", 999_999_999.0));
    // done=0 retries only after a day.
    db.set_youtube_history_state("feed-a", 0, 1000.0).unwrap();
    assert!(!db.youtube_history_due("feed-a", 1000.0 + 86_399.0));
    assert!(db.youtube_history_due("feed-a", 1000.0 + 86_400.0));
    // Never attempted: due.
    assert!(db.youtube_history_due("feed-b", 0.0));
    let (done, last) = db.youtube_history_state("feed-a").unwrap().unwrap();
    assert_eq!((done, last), (0, 1000.0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn youtube_history_state_repairs_legacy_rust_columns() {
    use blindrss_core::db::Db;
    let dir = std::env::temp_dir().join(format!(
        "blindrss-sd-legacy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rss.db");
    // A database an early Rust build created, with its own columns.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE youtube_history_state (
                feed_id TEXT PRIMARY KEY, state INTEGER NOT NULL DEFAULT 0,
                attempted_at INTEGER NOT NULL DEFAULT 0);
             INSERT INTO youtube_history_state VALUES ('feed-a', 2, 1234);",
        )
        .unwrap();
    }
    let db = Db::open_or_create(&path).unwrap();
    let (done, last) = db.youtube_history_state("feed-a").unwrap().unwrap();
    assert_eq!((done, last), (2, 1234.0));
    assert!(!db.youtube_history_due("feed-a", 999_999_999.0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn feedsearch_social_details_match_python() {
    use blindrss_core::feedsearch::{SearchProvider, parse_results};
    // Reddit: subscriber suffix on the description.
    let rows = parse_results(
        SearchProvider::Reddit,
        r#"{"data":{"children":[{"data":{"display_name":"rust","public_description":"Rust news","subscribers":1234}}]}}"#,
        20,
    )
    .unwrap();
    assert_eq!(rows[0].description, "Rust news (1234 subs)");
    // Lemmy: "Lemmy - name (N subs)"; Kbin for /m/ actors.
    let rows = parse_results(
        SearchProvider::Lemmy,
        r#"{"communities":[{"community":{"name":"rust","title":"Rust","actor_id":"https://lemmy.world/c/rust"},"counts":{"subscribers":42}}]}"#,
        20,
    )
    .unwrap();
    assert_eq!(rows[0].description, "Lemmy - rust (42 subs)");
    let rows = parse_results(
        SearchProvider::Lemmy,
        r#"{"communities":[{"community":{"name":"rust","title":"Rust","actor_id":"https://kbin.example/m/rust"},"counts":{}}]}"#,
        20,
    )
    .unwrap();
    assert_eq!(rows[0].url, "https://kbin.example/m/rust/rss");
    assert_eq!(rows[0].description, "Kbin - rust");
    // Mastodon: account detail carries @acct and followers; tag
    // detail carries recent uses from history.
    let rows = parse_results(
        SearchProvider::Mastodon,
        r#"{"accounts":[{"url":"https://mastodon.social/@alice","display_name":"Alice","acct":"alice","followers_count":99}],"hashtags":[{"name":"rust","url":"https://mastodon.social/tags/rust","history":[{"uses":7}]}]}"#,
        20,
    )
    .unwrap();
    assert_eq!(rows[0].description, "Mastodon user (@alice) (99 followers)");
    assert_eq!(rows[1].description, "Mastodon tag (7 recent uses)");
    // Bluesky: detail carries the handle and did.
    let rows = parse_results(
        SearchProvider::Bluesky,
        r#"{"actors":[{"handle":"alice.bsky.social","displayName":"Alice","did":"did:plc:abc"}]}"#,
        20,
    )
    .unwrap();
    assert_eq!(
        rows[0].description,
        "Bluesky user (@alice.bsky.social) (did:plc:abc)"
    );
    // PieFed community detail: name, domain, subscriptions.
    let rows = parse_results(
        SearchProvider::PieFed,
        r#"{"communities":[{"community":{"name":"rust","title":"Rust","actor_id":"https://piefed.social/c/rust","ap_domain":"piefed.social"},"counts":{"subscriptions_count":5}}]}"#,
        20,
    )
    .unwrap();
    assert_eq!(
        rows[0].description,
        "PieFed community (rust) - piefed.social (5 subs)"
    );
    // Mixcloud: follower counts with thousands separators.
    let rows = parse_results(
        SearchProvider::Mixcloud,
        r#"{"data":[{"name":"DJ","url":"https://www.mixcloud.com/dj/","follower_count":1234567}]}"#,
        20,
    )
    .unwrap();
    assert_eq!(rows[0].description, "Mixcloud user • 1,234,567 followers");
}

#[test]
fn feedsearch_mixcloud_playlists_and_bluesky_tag() {
    use blindrss_core::feedsearch::{bluesky_tag_candidate, parse_mixcloud_playlists};
    let rows = parse_mixcloud_playlists(
        r#"{"data":[{"name":"Morning Mix","url":"https://www.mixcloud.com/dj/morning-mix/"}]}"#,
        "DJ",
        3,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Morning Mix (DJ)");
    assert_eq!(rows[0].description, "Mixcloud playlist");
    assert_eq!(
        bluesky_tag_candidate("#rust-lang").as_deref(),
        Some("rust-lang")
    );
    assert_eq!(bluesky_tag_candidate("two words"), None);
    assert_eq!(bluesky_tag_candidate(""), None);
}

#[test]
fn feedsearch_youtube_variants_scoring_and_details() {
    use blindrss_core::feedsearch::{
        parse_youtube_results, youtube_match_score, youtube_query_variants,
    };
    assert_eq!(
        youtube_query_variants("rust programming tutorial"),
        vec![
            "rust programming tutorial",
            "rust programming",
            "rust tutorial",
            "programming tutorial",
        ]
    );
    assert_eq!(youtube_query_variants("single"), vec!["single"]);
    assert_eq!(
        youtube_match_score("Rust Programming Tutorial", "rust programming"),
        112
    );
    assert_eq!(youtube_match_score("Cooking Show", "rust programming"), 0);
    // Channel detail: owner label + play count, Python format.
    let rows = parse_youtube_results(
        r#"{"entries":[{"title":"Some video","channel":"Rustacean","uploader_id":"@rustacean","channel_id":"UC123","view_count":1500}]}"#,
        false,
        20,
    )
    .unwrap();
    assert_eq!(rows[0].title, "Rustacean");
    // Python's combine collapses a name equal to the handle.
    assert_eq!(
        rows[0].description,
        "YouTube channel (@rustacean) | 1,500 plays"
    );
    // Playlists rank by query match before play count.
    let rows = parse_youtube_results(
        r#"{"entries":[{"title":"Unrelated list","id":"PL1","view_count":99999},{"title":"Rust course","id":"PL2","view_count":10}]}"#,
        true,
        20,
    )
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].url.ends_with("playlist_id=PL1") || rows[0].url.ends_with("playlist_id=PL2"));
}

#[test]
fn feedsearch_groups_io_rows_carry_row_detail() {
    use blindrss_core::feedsearch::{SearchProvider, parse_html_results};
    let rows = parse_html_results(
        SearchProvider::GroupsIo,
        "<table><tr><td><a href=\"/g/rust\">Rust Readers</a> - A group about readers</td></tr></table>",
        10,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Rust Readers");
    assert_eq!(rows[0].description, "A group about readers");
    assert_eq!(rows[0].url, "https://groups.io/g/rust/rss");
}

#[test]
fn google_support_thread_view_decodes_the_whole_conversation() {
    assert!(is_google_support_thread_url(
        "https://support.google.com/chrome/thread/123456"
    ));
    assert!(!is_google_support_thread_url(
        "https://support.google.com/chrome/answer/123"
    ));
    // thread_view payload: index 1 = topic [ [id, ts], ..., title@8, body@12 ],
    // index 3 = [[author]], index 39 = [ [ reply-post, ..., [author] ] ].
    let mut topic = vec![serde_json::json!(null); 13];
    topic[0] = serde_json::json!(["t1", 1700000000u64]);
    topic[8] = serde_json::json!("Chrome crashes on launch");
    topic[12] = serde_json::json!("<p>Since the update Chrome will not open.</p>");
    let mut reply_post = vec![serde_json::json!(null); 4];
    reply_post[0] = serde_json::json!(["r1", 1700003600u64]);
    reply_post[3] = serde_json::json!("<p>Clear the cache and restart.</p><script>evil()</script>");
    let reply = serde_json::json!([reply_post, null, [["Helper Hannah"]]]);
    let mut data = vec![serde_json::json!(null); 40];
    data[1] = serde_json::Value::Array(topic);
    data[3] = serde_json::json!([["Original Olivia"]]);
    data[39] = serde_json::json!([[reply]]);
    let payload = serde_json::to_string(&serde_json::Value::Array(data)).unwrap();
    // JS single-quoted string with \u escapes, as Google serves it.
    let escaped = payload.replace('\\', "\\\\").replace('\'', "\\'");
    let html = format!("<html><body><script>var thread_view='{escaped}';</script></body></html>");
    let doc =
        google_support_thread_document(&html, "https://support.google.com/chrome/thread/123456");
    assert!(doc.contains("Chrome crashes on launch"), "{doc}");
    assert!(
        doc.contains("#1 Posted by Original Olivia — 2023-11-14 22:13 UTC"),
        "{doc}"
    );
    assert!(
        doc.contains("#2 Message by Helper Hannah — 2023-11-14 23:13 UTC"),
        "{doc}"
    );
    assert!(doc.contains("Clear the cache and restart."));
    assert!(!doc.contains("evil()"), "{doc}");
    assert_eq!(
        google_support_thread_document(
            "<html><body>no data</body></html>",
            "https://support.google.com/chrome/thread/123456"
        ),
        ""
    );
}
