//! Slice 6 tests: ported Python gates and dedicated extractors.
//! Inline pages mirror the shapes the Python regression tests use.

use blindrss_core::extract::{ExtractOutcome, extract_article};

fn article_text(outcome: ExtractOutcome) -> (String, &'static str) {
    match outcome {
        ExtractOutcome::Article { text, source, .. } => (text, source),
        other => panic!("expected Article, got {other:?}"),
    }
}

const LONG_PARA: &str = "This is a substantial paragraph of article body text that goes on for a while to make the point clearly and completely for the reader who is following along at home today.";

#[test]
fn paywall_stub_is_rejected() {
    let html = format!(
        "<html><body><article><p>Subscribe to read this story.</p><p>{LONG_PARA}</p><p>Subscribe to unlock unlimited access today.</p></article></body></html>"
    );
    // Short body with CTA phrases: Python `_looks_like_paywall_stub`
    // applies only under 600 chars, so use a stub-sized body.
    let stub = "<html><body><article><p>Big news happened today in the city.</p><p>Subscribe to read the full story and unlock this article.</p></article></body></html>";
    let _ = html;
    match extract_article(stub, "https://example.com/news/1", None) {
        ExtractOutcome::Gate { reason } => assert!(reason.contains("paywall"), "{reason}"),
        other => panic!("expected paywall Gate, got {other:?}"),
    }
}

#[test]
fn paywall_stub_falls_back_to_feed_content() {
    let stub = "<html><body><article><p>Subscribe to continue reading this article.</p></article></body></html>";
    let feed = format!("Feed supplied body. {LONG_PARA} {LONG_PARA}");
    let (text, source) = article_text(extract_article(stub, "https://example.com/n", Some(&feed)));
    assert_eq!(source, "feed");
    assert!(text.contains("Feed supplied body"));
}

#[test]
fn link_list_page_is_rejected() {
    let items: String = (0..8)
        .map(|i| format!("<li><a href='/s{i}'>Headline number {i} here</a></li>"))
        .collect();
    let html = format!("<html><body><ul>{items}</ul></body></html>");
    match extract_article(&html, "https://example.com/index", None) {
        ExtractOutcome::Gate { .. } | ExtractOutcome::Empty => {}
        other => panic!("expected rejection, got {other:?}"),
    }
}

#[test]
fn json_ld_recovers_body_when_dom_is_thin() {
    let body = format!("{LONG_PARA} {LONG_PARA} {LONG_PARA}");
    let html = format!(
        r#"<html><head><script type="application/ld+json">{{"@type":"NewsArticle","articleBody":"{body}"}}</script></head><body><nav>Menu links only</nav></body></html>"#
    );
    let (text, _) = article_text(extract_article(&html, "https://example.com/a", None));
    // The JSON-LD body must be the article text (the base engine may
    // surface it directly, or the jsonld path recovers it — either
    // way the body wins over the nav chrome).
    assert!(text.contains("substantial paragraph"));
    assert!(!text.contains("Menu links only"));
}

#[test]
fn register_extractor_picks_story_article_and_drops_kicker() {
    let story = (0..5).fold(String::new(), |mut acc, _| {
        acc.push_str(&format!("<p>{LONG_PARA}</p>"));
        acc
    });
    let html = format!(
        "<html><body><article><p>Teaser card text.</p></article><article><p>Security</p>{story}</article></body></html>"
    );
    let (text, source) = article_text(extract_article(
        &html,
        "https://www.theregister.com/2026/10/30/story/",
        None,
    ));
    assert_eq!(source, "site");
    assert!(!text.starts_with("Security"));
    assert!(text.contains("substantial paragraph"));
    assert!(!text.contains("Teaser card"));
}

#[test]
fn axios_next_data_body_wins() {
    let payload = serde_json::json!({"props":{"pageProps":{"data":{"story":{
        "bodyHtml": format!("<p>{LONG_PARA}</p><p>{LONG_PARA}</p>")}}}}});
    let html = format!(
        r#"<html><head><script id="__NEXT_DATA__" type="application/json">{payload}</script></head><body><div>Related stories and promos only</div></body></html>"#
    );
    let (text, source) = article_text(extract_article(&html, "https://www.axios.com/story", None));
    assert_eq!(source, "site");
    assert!(text.contains("substantial paragraph"));
}

