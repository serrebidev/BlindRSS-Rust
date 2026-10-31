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

/// Block markers, ported in full from Python
/// `core/article_extractor.py::_BOT_INTERSTITIAL_MARKERS`.
const GATE_MARKERS: &[&str] = &[
    "attention required! | cloudflare",
    "checking your browser before accessing",
    "cf-browser-verification",
    "__cf_chl_",
    "cf_chl_opt",
    "enable javascript and cookies to continue",
    "ddos protection by cloudflare",
    "performance & security by cloudflare",
    "we've detected unusual activity from your computer network",
    "let us know you're not a robot",
    "please verify you are a human",
    "verify you are human",
    "press & hold to confirm you",
    "please make sure your browser supports javascript and cookies",
    "block reference id",
    "why have i been blocked",
    "access to this page has been denied",
    "please complete the security check to access",
    "pardon our interruption",
    "as you were browsing, something about your browser made us think you were a bot",
    "performing security verification",
    "uses a security service to protect against malicious bots",
    "while the website verifies you are not a bot",
    "geo.captcha-delivery.com",
    "ct.captcha-delivery.com",
    "please enable js and disable any ad blocker",
    "valid tollbit token",
    "tollbit.dev",
];

/// Paywall call-to-action phrases (Python `_PAYWALL_CTA_RE`, flattened
/// to substrings). Judged only on short bodies (`_PAYWALL_STUB_MAX_LEN`).
const PAYWALL_CTA_PHRASES: &[&str] = &[
    "subscribe to unlock",
    "subscribe to read",
    "subscribe to continue",
    "subscribe to view",
    "to continue reading",
    "to keep reading",
    "this article is for subscribers",
    "this article is exclusive to subscribers",
    "this story is for subscribers",
    "this content is for subscribers",
    "sign in to read",
    "sign in to continue",
    "log in to read",
    "login to read",
    "become a subscriber to",
    "become a member to",
    "unlock this article",
    "unlock this story",
];
const PAYWALL_STUB_MAX_CHARS: usize = 600;

/// Python `_METERED_PREVIEW_MARKERS` + its reader notice.
const METERED_MARKERS: &[&str] = &["to read as many articles as you like"];
const METERED_NOTICE: &str = "Only the free preview of this article is published on the page — the rest requires a subscription. Open the original link in your browser to read it.";
const PAYWALL_NOTICE: &str = "This article is behind a paywall (subscription required), so the full text can't be fetched automatically. Open the original link in your browser to read it.";

/// The Register's leading section kickers (Python
/// `_THEREGISTER_DROP_LEADING_LABELS`).
const REGISTER_LEADING_LABELS: &[&str] = &[
    "security",
    "ai + ml",
    "ai and ml",
    "software",
    "systems",
    "networks",
    "storage",
    "devops",
    "personal tech",
    "science",
    "offbeat",
    "on-prem",
    "off-prem",
    "special features",
    "bootnotes",
    "emergent tech",
    "public sector",
    "legal",
    "cso",
    "spotlight",
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

/// Minimal HTML entity unescape for JSON-LD / Next-data bodies (Python
/// relies on the stdlib unescape in the same spots).
fn unescape_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// Convert an HTML fragment to plain text.
fn fragment_to_text(fragment: &str) -> String {
    html_to_text(fragment)
}

/// Extract the text inside every `<tag ...>...</tag>` block whose open
/// tag contains `open_hint` (case-insensitive). Used for article, p, li,
/// and script blocks without a full HTML parser.
fn blocks(html: &str, tag: &str, open_hint: &str) -> Vec<String> {
    let lower = html.to_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(pos) = lower[idx..].find(&open) {
        let start = idx + pos;
        let Some(gt) = lower[start..].find('>') else {
            break;
        };
        let open_tag = &lower[start..start + gt + 1];
        let inner_start = start + gt + 1;
        let Some(end) = lower[inner_start..].find(&close) else {
            break;
        };
        if open_hint.is_empty() || open_tag.contains(open_hint) {
            out.push(html[inner_start..inner_start + end].to_string());
        }
        idx = inner_start + end + close.len();
    }
    out
}

/// JSON-LD articleBody recovery (Python `_extract_json_ld_text`):
/// longest `articleBody`/`text` string in any ld+json block, >= 120
/// chars, markup stripped.
fn json_ld_text(html: &str) -> String {
    fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, val) in map {
                    if (k == "articleBody" || k == "text")
                        && let Some(s) = val.as_str()
                    {
                        out.push(s.to_string());
                    }
                    collect(val, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            _ => {}
        }
    }
    let mut best = String::new();
    for raw in blocks(html, "script", "application/ld+json") {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
            continue;
        };
        let mut candidates = Vec::new();
        collect(&data, &mut candidates);
        for c in candidates {
            let text = if c.contains('<') {
                fragment_to_text(&c)
            } else {
                unescape_entities(&c)
            };
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.chars().count() >= 120 && text.chars().count() > best.chars().count() {
                best = text;
            }
        }
    }
    best
}

