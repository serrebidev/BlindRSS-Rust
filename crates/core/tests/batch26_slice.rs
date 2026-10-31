//! Batch 26 (v0.1.26): the rich-reader HTML cleaner — port of Python
//! core/article_html.py. Sanitization via ammonia with Python's
//! allowlists; DOM passes on the balanced-element scanner.

use blindrss_core::article_html::{clean_article_html, render_rich_article};

const URL: &str = "https://example.com/news/story.html";

fn page(body: &str) -> String {
    format!("<html><head><title>T</title></head><body><article>{body}</article></body></html>")
}

#[test]
fn keeps_prose_links_images_drops_scripts_and_attrs() {
    let html = page(
        r#"<p class="x" style="color:red" onclick="evil()">Hello <a href="/other" title="Other" data-track="1">world</a></p>
<script>alert(1)</script>
<form><p>form text</p></form>
<p><img src="pic.jpg" alt="A picture" onerror="evil()"></p>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(out.contains("Hello"), "{out}");
    assert!(out.contains(r#"href="https://example.com/other""#), "{out}");
    assert!(out.contains(r#"title="Other""#), "{out}");
    assert!(!out.contains("data-track"), "{out}");
    assert!(!out.contains("onclick"), "{out}");
    assert!(!out.contains("alert"), "{out}");
    assert!(!out.contains("form text"), "{out}");
    assert!(
        out.contains(r#"src="https://example.com/news/pic.jpg""#),
        "{out}"
    );
    assert!(out.contains(r#"alt="A picture""#), "{out}");
    assert!(!out.contains("class="), "{out}");
}

#[test]
fn iframe_embed_kept_and_normalized() {
    let html = page(
        r#"<p>Watch this:</p><iframe src="https://www.youtube.com/embed/abcDEF123" width="560" height="315" frameborder="0"></iframe>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(out.contains("<iframe"), "{out}");
    // Player URLs normalize to pages a browser can open directly.
    assert!(
        out.contains("https://www.youtube.com/watch?v=abcDEF123"),
        "{out}"
    );
    assert!(!out.contains("frameborder"), "{out}");
}

#[test]
fn chrome_class_dropped_but_media_wrapper_kept() {
    let html = page(
        r#"<div class="related-stories"><p>Related clickbait headline</p></div>
<div class="social-embed"><iframe src="https://player.vimeo.com/video/12345"></iframe></div>
<p>Real body text that stays in the article.</p>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(!out.contains("clickbait"), "{out}");
    assert!(out.contains("https://vimeo.com/12345"), "{out}");
    assert!(out.contains("Real body text"), "{out}");
}

#[test]
fn boilerplate_and_widget_headers_dropped() {
    let html = page(
        r#"<p>This post contains affiliate links. We may earn a commission if you buy through them.</p>
<h2>Related stories</h2>
<p>The actual article paragraph with real reporting in it.</p>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(!out.contains("affiliate"), "{out}");
    assert!(!out.contains("Related stories"), "{out}");
    assert!(out.contains("actual article paragraph"), "{out}");
}

#[test]
fn byline_lines_dropped() {
    let html = page(
        r#"<p>By Jane Reporter</p><p>Updated: 4:14 PM ET</p>
<p>The story itself begins here with substance.</p>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(!out.contains("Jane Reporter"), "{out}");
    assert!(!out.contains("Updated"), "{out}");
    assert!(out.contains("story itself"), "{out}");
}

#[test]
fn twitter_blockquote_becomes_iframe() {
    let html = page(
        r#"<blockquote class="twitter-tweet"><p>Some tweet text</p><a href="https://twitter.com/jack/status/1234567890">link</a></blockquote>
<p>Article text around the tweet.</p>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(
        out.contains("https://platform.twitter.com/embed/Tweet.html?id=1234567890"),
        "{out}"
    );
}

#[test]
fn social_embed_without_id_becomes_permalink() {
    let html = page(
        r#"<blockquote class="instagram-media"><a href="https://www.instagram.com/someuser/">A photo</a></blockquote>
<p>Article text around the embed.</p>"#,
    );
    let out = clean_article_html(&html, URL, false);
    assert!(out.contains("View this post on Instagram"), "{out}");
}

#[test]
fn br_runs_collapse_and_zero_width_stripped() {
    let html = page("<p>First\u{200b} paragraph</p><br><br><p>Second paragraph</p>");
    let out = clean_article_html(&html, URL, false);
    assert!(!out.contains('\u{200b}'), "{out}");
    assert!(!out.contains("<br><br>"), "{out}");
    assert!(out.contains("First paragraph"), "{out}");
    assert!(out.contains("Second paragraph"), "{out}");
}

#[test]
fn feed_only_render_builds_header_and_lang() {
    let out = render_rich_article(
        "",
        "<p>Feed body text here.</p><script>x()</script>",
        "My Story",
        "Ann Author",
        "31 October 2026",
        "",
        "",
        "",
        "en",
    )
    .expect("rendered");
    assert!(out.starts_with("<article lang=\"en\">"), "{out}");
    assert!(out.contains("<h1>My Story</h1>"), "{out}");
    assert!(out.contains("31 October 2026 · Ann Author"), "{out}");
    assert!(out.contains("Feed body text here."), "{out}");
    assert!(!out.contains("x()"), "{out}");
}

#[test]
fn traf_prune_drops_link_cluster_keeps_prose() {
    let prose = concat!(
        "<p>The city council voted on Tuesday to approve the new transit plan, ",
        "a decision that ends two years of debate over bus routes and funding ",
        "for the downtown corridor project and its connecting neighbourhoods.</p>",
        "<p>Officials said the first phase will add forty electric buses to the ",
        "fleet and rebuild three stations, with construction expected to begin ",
        "in the spring and finish before the end of next year, they confirmed.</p>",
        "<p>Residents who spoke at the meeting welcomed the plan but asked the ",
        "council to protect existing stops in the east end, where many seniors ",
        "rely on the current route for medical appointments and daily shopping.</p>",
        "<p>The mayor said the budget includes money for those stops and promised ",
        "a full review of ridership data before any route is changed, adding that ",
        "public consultations will be held in every affected ward this winter.</p>"
    );
    let cluster = concat!(
        "<ul><li><a href=\"https://example.com/a\">Zebra quokka xylophone jamboree</a></li>",
        "<li><a href=\"https://example.com/b\">Wombat velvet umbrella kinetics</a></li>",
        "<li><a href=\"https://example.com/c\">Platypus nebula ostrich fjord</a></li></ul>"
    );
    let html = page(&format!("{prose}{cluster}"));
    let out = clean_article_html(&html, URL, true);
    assert!(out.contains("city council voted"), "{out}");
    assert!(!out.contains("Zebra quokka"), "{out}");
    assert!(!out.contains("Platypus nebula"), "{out}");
}

#[test]
fn empty_inputs_yield_nothing() {
    assert_eq!(clean_article_html("", URL, false), "");
    assert_eq!(
        clean_article_html("<script>only script</script>", URL, false),
        ""
    );
    assert!(render_rich_article("", "", "", "", "", "", "", "", "en").is_none());
}
