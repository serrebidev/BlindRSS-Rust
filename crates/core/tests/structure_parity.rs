use blindrss_core::article_html::clean_article_html;
use blindrss_core::extract::{StructureOptions, html_to_text_with_structure};

#[test]
fn inline_links_use_the_caret_then_the_first_link_on_its_line() {
    use blindrss_core::article_html::inline_link_at;
    let line = "See first (https://example.com/one), then https://example.com/two.";
    assert_eq!(
        inline_link_at(line, 55).as_deref(),
        Some("https://example.com/two")
    );
    assert_eq!(
        inline_link_at(line, 0).as_deref(),
        Some("https://example.com/one")
    );
    assert_eq!(inline_link_at("No link here", 4), None);
    assert_eq!(inline_link_at("javascript:bad()", 4), None);
    assert_eq!(
        inline_link_at(
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            0
        )
        .as_deref(),
        Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
    );
}

#[test]
fn structure_markers_preserve_nested_lists_quotes_and_links() {
    let options = StructureOptions {
        tables: true,
        headings: true,
        lists: true,
        quotes: true,
        links: true,
    };
    let html = "<h2>Why</h2><ol><li>First<ul><li>Nested</li></ul></li><li>Second</li></ol><blockquote><p>A quote</p></blockquote><p>See <a href='https://example.com/news'>report</a> and <a href='https://example.com/raw'>https://example.com/raw</a>.</p>";
    let text = html_to_text_with_structure(html, false, options);
    for marker in [
        "Heading level 2: Why",
        "1. First",
        "• Nested",
        "2. Second",
        "Quote:",
        "End of quote.",
        "report (https://example.com/news)",
    ] {
        assert!(text.contains(marker), "{marker}: {text}");
    }
    assert!(!text.contains("https://example.com/raw (https://example.com/raw)"));
}

#[test]
fn data_table_associates_headers_but_layout_table_stays_prose() {
    let html = "<table><caption>Results</caption><tr><th>Year</th><th>Patched</th></tr><tr><td>2025</td><td>140</td></tr></table><table><tr><td><p>Article prose</p></td><td>More prose</td></tr></table>";
    let text = html_to_text_with_structure(html, false, StructureOptions::default());
    assert!(
        text.contains("Table with 1 rows and 2 columns: Results."),
        "{text}"
    );
    assert!(text.contains("Row 1: Year: 2025; Patched: 140."), "{text}");
    assert!(text.contains("End of table."));
    assert!(text.contains("Article prose"));
    assert_eq!(text.matches("Table with").count(), 1);
    let plain = html_to_text_with_structure(
        html,
        false,
        StructureOptions {
            tables: false,
            ..Default::default()
        },
    );
    assert!(!plain.contains("Table with"));
}

#[test]
fn disabled_extra_markers_and_unsafe_link_targets_stay_plain() {
    let html = "<h3>Heading</h3><ul><li>Item</li></ul><blockquote>Quote</blockquote><a href='javascript:bad()'>bad</a><a href='https://example.com/two words'>space</a>";
    let text = html_to_text_with_structure(html, false, StructureOptions::default());
    assert!(!text.contains("Heading level"));
    assert!(!text.contains('•'));
    assert!(!text.contains("End of quote"));
    let links = html_to_text_with_structure(
        html,
        false,
        StructureOptions {
            links: true,
            ..Default::default()
        },
    );
    assert!(!links.contains("javascript"));
    assert!(!links.contains("two words"));
}