/// The Register dedicated extractor (Python `_extract_theregister_text`):
/// the <article> with the most paragraph text, paragraphs joined,
/// leading section kicker dropped, >= 300 chars.
fn register_text(html: &str) -> String {
    let mut best_text = String::new();
    let mut best_len = 0;
    for article in blocks(html, "article", "") {
        let paras: Vec<String> = blocks(&article, "p", "")
            .iter()
            .map(|p| fragment_to_text(p))
            .filter(|t| !t.is_empty())
            .collect();
        let total: usize = paras.iter().map(|p| p.chars().count()).sum();
        if total > best_len {
            best_len = total;
            let mut iter = paras.into_iter().peekable();
            if let Some(first) = iter.peek()
                && REGISTER_LEADING_LABELS.contains(&first.trim().to_lowercase().as_str())
            {
                iter.next();
            }
            best_text = iter.collect::<Vec<_>>().join("\n\n");
        }
    }
    if best_text.chars().count() < 300 {
        return String::new();
    }
    best_text
}

/// Axios dedicated extractor (Python `_extract_axios_text`): the story
/// body from the __NEXT_DATA__ JSON blob, >= 200 chars.
fn axios_text(html: &str) -> String {
    if !html.contains("__NEXT_DATA__") {
        return String::new();
    }
    for raw in blocks(html, "script", "__next_data__") {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
            continue;
        };
        let story = data
            .get("props")
            .and_then(|p| p.get("pageProps"))
            .and_then(|p| p.get("data"))
            .and_then(|d| d.get("story"));
        let Some(story) = story else { continue };
        let fragments: Vec<String> = match story.get("bodyHtml") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Object(map)) => ["beforeKeepReading", "afterKeepReading"]
                .iter()
                .filter_map(|k| map.get(*k).and_then(|v| v.as_str()).map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        let text = fragments
            .iter()
            .map(|f| fragment_to_text(f))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.chars().count() >= 200 {
            return text;
        }
    }
    String::new()
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// True when an extracted title signals an error page (404 and
/// friends), whatever the body looks like.
fn error_page_title(title: &str) -> bool {
    let t = title.to_lowercase();
    t.contains("404") || t.contains("not found") || t.contains("page not available")
}

/// True when the HEAD of an extracted body announces an error page
/// ("Error 404", "can't find the page", ...). Titles alone miss
/// these: readability may title the result after some on-page
/// heading while the body opens with the error itself.
fn error_page_text(text: &str) -> bool {
    let head: String = text.chars().take(800).collect::<String>().to_lowercase();
    [
        "error 404",
        "page not found",
        "can't find the page",
        "could not find the page",
        "couldn't find the page",
        "page you're looking for",
        "page you are looking for",
    ]
    .iter()
    .any(|m| head.contains(m))
}

/// Article prose shape: most of the text lives in long paragraph
/// lines. Chrome/navigation dumps are many short fragments, so
/// they fail this even when long overall.
fn looks_like_prose(text: &str) -> bool {
    let total = char_count(text);
    if total == 0 {
        return false;
    }
    let long_chars: usize = text
        .lines()
        .map(str::trim)
        .filter(|l| char_count(l) >= 120)
        .map(char_count)
        .sum();
    long_chars * 2 >= total
}

/// Python `_looks_like_paywall_stub`: short body + subscribe CTA.
fn looks_like_paywall_stub(text: &str) -> bool {
    if text.chars().count() > PAYWALL_STUB_MAX_CHARS {
        return false;
    }
    let low = text.to_lowercase();
    PAYWALL_CTA_PHRASES.iter().any(|p| low.contains(p))
}

/// Python `_looks_like_link_list` (constants ported; ratio rule kept):
/// short body, >= 4 lines, most lines short and not sentence-ended.
fn looks_like_link_list(text: &str) -> bool {
    if text.chars().count() > 1500 {
        return false;
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() < 4 {
        return false;
    }
    let linky = lines
        .iter()
        .filter(|l| l.chars().count() <= 90 && !l.ends_with(['.', '!', '?', '…']))
        .count();
    linky * 10 >= lines.len() * 7
}

/// Remove zero-width characters (Reuters anti-scraping injection;
/// they corrupt word matching and trip screen readers).
fn strip_zero_width(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
            )
        })
        .collect()
}

/// Fetch a page's HTML for full-text reading.
pub fn fetch_html(url: &str) -> Result<String, String> {
    fetch_html_with_cookies(url, "")
}

/// Like `fetch_html`, sending a site-cookies header when one is
/// provided (from the Netscape jar beside the config). The body
/// is decoded from raw bytes (header/meta charset chain), not
/// ureq's UTF-8 assumption.
pub fn fetch_html_with_cookies(url: &str, cookie_header: &str) -> Result<String, String> {
    let (bytes, content_type) = fetch_page_bytes(url, cookie_header)?;
    Ok(decode_bytes(&bytes, &content_type, ""))
}

