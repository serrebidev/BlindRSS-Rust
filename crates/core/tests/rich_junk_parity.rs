//! Rich-reader junk regression tests (Owner's Rich View report, 2026-11-13).
//!
//! - MacRumors: a WordPress `yt-facade` watch card and `linkback` footer
//!   lines inside `.js-content` must not survive the Rich cleaner; the
//!   video stays as a single iframe embed.
//! - The Atlantic: `[Read:]` / `[Watch:]` recommendation cards (standalone
//!   and glued inline) and the newsletter promo must not survive.
//! - Axios: `__NEXT_DATA__` photo captions must not reach the plain
//!   extractor, and the "Add Axios as your preferred source" promo must
//!   not survive the Rich cleaner.

use blindrss_core::article_html::clean_article_html;
use blindrss_core::extract::{ExtractOutcome, extract_article};

fn article_text(html: &str, url: &str) -> String {
    match extract_article(html, url, None) {
        ExtractOutcome::Article { text, .. } => text,
        other => panic!("expected Article, got {other:?}"),
    }
}

const BODY_PARA: &str = "This is a substantial paragraph of article body text that goes on for a while to make the point clearly and completely for the reader who is following along at home today.";

#[test]
fn macrumors_yt_facade_becomes_iframe_and_linkbacks_dropped() {
    let html = format!(
        r#"<html><head><meta property="og:url" content="https://www.macrumors.com/2026/11/13/iphone-duo-fold-testing/"></head><body><article><div class="content--2u3grYDr js-content"><div class="ugc--2nTu61bm"><p>{BODY_PARA}</p><p><div class="center-wrap"><div class="yt-facade" data-yt-id="V5CYMpUAzl0" data-title="Livestream: Day 15"><img class="yt-facade__thumb" src="https://i.ytimg.com/vi/V5CYMpUAzl0/maxresdefault.jpg" alt="maxresdefault"/><a class="yt-facade__title" href="https://www.youtube.com/watch?v=V5CYMpUAzl0">Livestream: Day 15</a><a class="yt-facade__channel" href="https://www.youtube.com/channel/UCOmcA3f_RrH6b9NmcNa4tdg">CNET</a><a class="yt-facade__watch" href="https://www.youtube.com/watch?v=V5CYMpUAzl0">Watch on <span>YouTube</span></a></div></div></p><p>{BODY_PARA}</p><div class="linkback">Related Roundup: <a href="https://www.macrumors.com/roundup/iphone-duo/">iPhone Duo</a></div><div class="linkback">Related Forum: <a href="https://forums.macrumors.com/forums/iphone.99">iPhone</a></div></div></div></article></body></html>"#
    );
    let url = "https://www.macrumors.com/2026/11/13/iphone-duo-fold-testing/";
    let out = clean_article_html(&html, url, true);
    let low = out.to_lowercase();
    assert!(out.contains(BODY_PARA), "body prose lost: {out}");
    assert!(!low.contains("watch on"), "Watch-on junk survived: {out}");
    assert!(!low.contains("ytimg.com"), "thumbnail survived: {out}");
    assert!(!low.contains("yt-facade"), "facade survived: {out}");
    assert!(!out.contains("Related Roundup"), "linkback survived: {out}");
    assert!(!out.contains("Related Forum"), "linkback survived: {out}");
    // The cleaner normalizes iframe sources to watch pages (Python
    // parity), so the video remains as one iframe, not the card.
    assert!(
        out.contains("src=\"https://www.youtube.com/watch?v=V5CYMpUAzl0\""),
        "video must remain as an iframe embed: {out}"
    );
    assert_eq!(
        out.matches("<a ").count(),
        0,
        "no junk anchors may remain: {out}"
    );
}

#[test]
fn atlantic_read_watch_cards_and_newsletter_promo_stripped_from_rich() {
    let html = format!(
        r#"<html><body><article><section class="ArticleBody_root"><div class="ArticleLegacyHtml_root"><p><small><i>This article was featured in the One Story to Read Today newsletter. </i><a href="https://www.theatlantic.com/newsletters/sign-up/one-story-to-read-today/"><i>Sign up for it here.</i></a></small></p></div><p class="ArticleParagraph_root">{BODY_PARA}</p><p>[Read: The best of the rest of the month]</p><p>{BODY_PARA}</p><p>[Watch: Highlights from this week]</p><p>{BODY_PARA} [Listen: A short extra card glued to prose]</p></section></article></body></html>"#
    );
    let url = "https://www.theatlantic.com/politics/2026/11/joe-biden-memoir-trump/689346/";
    let out = clean_article_html(&html, url, true);
    assert!(out.contains(BODY_PARA), "body prose lost: {out}");
    assert!(!out.contains("[Read:"), "Read card survived: {out}");
    assert!(!out.contains("[Watch:"), "Watch card survived: {out}");
    assert!(
        !out.contains("[Listen:"),
        "inline Listen card survived: {out}"
    );
    assert!(
        !out.contains("newsletter"),
        "newsletter promo survived: {out}"
    );
    assert!(
        !out.contains("Sign up for it here"),
        "promo link survived: {out}"
    );
}

#[test]
fn axios_photo_captions_stay_out_of_plain_text() {
    // __NEXT_DATA__ story bodyHtml carries one figure caption between
    // prose paragraphs, as Axios serves it.
    let next_data = serde_json::json!({
        "props": {"pageProps": {"data": {"story": {"bodyHtml": format!(
            "<p>{BODY_PARA}</p><p>{BODY_PARA}</p><figure><img alt=\"People stand behind police tape. Photo: Chip Somodevilla/Getty Images\" src=\"https://images.axios.com/x.jpg\"/><figcaption>People stand behind police tape in Logan Circle early Thursday. Photo: Chip Somodevilla/Getty Images</figcaption></figure><p>{BODY_PARA}</p>"
        )}}}}
    });
    let html = format!(
        r#"<html><head><script id="__NEXT_DATA__" type="application/json">{next_data}</script></head><body><div>Related stories and promos only</div></body></html>"#
    );
    let text = article_text(
        &html,
        "https://www.axios.com/local/washington-dc/2026/11/13/logan-circle-shooting-dc-barricade",
    );
    assert!(text.contains(BODY_PARA), "body prose lost: {text}");
    assert!(
        !text.contains("Photo:"),
        "photo caption leaked into text: {text}"
    );
    assert!(
        !text.contains("police tape"),
        "caption sentence leaked into text: {text}"
    );
}

#[test]
fn axios_preferred_source_promo_dropped_from_rich() {
    let html = format!(
        r#"<html><body><article><div><p>Add Axios as your preferred source to</p><p>see more of our stories on Google.</p><p>{BODY_PARA}</p><p>{BODY_PARA}</p></div></article></body></html>"#
    );
    let url =
        "https://www.axios.com/local/washington-dc/2026/11/13/logan-circle-shooting-dc-barricade";
    let out = clean_article_html(&html, url, true);
    assert!(out.contains(BODY_PARA), "body prose lost: {out}");
    assert!(
        !out.to_lowercase().contains("preferred source"),
        "promo survived: {out}"
    );
    assert!(
        !out.to_lowercase().contains("see more of our stories"),
        "promo tail survived: {out}"
    );
}