#[test]
fn configured_structure_reaches_full_text_extraction() {
    let mut config = blindrss_core::config::Config::default();
    for key in ["headings", "lists", "quotes", "links"] {
        config.set_path(&format!("article_structure_{key}"), serde_json::json!(true));
    }
    blindrss_core::extract::configure_text_structure(&config);
    let prose = "The research team carefully checked every observation before publishing these results. Their report describes the methods and evidence in sufficient detail for independent review. ".repeat(8);
    let html = format!(
        "<html><head><title>Research</title></head><body><article><h2>Findings</h2><p>{prose}</p><ul><li>A useful detail for readers.</li></ul><p>{prose}</p></article></body></html>"
    );
    let outcome =
        blindrss_core::extract::extract_article(&html, "https://example.com/research", None);
    blindrss_core::extract::configure_text_structure(&blindrss_core::config::Config::default());
    let blindrss_core::extract::ExtractOutcome::Article { text, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(text.contains("Heading level 2: Findings"), "{text}");
    assert!(text.contains("• A useful detail"), "{text}");
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 (R06): the rich reader gets the same publisher and
// SoCast body reconstructions as the text reader (Python
// clean_article_html runs them before the generic node pick).
// ---------------------------------------------------------------------

const BODY_A: &str = "The first body section explains the discovery in detail, walking through the evidence the team gathered over several months of careful observation and testing.";
const BODY_B: &str = "The second body section describes what the finding means in practice, quoting researchers who were not involved in the work and who urge cautious optimism today.";

#[test]
fn rich_ars_reconstruction_merges_post_content_parts_and_drops_junk() {
    let html = format!(
        "<html><body><div class=\"sidebar\">Sidebar promo card text that is not part of the story.</div><article><div class=\"card-text\">CARDJUNK teaser text inside the article but outside the body containers.</div><div class=\"post-content\"><p>{BODY_A}</p><div class=\"ars-interlude-container\"><p>INTERLUDEJUNK subscribe panel.</p></div></div><div class=\"post-content\"><p>{BODY_B}</p></div></article></body></html>"
    );
    let rich = clean_article_html(
        &html,
        "https://arstechnica.com/science/2026/11/story/",
        true,
    );
    assert!(rich.contains("first body section"), "{rich}");
    assert!(rich.contains("second body section"), "{rich}");
    assert!(!rich.contains("CARDJUNK"), "non-body card kept: {rich}");
    assert!(!rich.contains("INTERLUDEJUNK"), "interlude kept: {rich}");
    assert!(!rich.contains("Sidebar promo"), "{rich}");
}

#[test]
fn rich_zdnet_reconstruction_unwraps_related_flow_and_drops_disqus() {
    let html = format!(
        "<html><body><article><div class=\"teaser-card\">CARDJUNK read-next teaser.</div><div class=\"entry-content\"><p>{BODY_A}</p><div class=\"zd-related-stories-flow\"><p>RELATEDFLOWPROSE that ZDNet counts as article prose.</p></div><p>{BODY_B}</p><div class=\"zd-disqus\"><p>DISQUSJUNK comment widget.</p></div></div></article></body></html>"
    );
    let rich = clean_article_html(&html, "https://www.zdnet.com/article/story/", true);
    assert!(rich.contains("first body section"), "{rich}");
    assert!(rich.contains("second body section"), "{rich}");
    assert!(
        rich.contains("RELATEDFLOWPROSE"),
        "unwrapped prose lost: {rich}"
    );
    assert!(!rich.contains("DISQUSJUNK"), "disqus kept: {rich}");
    assert!(!rich.contains("CARDJUNK"), "teaser kept: {rich}");
}

#[test]
fn rich_macrumors_and_register_reconstructions() {
    let mac = format!(
        "<html><body><article><div class=\"ad-slot\">ADJUNK banner.</div><div class=\"js-content\"><p>{BODY_A}</p></div><div class=\"js-content\"><p>{BODY_B}</p></div></article></body></html>"
    );
    let rich = clean_article_html(&mac, "https://www.macrumors.com/2026/11/04/story/", true);
    assert!(rich.contains("first body section"), "{rich}");
    assert!(rich.contains("second body section"), "{rich}");
    assert!(!rich.contains("ADJUNK"), "{rich}");

    let reg = format!(
        "<html><body><article><div class=\"bodytext\"><p>{BODY_A}</p><p>{BODY_B}</p></div><div class=\"related-links\">RELJUNK more coverage links.</div></article></body></html>"
    );
    let rich = clean_article_html(&reg, "https://www.theregister.com/2026/11/04/story/", true);
    assert!(rich.contains("first body section"), "{rich}");
    assert!(rich.contains("second body section"), "{rich}");
    assert!(!rich.contains("RELJUNK"), "{rich}");
}

#[test]
fn rich_socast_reconstruction_merges_split_story() {
    // The story is split across two mainArticle elements with
    // related/newsletter widgets interleaved; the generic pick
    // alone keeps only the lead.
    let html = format!(
        "<html><body><article class=\"mainArticle\"><p>{BODY_A}</p><div class=\"items-wrapper\"><p>WIDGETJUNK related stories.</p></div></article><div class=\"bnl-title\"><p>BNLJUNK newsletter signup.</p></div><article class=\"mainArticle\"><p>{BODY_B}</p></article><script src=\"https://socastsrm.com/embed.js\"></script></body></html>"
    );
    let rich = clean_article_html(&html, "https://www.example-radio.com/news/story", true);
    assert!(rich.contains("first body section"), "lead lost: {rich}");
    assert!(
        rich.contains("second body section"),
        "continuation lost: {rich}"
    );
    assert!(!rich.contains("WIDGETJUNK"), "{rich}");
    assert!(!rich.contains("BNLJUNK"), "{rich}");
}

#[test]
fn rich_socast_lead_wrapper_variant() {
    // Theme variant: the lead lives in div.wpb-content-wrapper
    // inside a plain article; the continuation is a mainArticle.
    let html = format!(
        "<html><body><article><div class=\"wpb-content-wrapper\"><p>{BODY_A}</p></div></article><article class=\"mainArticle\"><p>{BODY_B}</p></article><script src=\"https://socastsrm.com/embed.js\"></script></body></html>"
    );
    let rich = clean_article_html(&html, "https://www.example-radio.com/news/other", true);
    assert!(rich.contains("first body section"), "lead lost: {rich}");
    assert!(
        rich.contains("second body section"),
        "continuation lost: {rich}"
    );
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 (R07): CSS-only quizzes never reach the rich reader
// either (Python strips them inside clean_article_html, before the
// trafilatura ground-truth text is computed).
// ---------------------------------------------------------------------

#[test]
fn rich_reader_strips_css_quiz() {
    let prose = "I spent a whole month typing on a phone with a physical keyboard, and the experience changed how I think about touchscreens and the apps I reach for every single day. ";
    let quiz = r##"<div class="vq">
      <input type="radio" id="vq-s0" name="vq-nav" checked>
      <input type="radio" id="vq-s1" name="vq-nav">
      <input type="radio" id="vq-1a" name="vq-1"><input type="radio" id="vq-1b" name="vq-1">
      <input type="radio" id="vq-2a" name="vq-2"><input type="radio" id="vq-2b" name="vq-2">
      <section class="vq-panel"><h2>The History of Android OS Trivia Challenge</h2>
      <p>How well do you know the remarkable journey of Android?</p></section>
      <section class="vq-panel"><h2>Your Score</h2><p>Thanks for playing!</p></section>
    </div>"##;
    let html = format!(
        "<html><body><main><article><p>{}{}{}</p>{}<p>{}{}{}</p></article></main></body></html>",
        prose, prose, prose, quiz, prose, prose, prose
    );
    let rich = clean_article_html(&html, "https://www.howtogeek.com/phone-keyboard/", true);
    for marker in ["Trivia Challenge", "Your Score", "Thanks for playing"] {
        assert!(!rich.contains(marker), "quiz marker in rich: {marker}");
    }
    assert!(rich.contains("typing on a phone"), "{rich}");
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 investigation: relative srcset through the
// sanitizer. Python absolutizes every srcset candidate against the
// article URL; the rich output must carry absolute candidates too.
// ---------------------------------------------------------------------

#[test]
fn rich_srcset_candidates_are_absolutized() {
    let html = format!(
        "<html><body><article><p>{BODY_A}</p><p>{BODY_B}</p><p><img src=\"pic.jpg\" srcset=\"small.jpg 480w, /static/mid.jpg 800w, https://cdn.test/big.jpg 1200w\" alt=\"chart\"></p></article></body></html>"
    );
    let rich = clean_article_html(&html, "https://example.com/news/story", true);
    assert!(
        rich.contains("https://example.com/news/small.jpg 480w"),
        "relative srcset candidate not absolutized: {rich}"
    );
    assert!(
        rich.contains("https://example.com/static/mid.jpg 800w"),
        "root-relative srcset candidate not absolutized: {rich}"
    );
    assert!(rich.contains("https://cdn.test/big.jpg 1200w"), "{rich}");
}

#[test]
fn semantic_source_documents_keep_replies_and_links_in_both_readers() {
    let html = "<html><head><title>Topic</title></head><body><h1>Topic</h1><section class='blindrss-groupsio-post'><h2>#1 Message by Alice</h2><p>First message.</p></section><section><h2>#2 Message by Bob</h2><p>Reply with <a href='https://example.com/reference'>reference</a>.</p><script>unsafe_script()</script><a href='javascript:unsafe()'>Unsafe target</a></section></body></html>";
    let rich = blindrss_core::article_html::clean_article_html(
        html,
        "https://groups.io/g/readers/topic/123",
        false,
    );
    for content in [
        "Message by Alice",
        "Message by Bob",
        "First message.",
        "https://example.com/reference",
    ] {
        assert!(rich.contains(content), "missing {content}: {rich}");
    }
    assert!(!rich.contains("unsafe_script"));
    assert!(!rich.contains("javascript:"));
    let plain = html_to_text_with_structure(
        &rich,
        false,
        StructureOptions {
            headings: true,
            links: true,
            ..Default::default()
        },
    );
    assert!(
        plain.contains("Heading level 2: #1 Message by Alice"),
        "{plain}"
    );
    assert!(
        plain.contains("Heading level 2: #2 Message by Bob"),
        "{plain}"
    );
    assert!(
        plain.contains("reference (https://example.com/reference)"),
        "{plain}"
    );
}
