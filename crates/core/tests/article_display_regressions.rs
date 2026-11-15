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

/// Hard-wrapped HTML inside a table-based page layout (the
/// netmeister.org blog shape): every paragraph must reach the
/// plain reader as ONE line (Ctrl+Up/Down paragraph navigation),
/// a pre-wrap quote's blank line stays a paragraph break, `<pre>`
/// code keeps its lines, and the layout cell holding the article
/// is never re-appended as "Table data".
#[test]
fn hard_wrapped_table_layout_page_reads_one_paragraph_per_line() {
    let para = |n: usize| {
        format!(
            "<p>\n            Paragraph {n} starts here and keeps going\n            across several source lines with a <a\n            href=\"https://example.com/{n}\">link in it</a>\n            and then it continues for a while longer so the\n            extractor sees real prose and not a stub.\n          </p>\n\n"
        )
    };
    let body: String = (1..=8).map(para).collect();
    let page = format!(
        "<!DOCTYPE html><html><head><title>Wrapped</title></head><body>\
         <h1>Site name</h1><h2>Wrapped</h2>\
         <table style=\"border-style:none; width:100%\"><tr><td>\n{body}\
         <blockquote style=\"white-space: pre-wrap;\">Quoted first line goes\nonto a second line here.\n\nSecond quoted paragraph\nalso wraps.</blockquote>\n\
         <pre>fn main() {{\n    run();\n}}</pre>\n\
         {more}</td></tr></table></body></html>",
        more = (9..=12).map(para).collect::<String>()
    );
    let text = match blindrss_core::extract::extract_article(
        &page,
        "https://example.com/blog/wrapped.html",
        None,
    ) {
        blindrss_core::ExtractOutcome::Article { text, .. } => text,
        other => panic!("expected an article, got {other:?}"),
    };
    for n in 1..=12 {
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("Paragraph {n} ")))
            .unwrap_or_else(|| panic!("paragraph {n} missing:\n{text}"));
        assert!(
            line.ends_with("not a stub."),
            "paragraph {n} split across lines:\n{text}"
        );
        assert!(line.contains("with a link in it and then"), "{line}");
    }
    assert!(
        text.lines()
            .any(|l| l == "Quoted first line goes onto a second line here."),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l == "Second quoted paragraph also wraps."),
        "{text}"
    );
    assert!(
        text.contains("fn main() {\nrun();\n}"),
        "pre lost its lines:\n{text}"
    );
    assert!(
        !text.contains("Table data"),
        "layout cell re-appended:\n{text}"
    );
}

#[test]
fn collapse_source_whitespace_keeps_markup_and_verbatim_blocks() {
    use blindrss_core::extract::collapse_source_whitespace as c;
    assert_eq!(
        c("<p>a\n  b <a\n href=\"x\">c</a>\n d</p>"),
        "<p>a b <a\n href=\"x\">c</a> d</p>"
    );
    assert_eq!(c("<pre>a\n  b</pre> x\n y"), "<pre>a\n  b</pre> x y");
    assert_eq!(
        c("<pre>a<pre>b\n</pre>c\n</pre>d\n e"),
        "<pre>a<pre>b\n</pre>c\n</pre>d e"
    );
    assert_eq!(
        c("<script>var a=1; // x\nb()</script>"),
        "<script>var a=1; // x\nb()</script>"
    );
    assert_eq!(c("<!-- a\n b -->x\n\ny"), "<!-- a\n b -->x y");
    assert_eq!(
        c("<div style=\"white-space:pre-wrap\">a\nb\n\nc<div>d</div>\ne</div>f\n\ng"),
        "<div style=\"white-space:pre-wrap\">a b\nc<div>d</div> e</div>f g"
    );
    assert_eq!(
        c("<img style=\"white-space:pre\">a\n b"),
        "<img style=\"white-space:pre\">a b"
    );
    assert_eq!(c("x < y\n z"), "x < y z");
}

#[test]
fn gsmarena_battery_table_reads_one_row_per_line() {
    // v0.1.112 report: the 370-row battery table blew the 100-row
    // layout cap and reached the reader as one "Table data: a | b"
    // line. Python lifts the cap for this table (max_rows=None).
    let rows: String = (1..=150)
        .map(|i| format!("<tr><td>Phone {i}</td><td>{}:00h</td></tr>", 200 - i))
        .collect();
    let html = format!(
        "<html><head><title>Battery life test results v2.0</title></head><body><article>\
         <p>This page puts together the stats for all phones we have tested in our most recent \
         Battery life test 2.0. Our former battery test is now discontinued, and the endurance \
         ratings are archived here. Find all about our battery life testing procedure here.</p>\
         <table id=\"battery-table\"><thead><tr><th>Phone</th><th>Your score</th></tr></thead>\
         <tbody>{rows}</tbody></table></article></body></html>"
    );
    let url = "https://www.gsmarena.com/battery-test-v2.php3";
    let blindrss_core::ExtractOutcome::Article { text, .. } =
        blindrss_core::extract::extract_article(&html, url, None)
    else {
        panic!("battery page did not extract");
    };
    assert!(!text.contains("Table data:"), "{text}");
    assert!(text.contains("Table with 150 rows and 2 columns."));
    assert!(text.contains("Row 1: Phone: Phone 1; Your score: 199:00h."));
    assert!(text.contains("Row 150: Phone: Phone 150; Your score: 50:00h."));
}

#[test]
fn android_authority_credits_and_google_promo_are_dropped() {
    // v0.1.112 report: full text opened with "Kaitlyn Cimino /
    // Android Authority" and "Add Android Authority on Google:".
    // Credits sit in hashed-class divs after each <picture>; the
    // page JSON names every one.
    let html = "<div class=\"e_a\"><picture><img alt=\"A watch\" src=\"a.jpg\"/></picture>\
        <div class=\"e_TI e_8G\"><div class=\"e_HC\">Kaitlyn Cimino / Android Authority</div></div></div>\
        <div class=\"e_mi\"><div class=\"e_hi\"><div class=\"e_li\">Add Android Authority on Google: </div>\
        <div class=\"e_ii\"><a href=\"https://andauth.co/x\">Google</a></div></div></div>\
        <p>Fitness trackers are having a moment.</p><h2>Amazfit Bip Max</h2>\
        <div class=\"e_a\"><picture><img alt=\"Bip\" src=\"b.jpg\"/></picture>\
        <div class=\"e_TI\"><div class=\"e_HC\">Amazfit</div></div></div>\
        <p>Amazfit makes cheap watches.</p>\
        <script>window.d={\"image\":{\"credits\":[{\"title\":\"Kaitlyn Cimino / Android Authority\",\"pLink\":null}]},\
        \"b\":{\"credits\":[{\"title\":\"Amazfit\",\"pLink\":null}]}}</script>";
    let out = blindrss_core::extract::drop_image_credits(html);
    assert!(
        !out.contains(">Kaitlyn Cimino / Android Authority<"),
        "{out}"
    );
    assert!(!out.contains("Add Android Authority on Google"), "{out}");
    assert!(!out.contains(">Amazfit<"), "{out}");
    assert!(out.contains("<h2>Amazfit Bip Max</h2>"));
    assert!(out.contains("<p>Fitness trackers are having a moment.</p>"));
    assert!(out.contains("<p>Amazfit makes cheap watches.</p>"));
}