#[test]
fn metered_preview_gets_notice() {
    let paras = (0..4).fold(String::new(), |mut acc, _| {
        acc.push_str(&format!("<p>{LONG_PARA}</p>"));
        acc
    });
    let html = format!(
        "<html><body><article>{paras}</article><div role=\"note\">Subscribe to read as many articles as you like.</div></body></html>"
    );
    let (text, _) = article_text(extract_article(&html, "https://example.com/metered", None));
    assert!(text.contains("Only the free preview"), "{text}");
}

#[test]
fn bbc_article_fixture_extracts() {
    let html = include_str!("fixtures/bbc_article.html");
    let (text, _) = article_text(extract_article(
        html,
        "https://www.bbc.com/news/articles/x",
        None,
    ));
    assert!(text.len() > 2_000, "got {} chars", text.len());
}

#[test]
fn bbc_current_article_fixture_extracts() {
    // Today's live BBC article (new markup, fetched 2026-10-30).
    let html = include_str!("fixtures/bbc_article_current.html");
    let (text, _) = article_text(extract_article(
        html,
        "https://www.bbc.co.uk/news/articles/c3vgxznlx7jzo",
        None,
    ));
    assert!(text.contains("Patriot"), "unexpected body: {:.100}", text);
    assert!(text.len() > 1_500, "got {} chars", text.len());
}

#[test]
fn bbc_404_fixture_is_gated_not_an_article() {
    // The error page must never be presented as an article: its
    // readability result is a headline river behind an "Error 404"
    // body. Regression guard for the engine-disagreement vetting.
    let html = include_str!("fixtures/bbc_404_page.html");
    match extract_article(html, "https://www.bbc.com/news/articles/gone", None) {
        ExtractOutcome::Gate { .. } | ExtractOutcome::Empty => {}
        other => panic!("expected rejection, got {other:?}"),
    }
}

#[test]
fn zero_width_characters_are_stripped() {
    let zw = "Hel\u{200B}lo wor\u{200D}ld, this is the article body text for the reader.";
    let paras = format!("<p>{zw}</p><p>{LONG_PARA}</p><p>{LONG_PARA}</p>");
    let html = format!("<html><body><article>{paras}</article></body></html>");
    let (text, _) = article_text(extract_article(&html, "https://example.com/zw", None));
    assert!(!text.contains('\u{200B}'));
    assert!(!text.contains('\u{200D}'));
    assert!(text.contains("Hello world"));
}

#[test]
fn html_to_text_strips_markup_and_decodes_entities() {
    use blindrss_core::extract::html_to_text;
    let html = "<p>Tom &amp; Jerry &#8212; back&nbsp;again.</p>\
        <p>Second <a href=\"https://x.test\">para</a> with <b>bold</b>.</p>\
        <script>var tracker = 1;</script>\
        <ul><li>First item</li><li>Second item</li></ul>\
        <p>It&rsquo;s &#x201C;fine&#x201D;.</p>";
    let text = html_to_text(html);
    assert!(!text.contains('<'), "markup leaked: {text}");
    assert!(!text.contains("tracker"), "script leaked: {text}");
    assert!(text.contains("Tom & Jerry \u{2014} back again."));
    assert!(text.contains("Second para with bold."));
    assert!(text.contains("First item\nSecond item"), "{text}");
    assert!(text.contains("It\u{2019}s \u{201C}fine\u{201D}."));
    // A bare less-than in prose is not a tag.
    assert!(html_to_text("5 < 10 and 10 > 5").contains("5 < 10 and 10 > 5"));
}

#[test]
fn feed_fallback_returns_clean_text_not_html() {
    let page = "<html><body><nav>Only nav here</nav></body></html>";
    let feed = format!("<p>Feed body here. {LONG_PARA}</p><p>{LONG_PARA}</p>");
    let (text, source) = article_text(extract_article(
        page,
        "https://example.com/nav-only",
        Some(&feed),
    ));
    assert_eq!(source, "feed");
    assert!(!text.contains('<'), "feed fallback leaked markup: {text}");
    assert!(text.contains("Feed body here."));
}
