//! Batch 25 (Audit 4): the postprocess stage port (Python
//! `_postprocess_extracted_text` and its strips) and the missing
//! site-specific extractors (socast, Reuters, GSMArena, publisher
//! body reconstruction). Cases ported from Python's
//! tests/test_boilerplate_removal.py, test_socast_extraction.py,
//! test_arstechnica_extraction.py and test_leaked_script_source.py.

use blindrss_core::extract::{ExtractOutcome, extract_article};
use blindrss_core::postprocess::{
    is_github_page_url, postprocess_extracted_text, published_date_label, strip_edge_nav_runs,
    strip_proxy_trailing_boilerplate,
};

fn unique_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn pp(text: &str, url: &str) -> String {
    postprocess_extracted_text(text, url, "")
}

fn article_text(html: &str, url: &str) -> String {
    match extract_article(html, url, None) {
        ExtractOutcome::Article { text, .. } => text,
        other => panic!("expected Article, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// BBC published line + date label (Python TestBbcPublishedLine)
// ---------------------------------------------------------------------

#[test]
fn bbc_published_line_completed_with_date() {
    let cleaned = postprocess_extracted_text(
        "- Published\nCryptocurrency giant Binance said on Thursday.",
        "https://www.bbc.co.uk/news/articles/cwg7xp8kx4v5o",
        "30 October 2026",
    );
    assert_eq!(
        cleaned,
        "- Published 30 October 2026\nCryptocurrency giant Binance said on Thursday."
    );
}

#[test]
fn bbc_published_line_dropped_without_date() {
    let cleaned = pp(
        "- Published\nCryptocurrency giant Binance said on Thursday.",
        "https://www.bbc.co.uk/news/articles/cwg7xp8kx4v5o",
    );
    assert_eq!(cleaned, "Cryptocurrency giant Binance said on Thursday.");
}

#[test]
fn bbc_completed_line_untouched_and_other_sites_untouched() {
    let text = "- Published 29 October 2026\nBody.";
    assert_eq!(
        pp(text, "https://www.bbc.co.uk/news/articles/cwg7xp8kx4v5o"),
        text
    );
    let body = "- Published\nCryptocurrency giant Binance said on Thursday.";
    assert_eq!(
        postprocess_extracted_text(body, "https://example.com/story", "30 October 2026"),
        body
    );
}

#[test]
fn published_date_label_sources() {
    assert_eq!(
        published_date_label("<script>{\"datePublished\":\"2026-10-30T00:37:59.699Z\"}</script>"),
        "30 October 2026"
    );
    assert_eq!(
        published_date_label(
            "<meta property=\"article:published_time\" content=\"2025-01-05T10:00:00Z\">"
        ),
        "5 January 2025"
    );
    assert_eq!(
        published_date_label("<html><body>no dates</body></html>"),
        ""
    );
    assert_eq!(published_date_label(""), "");
    assert_eq!(
        published_date_label("{\"datePublished\":\"not a date\"}"),
        ""
    );
}

// ---------------------------------------------------------------------
// Host strips (Python TestBoilerplateRemoval)
// ---------------------------------------------------------------------

#[test]
fn strip_9to5mac() {
    let text = "\nFTC: We use income earning auto affiliate links. More.\nYou’re reading 9to5Mac — experts who break news about Apple and its surrounding ecosystem, day after day. Be sure to check out our homepage for all the latest news, and follow 9to5Mac on Twitter, Facebook, and LinkedIn to stay in the loop. Don’t know where to start? Check out our exclusive stories, reviews, how-tos, and subscribe to our YouTube channelRyan got his start in journalism as an Editor at MacStories...\nReal Content Here.\n";
    let cleaned = pp(text, "https://9to5mac.com/some-article");
    assert!(!cleaned.contains("FTC: We use"));
    assert!(!cleaned.contains("You’re reading 9to5Mac"));
    assert!(!cleaned.contains("subscribe to our YouTube channel"));
    assert!(cleaned.contains("Real Content Here"));
}

#[test]
fn strip_9to5_current_promos() {
    let mac = "Real article.\nWorth checking out on Amazon\n- AirTag\n- AirPods";
    assert_eq!(pp(mac, "https://9to5mac.com/story"), "Real article.");
    let google =
        "Real article.\nJoin 9to5Google Pro to get more from your favorite site\n- Discord";
    assert_eq!(pp(google, "https://9to5google.com/story"), "Real article.");
    let toys = "Real deal.\nYou’re reading 9to5Toys — experts digging up all the latest deals.";
    assert_eq!(pp(toys, "https://9to5toys.com/story"), "Real deal.");
}

#[test]
fn strip_postmillennial() {
    let text =
        "Real final paragraph.\nJoin and support independent free thinkers!\nWe’re independent.";
    assert_eq!(
        pp(text, "https://thepostmillennial.com/story"),
        "Real final paragraph."
    );
}

#[test]
fn strip_globalnews() {
    let text = "\nBy Staff The Canadian Press\nPosted December 31, 2025 5:28 pm\n1 min read\nIf you get Global News from Instagram or Facebook - that will be changing. Find out how you can still connect with us.\nHide message barDescrease article font size\nIncrease article font size\nReal Article Content.\n";
    let cleaned = pp(text, "https://globalnews.ca/news/123");
    assert!(!cleaned.contains("By Staff"));
    assert!(!cleaned.contains("Posted December"));
    assert!(!cleaned.contains("1 min read"));
    assert!(!cleaned.contains("If you get Global News"));
    assert!(!cleaned.contains("Hide message bar"));
    assert!(cleaned.contains("Real Article Content"));
}

#[test]
fn strip_aljazeera() {
    let text = "\nPublished On 6 Jan 20266 Jan 2026\nClick here to share on social media\nshare2Save\nReal Content.\n";
    let cleaned = pp(text, "https://www.aljazeera.com/news");
    assert!(!cleaned.contains("Published On"));
    assert!(!cleaned.contains("Click here to share"));
    assert!(!cleaned.contains("share2Save"));
    assert!(cleaned.contains("Real Content"));
}

#[test]
fn strip_aljazeera_recommended_stories_list() {
    let text = "First real paragraph before the widget.\nRecommended Stories\nlist of 3 items- list 1 of 3‘False narrative’: Families challenge Trump’s visa suspension\n- list 2 of 3Pope Leo, Mamdani send pro-immigrant message\n- list 3 of 3At 250, America is still deciding who belongs\nSecond real paragraph after the widget.";
    let cleaned = pp(text, "https://www.aljazeera.com/news/x");
    assert!(cleaned.contains("First real paragraph"));
    assert!(cleaned.contains("Second real paragraph"));
    assert!(!cleaned.contains("list of 3 items"));
    assert!(!cleaned.contains("Recommended Stories"));
    assert!(!cleaned.contains("America is still deciding who belongs"));
}

#[test]
fn strip_bbc_sharesave() {
    let text = "\nShareSave\nAna Faguyon Capitol Hill\nShareSave\nReal Content.\n";
    let cleaned = pp(text, "https://www.bbc.com/news/articles/cgl8y4gx9lyo");
    assert!(!cleaned.contains("ShareSave"));
    assert!(cleaned.contains("Real Content"));
}

#[test]
fn strip_canada() {
    let text = "\nAdvertisement 1\nThis advertisement has not loaded yet, but your article continues below.\nAuthor of the article:\nRandi MannPublished Jan 06, 2026 • 6 minute read\nJoin the conversation\nReal Content.\nRead More\nAfrican safari: Sleep under the stars...\nArticle content\nShare this article in your social network\nTrending\nLatest National Stories\n";
    let cleaned = pp(text, "https://o.canada.com/travel");
    assert!(!cleaned.contains("Advertisement 1"));
    assert!(!cleaned.contains("This advertisement has not loaded"));
    assert!(!cleaned.contains("Author of the article"));
    assert!(!cleaned.contains("Join the conversation"));
    assert!(!cleaned.contains("Read More"));
    assert!(!cleaned.contains("Trending"));
    assert!(cleaned.contains("Real Content"));
}

#[test]
fn strip_castanet() {
    let text = "\n- Child killed by three dogsNova Scotia - 10:07 am\n- Urged to approve a pipelineCanada - 10:04 am\nReal Content.\n";
    let cleaned = pp(text, "http://www.castanet.net/rss/page-3.xml");
    assert!(!cleaned.contains("Child killed by three dogs"));
    assert!(cleaned.contains("Real Content"));
}

#[test]
fn strip_bloomberg_page_chrome() {
    let text = "\nAustralia Says Weighing All Options After Unjustified Tariffs - Bloomberg\n===============\n[Skip to content](http://www.bloomberg.com/news/articles/2026-02-22/australia-says-weighing-all-options-after-unjustified-tariffs#that-jump-content--default)\n[Bloomberg the Company & Its Products The Company & its Products](https://www.bloomberg.com/company/)\nUS Edition\n[](http://www.bloomberg.com/)\n[Subscribe](https://www.bloomberg.com/subscriptions?in_source=nav-mobileweb)\n[Technology](http://www.bloomberg.com/technology?source=eyebrow)\nAustralia Says Weighing All Options After Unjustified Tariffs\n===============================================================\nGift this article\nAdd us on Google\n[Contact us: Provide news feedback or report an error](https://www.bloomberg.com/help/question/submit-feedback-news-coverage/)\n[Confidential tip? Send a tip to our reporters](https://www.bloomberg.com/tips/)\n[Site feedback: Take our Survey](https://bmedia.iad1.qualtrics.com/jfe/form/xyz)\nBy [Angus Whitley](http://www.bloomberg.com/authors/AEaJLmK35vQ/angus-whitley)\nFebruary 22, 2026 at 4:18 AM UTC\nSave\nTranslate\nAustralia's government said it will examine all options after tariffs were imposed.\nOfficials said they are working closely with their embassy in Washington.\n[Before it's here, it's on the Bloomberg Terminal LEARN MORE](https://www.bloomberg.com/professional/solution/bloomberg-terminal-learn-more/)\n### More From Bloomberg\n[Home](https://www.bloomberg.com/)[BTV+](https://www.bloomberg.com/live)\n[Terms of Service](http://www.bloomberg.com/news/articles/2026-02-22/australia-says-weighing-all-options-after-unjustified-tariffs)\n2026 Bloomberg L.P. All Rights Reserved.\n";
    let cleaned = pp(
        text,
        "https://www.bloomberg.com/news/articles/2026-02-22/australia-says-weighing-all-options-after-unjustified-tariffs",
    );
    assert!(cleaned.contains("Australia's government said it will examine all options"));
    assert!(cleaned.contains("Officials said they are working closely"));
    assert!(!cleaned.contains("Skip to content"));
    assert!(!cleaned.contains("Contact us: Provide news feedback"));
    assert!(!cleaned.contains("By [Angus Whitley]"));
    assert!(!cleaned.contains("Save"));
    assert!(!cleaned.contains("Translate"));
    assert!(!cleaned.contains("Before it's here, it's on the Bloomberg Terminal"));
    assert!(!cleaned.contains("More From Bloomberg"));
    assert!(!cleaned.contains("All Rights Reserved"));
}

#[test]
fn strip_bloomberg_takeaways_and_plain_author() {
    let text = "\nNASA Delays Moon Mission to Fix Rocket, Rules Out March Launch - Bloomberg\n===============================================================\nGift this article\nAdd us on Google\n[Contact us: Provide news feedback or report an error](https://www.bloomberg.com/help/question/submit-feedback-news-coverage/)\n[Confidential tip? Send a tip to our reporters](https://www.bloomberg.com/tips/)\n[Site feedback: Take our Survey](https://bmedia.iad1.qualtrics.com/jfe/form/xyz)\nBy Bloomberg News\nFebruary 21, 2026 at 4:33 PM UTC\nUpdated on\nFebruary 21, 2026 at 5:05 PM UTC\nSave\nTranslate\n### **Takeaways** by Bloomberg AI[Subscribe](http://www.bloomberg.com/subscriptions)\nNASA is preparing to remove the rocket from the launch pad.\nThe agency said the rollout back to the hangar is needed for repairs.\n[Before it's here, it's on the Bloomberg Terminal LEARN MORE](https://www.bloomberg.com/professional/solution/bloomberg-terminal-learn-more/)\n";
    let cleaned = pp(
        text,
        "https://www.bloomberg.com/news/articles/2026-02-21/nasa-likely-to-delay-moon-mission-after-newly-found-rocket-issue",
    );
    assert!(cleaned.contains("NASA is preparing to remove the rocket"));
    assert!(cleaned.contains("The agency said the rollout back to the hangar"));
    assert!(!cleaned.contains("By Bloomberg News"));
    assert!(!cleaned.contains("Updated on"));
    assert!(!cleaned.contains("Translate"));
    assert!(!cleaned.contains("Takeaways"));
    assert!(!cleaned.contains("Before it's here, it's on the Bloomberg Terminal"));
}

#[test]
fn strip_zdnet_leading_boilerplate() {
    let text = "ZDNET Recommends: the best phones you can buy right now\nOur goal is to deliver the most accurate information and the best buying advice.\nReal story starts here.\nMore real content follows.";
    let cleaned = pp(text, "https://www.zdnet.com/article/best-phones/");
    assert_eq!(
        cleaned,
        "Real story starts here.\n\nMore real content follows."
    );
}

#[test]
fn strip_thetyee_fundraising_block() {
    let text = "Our journalism is supported by readers like you and we need your help.\nSupport Us Now\nReal story here.";
    assert_eq!(
        pp(text, "https://thetyee.ca/News/2026/10/30/story/"),
        "Real story here."
    );
}

#[test]
fn strip_axios_cards() {
    let text = "Add Axios as a preferred source on Google\n5 mins ago - Politics & Policy\nReal Axios story paragraph.\nGo deeper (4 min read)\nSecond real paragraph.";
    let cleaned = pp(text, "https://www.axios.com/2026/10/30/story");
    assert_eq!(
        cleaned,
        "Real Axios story paragraph.\n\nSecond real paragraph."
    );
}

#[test]
fn strip_wsj_copyright_tail() {
    let text = "Real body text.\n\nCopyright ©2026 Dow Jones & Company, Inc. All Rights Reserved. 87990cbe1234abcd5678";
    assert_eq!(
        pp(text, "https://www.wsj.com/articles/story"),
        "Real body text."
    );
}

#[test]
fn strip_techradar_gate_block() {
    let text = "Body paragraph one is here.\nFollow TechRadar for the best tech on Google News\nJane Doe is a staff writer who covers phones and tablets for the site.\nYou must confirm your public display name before commenting\nPlease logout and then login again to continue.";
    assert_eq!(
        pp(text, "https://www.techradar.com/news/story"),
        "Body paragraph one is here."
    );
}

#[test]
fn strip_ning_activity_wrapper() {
    let text = "Jane Doe\nposted a video\nA real title of the video post\nThis is the useful excerpt text of the post.\nSee more\n3 more…";
    let cleaned = pp(text, "https://example.ning.com/xn/detail/1");
    assert!(cleaned.contains("A real title of the video post"));
    assert!(cleaned.contains("useful excerpt text"));
    assert!(!cleaned.contains("posted a video"));
    assert!(!cleaned.contains("Jane Doe"));
}

// ---------------------------------------------------------------------
// Generic strips
// ---------------------------------------------------------------------

#[test]
fn leading_social_share_buttons_stripped() {
    let text = "Share on Facebook\nShare on Bluesky\nShare on X\nCopy Link\nTORONTO — A’ja Wilson and Jackie Young combined for 50 points.\nThe Tempo have now lost three straight games.";
    let cleaned = pp(text, "https://fraservalleytoday.ca/2026/07/20/story/");
    assert!(cleaned.starts_with("TORONTO — A’ja Wilson"));
    assert!(!cleaned.contains("Share on Facebook"));
    assert!(!cleaned.contains("Copy Link"));
    assert!(cleaned.contains("lost three straight games"));
}

#[test]
fn leading_share_strip_spares_real_prose() {
    let keep = "Share your thoughts about the game in the comments below.\nSecond line.";
    assert_eq!(pp(keep, "https://example.com/x"), keep);
    let fb = "Facebook announced a policy change today affecting millions of users.";
    assert_eq!(pp(fb, "https://example.com/y"), fb);
}

#[test]
fn sky_related_sections_removed() {
    let text = "The last paragraph of the article remains available.\nMore from Sky News entertainment:\nCelebrity headline that belongs to a related-story widget\nMore from Sky News:\nAnother related-story headline";
    assert_eq!(
        pp(text, "https://news.sky.com/story/example-13584148"),
        "The last paragraph of the article remains available."
    );
    let other = "Article body.\nMore from Sky News:\nA legitimate quoted heading.";
    assert_eq!(pp(other, "https://example.com/story"), other);
}

#[test]
fn atlantic_read_cards_removed() {
    let text = "Opening paragraph of the story.\n[Read: Is Marco Rubio the happiest Cabinet member?]\nMiddle paragraph of the story.\n[Read: How to fix the mess of college sports]\nClosing paragraph of the story.";
    let cleaned = pp(
        text,
        "https://www.theatlantic.com/culture/2026/11/marco-rubio-flights-football/689424/",
    );
    assert_eq!(
        cleaned,
        "Opening paragraph of the story.\n\nMiddle paragraph of the story.\n\nClosing paragraph of the story."
    );
    assert!(!cleaned.contains("[Read:"));
    assert!(!cleaned.contains("Marco Rubio the happiest"));
    assert!(!cleaned.contains("college sports"));
    let other = "Body.\n[Read: A related headline]";
    assert_eq!(pp(other, "https://example.com/story"), other);
}

#[test]
fn sky_akamai_wall_is_gate_not_article() {
    let html = "<html><body><p>Powered and protected by Akamai</p><p>Privacy</p></body></html>";
    match extract_article(
        html,
        "https://news.sky.com/story/four-shot-in-washington-dc-13611509",
        None,
    ) {
        ExtractOutcome::Gate { .. } => {}
        other => panic!("expected Gate, got {other:?}"),
    }
    let cleaned = pp(
        "Powered and protected by Akamai\nPrivacy",
        "https://news.sky.com/story/x",
    );
    assert_eq!(cleaned, "");
    let with_body = "Four people were shot in Washington DC.\nPowered and protected by Akamai";
    assert_eq!(
        pp(with_body, "https://news.sky.com/story/x"),
        "Four people were shot in Washington DC."
    );
}

#[test]
fn techcrunch_footer_boilerplate_removed() {
    let text = "Muse can now place calls to U.S. businesses.\nWhen you purchase through links in our articles, we may earn a small commission. This doesn't affect our editorial independence.\nSarah has worked as a reporter for TechCrunch since August 2011. She joined the company after having previously spent over three years at ReadWriteWeb.\nYou can contact or verify outreach from Sarah by emailing [email protected] or via encrypted message at sarahperez.01 on Signal.\nView Bio\n2027\n**San Francisco**\n**Get an Early Peek at the Next TechCrunch Disrupt**\n## Most Popular";
    let cleaned = pp(
        text,
        "https://techcrunch.com/2026/11/12/muse-can-now-place-calls-to-u-s-businesses-if-you-pay/",
    );
    assert_eq!(cleaned, "Muse can now place calls to U.S. businesses.");
    assert!(!cleaned.contains("purchase through links"));
    assert!(!cleaned.contains("has worked as a reporter"));
    assert!(!cleaned.contains("contact or verify outreach"));
    assert!(!cleaned.contains("View Bio"));
    assert!(!cleaned.contains("Most Popular"));
    // A legitimate mention elsewhere keeps its body.
    let other = "Sarah spoke about the launch at the conference.\nSecond real paragraph.";
    assert_eq!(
        pp(other, "https://techcrunch.com/2026/11/12/story/"),
        "Sarah spoke about the launch at the conference.\n\nSecond real paragraph."
    );
    assert_eq!(pp(other, "https://example.com/story"), other);
}

#[test]
fn recirculation_label_lines_dropped() {
    let text = "Real paragraph one.\nYou May Also Like\nReal paragraph two.";
    assert_eq!(
        pp(text, "https://example.com/story"),
        "Real paragraph one.\nReal paragraph two."
    );
}

#[test]
fn leaked_script_source_dropped_but_not_on_github() {
    let leaked = "Apple has quietly hidden a new Siri interface in the latest macOS beta, and testers have already found their way into it.\nlet cbeScripts = {\"redditEmbed\":[\"x\"]};\nlet firstElem = document.querySelector('.' + nodeType)\nif (firstElem == null) {\nconsole.warn(`CBE cannot find element`)\nreturn;\n}\nconst linkEl = document.createElement('link')\ndocument.head.append(linkEl)";
    let cleaned = pp(leaked, "https://mashable.com/article/siri");
    assert!(cleaned.contains("hidden a new Siri interface"));
    assert!(!cleaned.contains("cbeScripts"));
    assert!(!cleaned.contains("document.querySelector"));
    // GitHub pages are rebuilt from the API; their code IS content.
    let gh = pp(leaked, "https://github.com/serrebidev/BlindRSS/pull/42");
    assert!(gh.contains("cbeScripts"));
}

#[test]
fn github_page_url_detection() {
    assert!(is_github_page_url(
        "https://github.com/serrebidev/BlindRSS/pull/42"
    ));
    assert!(is_github_page_url(
        "https://github.com/serrebidev/BlindRSS/issues/7"
    ));
    assert!(is_github_page_url(
        "https://github.com/serrebidev/BlindRSS/commit/0123456789abcdef"
    ));
    assert!(is_github_page_url(
        "https://github.com/serrebidev/BlindRSS/releases/tag/v1.0"
    ));
    assert!(is_github_page_url(
        "https://github.com/orgs/community/discussions/1234"
    ));
    assert!(!is_github_page_url(
        "https://github.com/serrebidev/BlindRSS"
    ));
    assert!(!is_github_page_url("https://github.com/orgs/foo/issues"));
    assert!(!is_github_page_url("https://example.com/pull/42"));
    assert!(!is_github_page_url("https://github.com/search"));
}

#[test]
fn proxy_trailing_cookie_block_cut() {
    let body = "A long opening sentence of the article that carries real prose content for the reader to hear.\nAnother full sentence of article prose follows here with more detail about the story.\nA third sentence keeps the article going with still more substance and detail.\nThis website uses cookies to improve your experience.\nManage your privacy preferences here.";
    let cleaned = strip_proxy_trailing_boilerplate(body);
    assert!(cleaned.contains("third sentence keeps the article going"));
    assert!(!cleaned.contains("uses cookies"));
}

#[test]
fn edge_nav_runs_trimmed() {
    let text = "Home\nNews\nSports\nA real article sentence that is quite long and ends properly.\nAnother real sentence with plenty of words in it to be prose here.\nAbout\nContact\nPrivacy Policy";
    let cleaned = strip_edge_nav_runs(text);
    assert_eq!(
        cleaned,
        "A real article sentence that is quite long and ends properly.\nAnother real sentence with plenty of words in it to be prose here."
    );
}

// ---------------------------------------------------------------------
// Site extractors (end to end through the cascade)
// ---------------------------------------------------------------------

const SOCAST_HTML: &str = r#"<!doctype html><html><head><title>Story Headline | Site</title>
<meta property="og:title" content="Story Headline"></head><body>
<main><div class="site_width"><div class="sc-sidebar-wrapper"><div class="sc-content">
<article class="mainArticle">
  <section class="entry-header">
    <div class="aspect-ratio-content">
      <img src="https://media-cdn.socastsrm.com/lead.jpg" alt="Lead photo of the scene"/>
    </div>
    <div class="wp-caption-text">Caption describing the lead photo in detail here.</div>
    <h1>Story Headline</h1>
  </section>
  <div class="wpb-content-wrapper">
    <p>LEAD_ONE This is the first lead paragraph of the story body content here.</p>
    <p>LEAD_TWO This is the second lead paragraph continuing the story body content.</p>
    <div class="wpb_raw_code wpb_raw_html wpb_content_element"><div class="wpb_wrapper">
      <script>showAd('sidebar');</script></div></div>
    <div class="wpb_raw_code wpb_raw_html"><div class="pp-btn-container scWidgetContainer">
      <a href="/newsletter/">Join Our Daily Email</a></div></div>
  </div>
</article>
<div class="parallax-breakout"><script>adbutler();</script></div>
<article class="mainArticle"><div>
  <p>CONT_ONE The continuation paragraph appears after the mid-article ad break here.</p>
  <div class="posts items-wrapper">
    <a href="/other-story/" class="bnl-pp-happening item"><div class="bnl-info">
      <div class="highlight-text">Charges laid</div>
      <div class="bnl-title text-h4">RELATEDJUNK unrelated related-post headline</div>
      <div class="bnl-content"><span class="sc-time sc-item-detail">41m ago</span></div>
    </div></a>
  </div>
  <p>CONT_TWO The final body paragraph wraps the whole story up neatly at the end.</p>
  <div class="pp-more-wrapper"></div>
</div>
<div>by <span class="sc-author">The Canadian Press</span></div>
<footer class="entry-footer"></footer>
</article>
</div></div></div></main></body></html>"#;

#[test]
fn socast_split_body_rebuilt_in_order() {
    let text = article_text(
        SOCAST_HTML,
        "https://fraservalleytoday.ca/2026/07/15/story-headline/",
    );
    for token in ["LEAD_ONE", "LEAD_TWO", "CONT_ONE", "CONT_TWO"] {
        assert!(text.contains(token), "missing {token} in: {text}");
    }
    assert!(text.find("LEAD_ONE").unwrap() < text.find("CONT_ONE").unwrap());
    assert!(text.find("CONT_ONE").unwrap() < text.find("CONT_TWO").unwrap());
    assert!(!text.contains("RELATEDJUNK"));
    assert!(!text.contains("Join Our Daily Email"));
    assert!(!text.contains("The Canadian Press"));
    assert!(!text.contains("41m ago"));
}

#[test]
fn ars_publisher_body_reconstruction() {
    let lead = "Ukraine's engineers are adapting their communications equipment to keep essential services working under difficult battlefield conditions. ".repeat(5)
        + "The engineer specializes in repairing Starlink dishes.";
    let page = format!(
        "<html><head><title>Satellite communications</title></head><body><nav>Site navigation</nav><article class=\"double-column h-entry\"><header><h1>Satellite communications</h1></header><div class=\"grid\"><div><div class=\"post-content post-content-double\"><p>{lead}</p><div class=\"ars-interlude-container\"><p>Interlude promotion</p></div></div></div><aside>Sidebar promotion</aside></div><div class=\"grid\"><div><div class=\"post-content post-content-double\"><div class=\"page-anchor-wrapper\">Page tracking label</div><h2>Navigation alternatives</h2><p>Engineers built a <a href=\"/technology/backup/\">backup system</a> for these missions.</p></div></div></div><div class=\"grid\"><div><div class=\"post-content post-content-double\"><blockquote><p>The final section explains why communications resilience matters.</p></blockquote><script>unsafeScript()</script></div></div></div><section class=\"author-bio\"><p>Author biography</p></section><section class=\"comments\"><p>Reader comments</p></section></article><article><div class=\"post-content\"><p>Unrelated story</p></div></article></body></html>"
    );
    let text = article_text(&page, "https://arstechnica.com/gadgets/2026/10/starlink/");
    assert!(text.contains("repairing Starlink dishes."));
    assert!(text.contains("Navigation alternatives"));
    assert!(text.contains("communications resilience matters"));
    assert_eq!(text.matches("communications resilience matters").count(), 1);
    for junk in [
        "Interlude promotion",
        "Page tracking label",
        "Sidebar promotion",
        "Author biography",
        "Reader comments",
        "Unrelated story",
        "unsafeScript",
    ] {
        assert!(!text.contains(junk), "junk survived: {junk}");
    }
}

#[test]
fn ars_nested_body_sections_not_duplicated() {
    let page = "<article><div class=\"post-content\"><p>Opening.</p><div class=\"post-content\"><p>The final section explains why communications resilience matters.</p></div></div></article>";
    let text = article_text(page, "https://arstechnica.com/gadgets/2026/10/starlink/");
    assert!(text.contains("Opening."));
    assert_eq!(text.matches("communications resilience matters").count(), 1);
}

// ---------------------------------------------------------------------
// Brandon's 2026-11-08 reports: figure captions, the MacRumors
// rail/footer, and the Windows Latest syndication line.
// ---------------------------------------------------------------------

#[test]
fn ars_figure_caption_and_credit_lines_are_dropped() {
    // The caption and its Credit lines rode the Ars site path
    // twice (the figure sits in two nested post-content divs)
    // and the rich reader once. A second, hidden copy rides
    // in the PhotoSwipe lightbox block (pswp-caption-content,
    // not a figcaption). All paths must drop them; the body
    // around the figure stays.
    let body = "The ships slowed down and looped over the cable corridor for days, behaviour that suggests mapping work rather than fishing. ".repeat(3);
    let page = format!(
        "<html><body><article><div class=\"post-content\"><p>{body}</p><div class=\"post-content\"><figure class=\"ars-wp-img-shortcode\"><div class=\"ars-lightbox\"><img alt=\"Map\" src=\"/m.jpg\"></div><figcaption><div class=\"caption-content\">Multicolored lines show the cable routes. <span class=\"caption-credit\">Credit: <a href=\"https://example.com\">TeleGeography</a></span></div></figcaption><div class=\"pswp-caption-content\"><div class=\"caption-content\">Multicolored lines show the cable routes. <span class=\"caption-credit\">Credit: TeleGeography</span></div></div></figure><p>{body}</p></div></div></article></body></html>"
    );
    let url = "https://arstechnica.com/gadgets/2026/11/ships/";
    let text = article_text(&page, url);
    assert!(text.contains("suggests mapping work"));
    assert!(
        !text.contains("Multicolored lines"),
        "caption survived: {text}"
    );
    assert!(!text.contains("TeleGeography"), "credit survived: {text}");
    assert!(!text.contains("Credit:"), "credit label survived: {text}");
    let rich = blindrss_core::article_html::clean_article_html(&page, url, true);
    let rich_text = blindrss_core::extract::html_to_text(&rich);
    assert!(rich_text.contains("suggests mapping work"));
    assert!(
        !rich_text.contains("Multicolored lines"),
        "rich caption survived: {rich_text}"
    );
    assert!(
        !rich_text.contains("TeleGeography"),
        "rich credit survived: {rich_text}"
    );
}

#[test]
fn macrumors_popular_stories_and_footer_are_cut() {
    // Whole-page engines carry the "Popular Stories" rail
    // into the middle of the article and the site footer
    // after it; the postprocess strip cuts at the rail.
    let body = "The new display looks as good as you would expect from a laptop at this price, with true blacks and no bloom around bright highlights on dark scenes. ".repeat(3);
    let text_in = format!(
        "{body}\n\nPopular Stories\n\nRIP, MacBook Ultra Apple today sent out invites for an experience in Los Angeles.\n\n{body}\n\nPush Notifications\n\nApple's Safari browser includes a feature that allows users to subscribe to push notifications.\n\nOther Stories\n\n1 day ago by Joe Rossignol"
    );
    let out = pp(
        &text_in,
        "https://www.macrumors.com/2026/11/06/oled-macbook-pro-hands-on/",
    );
    assert!(out.contains("true blacks and no bloom"));
    assert!(!out.contains("Popular Stories"));
    assert!(!out.contains("RIP, MacBook Ultra"));
    assert!(!out.contains("Push Notifications"));
    assert!(!out.contains("Other Stories"));
    // A body paragraph that mentions the phrase mid-sentence
    // is not a heading and must survive.
    let mention =
        format!("{body}\n\nHe listed the Popular Stories of the year in his talk, in full.");
    let out2 = pp(
        &mention,
        "https://www.macrumors.com/2026/11/06/oled-macbook-pro-hands-on/",
    );
    assert!(out2.contains("in his talk, in full."));
}

#[test]
fn macrumors_image_credit_div_is_dropped_not_glued() {
    // Brandon's report (2026-11-08, on v0.1.75): the credit
    // survived and, worse, glued itself onto the next
    // paragraph ("...forum member sniffiesIf you'd rather
    // keep the logo dark..."). The credit is not a
    // figcaption: it is a short standalone div whose text
    // starts "Image credit:", dropped at the HTML level.
    let body = "The new display looks as good as you would expect from a laptop at this price, with true blacks and no bloom around bright highlights on dark scenes. ".repeat(3);
    let page = format!(
        "<html><body><article><div class=\"entry-content\"><p>{body}</p><p><img src=\"/logo.jpg\" class=\"aligncenter size-full\" /></p><div class=\"center-wrap\"><em>Image credit: MacRumors forum member <a href=\"https://forums.macrumors.com/threads/x\">sniffies</a></em></div><br /><p>If you'd rather keep the logo dark, Apple has shared a way to turn it off. Unusually though, there's no switch in System Settings.</p><p>{body}</p></div></article></body></html>"
    );
    let url = "https://www.macrumors.com/how-to/turn-glowing-apple-logo-macbook-pro/";
    let text = article_text(&page, url);
    assert!(text.contains("If you'd rather keep the logo dark"));
    assert!(
        !text.contains("Image credit"),
        "classic credit survived: {text}"
    );
    assert!(
        !text.contains("sniffies"),
        "classic credit name survived: {text}"
    );
    let rich = blindrss_core::article_html::clean_article_html(&page, url, true);
    let rich_text = blindrss_core::extract::html_to_text(&rich);
    assert!(rich_text.contains("If you'd rather keep the logo dark"));
    assert!(
        !rich_text.contains("Image credit"),
        "rich credit survived: {rich_text}"
    );
    assert!(
        !rich_text.contains("sniffies"),
        "rich credit name survived: {rich_text}"
    );
    // A long element that merely OPENS by discussing image
    // credits is body text, not a credit line: the drop only
    // fires on short standalone elements.
    let discussion = format!(
        "<html><body><article><div class=\"entry-content\"><p>Image credit: this explainer walks through how credit lines work, who writes them, why photographers insist on them, how editors trim them for space, and what happens to syndicated copies when the credit is stripped away in transit between wire services and the papers that republish their pictures every single day of the week.</p><p>{body}</p></div></article></body></html>"
    );
    let text2 = article_text(&discussion, url);
    assert!(
        text2.contains("how credit lines work"),
        "long credit discussion wrongly dropped: {text2}"
    );
}

#[test]
fn macrumors_glued_footer_is_cut_at_its_anchors() {
    // Brandon's pasted text (2026-11-08, on v0.1.75): in the
    // page variant his app is served, the whole footer is
    // ONE glued blob — no paragraph break before "Push
    // Notifications", and it runs straight into "Other
    // Stories" — so the paragraph-start cuts never fired.
    // The strip now also cuts at the footer's unambiguous
    // boilerplate anchors wherever they appear.
    let body = "The new display looks as good as you would expect from a laptop at this price, with true blacks and no bloom around bright highlights on dark scenes. ".repeat(3);
    let glued = format!(
        "{body}Push NotificationsApple's Safari browser includes a feature that allows users to subscribe to \"push\" notifications from any website. Pushes show up as notification badges on the top right of your screen and can be used to be instantly notified when there are new MacRumors stories.If you have any questions or problems, email support@macrumors.com.To Subscribe or UnsubscribeVideosGuidesOur comprehensive guide highlighting every major new addition in iOS 27.Other Stories1 day ago by Joe Rossignol3 days ago by Tim Hardwick"
    );
    let url = "https://www.macrumors.com/how-to/turn-glowing-apple-logo-macbook-pro/";
    let out = pp(&glued, url);
    assert!(out.contains("true blacks and no bloom"), "body lost: {out}");
    assert!(
        !out.contains("Push Notifications"),
        "push block survived: {out}"
    );
    assert!(
        !out.contains("support@macrumors.com"),
        "support line survived: {out}"
    );
    assert!(
        !out.contains("To Subscribe"),
        "subscribe tail survived: {out}"
    );
    assert!(
        !out.contains("Other Stories"),
        "stories tail survived: {out}"
    );
    // The subscribe anchor alone also cuts, even without
    // the push block before it.
    let glued2 = format!("{body}To Subscribe or UnsubscribeVideosGuides trailing junk");
    let out2 = pp(&glued2, url);
    assert!(out2.contains("true blacks and no bloom"));
    assert!(
        !out2.contains("To Subscribe"),
        "subscribe-only tail survived: {out2}"
    );
}

#[test]
fn macrumors_footer_anchors_cut_across_line_breaks() {
    // Brandon's report (2026-11-09, on v0.1.78): the footer
    // still trailed the body. Extraction emits block
    // fragments on separate lines, so at strip time the
    // anchors are split over lines ("To Subscribe or\n
    // Unsubscribe") and only glue into the verbatim
    // sentences when the reader text collapses. The page
    // also uses a curly apostrophe in "Apple's". The
    // anchors match across whitespace runs either way.
    let body = "The new display looks as good as you would expect from a laptop at this price, with true blacks and no bloom around bright highlights on dark scenes. ".repeat(3);
    let url = "https://www.macrumors.com/how-to/turn-glowing-apple-logo-macbook-pro/";
    // Split subscribe + support anchors.
    let split = format!(
        "{body}Some trailing body words.\nTo Subscribe or\nUnsubscribeVideosGuidesOur comprehensive guide.\nIf you have any questions or problems,\nemail support@macrumors.com.Other Stories1 day ago by Joe Rossignol"
    );
    let out = pp(&split, url);
    assert!(out.contains("trailing body words"), "body lost: {out}");
    assert!(!out.contains("Unsubscribe"), "split tail survived: {out}");
    assert!(
        !out.contains("support@macrumors.com"),
        "split support survived: {out}"
    );
    // Curly apostrophe + heading split from the pitch.
    let curly = format!(
        "{body}Some trailing body words.\nPush\nNotificationsApple\u{2019}s Safari browser includes\na feature that allows users to subscribe."
    );
    let out2 = pp(&curly, url);
    assert!(out2.contains("trailing body words"), "body lost: {out2}");
    assert!(
        !out2.contains("Notifications"),
        "curly push block survived: {out2}"
    );
}

#[test]
fn windowslatest_caption_credit_and_syndication_line_are_dropped() {
    // Captions (with "Credit: Windows Latest") come from
    // figcaptions and die with them; the trailing "The post
    // ... appeared first on Windows Latest" line is cut by
    // the site strip even when an engine inlines it.
    let body = "Microsoft is testing a welcome change to the sign-in screen, where you can type your PIN while Windows Hello is still checking your face. ".repeat(3);
    let page = format!(
        "<html><body><article><div class=\"entry-content\"><p>{body}</p><figure><img src=\"/a.jpg\" alt=\"Sign-in screen\"><figcaption>Windows 11 sign-in screen now shows PIN textbox. Credit: Windows Latest</figcaption></figure><p>{body}</p></div></article></body></html>"
    );
    let url = "https://www.windowslatest.com/2026/11/07/windows-hello-update/";
    let text = article_text(&page, url);
    assert!(text.contains("type your PIN"));
    assert!(!text.contains("PIN textbox"), "caption survived: {text}");
    assert!(!text.contains("Credit:"), "credit survived: {text}");
    let with_footer = format!(
        "{body}\n\nThe post Windows 11 won't make you wait appeared first on Windows Latest"
    );
    let out = pp(&with_footer, url);
    assert!(out.contains("type your PIN"));
    assert!(!out.contains("appeared first on"));
}

#[test]
fn wired_subscribe_promo_is_dropped() {
    // Brandon's report, 2026-11-11: every Wired commerce story
    // ended with the in-body subscription pitch. It sits in a
    // `<p class="paywall">` like the paywalled body itself, so
    // the site strip removes it from the text, wherever it lands.
    let body = "The WalkingPad C2 Foldable Walking Treadmill track is compact when folded, making it a great option for storage in small spaces and apartments. ".repeat(3);
    let page = format!(
        "<html><body><article><div class=\"body__inner-container\"><p>{body}</p><h2 class=\"paywall\">More Walking Pad Treadmill Deals</h2><p class=\"paywall\">Here are some more early Black Friday discounts on walking pads and treadmills I have tested and recommend.</p><hr class=\"paywall\"/><p class=\"paywall\"><strong><a href=\"https://www.wired.com/v2/offers/wir_generic_ctrl\"><em>Power up with unlimited access to</em> WIRED<em>.</em> <em>Get best-in-class reporting and exclusive subscriber content that's too important to ignore. </em><em><a href=\"https://www.wired.com/v2/offers/wir_generic_ctrl\">Subscribe Today</a></em><em>.</em></a></strong></p></div></article></body></html>"
    );
    let url = "https://www.wired.com/story/best-black-friday-walking-pad-treadmill-deals-11-11-26/";
    let text = article_text(&page, url);
    assert!(
        text.contains("great option for storage"),
        "body lost: {text}"
    );
    assert!(
        text.contains("tested and recommend"),
        "paywall body lost: {text}"
    );
    assert!(!text.contains("Power up"), "promo survived: {text}");
    assert!(!text.contains("Subscribe Today"), "promo survived: {text}");
    // Any engine's spacing of the pitch, and its JSON-LD twin.
    for promo in [
        "Power up with unlimited access to WIRED.Get best-in-class reporting and exclusive subscriber content that's too important to ignore. Subscribe Today.",
        "Power up with unlimited access to WIRED. Get best-in-class reporting and exclusive subscriber content that\u{2019}s too important to ignore. Subscribe Today.",
    ] {
        let out = pp(&format!("{body}\n\n{promo}"), url);
        assert!(out.contains("great option for storage"));
        assert!(!out.contains("Power up"), "promo survived: {out}");
    }
    // Other hosts keep the sentence: it is only Wired's chrome.
    let other = pp(
        &format!("{body}\n\nPower up with unlimited access to WIRED. Subscribe Today."),
        "https://example.com/story/1",
    );
    assert!(other.contains("Power up"));
}

#[test]
fn nested_inline_wrapper_keeps_following_space() {
    // rust-trafilatura drops an inline wrapper with no text of its
    // own (`<em><strong>x</strong></em>`) when its tail is only
    // whitespace, and the space goes with it: Wired printed
    // "Will Knight’sAI Lab" and "newslettershere". Python's
    // trafilatura keeps the space; keep_inline_gaps moves the gap
    // into the preceding text before every trafilatura pass.
    use blindrss_core::extract::keep_inline_gaps;
    let body = "Much like vibe coding a few years ago, we may well see breakthroughs in vibe robotics as hobbyists and engineers realize how easy it is. ".repeat(3);
    let page = format!(
        "<html><body><article><p>{body}</p><p>{body}</p><p><em>This is an edition of</em> <a href=\"/author/will-knight/\"><em><strong>Will Knight's</strong></em></a> <em><a href=\"/newsletter\"><strong>AI Lab newsletter</strong></a>. Read previous newsletters</em> <a href=\"/tag/ai-lab/\"><em><strong>here.</strong></em></a></p></article></body></html>"
    );
    let prepped = keep_inline_gaps(&page);
    assert!(prepped.contains("Will Knight's </strong></em></a><em>"));
    let opts = rust_trafilatura::Options {
        focus: rust_trafilatura::ExtractionFocus::FavorPrecision,
        ..Default::default()
    };
    let text = rust_trafilatura::extract(prepped.as_bytes(), &opts)
        .unwrap()
        .content_text;
    assert!(
        text.contains(
            "This is an edition of Will Knight's AI Lab newsletter. Read previous newsletters here."
        ),
        "space lost: {text}"
    );
    // Gaps before text, and gaps outside inline runs, are untouched.
    assert_eq!(
        keep_inline_gaps("<p>a</em> b</p> <p>c</p>"),
        "<p>a</em> b</p> <p>c</p>"
    );
}

#[test]
fn reuters_paragraph_nodes_only() {
    let html = r#"<html><body><div data-testid="ArticleBody">
<div data-testid="paragraph-0">Oct 31 - The first body paragraph of the Reuters story carries enough text to matter here, opens new tab</div>
<div data-testid="paragraph-1">The second body paragraph continues the story with more detail for the reader.</div>
<div data-testid="paragraph-2">The third body paragraph wraps up the Reuters story body text completely.</div>
</div><div data-testid="paragraph-9">Read Next teaser headline that must not appear in the output.</div></body></html>"#;
    let text = article_text(html, "https://www.reuters.com/world/story-2026-10-31/");
    assert!(text.contains("first body paragraph of the Reuters story"));
    assert!(text.contains("third body paragraph wraps up"));
    assert!(!text.contains("opens new tab"));
    assert!(!text.contains("Read Next teaser"));
}

#[test]
fn gsmarena_review_body_with_headings() {
    let html = r#"<html><body><div id="review-body">
<h3>Introduction</h3>
<p>The first paragraph of the review carries a good amount of text about the phone and its design language overall.</p>
<ul><li>Specs item one for the phone</li><li>Specs item two for the phone</li></ul>
<h3>Display</h3>
<p>The second paragraph talks about display quality, brightness levels, and outdoor visibility in strong sunlight conditions.</p>
<p>The third paragraph covers performance and battery endurance results from our standardized testing procedure in detail.</p>
</div></body></html>"#;
    let text = article_text(html, "https://www.gsmarena.com/phone-review-123.php");
    assert!(text.contains("Introduction"));
    assert!(text.contains("Specs item one"));
    assert!(text.contains("Display"));
    assert!(text.contains("standardized testing procedure"));
}

#[test]
fn rebelnews_author_profile_removed() {
    let html = "<html><body><article><p>Real article paragraph with enough readable content to extract and keep in the output text here.</p></article><section class=\"posts-profile\"><p>Reporter biography should not be in the article.</p></section></body></html>";
    let text = article_text(html, "https://www.rebelnews.com/story");
    assert!(text.contains("Real article paragraph"));
    assert!(!text.contains("Reporter biography"));
}

#[test]
fn image_alt_markers_kept_only_when_enabled() {
    use blindrss_core::extract::{html_to_text, html_to_text_opts, image_alt_marker};
    let html = "<p>Before</p><img src=\"a.jpg\" alt=\"A sleepy dog on a rug\"><p>Middle</p><img src=\"b.jpg\"><p>After</p>";
    let dropped = html_to_text(html);
    assert!(!dropped.contains("[Image"), "default drops: {dropped}");
    let kept = html_to_text_opts(html, true);
    assert!(kept.contains("[Image: A sleepy dog on a rug]"), "{kept}");
    assert!(kept.contains("[Image]"), "no-alt marker: {kept}");
    // Title is the fallback when alt is missing.
    let titled = html_to_text_opts("<img src=\"c.jpg\" title=\"Chart title\">", true);
    assert!(titled.contains("[Image: Chart title]"), "{titled}");
    // Whitespace inside alt collapses; empty alt is a bare marker.
    assert_eq!(
        image_alt_marker("  multi\n line  alt "),
        "[Image: multi line alt]"
    );
    assert_eq!(image_alt_marker(""), "[Image]");
    assert_eq!(image_alt_marker("   "), "[Image]");
}

#[test]
fn feed_show_images_roundtrip() {
    let dir = std::env::temp_dir().join(format!("blindrss-batch25-{}", unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = blindrss_core::db::Db::open_or_create(&dir.join("rss.db")).unwrap();
    db.insert_feed_if_missing(&blindrss_core::db::Feed {
        id: "f1".to_string(),
        url: "https://example.com/feed".to_string(),
        title: "Feed".to_string(),
        title_is_custom: false,
        category: None,
        icon_url: None,
    })
    .unwrap();
    assert_eq!(db.feed_show_images("f1").unwrap(), None);
    db.set_feed_show_images("f1", Some(true)).unwrap();
    assert_eq!(db.feed_show_images("f1").unwrap(), Some(true));
    db.set_feed_show_images("f1", Some(false)).unwrap();
    assert_eq!(db.feed_show_images("f1").unwrap(), Some(false));
    db.set_feed_show_images("f1", None).unwrap();
    assert_eq!(db.feed_show_images("f1").unwrap(), None);
    assert_eq!(db.feed_show_images("missing").unwrap(), None);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ssl_cert_error_detection() {
    use blindrss_core::http::looks_like_ssl_cert_error;
    assert!(looks_like_ssl_cert_error("fetch failed: UnknownIssuer"));
    assert!(looks_like_ssl_cert_error(
        "tls error: invalid peer certificate: UnknownIssuer"
    ));
    assert!(looks_like_ssl_cert_error(
        "SSL certificate problem: self-signed certificate in certificate chain"
    ));
    assert!(looks_like_ssl_cert_error("certificate has expired"));
    assert!(!looks_like_ssl_cert_error(
        "fetch failed: Connection refused (os error 111)"
    ));
    assert!(!looks_like_ssl_cert_error("fetch failed: HTTP 404"));
    assert!(!looks_like_ssl_cert_error("dns error: no such host"));
}

#[test]
fn user_tool_paths_take_priority() {
    use blindrss_core::dependency::{set_user_tool_paths, user_tool_path};
    let dir = std::env::temp_dir().join(format!("blindrss-tools-{}", unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("ffmpeg");
    std::fs::write(&fake, b"fake").unwrap();
    // Directory form: the value names a folder containing the tool.
    let mut mapping = std::collections::HashMap::new();
    mapping.insert("ffmpeg".to_string(), dir.to_string_lossy().to_string());
    set_user_tool_paths(&mapping);
    assert_eq!(
        user_tool_path("ffmpeg", &["ffmpeg.exe", "ffmpeg"]),
        Some(fake.clone())
    );
    // File form, with quotes and an .exe-suffixed key.
    let mut mapping2 = std::collections::HashMap::new();
    mapping2.insert(
        "ffmpeg.exe".to_string(),
        format!("\"{}\"", fake.to_string_lossy()),
    );
    set_user_tool_paths(&mapping2);
    assert_eq!(user_tool_path("ffmpeg", &["ffmpeg"]), Some(fake.clone()));
    // Empty registry: no override.
    set_user_tool_paths(&std::collections::HashMap::new());
    assert_eq!(user_tool_path("ffmpeg", &["ffmpeg"]), None);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn open_command_parsing() {
    use blindrss_core::opencmd::build_open_command;
    assert_eq!(
        build_open_command("chrome --incognito %1", "http://x/a").unwrap(),
        vec!["chrome", "--incognito", "http://x/a"]
    );
    assert_eq!(
        build_open_command("open --url=%1", "http://x").unwrap(),
        vec!["open", "--url=http://x"]
    );
    assert_eq!(
        build_open_command("firefox", "http://x").unwrap(),
        vec!["firefox", "http://x"]
    );
    assert_eq!(
        build_open_command("chrome %1", "http://x/a b c").unwrap(),
        vec!["chrome", "http://x/a b c"]
    );
    assert!(build_open_command("   ", "http://x").is_err());
    assert!(build_open_command("chrome \"unterminated", "http://x").is_err());
    // Quoted program path with a space (POSIX mode on this host).
    assert_eq!(
        build_open_command("\"/opt/my browser/browser\" --new-tab %1", "http://x").unwrap(),
        vec!["/opt/my browser/browser", "--new-tab", "http://x"]
    );
}

#[test]
fn category_tree_collection_and_delete() {
    let dir = std::env::temp_dir().join(format!("blindrss-cattree-{}", unique_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = blindrss_core::db::Db::open_or_create(&dir.join("rss.db")).unwrap();
    db.add_category("News").unwrap();
    db.add_category("News / Tech").unwrap();
    db.add_category("News / Tech / AI").unwrap();
    db.add_category("Sports").unwrap();
    let mk = |id: &str, cat: Option<&str>| blindrss_core::db::Feed {
        id: id.to_string(),
        url: format!("https://example.com/{id}"),
        title: id.to_string(),
        title_is_custom: false,
        category: cat.map(|c| c.to_string()),
        icon_url: None,
    };
    db.insert_feed_if_missing(&mk("f1", Some("News"))).unwrap();
    db.insert_feed_if_missing(&mk("f2", Some("News / Tech / AI")))
        .unwrap();
    db.insert_feed_if_missing(&mk("f3", Some("Sports")))
        .unwrap();
    db.insert_feed_if_missing(&mk("f4", None)).unwrap();
    let tree = db.feeds_in_category_tree("News").unwrap();
    let mut ids: Vec<&str> = tree.iter().map(|f| f.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["f1", "f2"]);
    assert!(db.delete_category_tree("News").unwrap());
    let titles: Vec<String> = db
        .categories()
        .unwrap()
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles, vec!["Sports".to_string()]);
    assert!(!db.delete_category_tree("News").unwrap());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn atlantic_author_and_split_cards_removed_from_text() {
    // Brandon's report, 2026-11-13: "[Ian Bogost: Airport chaos
    // began long before the ground stop]" mid-article.
    use blindrss_core::postprocess::strip_atlantic_cards_keep_layout;
    let url = "https://www.theatlantic.com/ideas/2026/11/flights-grounded-why/689446/";
    let text = "Opening paragraph.\n\n[Ian Bogost: Airport chaos began long before the ground stop]\n\nMiddle paragraph.\n\n[Read: ]Imagine every AI agent on Earth buying the same cheap paper towels\n\n[Editor's note: This story was updated.]\n\nClosing paragraph.";
    for cleaned in [pp(text, url), strip_atlantic_cards_keep_layout(text)] {
        assert!(!cleaned.contains("Ian Bogost"), "{cleaned}");
        assert!(!cleaned.contains("paper towels"), "{cleaned}");
        assert!(cleaned.contains("Editor's note"), "{cleaned}");
        assert!(cleaned.contains("Middle paragraph."), "{cleaned}");
    }
    assert_eq!(
        strip_atlantic_cards_keep_layout(text),
        "Opening paragraph.\n\nMiddle paragraph.\n\n[Editor's note: This story was updated.]\n\nClosing paragraph."
    );
    // Single-newline lines keep their layout.
    assert_eq!(
        strip_atlantic_cards_keep_layout("Item one\nItem two\n[Read: A card]\nItem three"),
        "Item one\nItem two\nItem three"
    );
}

#[test]
fn windowslatest_feed_footer_cut_keeps_layout() {
    use blindrss_core::postprocess::strip_windowslatest_footer_keep_layout;
    let body = "Para one.\n\nItem a\nItem b\n\nThe post Title appeared first on Windows Latest";
    assert_eq!(
        strip_windowslatest_footer_keep_layout(body),
        "Para one.\n\nItem a\nItem b"
    );
    assert_eq!(
        strip_windowslatest_footer_keep_layout("No footer."),
        "No footer."
    );
}
