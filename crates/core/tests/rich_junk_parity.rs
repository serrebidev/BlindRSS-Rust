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

#[test]
fn atlantic_author_and_split_link_cards_stripped_from_rich() {
    // Brandon's report, 2026-11-13: Atlantic bodies print
    // "[Ian Bogost: Airport chaos began long before the ground
    // stop]" mid-article, and in the Rich View each was a link.
    // Shapes from stored Atlantic feed bodies: an author-label
    // card, a Read card, and the split "[Read: ]Headline" card.
    let html = format!(
        r#"<html><body><article><p>{BODY_PARA}</p><p><i>[<a href="https://www.theatlantic.com/ideas/2026/11/airport-chaos/689400/">Ian Bogost: Airport chaos began long before the ground stop</a>]</i></p><p>{BODY_PARA}</p><p><i>[<a href="https://www.theatlantic.com/technology/2026/10/ai-agent-chaos-e-commerce/689010/">Read: </a>]</i><a href="https://www.theatlantic.com/technology/2026/10/ai-agent-chaos-e-commerce/689010/">Imagine every AI agent on Earth buying the same cheap paper towels</a></p><p>{BODY_PARA} See <a href="https://example.com/x">[the report]</a>: it matters.</p><p>[Editor's note: This article was updated.]</p></article></body></html>"#
    );
    let url = "https://www.theatlantic.com/ideas/2026/11/flights-grounded-why/689446/";
    let out = clean_article_html(&html, url, false);
    assert!(out.contains(BODY_PARA), "body prose lost: {out}");
    assert!(!out.contains("Ian Bogost"), "author card survived: {out}");
    assert!(
        !out.contains("paper towels"),
        "split card headline survived: {out}"
    );
    assert!(!out.contains("Read: "), "split card label survived: {out}");
    assert!(
        out.contains("it matters"),
        "prose with a bracketed link lost: {out}"
    );
    assert!(
        out.contains("Editor"),
        "editor's note (no link) lost: {out}"
    );
}

/// Windows Latest feed body around its TagDiv gallery (trimmed from
/// the 2026-11-14 iPhone Duo story's feed item).
const WL_GALLERY: &str = r#"<h2>How Google used Gemini to run Windows 11 on an iPhone Duo, and why it&#8217;s a big deal</h2>
<style type="text/css">#tdi_1 .td-doubleSlider-2 .td-item1 { background: url(https://www.windowslatest.com/a-80x60.jpg) 0 0 no-repeat; }</style>
<div id="tdi_1" class="td-gallery td-slide-on-2-columns"><div class="post_td_gallery"><div class="td-gallery-slide-top"><div class="td-gallery-title">Windows 11 running on iPhone Duo</div><div class="td-gallery-controls-wrapper"><div class="td-gallery-slide-count"><span class="td-gallery-slide-item-focus">1</span> of 3</div><div class="td-gallery-slide-prev-next-but"><i class = "td-icon-left doubleSliderPrevButton"></i><i class = "td-icon-right doubleSliderNextButton"></i></div></div></div>
<div class = "td-doubleSlider-1 "><div class = "td-slider"><div class = "td-slide-item td-item1"><figure class="td-slide-galery-figure td-slide-popup-gallery"><a class="slide-gallery-image-link" href="https://www.windowslatest.com/a.jpg" title="Windows 11 desktop on iPhone Duo"><img src="https://www.windowslatest.com/a-560x420.jpg" alt="Windows 11 desktop on iPhone Duo"></a></figure></div></div></div>
<div class = "td-doubleSlider-2"><div class = "td-slider"><div class = "td-button td-item1"><div class = "td-border"></div></div></div></div></div></div>
<p>It&#8217;s not the first time we&#8217;ve seen Windows 11 running on an iPhone, but this project is particularly interesting because Reshi used Gemini to help build it.</p>"#;

#[test]
fn windowslatest_gallery_chrome_and_footer_stripped() {
    // Brandon's report, 2026-11-13: Windows Latest bodies read
    // "Windows 11 running on iPhone Duo / 1 of 3" mid-article in
    // both views; the Rich View also kept the empty featured-image
    // link and the "The post ... appeared first on" footer.
    let url =
        "https://www.windowslatest.com/2026/11/14/windows-11-can-run-on-apples-foldable-iphone/";
    let feed = format!(
        r#"<div><a href="https://www.windowslatest.com/featured.jpg"></a></div><p>{BODY_PARA}</p>{WL_GALLERY}<p>{BODY_PARA}</p><p>The post <a href="{url}">Windows 11 can run on Apple's foldable iPhone</a> appeared first on <a href="https://www.windowslatest.com">Windows Latest</a></p>"#
    );
    let text = blindrss_core::extract::html_to_text_opts(&feed, false);
    assert!(
        text.contains("why it’s a big deal"),
        "section heading lost: {text}"
    );
    assert!(!text.contains("1 of 3"), "slide counter survived: {text}");
    assert!(
        !text.contains("running on iPhone Duo\n"),
        "gallery title survived: {text}"
    );
    let rich = clean_article_html(&feed, url, false);
    assert!(rich.contains(BODY_PARA), "body prose lost: {rich}");
    assert!(rich.contains("big deal"), "section heading lost: {rich}");
    assert!(!rich.contains("of 3"), "slide counter survived: {rich}");
    assert!(
        !rich.contains("td-gallery-title") && !rich.contains(">Windows 11 running on iPhone Duo<"),
        "gallery title survived: {rich}"
    );
    assert!(
        !rich.contains("featured.jpg\"></a>"),
        "empty image link survived: {rich}"
    );
    assert!(
        !rich.contains("appeared first on"),
        "feed footer survived: {rich}"
    );
}
