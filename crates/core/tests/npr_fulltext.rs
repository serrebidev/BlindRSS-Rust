use blindrss_core::extract::{ExtractOutcome, extract_article};

const URL: &str = "https://www.npr.org/2026/11/11/nx-s1-6037497/a-kennedy-relative-talks-about-his-familys-lawsuit-against-trumps-250-foot-arch";
const SUMMARY: &str = "NPR's Scott Detrow speaks with Max Kennedy, son of Robert Kennedy Sr., about the Kennedy family's lawsuit to halt the construction of President Trump's 250-foot arch near Arlington National Cemetery.";

#[test]
fn short_npr_text_edition_keeps_only_story_body() {
    let html = include_str!("fixtures/npr_short_text.html");
    let ExtractOutcome::Article { text, .. } = extract_article(html, URL, None) else {
        panic!("NPR's short summary is a complete article body");
    };
    assert_eq!(text, SUMMARY);
}

#[test]
fn short_npr_main_page_excludes_player_and_disclaimer() {
    let html = format!(
        r#"<html><head><title>Kennedy lawsuit</title></head><body>
        <article><h1>Kennedy lawsuit</h1><div class="audio-module">Download Embed
        <textarea>&lt;iframe title="NPR embedded audio player"&gt;</textarea></div>
        <div id="storytext"><p>{SUMMARY}</p><aside>Sponsor Message</aside></div>
        <div class="anti-fraud-message"><p>NPR does not offer or accept money for coverage or interviews.
        Anyone who makes such an offer or request is not an NPR employee and does not represent NPR.</p></div>
        </article></body></html>"#
    );
    let ExtractOutcome::Article { text, .. } = extract_article(&html, URL, None) else {
        panic!("NPR summary must survive without a transcript");
    };
    assert_eq!(text, SUMMARY);
}

#[test]
fn npr_transcript_keeps_all_paragraphs() {
    let html = r#"<html><head><title>Interview</title></head><body>
        <div class="paragraphs-container"><p>SCOTT DETROW, HOST:</p>
        <p>Today we discuss the lawsuit.</p><p>MAX KENNEDY: Here is our response.</p></div>
        <footer>Terms of Use</footer></body></html>"#;
    let ExtractOutcome::Article { text, .. } = extract_article(html, URL, None) else {
        panic!("transcript must remain readable");
    };
    assert_eq!(
        text,
        "SCOTT DETROW, HOST:\n\nToday we discuss the lawsuit.\n\nMAX KENNEDY: Here is our response."
    );
}

#[test]
fn npr_summary_does_not_replace_fuller_feed() {
    let feed =
        "The feed already contains a complete transcript with additional reporting. ".repeat(20);
    let html = include_str!("fixtures/npr_short_text.html");
    let ExtractOutcome::Article { text, .. } = extract_article(html, URL, Some(&feed)) else {
        panic!("fuller feed must survive");
    };
    assert_eq!(text, feed.trim());
}

#[test]
fn empty_npr_body_keeps_normal_feed_fallback() {
    let feed =
        "Complete reporting from the feed remains available when NPR returns an empty shell. "
            .repeat(20);
    let html = "<html><head><title>Story</title></head><body><div class=\"paragraphs-container\"></div></body></html>";
    let ExtractOutcome::Article { text, source, .. } = extract_article(html, URL, Some(&feed))
    else {
        panic!("empty NPR shell must not discard the feed");
    };
    assert_eq!(text, feed.trim());
    assert_eq!(source, "feed");
}

#[test]
fn npr_body_rule_is_host_scoped() {
    let html = include_str!("fixtures/npr_short_text.html");
    let outcome = extract_article(html, "https://example.org/nx-s1-6037497/story", None);
    assert!(!matches!(
        outcome,
        ExtractOutcome::Article { source: "npr", .. }
    ));
}