/// Convert an HTML fragment (feed content, articleBody payloads,
/// stored bodies) to clean readable plain text. Feed content is
/// stored as HTML; showing it raw put markup in the reader
/// (Brandon, 2026-10-30). Rules: script/style/svg dropped with
/// their contents; block elements become paragraph breaks; list
/// items become lines; entities decoded (named common set +
/// numeric); whitespace normalized (never more than one blank
/// line).
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut skipping: Option<&'static str> = None;
    while i < bytes.len() {
        if let Some(close) = skipping {
            if lower[i..].starts_with(close) {
                i += close.len();
                skipping = None;
            } else {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'<'
            && i + 1 < bytes.len()
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'/' || bytes[i + 1] == b'!')
        {
            let Some(end) = html[i..].find('>') else {
                break;
            };
            let tag_src = &lower[i + 1..i + end];
            let is_close = tag_src.starts_with('/');
            let tag = tag_src
                .trim_start_matches('/')
                .split([' ', '\t', '\n', '/'])
                .next()
                .unwrap_or("");
            // Breaks come from opening tags only; a close tag right
            // after would double every paragraph/list break.
            if is_close {
                i += end + 1;
                continue;
            }
            match tag {
                "script" => skipping = Some("</script"),
                "style" => skipping = Some("</style"),
                "svg" => skipping = Some("</svg"),
                "br" | "hr" => out.push('\n'),
                "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "aside"
                | "nav" | "blockquote" | "pre" | "table" | "tr" | "ul" | "ol" | "figure"
                | "figcaption" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    out.push_str("\n\n");
                }
                "li" => out.push('\n'),
                "td" | "th" => out.push(' '),
                _ => {}
            }
            i += end + 1;
            continue;
        }
        if bytes[i] == b'&' {
            let Some(semi) = html[i..].find(';').filter(|s| *s <= 12) else {
                out.push('&');
                i += 1;
                continue;
            };
            let entity = &html[i + 1..i + semi];
            let decoded = decode_entity(entity);
            match decoded {
                Some(text) => {
                    out.push_str(&text);
                    i += semi + 1;
                }
                None => {
                    out.push('&');
                    i += 1;
                }
            }
            continue;
        }
        // A '<' that does not open a tag (prose like "5 < 10") is
        // literal text; consume it singly so the run below always
        // makes progress.
        if bytes[i] == b'<' {
            out.push('<');
            i += 1;
            continue;
        }
        // Plain text run up to the next tag/entity.
        let start = i;
        while i < bytes.len() && bytes[i] != b'<' && bytes[i] != b'&' {
            i += 1;
        }
        out.push_str(&html[start..i]);
    }
    normalize_text(&out)
}

fn decode_entity(entity: &str) -> Option<String> {
    if let Some(num) = entity.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "rsquo" => "\u{2019}",
        "lsquo" => "\u{2018}",
        "rdquo" => "\u{201D}",
        "ldquo" => "\u{201C}",
        "hellip" => "\u{2026}",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "copy" => "\u{00A9}",
        "reg" => "\u{00AE}",
        "trade" => "\u{2122}",
        "laquo" => "\u{00AB}",
        "raquo" => "\u{00BB}",
        "middot" => "\u{00B7}",
        "bull" => "\u{2022}",
        "dagger" => "\u{2020}",
        "permil" => "\u{2030}",
        "euro" => "\u{20AC}",
        "pound" => "\u{00A3}",
        "yen" => "\u{00A5}",
        "cent" => "\u{00A2}",
        "deg" => "\u{00B0}",
        "plusmn" => "\u{00B1}",
        "times" => "\u{00D7}",
        "divide" => "\u{00F7}",
        "micro" => "\u{00B5}",
        "sect" => "\u{00A7}",
        "para" => "\u{00B6}",
        _ => return None,
    };
    Some(named.to_string())
}

/// Collapse runs of spaces/tabs, cap blank lines at one, trim lines.
fn normalize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    for line in text.lines() {
        let cleaned: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if cleaned.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
            out.push('\n');
        } else {
            blank_run = 0;
            out.push_str(&cleaned);
            out.push('\n');
        }
    }
    out.trim().to_string()
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

/// One rust-trafilatura pass with an explicit focus.
fn extract_focus(html: &str, focus: rust_trafilatura::ExtractionFocus) -> (String, String) {
    let opts = rust_trafilatura::Options {
        focus,
        ..Default::default()
    };
    rust_trafilatura::extract(html.as_bytes(), &opts)
        .ok()
        .map(|r| (r.metadata.title.clone(), r.content_text.trim().to_string()))
        .unwrap_or_default()
}

