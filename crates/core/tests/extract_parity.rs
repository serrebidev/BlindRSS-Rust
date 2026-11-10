//! Slice 6 tests: ported Python gates and dedicated extractors.
//! Inline pages mirror the shapes the Python regression tests use.

use blindrss_core::extract::{
    ExtractOutcome, expand_sky_flourish_tables, extract_article, flourish_tables_html,
    strip_css_quizzes,
};

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

// ---------------------------------------------------------------------
// Brandon's full-text report, 2026-10-30: the 9to5Mac WhatsApp piece
// came out with the page's footer boilerplate attached (Amazon
// picks, FTC disclosure) because the base engine ran balanced and
// the pick preferred length. Python runs trafilatura
// precision-first; precision mode on this exact page reproduces
// Python's output character for character in length and ending.
// ---------------------------------------------------------------------

#[test]
fn nine_to_five_mac_has_no_footer_boilerplate() {
    let html = include_str!("fixtures/9to5mac_whatsapp.html");
    let url = "https://9to5mac.com/2026/10/30/whatsapp-announces-four-new-features-to-make-group-chats-easier-to-manage/";
    let ExtractOutcome::Article { text, .. } = extract_article(html, url, None) else {
        panic!("expected an article outcome");
    };
    assert!(text.contains("pin messages indefinitely"), "body missing");
    assert!(
        text.trim_end().ends_with("follow this link."),
        "article should end at the last body paragraph, tail: {}",
        &text[text.len().saturating_sub(120)..]
    );
    for junk in [
        "Worth checking out",
        "FTC:",
        "reading 9to5Mac",
        "MacBook Neo",
        "Marcus Mendes is a Brazilian",
    ] {
        assert!(!text.contains(junk), "boilerplate leaked: {junk}");
    }
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 (R05): Sky Flourish tables. Fixtures mirror Python
// tests/test_sky_flourish_tables.py: the dataset lives only in the
// embed's _Flourish_data JS variables, with typed new Date() cells.
// ---------------------------------------------------------------------

const SKY_PAGE: &str = r##"
<html><head><title>Sky eclipse table</title></head><body><article>
<p>The eclipse will be visible across the country, with timings varying by location.</p>
<p>First contact timings for each city are shown in the interactive table below.</p>
<div class="sdc-article-widget sdc-article-custom-markup">
  <div class="flourish-embed flourish-table"
       data-src="visualisation/29649536?2559214">
    <script src="https://public.flourish.studio/resources/embed.js"></script>
    <noscript><img alt="table visualization"></noscript>
  </div>
</div>
<p>The moon will then move slowly in front of the sun until maximum coverage.</p>
<p>Viewers should use certified eclipse glasses throughout the partial eclipse.</p>
</article></body></html>
"##;

const FLOURISH_EMBED: &str = r##"
<html><body><script>
var _Flourish_data_column_names = {
  "rows":{"columns":["Stage","Norwich","London, Bristol and Cardiff"]}
},
_Flourish_data_metadata = {
  "rows":{"columns":[
    {"type":"string","output_format_id":"string$arbitrary_string"},
    {"type":"datetime","output_format_id":"datetime$%-I.%M%p"},
    {"type":"datetime","output_format_id":"datetime$%-I.%M%p"}
  ]}
},
_Flourish_data = {"rows":[
  {"columns":["First contact",new Date(-2208923100000),new Date(-2208922980000)]},
  {"columns":["Eclipse maximum",new Date(-2208919740000),new Date(-2208919680000)]},
  {"columns":["End of eclipse",new Date(-2208916560000),new Date(-2208916440000)]}
]};
</script></body></html>
"##;

#[test]
fn flourish_decoder_builds_semantic_tables_from_js_data() {
    let tables = flourish_tables_html(FLOURISH_EMBED);
    assert!(tables.contains("<table"), "{tables}");
    for heading in ["Stage", "Norwich", "London, Bristol and Cardiff"] {
        assert!(
            tables.contains(&format!("<th scope=\"col\">{heading}</th>")),
            "heading {heading}: {tables}"
        );
    }
    // Typed dates render through the published d3-style format.
    assert!(tables.contains("<td>First contact</td>"), "{tables}");
    assert!(tables.contains("<td>6.15PM</td>"), "{tables}");
    assert!(tables.contains("<td>6.17PM</td>"), "{tables}");
    assert!(tables.contains("<td>8.04PM</td>"), "{tables}");
    assert!(!tables.contains("new Date"), "{tables}");
    // The expanded table linearizes like Python's classic reader.
    let page = format!("<article><p>Intro.</p>{tables}</article>");
    let text = blindrss_core::extract::html_to_text_with_structure(
        &page,
        false,
        blindrss_core::extract::StructureOptions {
            tables: true,
            ..Default::default()
        },
    );
    assert!(text.contains("Table with 3 rows and 3 columns."), "{text}");
    assert!(
        text.contains(
            "Row 1: Stage: First contact; Norwich: 6.15PM; London, Bristol and Cardiff: 6.17PM."
        ),
        "{text}"
    );
    assert!(
        text.contains("Row 3: Stage: End of eclipse; Norwich: 8.04PM"),
        "{text}"
    );
}

#[test]
fn flourish_decoder_binding_key_rows_and_split_parts() {
    // Templates without a "columns" list map binding keys to
    // human-readable names; >100 rows split into captioned parts.
    let rows: String = (0..101)
        .map(|i| format!("{{\"when\":\"Day {i}\",\"what\":\"Event {i}\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    let embed = format!(
        "<script>var _Flourish_data_column_names = {{\"rows\":{{\"when\":\"When\",\"what\":\"What\"}}}}, _Flourish_data = {{\"rows\":[{rows}]}};</script>"
    );
    let tables = flourish_tables_html(&embed);
    assert!(tables.contains("<th scope=\"col\">When</th>"), "{tables}");
    assert!(tables.contains("<td>Day 0</td>"), "{tables}");
    assert!(
        tables.contains("<caption>Table, part 1 of 2</caption>"),
        "{tables}"
    );
    assert!(
        tables.contains("<caption>Table, part 2 of 2</caption>"),
        "{tables}"
    );
    assert!(tables.contains("<td>Day 100</td>"), "{tables}");
    // No decodable dataset -> no tables.
    assert_eq!(flourish_tables_html("<html><body>plain</body></html>"), "");
    assert_eq!(
        flourish_tables_html("<script>var _Flourish_data = {\"rows\":[]};</script>"),
        ""
    );
}

#[test]
fn flourish_expansion_is_sky_only_and_needs_the_marker() {
    // Non-Sky pages never fetch (the guard returns before any I/O).
    assert_eq!(
        expand_sky_flourish_tables(SKY_PAGE, "https://example.com/article"),
        SKY_PAGE
    );
    // A Sky page without a Flourish table returns untouched.
    let plain = "<html><body><article><p>No widgets here.</p></article></body></html>";
    assert_eq!(
        expand_sky_flourish_tables(plain, "https://news.sky.com/story/x-123"),
        plain
    );
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 (R07): CSS-only quizzes. Fixtures mirror Python
// tests/test_article_quiz_removal.py (the How-To Geek vq widget).
// ---------------------------------------------------------------------

const QUIZ_PROSE: &str = "I spent a whole month typing on a phone with a physical keyboard, and the experience changed how I think about touchscreens and the apps I reach for every single day. ";
const QUIZ_AFTER: &str = "However, my favorite thing about the keyboard turned out to be the shortcuts, because every key can launch an app or a contact from anywhere on the phone. ";
const QUIZ_WIDGET: &str = r##"
<div class="w-display-card-vignette"><div class="vq">
  <input type="radio" id="vq-s0" name="vq-nav" checked>
  <input type="radio" id="vq-s1" name="vq-nav">
  <input type="radio" id="vq-1a" name="vq-1"><input type="radio" id="vq-1b" name="vq-1">
  <input type="radio" id="vq-2a" name="vq-2"><input type="radio" id="vq-2b" name="vq-2">
  <div class="vq-wrap">
    <section class="vq-panel"><h2>The History of Android OS<br>Trivia Challenge</h2>
    <p>From a scrappy startup to the most popular operating system, how well do you
    know the remarkable journey of Android and everyone who worked on it?</p></section>
    <section class="vq-panel"><p>In what year was Android Inc. originally founded by
    Andy Rubin and his co-founders in California?</p>
    <div class="vq-fb">Correct! Android Inc. was founded in October 2003 in Palo Alto,
    California, before the team pivoted to building software for mobile phones.</div>
    <label class="vq-btn" for="vq-sR">See My Score</label></section>
    <section class="vq-panel"><h2>Your Score</h2><p>Thanks for playing!</p></section>
  </div>
</div></div>
"##;

fn quiz_page(middle: &str) -> String {
    format!(
        "<html><head><title>Keyboard phone</title></head><body><main><article><section id=\"article-body\"><p>{}{}{}</p>{}<h2>I love this keyboard</h2><p>{}{}{}</p></section></article></main></body></html>",
        QUIZ_PROSE, QUIZ_PROSE, QUIZ_PROSE, middle, QUIZ_AFTER, QUIZ_AFTER, QUIZ_AFTER
    )
}

#[test]
fn css_quiz_removed_and_prose_kept() {
    let out = strip_css_quizzes(&quiz_page(QUIZ_WIDGET));
    for marker in [
        "Trivia Challenge",
        "founded by",
        "Correct!",
        "Your Score",
        "Thanks for playing",
    ] {
        assert!(!out.contains(marker), "quiz marker leaked: {marker}");
    }
    assert!(out.contains("typing on a phone"));
    assert!(out.contains("launch an app"));
    // End to end through the classic extractor.
    let (text, _) = article_text(extract_article(
        &quiz_page(QUIZ_WIDGET),
        "https://www.howtogeek.com/phone-keyboard/",
        None,
    ));
    for marker in ["Trivia Challenge", "Correct!", "Thanks for playing"] {
        assert!(!text.contains(marker), "quiz marker in text: {marker}");
    }
    assert!(text.contains("typing on a phone"), "{text}");
}

#[test]
fn css_quiz_guards_keep_polls_tabs_and_wrappers() {
    // One radio group is a poll / CSS tabs: content stays, byte-identical.
    let tabs = "<div class=\"tabs\"><input type=\"radio\" name=\"tab\" id=\"t1\" checked><input type=\"radio\" name=\"tab\" id=\"t2\"><section><p>The first tab explains the real specifications.</p></section></div>";
    let page = quiz_page(tabs);
    assert_eq!(strip_css_quizzes(&page), page);
    // Radios directly under a page wrapper are never a removable widget.
    let radios: String = (0..4)
        .map(|i| format!("<input type=\"radio\" name=\"g{i}\">"))
        .collect();
    let page = format!(
        "<html><body>{radios}<p>{}</p></body></html>",
        QUIZ_PROSE.trim()
    );
    assert!(strip_css_quizzes(&page).contains(QUIZ_PROSE.trim()));
    let wrapped = format!(
        "<html><body><form>{radios}<p>{}</p></form></body></html>",
        QUIZ_PROSE.trim()
    );
    assert!(strip_css_quizzes(&wrapped).contains(QUIZ_PROSE.trim()));
    // No radios at all: untouched, and empty stays empty.
    let page = quiz_page("<p>No widget here at all.</p>");
    assert_eq!(strip_css_quizzes(&page), page);
    assert_eq!(strip_css_quizzes(""), "");
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 (R08): JSON-LD bodies keep their opening prose and
// paragraph structure, and a missing lede is re-attached once.
// ---------------------------------------------------------------------

#[test]
fn json_ld_body_keeps_standfirst_and_paragraph_structure() {
    // Sky shape: the standfirst sits before the first <p> inside
    // articleBody; the flattened single-line form loses both.
    let body = format!(
        "The standfirst sentence introduces this story for the reader.<p>{LONG_PARA}</p><p>{LONG_PARA}</p>"
    );
    let payload = serde_json::json!({"@type":"NewsArticle","articleBody": body}).to_string();
    let html = format!(
        "<html><head><script type=\"application/ld+json\">{payload}</script></head><body><nav>Menu links only</nav></body></html>"
    );
    let (text, source) = article_text(extract_article(&html, "https://example.com/skyish", None));
    assert_eq!(source, "jsonld", "{text}");
    assert!(
        text.starts_with("The standfirst sentence introduces this story"),
        "{text}"
    );
    // Paragraphs stay separate paragraphs, not one flattened line.
    let paras: Vec<&str> = text
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .collect();
    assert!(paras.len() >= 3, "paragraphs flattened: {text}");
}

#[test]
fn json_ld_missing_lede_is_reattached_from_dom_paragraphs() {
    // Wired shape: articleBody drops the styled lede; the DOM
    // <article> still carries it (plus one body paragraph — the
    // rendered page is paywall-thin, so the JSON-LD body wins and
    // the patch must re-attach the lede from the DOM paragraphs).
    let lede = "The lede paragraph opens the story with a styled drop cap and sets up everything that follows for the reader.";
    let json_body = format!("{LONG_PARA} {LONG_PARA} {LONG_PARA} {LONG_PARA}");
    let payload = serde_json::json!({"@type":"NewsArticle","articleBody": json_body}).to_string();
    let dom_paras = format!("<p>{lede}</p><p>{LONG_PARA}</p>");
    let html = format!(
        "<html><head><script type=\"application/ld+json\">{payload}</script></head><body><header>Site header links and taglines for the whole publication network.</header><article>{dom_paras}</article><footer>Footer legal text and more links.</footer></body></html>"
    );
    let (text, source) = article_text(extract_article(&html, "https://example.com/wiredish", None));
    assert_eq!(source, "jsonld", "{text}");
    assert!(text.starts_with(lede), "lede not re-attached: {text}");
    assert_eq!(text.matches(lede).count(), 1, "lede duplicated: {text}");
}

#[test]
fn json_ld_page_with_script_code_still_yields_prose() {
    // Mashable shape: the articleBody carries leaked JS as
    // trailing "text". The JSON-LD candidate itself is stripped
    // before it competes on length — pinned directly in
    // extract.rs's unit tests (json_ld_candidate_strips_leaked_
    // script_code). End to end, this test pins only that the
    // prose survives: on a body-less synthetic page the base
    // engine (rust-trafilatura) mines the same ld+json body,
    // flattens it to a single line, and wins on bulk, so the
    // line-based postprocess strip cannot fire on its text —
    // a recorded engine divergence in PARITY.md (Task 14), not
    // something the candidate layer controls.
    let code = "let cbeScripts = {};\ndocument.querySelectorAll('.cbe').forEach(function (item) {\n  new cbeScriptObserver(item, cbeScripts[item]);\n});\nwindow.addEventListener('load', function () { console.log('ready'); });";
    let body = format!("{LONG_PARA}\n{LONG_PARA}\n{code}");
    let payload = serde_json::json!({"@type":"NewsArticle","articleBody": body}).to_string();
    let html = format!(
        "<html><head><script type=\"application/ld+json\">{payload}</script></head><body><article><p>{LONG_PARA}</p><p>{LONG_PARA}</p></article></body></html>"
    );
    let (text, _) = article_text(extract_article(
        &html,
        "https://example.com/mashableish",
        None,
    ));
    assert!(text.contains("substantial paragraph"), "{text}");
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 (R09): lead recovery is allowlist-gated, guarded by
// the meta description, and never slices text at a byte offset.
// ---------------------------------------------------------------------

const WM_DESC: &str = "The city council approved the new tram line on Tuesday after a long debate, and construction is now expected to begin early next spring.";

#[test]
fn lead_recovery_prepends_meta_description_on_allowlisted_host() {
    let paras: String = (0..5)
        .map(|i| {
            format!("<p>{LONG_PARA} Section {i} adds further confirmed detail to the report.</p>")
        })
        .collect();
    let html = format!(
        "<html><head><meta property=\"og:description\" content=\"{WM_DESC}\"></head><body><article>{paras}</article></body></html>"
    );
    let (text, _) = article_text(extract_article(
        &html,
        "https://www.wirtualnemedia.pl/artykul/tram-line",
        None,
    ));
    assert!(text.starts_with(WM_DESC), "lead not recovered: {text}");
    assert_eq!(text.matches(WM_DESC).count(), 1, "{text}");
}

#[test]
fn lead_recovery_uses_allowlisted_lead_node_when_it_matches_desc() {
    let paras: String = (0..5)
        .map(|i| {
            format!("<p>{LONG_PARA} Section {i} adds further confirmed detail to the report.</p>")
        })
        .collect();
    let html = format!(
        "<html><head><meta property=\"og:description\" content=\"{WM_DESC}\"></head><body><div class=\"wm-article-header-lead\">{WM_DESC}</div><article>{paras}</article></body></html>"
    );
    let (text, _) = article_text(extract_article(
        &html,
        "https://wirtualnemedia.pl/artykul/tram-line",
        None,
    ));
    assert!(text.starts_with(WM_DESC), "lead node not used: {text}");
    assert_eq!(text.matches(WM_DESC).count(), 1, "{text}");
}

#[test]
fn lead_recovery_never_runs_on_other_hosts() {
    // Same page, different host: no description prepend, and no
    // generic recall-paragraph prepend either (Python's recovery
    // is allowlist-only; unrelated prose must stay out).
    let paras: String = (0..5)
        .map(|i| {
            format!("<p>{LONG_PARA} Section {i} adds further confirmed detail to the report.</p>")
        })
        .collect();
    let html = format!(
        "<html><head><meta property=\"og:description\" content=\"{WM_DESC}\"></head><body><article>{paras}</article></body></html>"
    );
    let (text, _) = article_text(extract_article(&html, "https://example.com/story", None));
    assert!(
        !text.contains(WM_DESC),
        "desc leaked on foreign host: {text}"
    );
}

#[test]
fn lead_recovery_ignores_unrelated_lead_node() {
    // The allowlisted lead node does not overlap the description:
    // it must not be prepended (the description fallback may be).
    let unrelated = "A completely different opening about football results and transfer rumours from the weekend league matches.";
    let paras: String = (0..5)
        .map(|i| {
            format!("<p>{LONG_PARA} Section {i} adds further confirmed detail to the report.</p>")
        })
        .collect();
    let html = format!(
        "<html><head><meta property=\"og:description\" content=\"{WM_DESC}\"></head><body><div class=\"wm-article-header-lead\">{unrelated}</div><article>{paras}</article></body></html>"
    );
    let (text, _) = article_text(extract_article(
        &html,
        "https://wirtualnemedia.pl/artykul/tram-line",
        None,
    ));
    assert!(
        !text.starts_with(unrelated),
        "unrelated lead prepended: {text}"
    );
}

#[test]
fn extraction_survives_multibyte_char_at_byte_120() {
    // The old lean recovery sliced the first paragraph's key at
    // byte 120, panicking when a multibyte character straddled it.
    let first = format!(
        "{}é{}",
        "a".repeat(119),
        " and the story continues with plenty more detail for everyone reading along today."
    );
    let paras = format!("<p>{first}</p><p>{LONG_PARA}</p><p>{LONG_PARA}</p><p>{LONG_PARA}</p>");
    let html = format!("<html><body><article>{paras}</article></body></html>");
    let (text, _) = article_text(extract_article(&html, "https://example.com/unicode", None));
    assert!(text.contains("story continues"), "{text}");
}

#[test]
fn cbc_story_extracts_in_full_not_the_feed_lede() {
    let html = include_str!("fixtures/cbc_syndicated.html");
    // The CBC feed supplies only this lede paragraph; Brandon's
    // reader showed exactly this and nothing more.
    let lede = "Canada's foreign affairs minister says the decision to let Russia back into the ice hockey world championships came as a \"great disappointment\" on Friday, as the country's ongoing invasion of Ukraine stretches toward its fifth year.";
    let ExtractOutcome::Article { text, source, .. } =
        extract_article(html, "https://www.byteseu.com/2486759/", Some(lede))
    else {
        panic!("expected an article outcome");
    };
    assert_ne!(source, "feed", "must not fall back to the feed lede");
    assert!(text.contains("Power & Politics"), "middle missing");
    assert!(
        text.contains("100-year partnership"),
        "ending missing — extraction truncated"
    );
    assert!(
        text.chars().count() > 2_500,
        "suspiciously short: {}",
        text.chars().count()
    );
}

#[test]
fn bbc_published_label_carries_its_date() {
    // BBC's metadata strip is a visually-hidden "Published" plus a <time> the
    // engines drop; the reader showed a bare "Published" (reported 2026-11-09).
    let html = include_str!("fixtures/bbc_published_label.html");
    let (text, _) = article_text(extract_article(
        html,
        "https://www.bbc.co.uk/news/articles/cqg7x7dpvym8o?at_medium=RSS&at_campaign=rss",
        None,
    ));
    let first = text.lines().next().unwrap_or("");
    assert_eq!(first, "Published 9 November 2026", "lead: {:.120}", text);
}

#[test]
fn macrumors_byline_is_its_own_paragraph() {
    // Readability's raw text ran the byline into the first paragraph:
    // "...3:07 pm PST by Juli CloverThe iOS 27.2.1..." (reported 2026-11-09).
    let html = include_str!("fixtures/macrumors_byline.html");
    let (text, _) = article_text(extract_article(
        html,
        "https://www.macrumors.com/2026/11/09/ios-27-2-1-security-fixes/",
        None,
    ));
    assert!(!text.contains("Juli CloverThe"), "{:.200}", text);
    let mut paras = text.split("\n\n");
    assert_eq!(
        paras.next(),
        Some("Monday November 9, 2026 3:07 pm PST by Juli Clover")
    );
    assert!(
        paras
            .next()
            .unwrap_or("")
            .starts_with("The iOS 27.2.1, iPadOS 27.2.1")
    );
}
