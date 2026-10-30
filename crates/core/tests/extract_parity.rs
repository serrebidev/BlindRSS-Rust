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
fn zero_width_characters_are_stripped() {
    let zw = "Hel\u{200B}lo wor\u{200D}ld, this is the article body text for the reader.";
    let paras = format!("<p>{zw}</p><p>{LONG_PARA}</p><p>{LONG_PARA}</p>");
    let html = format!("<html><body><article>{paras}</article></body></html>");
    let (text, _) = article_text(extract_article(&html, "https://example.com/zw", None));
    assert!(!text.contains('\u{200B}'));
    assert!(!text.contains('\u{200D}'));
    assert!(text.contains("Hello world"));
}