/// Whitespace-collapsed paragraph key for cross-pass matching.
fn para_key(p: &str) -> String {
    p.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Python's lead recovery, lean form: when the precision pass
/// dropped the article's opening paragraphs, the recall pass
/// usually kept them. Prepend recall paragraphs that come before
/// the precision text's first paragraph and are absent from it
/// (at most three, each substantial). Python's full version also
/// matches the meta description against an allowlist; that depth
/// stays on the parity list.
fn recover_lead(precision: &str, recall: &str) -> String {
    let prec_paras: Vec<&str> = precision
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    let rec_paras: Vec<&str> = recall
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    let Some(first_prec) = prec_paras.first() else {
        return precision.to_string();
    };
    let first_key = para_key(first_prec);
    let Some(pos) = rec_paras
        .iter()
        .position(|p| para_key(p).starts_with(&first_key[..first_key.len().min(120)]))
    else {
        return precision.to_string();
    };
    if pos == 0 || pos > 3 {
        return precision.to_string();
    }
    let prec_keys: Vec<String> = prec_paras.iter().map(|p| para_key(p)).collect();
    let lead: Vec<&str> = rec_paras[..pos]
        .iter()
        .copied()
        .filter(|p| p.chars().count() >= 40 && !prec_keys.contains(&para_key(p)))
        .collect();
    if lead.is_empty() {
        return precision.to_string();
    }
    format!("{}\n\n{}", lead.join("\n\n"), precision)
}

/// Run the cascade over a fetched page.
///
/// `feed_text` is the article body the feed itself supplied, if any; the
/// prefer-feed-content rule can keep it when the scrape is worse.
pub fn extract_article(html_in: &str, url: &str, feed_text: Option<&str>) -> ExtractOutcome {
    // Bloomberg video pages carry their text in the JSON-LD
    // description; the generic cascade sees a video shell.
    if let Some(text) = bloomberg_video_text(html_in, url) {
        return ExtractOutcome::Article {
            title: String::new(),
            text,
            source: "bloomberg",
        };
    }
    match extract_article_impl(html_in, url, feed_text) {
        ExtractOutcome::Article {
            title,
            text,
            source,
        } => ExtractOutcome::Article {
            title,
            text: apply_site_cleaners(&text, url),
            source,
        },
        other => other,
    }
}

fn extract_article_impl(html_in: &str, url: &str, feed_text: Option<&str>) -> ExtractOutcome {
    // Pre-clean zero-width characters from the page itself: engines
    // can mangle them into mojibake before the output strip can run.
    let cleaned;
    let html = if html_in.chars().any(|c| {
        matches!(
            c,
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
        )
    }) {
        cleaned = strip_zero_width(html_in);
        &cleaned
    } else {
        html_in
    };
    let html_lower = html.to_lowercase();
    let marker = gate_marker(&html_lower);

    // Feed bodies are stored HTML: convert once, so the length
    // comparisons and the feed fallbacks all work on the same clean
    // text the reader will show (never raw markup).
    let feed_owned = feed_text.map(html_to_text);
    let feed_text = feed_owned.as_deref();

    // Base engine: rust-trafilatura, precision-first — Python's
    // recipe (article_extractor._trafilatura_extract_text):
    // favor_precision is the default pass because balanced/recall
    // drag in footer and affiliate boilerplate (measured on
    // Brandon's 9to5Mac example, 2026-10-30: balanced kept the
    // "Worth checking out on Amazon" list and the FTC disclosure;
    // precision matched Python's output exactly). Recall runs as
    // the fallback when precision is too short, and for lead
    // recovery when precision dropped the intro paragraphs.
    let prec = extract_focus(html, rust_trafilatura::ExtractionFocus::FavorPrecision);
    let rec = extract_focus(html, rust_trafilatura::ExtractionFocus::FavorRecall);
    let base = if char_count(&prec.1) >= 200 {
        let title = if prec.0.is_empty() {
            rec.0.clone()
        } else {
            prec.0.clone()
        };
        (title, recover_lead(&prec.1, &rec.1))
    } else if char_count(&rec.1) > char_count(&prec.1) {
        rec
    } else {
        prec
    };
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

    // Engine-disagreement rule: a thin base plus a much longer
    // readability result is ambiguous — on an error page it means
    // readability inflated the chrome (BBC 404 evidence), but on a
    // real article it means the base engine missed and readability
    // is right (current BBC news pages: rust-trafilatura is thin
    // there, and the blanket rejection discarded good full text —
    // Brandon's "full text is missing", 2026-10-30). Vet the long
    // result: error-page titles or chrome-shaped (non-prose) text
    // stay gated; prose falls through to the normal pick.
    if base_chars < THIN_CHARS && second_chars > base_chars * 3 && second_chars > 900 {
        let errorish =
            error_page_title(&second.0) || error_page_title(&base.0) || error_page_text(&second.1);
        if errorish || !looks_like_prose(&second.1) {
            return ExtractOutcome::Gate {
                reason: "engines disagree: thin base vs inflated readability (error/chrome page)"
                    .to_string(),
            };
        }
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

    // Dedicated site extractors and structured data run before the
    // generic pick (Python order): The Register and Axios have
    // canonical bodies the generic engines miss; JSON-LD articleBody
    // recovers bodies when the DOM scrape is thin.
    let host = host_of(url);
    let site_text = if host.contains("theregister.com") || host.contains("theregister.co.uk") {
        register_text(html)
    } else if host.contains("axios.com") {
        axios_text(html)
    } else {
        String::new()
    };
    let jsonld = if site_text.is_empty() {
        json_ld_text(html)
    } else {
        String::new()
    };

    // Pick the better candidate: the longer one, unless the second is
    // only marginally longer (base wins ties; it keeps tables better).
    let (title, mut text, source) = if !site_text.is_empty() {
        (base.0.clone(), site_text, "site")
    } else if char_count(&jsonld) > base_chars.max(second_chars) {
        (base.0.clone(), jsonld, "jsonld")
    } else if second_chars > base_chars.saturating_add(base_chars / 4) {
        (second.0, second.1, "second")
    } else {
        (base.0, base.1, "base")
    };

    // Stub rejections (Python `_looks_like_paywall_stub` /
    // `_looks_like_link_list`): never present a stub as the article;
    // fall back to substantial feed content, else report the gate.
    if looks_like_paywall_stub(&text) || looks_like_link_list(&text) {
        if let Some(feed) = feed_text
            && char_count(feed) >= 200
        {
            return ExtractOutcome::Article {
                title,
                text: feed.trim().to_string(),
                source: "feed",
            };
        }
        let reason = if looks_like_paywall_stub(&text) {
            PAYWALL_NOTICE.to_string()
        } else {
            "page is a link list, not an article".to_string()
        };
        return ExtractOutcome::Gate { reason };
    }

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
    // Reuters-style zero-width injection is stripped for clean
    // word matching and screen-reader output.
    text = strip_zero_width(&text);
    // Metered preview: say so instead of trailing off mid-story.
    if METERED_MARKERS.iter().any(|m| html_lower.contains(m)) {
        text.push_str("\n\n");
        text.push_str(METERED_NOTICE);
    }
    ExtractOutcome::Article {
        title,
        text,
        source,
    }
}

// ---------------------------------------------------------------------------
// Fetch layer: byte-accurate decoding + the fallback chain
// (Python `_fetch_page` / `_try_fallbacks`).
//
// Order (Python's, minus the two stages a desktop app without
// a browser engine or a TLS-impersonation library cannot run —
// curl_cffi impersonation and the warm-browser fetch are
// documented non-ports in PARITY §7):
//   direct (with stored site cookies)
//   -> Sky News via Google Translate (news.sky.com only)
//   -> Jina reader as text (only when a gate was seen)
//   -> Smry (always)
//   -> Wayback Machine (always, last resort)
// Every fallback body is re-checked against the gate markers
// before it is trusted, exactly like Python.
// ---------------------------------------------------------------------------

/// Decode an HTTP body the Python way (issue #75): a per-feed
/// override wins, then BOM, then the Content-Type charset,
/// then a <meta charset> declaration, then UTF-8 — always with
/// replacement so a wrong guess shows text instead of failing.
pub fn decode_bytes(bytes: &[u8], content_type: &str, override_enc: &str) -> String {
    let label = if !override_enc.trim().is_empty() {
        override_enc.trim().to_string()
    } else if let Some(enc) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .0
            .decode_without_bom_handling_and_without_replacement(&bytes[enc.1..])
            .map(|c| c.into_owned())
            .unwrap_or_else(|| enc.0.decode(&bytes[enc.1..]).0.into_owned());
    } else if let Some(cs) = charset_from_content_type(content_type) {
        cs
    } else if let Some(cs) = charset_from_meta(bytes) {
        cs
    } else {
        "utf-8".to_string()
    };
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(enc) => enc.decode(bytes).0.into_owned(),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn charset_from_content_type(content_type: &str) -> Option<String> {
    for part in content_type.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("charset=") {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn charset_from_meta(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let head = String::from_utf8_lossy(head).to_lowercase();
    if let Some(pos) = head.find("charset") {
        let rest = &head[pos + "charset".len()..];
        let rest = rest.trim_start_matches([' ', '=', '"', '\'']);
        let label: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !label.is_empty() {
            return Some(label);
        }
    }
    None
}

/// Fetch a URL's body bytes + Content-Type through the shared
/// agent, sending the site's stored cookie header when given.
pub fn fetch_page_bytes(url: &str, cookie_header: &str) -> Result<(Vec<u8>, String), String> {
    let req = crate::http::agent().get(url);
    let req = if cookie_header.is_empty() {
        req
    } else {
        req.header("Cookie", cookie_header)
    };
    let mut resp = req.call().map_err(|e| format!("fetch failed: {e}"))?;
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| format!("read failed: {e}"))?;
    Ok((bytes, content_type))
}

/// Percent-encode a URL for use as a query parameter value.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// True when the body looks like a bot gate (Python's
/// `_looks_like_bot_interstitial` approximation over the
/// ported marker set).
fn looks_gated(body: &str) -> bool {
    let lower = body.to_lowercase();
    gate_marker(&lower).is_some() && body.len() < 60_000
}

/// Jina reader proxy as plain text (Python `_download_via_
/// jina` + `_download_via_jina_as_text`): the proxy answers in
/// Markdown; the "Markdown Content:" section is converted to
/// plain text so no **labels**, * bullets or [text](url)
/// debris reaches the reader.
pub fn fetch_via_jina_text(url: &str) -> Option<String> {
    let target = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))
        .unwrap_or(url.trim());
    if target.is_empty() {
        return None;
    }
    let mut resp = crate::http::agent()
        .get(&format!("https://r.jina.ai/http://{target}"))
        .header("Accept", "text/plain, text/markdown, */*;q=0.8")
        .call()
        .ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let text = match text.split_once("Markdown Content:") {
        Some((_, rest)) => rest.trim().to_string(),
        None => text,
    };
    if text.trim().is_empty() || looks_gated(&text) {
        return None;
    }
    Some(markdown_to_text(&text))
}

/// Convert proxy Markdown to plain text: links keep their
/// label, images drop, emphasis markers and heading hashes go.
pub fn markdown_to_text(md: &str) -> String {
    let mut out_lines: Vec<String> = Vec::new();
    for line in md.lines() {
        let mut l = line.trim().to_string();
        // Images: ![alt](url) -> drop entirely.
        while let Some(start) = l.find("![") {
            if let Some(end) = l[start..].find(')') {
                l.replace_range(start..start + end + 1, "");
            } else {
                break;
            }
        }
        // Links: [label](url) -> label.
        let mut guard = 0;
        while let Some(open) = l.find('[') {
            guard += 1;
            if guard > 50 {
                break;
            }
            let Some(close) = l[open..].find("](") else {
                break;
            };
            let label = l[open + 1..open + close].to_string();
            let Some(end) = l[open + close..].find(')') else {
                break;
            };
            l.replace_range(open..open + close + end + 1, &label);
        }
        let l = l.trim_start_matches(['#', '>', ' ']);
        let l = l.trim_start_matches("* ").trim_start_matches("- ");
        let l = l.replace("**", "").replace("__", "");
        let l = l.trim().to_string();
        if !l.is_empty()
            || out_lines
                .last()
                .map(|p: &String| !p.is_empty())
                .unwrap_or(false)
        {
            out_lines.push(l);
        }
    }
    out_lines.join("\n").trim().to_string()
}

/// Parse a Smry SSE stream into (title, plain text): find the
/// `data:` line whose JSON carries an "article" object
/// (Python `_download_via_smry`). Minimum 300 chars of text.
pub fn parse_smry_stream(sse: &str) -> Option<(String, String)> {
    for line in sse.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
            continue;
        };
        let Some(article) = value.get("article").filter(|a| a.is_object()) else {
            continue;
        };
        let plain = article
            .get("textContent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                article
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(html_to_text)
            })?;
        if plain.trim().chars().count() < 300 {
            return None;
        }
        let title = article
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return Some((title, plain.trim().to_string()));
    }
    None
}

