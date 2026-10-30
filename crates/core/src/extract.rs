//! Hybrid full-text extraction cascade (Stage 3, slice 1).
//!
//! Shape decided by the Phase 3 benchmark v2: a trafilatura-family base,
//! a Readability second opinion, and BlindRSS's own ported layer owning
//! every final call. Rules ported so far:
//! - Bot-interstitial / block markers from Python `_BOT_INTERSTITIAL_MARKERS`
//!   (subset: the highest-signal entries), judged with the Python rule
//!   that a gate body is small (`_BOT_INTERSTITIAL_MAX_BODY_LEN`).
//! - Engine-disagreement rule (benchmark v2 evidence): when the base
//!   engine is thin but Readability returns much more, the page is
//!   suspect (the BBC 404 case: base 380 chars vs Readability 2,225 of
//!   navigation chrome). Thin + disagreement means non-article, never
//!   "take the longer one".
//! - Prefer-feed-content: a substantial feed body beats a thin scrape.
//! - Missing-table re-append (miniature): table cell text present in the
//!   page but absent from the extraction is appended, so tables are not
//!   silently dropped (the Heise case Python itself fails today).

/// Highest-signal block markers, ported from Python
/// `core/article_extractor.py::_BOT_INTERSTITIAL_MARKERS`.
const GATE_MARKERS: &[&str] = &[
    "attention required! | cloudflare",
    "checking your browser before accessing",
    "cf-browser-verification",
    "__cf_chl_",
    "enable javascript and cookies to continue",
    "ddos protection by cloudflare",
    "please verify you are a human",
    "verify you are human",
    "press & hold to confirm you",
    "why have i been blocked",
    "access to this page has been denied",
    "please complete the security check to access",
    "pardon our interruption",
    "performing security verification",
    "while the website verifies you are not a bot",
    "geo.captcha-delivery.com",
    "please enable js and disable any ad blocker",
    "valid tollbit token",
];

/// Python `_BOT_INTERSTITIAL_MAX_BODY_LEN`: block bodies are short.
const GATE_MAX_BODY_CHARS: usize = 1500;
/// Below this, a trafilatura result counts as "thin" (benchmark v2).
const THIN_CHARS: usize = 600;

#[derive(Debug, Clone, PartialEq)]
pub enum ExtractOutcome {
    Article {
        title: String,
        text: String,
        /// Which stage produced the accepted text: "base", "second", "feed".
        source: &'static str,
    },
    /// The page is a block/challenge/error page, not an article.
    Gate { reason: String },
    /// Nothing usable could be extracted.
    Empty,
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

fn gate_marker(html_lower: &str) -> Option<&'static str> {
    GATE_MARKERS
        .iter()
        .find(|m| html_lower.contains(**m))
        .copied()
}

/// Extract table cell texts from raw HTML (tags stripped), for the
/// missing-table re-append rule.
fn table_lines(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = html.to_lowercase();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find("<td") {
        let start = idx + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let cell_start = start + gt + 1;
        let Some(end) = lower[cell_start..].find("</td>") else {
            break;
        };
        let cell_html = &html[cell_start..cell_start + end];
        let text = strip_tags(cell_html).trim().to_string();
        if text.chars().count() >= 2 {
            out.push(text);
        }
        idx = cell_start + end + 5;
    }
    out
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run the cascade over a fetched page.
///
/// `feed_text` is the article body the feed itself supplied, if any; the
/// prefer-feed-content rule can keep it when the scrape is worse.
pub fn extract_article(html: &str, url: &str, feed_text: Option<&str>) -> ExtractOutcome {
    let html_lower = html.to_lowercase();
    let marker = gate_marker(&html_lower);

    // Base engine: rust-trafilatura.
    let base = rust_trafilatura::extract(html.as_bytes(), &rust_trafilatura::Options::default())
        .ok()
        .map(|r| (r.metadata.title.clone(), r.content_text.trim().to_string()))
        .unwrap_or_default();
    let base_chars = char_count(&base.1);

    // Gate rule: a marker plus a small extracted body is a block page.
    if let Some(m) = marker
        && base_chars <= GATE_MAX_BODY_CHARS
    {
        return ExtractOutcome::Gate {
            reason: format!("bot interstitial marker: {m}"),
        };
    }

    // Second opinion: Readability (dom_smoothie).
    let second = dom_smoothie::Readability::new(html.to_string(), Some(url), None)
        .and_then(|mut r| r.parse())
        .map(|a| (a.title, a.text_content.trim().to_string()))
        .unwrap_or_default();
    let second_chars = char_count(&second.1);

    // Engine-disagreement rule (BBC 404 evidence).
    if base_chars < THIN_CHARS && second_chars > base_chars * 3 && second_chars > 900 {
        return ExtractOutcome::Gate {
            reason: "engines disagree: thin base vs inflated readability (error/chrome page)"
                .to_string(),
        };
    }
    if base_chars < THIN_CHARS && second_chars < THIN_CHARS {
        // Both thin: prefer substantial feed content before giving up.
        if let Some(feed) = feed_text
            && char_count(feed) >= 200
        {
            return ExtractOutcome::Article {
                title: base.0.clone(),
                text: feed.trim().to_string(),
                source: "feed",
            };
        }
        if base_chars == 0 && second_chars == 0 {
            return ExtractOutcome::Empty;
        }
    }

    // Pick the better candidate: the longer one, unless the second is
    // only marginally longer (base wins ties; it keeps tables better).
    let (title, mut text, source) = if second_chars > base_chars.saturating_add(base_chars / 4) {
        (second.0, second.1, "second")
    } else {
        (base.0, base.1, "base")
    };

    // Prefer-feed-content: a clearly more substantial feed body wins.
    if let Some(feed) = feed_text {
        let feed = feed.trim();
        if char_count(feed) > char_count(&text) && char_count(feed) >= 800 {
            return ExtractOutcome::Article {
                title,
                text: feed.to_string(),
                source: "feed",
            };
        }
    }

    // Missing-table re-append: keep table data the extractor dropped.
    if html_lower.contains("<table") {
        let missing: Vec<String> = table_lines(html)
            .into_iter()
            .filter(|cell| !text.contains(cell.as_str()))
            .collect();
        if !missing.is_empty() {
            text.push_str("\n\nTable data: ");
            text.push_str(&missing.join(" | "));
        }
    }

    if char_count(&text) == 0 {
        return ExtractOutcome::Empty;
    }
    ExtractOutcome::Article {
        title,
        text,
        source,
    }
}