/// Smry reader endpoint (SSE).
pub fn fetch_via_smry(url: &str) -> Option<(String, String)> {
    let api = format!(
        "https://smry.ai/api/article/auto/stream?url={}&uiLocale=en",
        url_encode(url)
    );
    let mut resp = crate::http::agent()
        .get(&api)
        .header(
            "Accept",
            "text/event-stream, application/json;q=0.9, */*;q=0.8",
        )
        .call()
        .ok()?;
    let sse = resp.body_mut().read_to_string().ok()?;
    parse_smry_stream(&sse)
}

/// Rewrite a Wayback snapshot URL to its raw `id_` form
/// (Python `_wayback_raw_url`).
pub fn wayback_raw_url(snapshot: &str) -> String {
    let Some(pos) = snapshot.find("/web/") else {
        return snapshot.to_string();
    };
    let after = &snapshot[pos + 5..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 || digits.len() > 14 {
        return snapshot.to_string();
    }
    let rest = &after[digits.len()..];
    if rest.starts_with("id_/") {
        return snapshot.to_string();
    }
    if let Some(stripped) = rest.strip_prefix('/') {
        return format!("{}id_/{}", &snapshot[..pos + 5 + digits.len()], stripped);
    }
    snapshot.to_string()
}

/// Wayback Machine: closest snapshot, raw form first
/// (Python `_download_via_wayback`).
pub fn fetch_via_wayback(url: &str, cookie_header: &str) -> Option<String> {
    let api = format!(
        "https://archive.org/wayback/available?url={}",
        url_encode(url)
    );
    let mut resp = crate::http::agent().get(&api).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let closest = value.get("archived_snapshots")?.get("closest")?;
    if closest.get("available").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let snap = closest.get("url")?.as_str()?.trim().to_string();
    let snap = snap
        .strip_prefix("http://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or(snap);
    for candidate in [wayback_raw_url(&snap), snap] {
        if let Ok((bytes, ct)) = fetch_page_bytes(&candidate, cookie_header) {
            let body = decode_bytes(&bytes, &ct, "");
            if !body.trim().is_empty() && !looks_gated(&body) {
                return Some(body);
            }
        }
    }
    None
}

/// Sky News via the Google Translate web proxy (Python
/// `_download_sky_via_google_translate`): Sky's Akamai edge
/// 403s plain clients; the translate route serves the live
/// page with its markup intact.
pub fn fetch_via_sky_translate(url: &str) -> Option<String> {
    let host = url.split('/').nth(2).unwrap_or("").to_lowercase();
    if host != "news.sky.com" {
        return None;
    }
    let after_host = &url[url.find("news.sky.com")? + "news.sky.com".len()..];
    let (path, query) = match after_host.split_once('?') {
        Some((p, q)) => (p, q.to_string()),
        None => (after_host, String::new()),
    };
    let path = if path.is_empty() { "/" } else { path };
    let query = if query.is_empty() {
        "_x_tr_sl=auto&_x_tr_tl=en&_x_tr_hl=en".to_string()
    } else {
        format!("{query}&_x_tr_sl=auto&_x_tr_tl=en&_x_tr_hl=en")
    };
    let proxy = format!("https://news-sky-com.translate.goog{path}?{query}");
    let (bytes, ct) = fetch_page_bytes(&proxy, "").ok()?;
    let body = decode_bytes(&bytes, &ct, "");
    if body.trim().is_empty() || looks_gated(&body) {
        None
    } else {
        Some(body)
    }
}

/// Fetch a page and extract its full text, running Python's
/// fallback chain when the direct fetch is gated or empty.
/// Returns the outcome plus the direct-fetch HTML when there
/// was one (metadata enrichment reads it).
///
/// `source` on an Article outcome names the winning stage:
/// the cascade's own label for a direct win, or "sky",
/// "jina", "smry", "wayback".
pub fn fetch_full_text(
    url: &str,
    cookie_header: &str,
    feed_text: Option<&str>,
    encoding_override: &str,
) -> (ExtractOutcome, Option<String>) {
    // Thread reconstructions (Lemmy/GitHub/Reddit/Discourse
    // APIs) beat HTML scraping for thread URLs.
    if let Some((title, text)) = crate::threads::thread_text(url) {
        return (
            ExtractOutcome::Article {
                title,
                text,
                source: "thread",
            },
            None,
        );
    }
    let mut first_html: Option<String> = None;
    let direct = fetch_page_bytes(url, cookie_header)
        .map(|(bytes, ct)| decode_bytes(&bytes, &ct, encoding_override));
    let mut direct_outcome: Option<ExtractOutcome> = None;
    let mut gate_seen = false;
    if let Ok(html) = &direct {
        first_html = Some(html.clone());
        let outcome = extract_article(html, url, feed_text);
        gate_seen = matches!(outcome, ExtractOutcome::Gate { .. });
        if matches!(outcome, ExtractOutcome::Article { .. }) {
            return (outcome, first_html);
        }
        direct_outcome = Some(outcome);
    }
    let relabel = |outcome: ExtractOutcome, label: &'static str| match outcome {
        ExtractOutcome::Article { title, text, .. } => ExtractOutcome::Article {
            title,
            text,
            source: label,
        },
        other => other,
    };
    // Sky via Google Translate (host-gated inside the fn).
    if let Some(html) = fetch_via_sky_translate(url) {
        let outcome = extract_article(&html, url, feed_text);
        if matches!(outcome, ExtractOutcome::Article { .. }) {
            return (relabel(outcome, "sky"), first_html);
        }
    }
    // Jina as text, only when a gate was seen.
    if gate_seen
        && let Some(text) = fetch_via_jina_text(url)
        && text.chars().count() >= 200
    {
        return (
            ExtractOutcome::Article {
                title: String::new(),
                text,
                source: "jina",
            },
            first_html,
        );
    }
    // Smry (always tried).
    if let Some((title, text)) = fetch_via_smry(url) {
        return (
            ExtractOutcome::Article {
                title,
                text,
                source: "smry",
            },
            first_html,
        );
    }
    // Wayback (last resort).
    if let Some(html) = fetch_via_wayback(url, cookie_header) {
        let outcome = extract_article(&html, url, feed_text);
        if matches!(outcome, ExtractOutcome::Article { .. }) {
            return (relabel(outcome, "wayback"), first_html);
        }
    }
    // Nothing worked: report the direct outcome (Gate/Empty)
    // or the fetch failure as a Gate with its reason.
    match direct_outcome {
        Some(outcome) => (outcome, first_html),
        None => (
            ExtractOutcome::Gate {
                reason: direct.err().unwrap_or_else(|| "fetch failed".to_string()),
            },
            first_html,
        ),
    }
}

// ---------------------------------------------------------------------------
// Site-specific text cleaners (Python's boilerplate strips),
// applied to every cascade Article result.
// ---------------------------------------------------------------------------

fn host_matches(url: &str, domain: &str) -> bool {
    let host = host_of(url);
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// BBC: drop the fused "ShareSave" chrome and complete (or
/// drop) the dangling "- Published" byline lead.
pub fn strip_bbc_boilerplate(text: &str) -> String {
    let mut t = text.replace("ShareSave", "");
    let trimmed = t.trim_start().to_string();
    if let Some(rest) = trimmed.strip_prefix("- Published\n") {
        t = rest.to_string();
    }
    t
}

/// WSJ / Dow Jones: strip the trailing copyright line + the
/// opaque tracking hash after "All Rights Reserved.".
pub fn strip_wsj_boilerplate(text: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.rfind("copyright") {
        let tail = &text[pos..];
        if tail.to_lowercase().contains("dow jones")
            && tail.to_lowercase().contains("all rights reserved.")
        {
            return text[..pos].trim_end().to_string();
        }
    }
    text.to_string()
}

/// TechRadar (Future sites): cut the trailing comment-gate
/// block (gate line + author bio + optional promo line above).
pub fn strip_techradar_boilerplate(text: &str) -> String {
    let paras: Vec<&str> = text.split("\n\n").collect();
    let gate = paras.iter().position(|p| {
        p.to_lowercase()
            .starts_with("you must confirm your public display name before commenting")
    });
    let Some(mut cut) = gate else {
        return text.to_string();
    };
    if cut >= 1 {
        cut -= 1; // author bio above the gate
    }
    if cut >= 1 {
        let above = paras[cut - 1].to_lowercase();
        if above.starts_with("follow techradar") || above.starts_with("sign up for breaking news") {
            cut -= 1;
        }
    }
    paras[..cut].join("\n\n").trim().to_string()
}

/// Apply the site cleaners for a URL to extracted text.
pub fn apply_site_cleaners(text: &str, url: &str) -> String {
    let mut t = text.to_string();
    if host_matches(url, "bbc.com") || host_matches(url, "bbc.co.uk") {
        t = strip_bbc_boilerplate(&t);
    }
    if host_matches(url, "wsj.com") {
        t = strip_wsj_boilerplate(&t);
    }
    if host_matches(url, "techradar.com") {
        t = strip_techradar_boilerplate(&t);
    }
    t
}

/// Bloomberg video pages: the article text lives in the
/// JSON-LD description (Python `_extract_bloomberg_video_
/// text`). Returns the longest clean description >= 80 chars.
pub fn bloomberg_video_text(html: &str, url: &str) -> Option<String> {
    if !host_matches(url, "bloomberg.com") {
        return None;
    }
    let path = url.to_lowercase();
    if !path.contains("/videos/") && !path.contains("/news/videos/") {
        return None;
    }
    let mut candidates: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("application/ld+json") {
        let after = &rest[pos..];
        let Some(start) = after.find('>') else { break };
        let Some(end) = after.find("</script>") else {
            break;
        };
        let raw = &after[start + 1..end];
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
            collect_descriptions(&value, &mut candidates);
        }
        rest = &after[end..];
    }
    if let Some(desc) = meta_content(html, "description") {
        candidates.push(desc);
    }
    candidates
        .into_iter()
        .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|c| c.chars().count() >= 80 && !looks_gated(c))
        .max_by_key(|c| c.chars().count())
}

fn collect_descriptions(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if k == "description"
                    && let Some(s) = v.as_str()
                {
                    out.push(s.to_string());
                }
                collect_descriptions(v, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_descriptions(item, out);
            }
        }
        _ => {}
    }
}

/// Read a <meta name/property="..." content="..."> value with
/// a tolerant hand scan (attribute order varies).
pub fn meta_content(html: &str, name: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let mut search = 0usize;
    while let Some(pos) = lower[search..].find("<meta") {
        let start = search + pos;
        let end = lower[start..].find('>').map(|e| start + e)?;
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        if (tag_lower.contains(&format!("\"{}\"", name.to_lowercase()))
            || tag_lower.contains(&format!("'{}\'", name.to_lowercase())))
            && let Some(cpos) = tag_lower.find("content=")
        {
            let rest = &tag[cpos + "content=".len()..];
            let rest = rest.trim_start();
            if let Some(stripped) = rest.strip_prefix('"') {
                if let Some(close) = stripped.find('"') {
                    return Some(stripped[..close].to_string());
                }
            } else if let Some(stripped) = rest.strip_prefix('\'')
                && let Some(close) = stripped.find('\'')
            {
                return Some(stripped[..close].to_string());
            }
        }
        search = end;
    }
    None
}
