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

use regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const EMPTY_EXTRACTION_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
struct EmptyExtractions(VecDeque<((String, String), Instant)>);

impl EmptyExtractions {
    fn contains(&mut self, url: &str, encoding: &str, now: Instant) -> bool {
        let Some(index) = self
            .0
            .iter()
            .position(|((u, e), _)| u == url && e == encoding)
        else {
            return false;
        };
        let entry = self.0.remove(index).unwrap();
        if now.duration_since(entry.1) >= EMPTY_EXTRACTION_TTL {
            return false;
        }
        self.0.push_back(entry);
        true
    }

    fn remember(&mut self, url: &str, encoding: &str, now: Instant) {
        self.0.retain(|((u, e), _)| u != url || e != encoding);
        if self.0.len() == 256 {
            self.0.pop_front();
        }
        self.0
            .push_back(((url.to_string(), encoding.to_string()), now));
    }
}

static EMPTY_EXTRACTIONS: OnceLock<Mutex<EmptyExtractions>> = OnceLock::new();

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

pub(crate) fn char_count(s: &str) -> usize {
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
pub(crate) fn unescape_entities(s: &str) -> String {
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

/// Fix `word ,` spacing left by tag-boundary joins (Python's
/// `re.sub(r"\s+([,.;:!?])", r"\1", ...)` in the fragment readers).
fn fix_punct_spacing(text: &str) -> String {
    static PUNCT_RE: OnceLock<Regex> = OnceLock::new();
    let re = PUNCT_RE.get_or_init(|| Regex::new(r"\s+([,.;:!?])").unwrap());
    re.replace_all(text, "$1").to_string()
}

/// Python `_html_fragment_to_text`: convert a trusted CMS HTML
/// body fragment into paragraph text — leaf block elements
/// (p/li/headings/blockquote) become paragraphs; embed loaders
/// and tracking markup never reach the text.
fn html_fragment_to_text(fragment: &str) -> String {
    let (cleaned, _) = remove_elements(fragment, &|name, _| {
        matches!(name, "script" | "style" | "noscript" | "template")
    });
    let spans = element_spans(&cleaned);
    let blocks: Vec<&NamedSpan> = spans
        .iter()
        .filter(|s| {
            matches!(
                s.name.as_str(),
                "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote"
            )
        })
        .collect();
    let mut paras: Vec<String> = Vec::new();
    for block in &blocks {
        // Skip container blocks (a blockquote wrapping <p>s);
        // keep the leaf blocks.
        let is_container = spans
            .iter()
            .any(|s| (s.name == "p" || s.name == "li") && block.contains(s));
        if is_container {
            continue;
        }
        let text = fix_punct_spacing(&crate::postprocess::normalize_whitespace(
            &html_to_text_raw(block.inner(&cleaned), false),
        ));
        if !text.is_empty() {
            paras.push(text);
        }
    }
    if !paras.is_empty() {
        return paras.join("\n\n");
    }
    crate::postprocess::normalize_whitespace(&html_to_text_raw(&cleaned, false))
}

/// Python `_extract_article_paragraph_text`: the visible <p>
/// paragraphs of the page's <article> (else <main>), for JSON-LD
/// lead comparison.
fn article_paragraph_text(html: &str) -> String {
    let spans = element_spans(html);
    let container = spans
        .iter()
        .find(|s| s.name == "article")
        .or_else(|| spans.iter().find(|s| s.name == "main"));
    let Some(container) = container else {
        return String::new();
    };
    let mut paras: Vec<String> = Vec::new();
    for p in spans
        .iter()
        .filter(|s| s.name == "p" && container.contains(s))
    {
        let text = fix_punct_spacing(&crate::postprocess::normalize_whitespace(
            &html_to_text_raw(p.inner(html), false),
        ));
        if !text.is_empty() {
            paras.push(text);
        }
    }
    paras.join("\n\n")
}

/// Python `_prepend_missing_lead`: re-attach opening paragraphs
/// that only `alt` captured to `base`, when `alt` then lines up
/// with the very start of `base` — the alignment requirement is
/// what keeps unrelated prose from being prepended.
fn prepend_missing_lead(base: &str, alt: &str) -> String {
    let base_norm = para_key(base);
    if base_norm.is_empty() {
        return base.to_string();
    }
    let base_head: String = base_norm.chars().take(400).collect();
    let mut leads: Vec<String> = Vec::new();
    let mut aligned = false;
    for p in crate::postprocess::split_paragraphs(alt).iter().take(4) {
        let pn = para_key(p);
        if pn.is_empty() {
            continue;
        }
        let snippet: String = pn.chars().take(60).collect();
        if base_head.contains(&snippet) {
            aligned = true;
            break;
        }
        if base_norm.contains(&pn) {
            // Present deeper in base: base is not simply missing
            // its head; don't guess.
            break;
        }
        if leads.len() >= 2 {
            break;
        }
        if !is_reasonable_lead_paragraph(p) {
            break;
        }
        leads.push(p.clone());
    }
    if !aligned || leads.is_empty() {
        return base.to_string();
    }
    let mut parts = leads;
    parts.push(base.to_string());
    crate::postprocess::normalize_whitespace(&parts.join("\n\n"))
}

/// Python `_patch_json_ld_missing_lead`.
fn patch_json_ld_missing_lead(json_text: &str, alts: &[&str]) -> String {
    let mut patched = crate::postprocess::normalize_whitespace(json_text);
    for alt in alts {
        if !alt.is_empty() {
            let alt_norm = crate::postprocess::normalize_whitespace(alt);
            patched = prepend_missing_lead(&patched, &alt_norm);
        }
    }
    patched
}

/// JSON-LD articleBody recovery (Python `_extract_json_ld_text`):
/// longest `articleBody`/`text` value in any ld+json block, at
/// least 120 chars. Markup bodies keep their paragraph structure
/// and any standfirst before the first tag; plain bodies are
/// entity-decoded; leaked script code is stripped before the
/// length comparison so junk cannot win on bulk.
fn json_ld_text(html: &str) -> String {
    fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for field in ["articleBody", "text"] {
                    match map.get(field) {
                        Some(serde_json::Value::String(s)) => out.push(s.clone()),
                        Some(serde_json::Value::Array(items)) => {
                            let joined = items
                                .iter()
                                .filter_map(|i| i.as_str())
                                .collect::<Vec<_>>()
                                .join(" ");
                            if !joined.is_empty() {
                                out.push(joined);
                            }
                        }
                        _ => {}
                    }
                }
                for val in map.values() {
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
    static ESCAPED_BLOCK_RE: OnceLock<Regex> = OnceLock::new();
    let escaped_blocks =
        ESCAPED_BLOCK_RE.get_or_init(|| Regex::new(r"(?i)&lt;/(?:p|h[1-6]|li)&gt;").unwrap());
    static FIRST_TAG_RE: OnceLock<Regex> = OnceLock::new();
    let first_tag_re = FIRST_TAG_RE.get_or_init(|| Regex::new(r"(?i)<[a-z][^>]*>").unwrap());
    let mut cleaned: Vec<String> = Vec::new();
    for raw in blocks(html, "script", "application/ld+json") {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
            continue;
        };
        let mut candidates = Vec::new();
        collect(&data, &mut candidates);
        for mut c in candidates {
            let mut first_tag = first_tag_re.find(&c).map(|m| m.start());
            if first_tag.is_none() && escaped_blocks.find_iter(&c).take(3).count() == 3 {
                // The markup itself is entity-escaped; unescaped
                // once, it is ordinary markup. Several block
                // closers are required so prose mentioning a tag
                // stays prose.
                c = decode_entities_full(&c);
                first_tag = first_tag_re.find(&c).map(|m| m.start());
            }
            let c = match first_tag {
                Some(pos) => {
                    // Some publishers (notably Sky News) put the
                    // standfirst directly before the first <p>:
                    // preserve that leading text, then read the
                    // fragment as paragraphs.
                    let prefix =
                        decode_entities_full(&crate::postprocess::normalize_whitespace(&c[..pos]));
                    let fragment_text = html_fragment_to_text(&c[pos..]);
                    [prefix, fragment_text]
                        .into_iter()
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                }
                None => decode_entities_full(&c),
            };
            let t = crate::postprocess::strip_embedded_script_code(
                &crate::postprocess::normalize_whitespace(&c),
            );
            if !t.is_empty() {
                cleaned.push(t);
            }
        }
    }
    let best = cleaned
        .into_iter()
        .max_by_key(|t| t.chars().count())
        .unwrap_or_default();
    if best.chars().count() < 120 {
        return String::new();
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

pub(crate) fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// Balanced-element helpers for the Audit 4 site extractors. The
// `blocks` helper stops at the first close tag, which truncates
// elements nesting the same tag; these track depth instead.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct TagTok {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) name: String,
    pub(crate) close: bool,
    pub(crate) self_closing: bool,
}

/// Tokenize a document's tags (quote-aware `>` scan). Tag names
/// are lowercased; attribute text stays in the source slices.
pub(crate) fn tag_tokens(html: &str) -> Vec<TagTok> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' || i + 1 >= bytes.len() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let close = bytes[j] == b'/';
        if close {
            j += 1;
        }
        if j >= bytes.len() || !bytes[j].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let name_start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-') {
            j += 1;
        }
        let name = html[name_start..j].to_lowercase();
        let mut k = j;
        let mut quote: Option<u8> = None;
        while k < bytes.len() {
            match quote {
                Some(q) if bytes[k] == q => quote = None,
                Some(_) => {}
                None if bytes[k] == b'"' || bytes[k] == b'\'' => quote = Some(bytes[k]),
                None if bytes[k] == b'>' => break,
                None => {}
            }
            k += 1;
        }
        if k >= bytes.len() {
            break;
        }
        // Void elements (link, meta, img, hr, ...) never have a
        // close tag in HTML, with or without the XHTML slash:
        // treating one as an open tag makes the balanced scanner
        // hunt for a close that does not exist and give up on the
        // rest of the document.
        let self_closing =
            !close && ((k > j && bytes[k - 1] == b'/') || VOID_TAGS.contains(&name.as_str()));
        out.push(TagTok {
            start: i,
            end: k + 1,
            name,
            close,
            self_closing,
        });
        i = k + 1;
    }
    out
}

/// The value of attribute `name` in an open tag's source, if any.
pub(crate) fn attr_value(open_tag: &str, name: &str) -> Option<String> {
    let lower = open_tag.to_lowercase();
    let mut idx = 0usize;
    while let Some(pos) = lower[idx..].find(name) {
        let start = idx + pos;
        let before_ok = start > 0 && lower.as_bytes()[start - 1].is_ascii_whitespace();
        let rest = &lower[start + name.len()..];
        let rest_trim = rest.trim_start();
        if before_ok && rest_trim.starts_with('=') {
            let after_eq = start + name.len() + (rest.len() - rest_trim.len()) + 1;
            let v = open_tag[after_eq..].trim_start();
            if let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let end = v[1..].find(q).map(|e| e + 1).unwrap_or(v.len());
                return Some(v[1..end].to_string());
            }
            let end = v
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(v.len());
            return Some(v[..end].to_string());
        }
        idx = start + name.len();
    }
    None
}

/// True when the open tag's class list contains `token` as a whole
/// class name (CSS class-selector semantics).
pub(crate) fn has_class(open_tag: &str, token: &str) -> bool {
    attr_value(open_tag, "class")
        .map(|v| v.split_whitespace().any(|t| t == token))
        .unwrap_or(false)
}

#[derive(Clone)]
pub(crate) struct ElemSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) inner_start: usize,
    pub(crate) inner_end: usize,
}

/// Top-level elements whose open tag satisfies `pred` (optionally
/// restricted to one tag name), with balanced same-tag nesting.
pub(crate) fn find_elements(
    html: &str,
    tag: Option<&str>,
    pred: &dyn Fn(&str) -> bool,
) -> Vec<ElemSpan> {
    let toks = tag_tokens(html);
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < toks.len() {
        let tok = &toks[i];
        if tok.close
            || (tag.is_some() && tok.name != tag.unwrap())
            || !pred(&html[tok.start..tok.end])
        {
            i += 1;
            continue;
        }
        if tok.self_closing {
            out.push(ElemSpan {
                start: tok.start,
                end: tok.end,
                inner_start: tok.end,
                inner_end: tok.end,
            });
            i += 1;
            continue;
        }
        // Depth-scan for the matching close tag.
        let mut depth = 1usize;
        let mut j = i + 1;
        let mut found: Option<ElemSpan> = None;
        while j < toks.len() {
            let t2 = &toks[j];
            if t2.name == tok.name {
                if t2.close {
                    depth -= 1;
                    if depth == 0 {
                        found = Some(ElemSpan {
                            start: tok.start,
                            end: t2.end,
                            inner_start: tok.end,
                            inner_end: t2.start,
                        });
                        break;
                    }
                } else if !t2.self_closing {
                    depth += 1;
                }
            }
            j += 1;
        }
        match found {
            Some(span) => {
                out.push(span);
                // Continue after this element (top-level only).
                i = j + 1;
            }
            None => break, // unbalanced: nothing trustworthy after
        }
    }
    out
}

/// One balanced element (nested included), in the same shape as
/// article_html's private scanner: byte spans for the whole
/// element and its inner content. Sorted by (start, end desc), so
/// an ancestor always precedes its descendants.
#[derive(Clone)]
pub(crate) struct NamedSpan {
    pub(crate) name: String,
    pub(crate) start: usize,
    pub(crate) open_end: usize,
    pub(crate) inner_start: usize,
    pub(crate) inner_end: usize,
    pub(crate) end: usize,
}

impl NamedSpan {
    pub(crate) fn open<'a>(&self, html: &'a str) -> &'a str {
        &html[self.start..self.open_end]
    }
    pub(crate) fn inner<'a>(&self, html: &'a str) -> &'a str {
        &html[self.inner_start..self.inner_end]
    }
    /// True when `other` lies strictly inside this element.
    pub(crate) fn contains(&self, other: &NamedSpan) -> bool {
        self.start <= other.start
            && other.end <= self.end
            && (self.start, self.end) != (other.start, other.end)
    }
}

/// All elements in the document (BeautifulSoup-shaped view).
pub(crate) fn element_spans(html: &str) -> Vec<NamedSpan> {
    let toks = tag_tokens(html);
    let mut out: Vec<NamedSpan> = Vec::new();
    let mut stack: Vec<NamedSpan> = Vec::new();
    for tok in &toks {
        if tok.close {
            if let Some(pos) = stack.iter().rposition(|e| e.name == tok.name) {
                while stack.len() > pos + 1 {
                    let mut e = stack.pop().unwrap();
                    e.inner_end = tok.start;
                    e.end = tok.start;
                    out.push(e);
                }
                let mut e = stack.pop().unwrap();
                e.inner_end = tok.start;
                e.end = tok.end;
                out.push(e);
            }
            continue;
        }
        let e = NamedSpan {
            name: tok.name.clone(),
            start: tok.start,
            open_end: tok.end,
            inner_start: tok.end,
            inner_end: tok.end,
            end: tok.end,
        };
        if tok.self_closing {
            out.push(e);
        } else {
            stack.push(e);
        }
    }
    for mut e in stack {
        e.inner_end = html.len();
        e.end = html.len();
        out.push(e);
    }
    out.sort_by_key(|e| (e.start, std::cmp::Reverse(e.end)));
    out
}

/// Remove the given (start, end) spans; spans nested inside an
/// earlier removal are dropped with it.
pub(crate) fn remove_spans(html: &str, mut spans: Vec<(usize, usize)>) -> String {
    if spans.is_empty() {
        return html.to_string();
    }
    spans.sort_by_key(|(s, e)| (*s, std::cmp::Reverse(*e)));
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    for (s, e) in spans {
        if s < pos {
            continue;
        }
        out.push_str(&html[pos..s]);
        pos = e;
    }
    out.push_str(&html[pos..]);
    out
}

// ---------------------------------------------------------------------------
// CSS-only quiz removal (Python utils.strip_css_quizzes, v2.5.7).
// These widgets (How-To Geek's div.vq) answer through radio inputs
// and :checked rules, so every question, explanation and score
// panel is ordinary section/p text no extractor can tell from
// prose. Their stable signal is several radio groups in one
// container. Runs before any extraction in both readers.
// ---------------------------------------------------------------------------

const QUIZ_MIN_RADIO_GROUPS: usize = 3;
const QUIZ_PAGE_WRAPPERS: &[&str] = &["html", "body", "main", "article", "form"];

/// Remove embedded CSS-only quizzes from a page. Returns `html`
/// untouched (byte-identical) when no quiz can be present: fewer
/// than three distinct radio group names on the page, or every
/// candidate container is a page wrapper / holds the article.
pub fn strip_css_quizzes(html: &str) -> String {
    if html.is_empty() {
        return html.to_string();
    }
    static RADIO_TAG_RE: OnceLock<Regex> = OnceLock::new();
    let radio_tag_re = RADIO_TAG_RE
        .get_or_init(|| Regex::new(r#"(?i)<input\b[^>]*\btype\s*=\s*["']?radio\b[^>]*>"#).unwrap());
    static INPUT_NAME_RE: OnceLock<Regex> = OnceLock::new();
    let input_name_re =
        INPUT_NAME_RE.get_or_init(|| Regex::new(r#"(?i)\bname\s*=\s*["']?([^"'\s>]+)"#).unwrap());
    let mut page_names: HashSet<String> = HashSet::new();
    for tag in radio_tag_re.find_iter(html) {
        if let Some(cap) = input_name_re.captures(tag.as_str()) {
            page_names.insert(cap[1].to_string());
        }
    }
    if page_names.len() < QUIZ_MIN_RADIO_GROUPS {
        return html.to_string();
    }
    let spans = element_spans(html);
    // Group radio inputs by their direct parent element (the
    // innermost containing span); radios with no parent element
    // belong to the document root, a page wrapper by definition.
    let mut groups: HashMap<Option<usize>, HashSet<String>> = HashMap::new();
    for input in spans.iter().filter(|s| s.name == "input") {
        let open = input.open(html);
        let is_radio = attr_value(open, "type")
            .map(|t| t.eq_ignore_ascii_case("radio"))
            .unwrap_or(false);
        if !is_radio {
            continue;
        }
        let Some(name) = attr_value(open, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        let parent = spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains(input))
            .min_by_key(|(_, s)| s.end - s.start)
            .map(|(i, _)| i);
        groups.entry(parent).or_default().insert(name);
    }
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for (parent, names) in &groups {
        if names.len() < QUIZ_MIN_RADIO_GROUPS {
            continue;
        }
        let Some(parent) = parent.map(|i| &spans[i]) else {
            continue; // the document root is a page wrapper
        };
        if QUIZ_PAGE_WRAPPERS.contains(&parent.name.as_str()) {
            continue;
        }
        // A container holding the article body is not the widget.
        let holds_article = spans
            .iter()
            .any(|s| (s.name == "article" || s.name == "main") && parent.contains(s));
        if holds_article {
            continue;
        }
        removals.push((parent.start, parent.end));
    }
    if removals.is_empty() {
        return html.to_string();
    }
    remove_spans(html, removals)
}

const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Remove every element whose (tag name, open tag) satisfies
/// `pred`, subtrees included. Returns the new text and whether
/// anything was removed.
pub(crate) fn remove_elements(html: &str, pred: &dyn Fn(&str, &str) -> bool) -> (String, bool) {
    let toks = tag_tokens(html);
    let mut removals: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0usize;
    let mut i = 0usize;
    while i < toks.len() {
        let tok = &toks[i];
        if tok.close || tok.start < cursor {
            i += 1;
            continue;
        }
        if !pred(&tok.name, &html[tok.start..tok.end]) {
            i += 1;
            continue;
        }
        if tok.self_closing || VOID_TAGS.contains(&tok.name.as_str()) {
            removals.push((tok.start, tok.end));
            cursor = tok.end;
            i += 1;
            continue;
        }
        let mut depth = 1usize;
        let mut j = i + 1;
        let mut end = tok.end;
        while j < toks.len() {
            let t2 = &toks[j];
            if t2.name == tok.name {
                if t2.close {
                    depth -= 1;
                    if depth == 0 {
                        end = t2.end;
                        break;
                    }
                } else if !t2.self_closing {
                    depth += 1;
                }
            }
            j += 1;
        }
        removals.push((tok.start, end));
        cursor = end;
        i = j + 1;
    }
    if removals.is_empty() {
        return (html.to_string(), false);
    }
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    for (s, e) in &removals {
        out.push_str(&html[pos..*s]);
        pos = *e;
    }
    out.push_str(&html[pos..]);
    (out, true)
}

/// Remove elements matching a simple selector: `tag`, `.class`,
/// `#id`, `tag.class`, or `tag#id`.
fn remove_selector(html: &str, selector: &str) -> (String, bool) {
    let (tag, kind, value) = if let Some(rest) = selector.strip_prefix('#') {
        (None, "id", rest.to_string())
    } else if let Some(rest) = selector.strip_prefix('.') {
        (None, "class", rest.to_string())
    } else if let Some((t, v)) = selector.split_once('.') {
        (Some(t.to_lowercase()), "class", v.to_string())
    } else if let Some((t, v)) = selector.split_once('#') {
        (Some(t.to_lowercase()), "id", v.to_string())
    } else {
        (Some(selector.to_lowercase()), "", String::new())
    };
    remove_elements(html, &|name, open| {
        if let Some(t) = &tag
            && name != t
        {
            return false;
        }
        match kind {
            "id" => attr_value(open, "id").map(|v| v == value).unwrap_or(false),
            "class" => has_class(open, &value),
            _ => true,
        }
    })
}

/// Collapse an HTML fragment's text the way BeautifulSoup's
/// `get_text(" ", strip=True)` does: single spaces, trimmed.
pub(crate) fn leaf_text(fragment: &str) -> String {
    fragment_to_text(fragment)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drop every `<figcaption>` subtree (Brandon's reports,
/// 2026-11-08): figure captions and their "Credit:" /
/// "Image:" lines are photo furniture, not article prose —
/// Ars Technica, Windows Latest and others print them inside
/// the body containers, so they survived every text path.
/// The image itself stays (alt markers are a separate,
/// opt-in feature); only the caption element goes. Runs on
/// the raw page in `extract_article_opts` before any engine
/// reads it, and inside both publisher-body reconstructions,
/// so the text cascade and the rich reader agree.
pub fn drop_figcaptions(html: &str) -> String {
    let (out, _) = remove_elements(html, &|name, _open| name == "figcaption");
    out
}

/// Drop PhotoSwipe lightbox caption blocks: elements whose
/// class carries `pswp-caption-content` hold a hidden copy of
/// the figure caption (caption text plus its "Credit:" line)
/// for the lightbox UI. They are not `<figcaption>` elements,
/// so `drop_figcaptions` misses them — on Ars Technica pages
/// the copy inside the visible figure was removed while this
/// hidden twin survived and printed once (Brandon's report,
/// 2026-11-08). Class-token match, so hashed suffixes and
/// sibling classes are fine.
pub fn drop_pswp_captions(html: &str) -> String {
    let (out, _) = remove_elements(html, &|_name, open| has_class(open, "pswp-caption-content"));
    out
}

/// Reuters dedicated extractor (Python `_extract_reuters_text`):
/// the `data-testid="paragraph-N"` nodes inside
/// `data-testid="ArticleBody"`, >= 200 chars.
fn reuters_text(html: &str) -> String {
    let containers = find_elements(html, None, &|open| {
        attr_value(open, "data-testid")
            .map(|v| v == "ArticleBody")
            .unwrap_or(false)
    });
    let Some(c) = containers.into_iter().next() else {
        return String::new();
    };
    let inner = &html[c.inner_start..c.inner_end];
    static OPENS_NEW_TAB: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r",?\s*opens new tab\b").unwrap());
    let mut paras = Vec::new();
    for p in find_elements(inner, None, &|open| {
        attr_value(open, "data-testid")
            .map(|v| v.starts_with("paragraph-"))
            .unwrap_or(false)
    }) {
        let text = leaf_text(&inner[p.inner_start..p.inner_end]);
        let text = OPENS_NEW_TAB.replace_all(&text, "");
        let text = crate::postprocess::normalize_whitespace(&text);
        if !text.is_empty() {
            paras.push(text);
        }
    }
    let text = crate::postprocess::normalize_whitespace(&paras.join("\n\n"));
    if text.chars().count() < 200 {
        return String::new();
    }
    text
}

/// GSMArena dedicated extractor (Python `_extract_gsmarena_text`):
/// heading/paragraph/list blocks of #review-body (or
/// div.article-body) in document order, >= 300 chars.
fn gsmarena_text(html: &str) -> String {
    let container = find_elements(html, None, &|open| {
        attr_value(open, "id")
            .map(|v| v == "review-body")
            .unwrap_or(false)
    })
    .into_iter()
    .next()
    .or_else(|| {
        find_elements(html, Some("div"), &|open| has_class(open, "article-body"))
            .into_iter()
            .next()
    });
    let Some(c) = container else {
        return String::new();
    };
    let inner = &html[c.inner_start..c.inner_end];
    let mut found: Vec<(usize, String)> = Vec::new();
    for tag in ["h1", "h2", "h3", "h4", "p", "li"] {
        for sp in find_elements(inner, Some(tag), &|_| true) {
            let frag = &inner[sp.inner_start..sp.inner_end];
            let low = frag.to_lowercase();
            // Skip container blocks (a block wrapping other blocks).
            if low.contains("<p") || low.contains("<li") {
                continue;
            }
            let text = leaf_text(frag);
            if !text.is_empty() {
                found.push((sp.start, text));
            }
        }
    }
    found.sort_by_key(|(s, _)| *s);
    static SPACE_BEFORE_PUNCT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\s+([,.;:!?])").unwrap());
    let lines: Vec<String> = found
        .into_iter()
        .map(|(_, t)| SPACE_BEFORE_PUNCT.replace_all(&t, "$1").into_owned())
        .collect();
    let text = crate::postprocess::normalize_whitespace(&lines.join("\n\n"));
    if text.chars().count() < 300 {
        return String::new();
    }
    text
}

/// Top Tech Tidbits dedicated extractor (Python
/// `_extract_toptechtidbits_text`): isolate div.post-container,
/// drop its share/subscribe chrome, extract the fragment,
/// >= 2000 chars.
fn toptechtidbits_text(html: &str) -> String {
    let containers = find_elements(html, Some("div"), &|open| has_class(open, "post-container"));
    let Some(c) = containers.into_iter().next() else {
        return String::new();
    };
    let mut frag = html[c.inner_start..c.inner_end].to_string();
    for sel in [
        ".sd-content",
        ".sharedaddy",
        ".jp-relatedposts",
        ".jetpack-subscription-modal",
        ".post-meta",
        ".post-byline",
    ] {
        frag = remove_selector(&frag, sel).0;
    }
    let text = extract_focus(&frag, rust_trafilatura::ExtractionFocus::FavorPrecision).1;
    if text.chars().count() < 2000 {
        return String::new();
    }
    text
}

/// Python `_extract_without_dom_boilerplate`: remove the given
/// DOM sections, then run the normal base extraction on what
/// remains. Empty when nothing was removed.
fn extract_without_dom_boilerplate(html: &str, selectors: &[&str]) -> String {
    let mut cleaned = html.to_string();
    let mut removed_any = false;
    for sel in selectors {
        let (next, removed) = remove_selector(&cleaned, sel);
        if removed {
            removed_any = true;
            cleaned = next;
        }
    }
    if !removed_any {
        return String::new();
    }
    extract_focus(&cleaned, rust_trafilatura::ExtractionFocus::FavorPrecision).1
}

/// Publisher body reconstruction (Python
/// `core/article_html.py::_publisher_body_reconstruct`, as used by
/// `_extract_site_specific_text` for arstechnica.com and
/// zdnet.com): the verified body containers inside <article>,
/// minus junk widgets, as text.
fn publisher_body_text(html: &str, url: &str) -> String {
    let host = host_of(url);
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let (tag, class): (Option<&str>, &str) = match host {
        "arstechnica.com" => (Some("div"), "post-content"),
        "zdnet.com" => (None, "entry-content"),
        _ => return String::new(),
    };
    let articles = find_elements(html, Some("article"), &|_| true);
    let Some(article) = articles.into_iter().next() else {
        return String::new();
    };
    let inner = &html[article.inner_start..article.inner_end];
    let parts = find_elements(inner, tag, &|open| has_class(open, class));
    // Nested body wrappers belong to their outer section; keep
    // each only once (drop parts contained in another part).
    let keep: Vec<bool> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            !parts
                .iter()
                .enumerate()
                .any(|(j, q)| j != i && q.start <= p.start && p.end <= q.end)
        })
        .collect();
    let mut joined = String::new();
    for (p, k) in parts.iter().zip(keep.iter()) {
        if *k {
            joined.push_str(&inner[p.start..p.end]);
        }
    }
    if joined.is_empty() {
        return String::new();
    }
    const JUNK: &[&str] = &[
        "ars-interlude-container",
        "page-anchor-wrapper",
        "articlesByTag",
        "google-ad",
        "wp-block-ziff-davis-exco-video",
        "zd-disqus",
        "articleAuthorBioFooter-container",
        "zd-post-footer-pattern",
    ];
    let (cleaned, _) = remove_elements(&joined, &|name, open| {
        name == "figcaption" || JUNK.iter().any(|c| has_class(open, c))
    });
    crate::postprocess::normalize_whitespace(&html_to_text(&cleaned))
}

const SOCAST_JUNK_CLASS_TOKENS: &[&str] = &[
    "posts",
    "items-wrapper",
    "bnl-pp-happening",
    "bnl-info",
    "bnl-title",
    "bnl-content",
    "pp-more-wrapper",
    "wpb_raw_html",
    "wpb_raw_code",
    "sc-author",
    "entry-footer",
    "entry-meta",
    "report_an_error",
    "highlight-text",
    "newsletter",
    "scwidgetcontainer",
    "sc-item-detail",
];

/// Python `_is_socast_page`: socast / Pattison-portals radio-CMS
/// article pages (split-body layout).
pub(crate) fn is_socast_page(html: &str) -> bool {
    let low = html.to_lowercase();
    if low.contains("socastsrm.com") {
        return true;
    }
    low.contains("wpb-content-wrapper") && low.contains("mainarticle") && low.contains("sc-content")
}

/// Socast dedicated extractor (Python `_extract_socast_text`):
/// rebuild the story body from its split containers, skipping the
/// interleaved related-post/newsletter/ad widgets.
fn socast_text(html: &str) -> String {
    let mut containers = find_elements(html, Some("div"), &|open| {
        has_class(open, "wpb-content-wrapper")
    });
    containers.extend(find_elements(html, Some("article"), &|open| {
        has_class(open, "mainArticle")
    }));
    containers.sort_by_key(|c| c.start);
    let mut paras: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in &containers {
        let frag = &html[c.inner_start..c.inner_end];
        // Drop junk widget subtrees (Python checks each node's
        // ancestors for the junk class tokens; removing the
        // subtree at the junk wrapper is the same cut). Leaf
        // blocks are exempt: Python never checks the node itself.
        let (cleaned, _) = remove_elements(frag, &|name, open| {
            if name == "p" || name == "li" {
                return false;
            }
            let cls = attr_value(open, "class").unwrap_or_default().to_lowercase();
            !cls.is_empty() && SOCAST_JUNK_CLASS_TOKENS.iter().any(|t| cls.contains(t))
        });
        let mut found: Vec<(usize, String)> = Vec::new();
        for tag in ["p", "li"] {
            for sp in find_elements(&cleaned, Some(tag), &|_| true) {
                let frag_inner = &cleaned[sp.inner_start..sp.inner_end];
                let low = frag_inner.to_lowercase();
                if low.contains("<p") || low.contains("<li") {
                    continue; // container block; Python skips it too
                }
                let text = leaf_text(frag_inner);
                if !text.is_empty() {
                    found.push((sp.start, text));
                }
            }
        }
        found.sort_by_key(|(s, _)| *s);
        for (_, text) in found {
            if seen.insert(text.to_lowercase()) {
                paras.push(text);
            }
        }
    }
    crate::postprocess::normalize_whitespace(&paras.join("\n\n"))
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
pub(crate) fn strip_zero_width(s: &str) -> String {
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
/// Python `core/utils.py::_image_alt_marker`: `[Image: alt]`, or
/// `[Image]` when there is no alt text. Alt whitespace collapses
/// so a multi-line alt reads as one line.
pub fn image_alt_marker(alt: &str) -> String {
    let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
    if alt.is_empty() {
        "[Image]".to_string()
    } else {
        format!("[Image: {alt}]")
    }
}

/// Reader structure preferences mirror Python's `ARTICLE_STRUCTURE_DEFAULTS`.
#[derive(Debug, Clone, Copy)]
pub struct StructureOptions {
    pub tables: bool,
    pub headings: bool,
    pub lists: bool,
    pub quotes: bool,
    pub links: bool,
}

impl Default for StructureOptions {
    fn default() -> Self {
        Self {
            tables: true,
            headings: false,
            lists: false,
            quotes: false,
            links: false,
        }
    }
}

static TEXT_STRUCTURE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Apply once at startup and again after Settings changes.
pub fn configure_text_structure(cfg: &crate::config::Config) {
    let bits = ["tables", "headings", "lists", "quotes", "links"]
        .iter()
        .enumerate()
        .fold(0, |bits, (i, key)| {
            bits | (u8::from(cfg.get_bool(&format!("article_structure_{key}"), i == 0)) << i)
        });
    TEXT_STRUCTURE.store(bits, std::sync::atomic::Ordering::Relaxed);
}

fn structure_options() -> StructureOptions {
    let bits = TEXT_STRUCTURE.load(std::sync::atomic::Ordering::Relaxed);
    StructureOptions {
        tables: bits & 1 != 0,
        headings: bits & 2 != 0,
        lists: bits & 4 != 0,
        quotes: bits & 8 != 0,
        links: bits & 16 != 0,
    }
}

fn escape_structure_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn table_text(html: &str) -> String {
    if tag_tokens(html).iter().any(|t| {
        !t.close
            && matches!(
                t.name.as_str(),
                "p" | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "article"
                    | "section"
                    | "aside"
                    | "nav"
                    | "form"
                    | "ul"
                    | "ol"
                    | "blockquote"
                    | "figure"
            )
    }) {
        return String::new();
    }
    // A nested table is layout, as in Python's format_table_text.
    if tag_tokens(html)
        .iter()
        .filter(|t| !t.close && t.name == "table")
        .count()
        > 1
    {
        return String::new();
    }
    let mut header: Option<Vec<String>> = None;
    let mut grid = Vec::new();
    for row in find_elements(html, Some("tr"), &|_| true) {
        let row_html = &html[row.inner_start..row.inner_end];
        let mut cells = find_elements(row_html, Some("th"), &|_| true);
        let all_headers =
            !cells.is_empty() && find_elements(row_html, Some("td"), &|_| true).is_empty();
        cells.extend(find_elements(row_html, Some("td"), &|_| true));
        cells.sort_by_key(|cell| cell.start);
        let texts: Vec<String> = cells
            .iter()
            .map(|cell| {
                html_to_text_raw(&row_html[cell.inner_start..cell.inner_end], false)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        if texts.iter().all(|text| text.is_empty()) {
            continue;
        }
        if texts.iter().any(|text| text.chars().count() > 400) {
            return String::new();
        }
        let in_head = html[..row.start]
            .rfind("<thead")
            .is_some_and(|start| !html[start..row.start].contains("</thead"));
        if header.is_none() && grid.is_empty() && (all_headers || in_head) {
            header = Some(texts);
        } else {
            grid.push(texts);
            if grid.len() > 100 {
                return String::new();
            }
        }
    }
    let columns = grid
        .iter()
        .chain(header.iter())
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    if columns < 2 || grid.is_empty() {
        return header
            .into_iter()
            .flatten()
            .chain(grid.into_iter().flatten())
            .collect::<Vec<_>>()
            .join("\n");
    }
    let caption = find_elements(html, Some("caption"), &|_| true)
        .first()
        .map(|c| html_to_text_raw(&html[c.inner_start..c.inner_end], false))
        .unwrap_or_default();
    let mut lines = vec![format!(
        "Table with {} rows and {columns} columns{}.",
        grid.len(),
        if caption.is_empty() {
            String::new()
        } else {
            format!(": {caption}")
        }
    )];
    for (i, row) in grid.iter().enumerate() {
        let pairs: Vec<String> = row
            .iter()
            .enumerate()
            .filter(|(_, value)| !value.is_empty())
            .map(|(column, value)| {
                if let Some(header) = header.as_ref().filter(|h| h.len() == row.len()) {
                    format!("{}: {value}", header[column])
                } else {
                    value.clone()
                }
            })
            .collect();
        lines.push(format!("Row {}: {}.", i + 1, pairs.join("; ")));
    }
    lines.push("End of table.".into());
    lines.join("\n")
}

fn linearize_structure_html(html: &str, options: StructureOptions) -> String {
    let mut html = html.to_string();
    if options.tables {
        let mut tables = find_elements(&html, Some("table"), &|_| true);
        tables.reverse();
        for table in tables {
            let text = table_text(&html[table.start..table.end]);
            if !text.is_empty() {
                let paragraphs = text
                    .lines()
                    .map(|line| format!("<p>{}</p>", escape_structure_text(line)))
                    .collect::<String>();
                html.replace_range(table.start..table.end, &paragraphs);
            }
        }
    }
    if !(options.headings || options.lists || options.quotes || options.links) {
        return html;
    }
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut lists: Vec<(bool, usize)> = Vec::new();
    let mut anchors: Vec<(usize, String)> = Vec::new();
    for token in tag_tokens(&html) {
        let name = token.name.as_str();
        let open = &html[token.start..token.end];
        if options.headings && matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
            edits.push((
                token.start,
                token.end,
                if token.close {
                    "</p>".into()
                } else {
                    format!("<p>Heading level {}: ", &name[1..])
                },
            ));
        }
        if options.lists {
            if matches!(name, "ul" | "ol") {
                if token.close {
                    lists.pop();
                } else {
                    lists.push((name == "ol", 0));
                }
            } else if name == "li"
                && !token.close
                && let Some((ordered, index)) = lists.last_mut()
            {
                *index += 1;
                edits.push((
                    token.end,
                    token.end,
                    if *ordered {
                        format!("{index}. ")
                    } else {
                        "• ".into()
                    },
                ));
            }
        }
        if options.quotes && name == "blockquote" {
            edits.push((
                token.start,
                token.end,
                if token.close {
                    "<p>End of quote.</p></div>".into()
                } else {
                    "<div><p>Quote:</p>".into()
                },
            ));
        }
        if options.links && name == "a" {
            if token.close {
                if let Some((start, href)) = anchors.pop() {
                    let text = html_to_text_raw(&html[start..token.start], false);
                    if !href.is_empty() && !text.contains(&href) {
                        let label = if text.trim().is_empty() {
                            href
                        } else {
                            format!(" ({href})")
                        };
                        edits.push((token.start, token.start, escape_structure_text(&label)));
                    }
                }
            } else {
                let href = attr_value(open, "href")
                    .map(|h| unescape_entities(&h))
                    .unwrap_or_default();
                let allowed = (href.starts_with("http://") || href.starts_with("https://"))
                    && !href.chars().any(|c| c.is_whitespace() || c.is_control());
                anchors.push((token.end, if allowed { href } else { String::new() }));
            }
        }
    }
    edits.sort_by_key(|(start, end, _)| (*start, *end));
    for (start, end, replacement) in edits.into_iter().rev() {
        html.replace_range(start..end, &replacement);
    }
    html
}

/// Explicit preferences keep callers and tests independent of global settings.
pub fn html_to_text_with_structure(
    html: &str,
    keep_alt: bool,
    options: StructureOptions,
) -> String {
    html_to_text_raw(&linearize_structure_html(html, options), keep_alt)
}

pub fn html_to_text(html: &str) -> String {
    html_to_text_opts(html, false)
}

/// html_to_text with the image-alt option (Python
/// `utils.html_to_text(include_images=...)`): when `keep_alt` is
/// set, each <img> becomes its `[Image: alt]` marker in document
/// order (alt, falling back to title) instead of dropping.
pub fn html_to_text_opts(html: &str, keep_alt: bool) -> String {
    html_to_text_with_structure(html, keep_alt, structure_options())
}

fn html_to_text_raw(html: &str, keep_alt: bool) -> String {
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
                "img" if keep_alt => {
                    let tag_full = &html[i..=i + end];
                    let alt = attr_value(tag_full, "alt")
                        .filter(|a| !a.trim().is_empty())
                        .or_else(|| attr_value(tag_full, "title"))
                        .unwrap_or_default();
                    out.push_str(&image_alt_marker(&unescape_entities(&alt)));
                }
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
pub(crate) fn extract_focus(
    html: &str,
    focus: rust_trafilatura::ExtractionFocus,
) -> (String, String) {
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

/// Decode HTML entities in an attribute value (BeautifulSoup
/// decodes them while parsing; `attr_value` returns raw text).
fn decode_entities_full(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos + 1..];
        let Some(end) = tail.find(';') else {
            out.push('&');
            rest = tail;
            continue;
        };
        let entity = &tail[..end];
        match decode_entity(entity) {
            Some(decoded) => {
                out.push_str(&decoded);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Python `_extract_meta_content`: the first `<meta>` matching
/// every (attr, value) pair of the first candidate that yields a
/// non-empty stripped `content`.
fn first_meta_content(html: &str, candidates: &[(&str, &str)]) -> String {
    let spans = element_spans(html);
    let metas: Vec<&NamedSpan> = spans.iter().filter(|s| s.name == "meta").collect();
    for (attr, value) in candidates {
        for meta in &metas {
            let open = meta.open(html);
            if attr_value(open, attr).as_deref() == Some(value) {
                let content = attr_value(open, "content").unwrap_or_default();
                let content = decode_entities_full(content.trim());
                if !content.is_empty() {
                    return content;
                }
            }
        }
    }
    String::new()
}

/// Python `_extract_meta_description`.
fn meta_description(html: &str) -> String {
    first_meta_content(
        html,
        &[
            ("property", "og:description"),
            ("name", "description"),
            ("name", "twitter:description"),
        ],
    )
}

/// Python `_extract_page_title`: meta title, else the <title> text.
fn page_title(html: &str) -> String {
    let meta = first_meta_content(
        html,
        &[
            ("property", "og:title"),
            ("name", "twitter:title"),
            ("name", "title"),
        ],
    );
    if !meta.is_empty() {
        return meta;
    }
    element_spans(html)
        .iter()
        .find(|s| s.name == "title")
        .map(|s| fragment_text(s.inner(html)))
        .unwrap_or_default()
}

/// Python `_strip_title_suffix`: drop a site-name suffix/prefix
/// at the first separator that occurs, keeping the longest side.
fn strip_title_suffix(title: &str) -> String {
    let t = title.trim();
    for sep in [" | ", " \u{2014} ", " \u{2013} "] {
        if t.contains(sep) {
            let mut parts = t.rsplitn(2, sep);
            let tail = parts.next().unwrap_or("");
            let head = parts.next().unwrap_or("");
            return if tail.chars().count() >= head.chars().count() {
                tail.trim().to_string()
            } else {
                head.trim().to_string()
            };
        }
    }
    t.to_string()
}

/// Python `_strip_trailing_ellipsis`.
fn strip_trailing_ellipsis(text: &str) -> String {
    static ELLIPSIS_RE: OnceLock<Regex> = OnceLock::new();
    let re = ELLIPSIS_RE.get_or_init(|| Regex::new(r"(?:\.\.\.|…)\s*$").unwrap());
    re.replace(text.trim(), "").trim().to_string()
}

/// Python `_is_reasonable_lead_paragraph`: 40..=800 chars, and
/// either sentence punctuation or at least 120 chars.
fn is_reasonable_lead_paragraph(text: &str) -> bool {
    let t = text.trim();
    let len = t.chars().count();
    if !(40..=800).contains(&len) {
        return false;
    }
    if t.contains(['.', '!', '?']) {
        return true;
    }
    len >= 120
}

/// Hosts whose lead recovery is enabled (Python
/// `_LEAD_RECOVERY_ALLOWED_NETLOC_SUFFIXES`): sites with a
/// meaningful lead/intro the precision pass may skip.
const LEAD_RECOVERY_SUFFIXES: &[&str] = &["wirtualnemedia.pl"];

/// Python `_lead_recovery_enabled`.
fn lead_recovery_enabled(url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    let host = host_of(url);
    let host = host.split(':').next().unwrap_or("");
    LEAD_RECOVERY_SUFFIXES
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Python `_extract_allowlisted_lead_from_html`: the
/// wirtualnemedia.pl header lead node, when present.
fn allowlisted_lead_from_html(html: &str, url: &str) -> String {
    let host = host_of(url);
    let host = host.split(':').next().unwrap_or("");
    if host == "wirtualnemedia.pl" || host.ends_with(".wirtualnemedia.pl") {
        let spans = element_spans(html);
        if let Some(node) = spans
            .iter()
            .find(|s| s.name == "div" && has_class(s.open(html), "wm-article-header-lead"))
        {
            return fragment_text(node.inner(html));
        }
    }
    String::new()
}

/// Python `_recover_intro_paragraphs`: scan the recall opening
/// for reasonable paragraphs absent from the precision text, but
/// only trust them when one of them overlaps the meta
/// description's hit snippet — the guard that keeps unrelated
/// prose out.
fn recover_intro_paragraphs(
    recall_text: &str,
    precision_paras_norm: &HashSet<String>,
    page_title_norm: &str,
    desc_hit_snippet: &str,
) -> Vec<String> {
    let mut intro: Vec<String> = Vec::new();
    let mut desc_hit = false;
    for p in crate::postprocess::split_paragraphs(recall_text)
        .iter()
        .take(8)
    {
        let pn = para_key(p);
        if pn.is_empty() {
            continue;
        }
        if precision_paras_norm.contains(&pn) {
            break;
        }
        if !page_title_norm.is_empty() && pn == page_title_norm {
            continue;
        }
        if !is_reasonable_lead_paragraph(p) {
            continue;
        }
        if !desc_hit_snippet.is_empty() && pn.contains(desc_hit_snippet) {
            desc_hit = true;
        }
        intro.push(p.clone());
        if intro.len() >= 2 {
            break;
        }
    }
    if intro.is_empty() || !desc_hit {
        return Vec::new();
    }
    intro
}

/// Python `_attempt_lead_recovery`, with the recall text already
/// in hand (Python extracts it lazily through `do_extract` after
/// the description gates pass; the result is the same text).
/// Returns the recovered full text, or None to keep precision.
fn attempt_lead_recovery(
    html: &str,
    url: &str,
    precision_text: &str,
    recall_text: &str,
) -> Option<String> {
    if !lead_recovery_enabled(url) {
        return None;
    }
    let desc = strip_trailing_ellipsis(&meta_description(html));
    let desc_norm = para_key(&desc);
    if desc_norm.is_empty() || desc_norm.chars().count() < 60 {
        return None;
    }
    let desc_snippet: String = desc_norm.chars().take(120).collect();
    let desc_hit_snippet: String = desc_norm.chars().take(80).collect();
    let precision_norm = para_key(precision_text);
    if precision_norm.contains(&desc_snippet) {
        return None;
    }
    let fallback_prepend = |desc: &str| -> Option<String> {
        // Python `_fallback_prepend_meta_desc`: prepend the
        // cleaned meta description itself (allowlist-only,
        // intentionally conservative).
        if !is_reasonable_lead_paragraph(desc) {
            return None;
        }
        Some(format!("{desc}\n\n{precision_text}").trim().to_string())
    };
    let lead_html = allowlisted_lead_from_html(html, url);
    let lead_norm = para_key(&lead_html);
    if !lead_norm.is_empty()
        && lead_norm.contains(&desc_hit_snippet)
        && !precision_norm.contains(&lead_norm)
        && is_reasonable_lead_paragraph(&lead_html)
    {
        return Some(
            format!("{lead_html}\n\n{precision_text}")
                .trim()
                .to_string(),
        );
    }
    let rec = recall_text.trim();
    if rec.is_empty() {
        return fallback_prepend(&desc);
    }
    let rec_head: String = rec.chars().take(8000).collect();
    if !para_key(&rec_head).contains(&desc_snippet) {
        return fallback_prepend(&desc);
    }
    let title_norm = para_key(&strip_title_suffix(&page_title(html)));
    let precision_paras_norm: HashSet<String> =
        crate::postprocess::split_paragraphs(precision_text)
            .iter()
            .map(|p| para_key(p))
            .collect();
    let intro =
        recover_intro_paragraphs(rec, &precision_paras_norm, &title_norm, &desc_hit_snippet);
    if intro.is_empty() {
        return fallback_prepend(&desc);
    }
    let mut parts = intro;
    parts.push(precision_text.to_string());
    Some(parts.join("\n\n").trim().to_string())
}

/// Run the cascade over a fetched page.
///
/// `feed_text` is the article body the feed itself supplied, if any; the
/// prefer-feed-content rule can keep it when the scrape is worse.
/// GSMArena battery-test pages: the server sends newest phones
/// first and the page's own script sorts the "Your score" column
/// descending. Python reproduces that one DOM operation on the
/// source HTML; this is the string-level equivalent (the table
/// is flat — a nested table inside it bails out unchanged).
fn is_gsmarena_battery_test_url(url: &str) -> bool {
    if !host_matches(url, "gsmarena.com") {
        return false;
    }
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    path.ends_with("/battery-test-v2.php3")
}

/// Parse GSMArena's `25:35h` duration values to minutes.
fn gsmarena_duration_minutes(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let hours = &text[start..i];
            if i < bytes.len() && bytes[i] == b':' {
                let mstart = i + 1;
                let mut mend = mstart;
                while mend < bytes.len() && bytes[mend].is_ascii_digit() {
                    mend += 1;
                }
                if mend - mstart == 2 && bytes.get(mend) == Some(&b'h') && hours.len() <= 3 {
                    return Some(
                        hours.parse::<i64>().ok()? * 60 + text[mstart..mend].parse::<i64>().ok()?,
                    );
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Visible text of an HTML fragment (tags stripped, the common
/// entities decoded, whitespace collapsed).
fn fragment_text(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply the battery table's default browser order to the page
/// HTML (Python's _normalize_gsmarena_battery_table_html).
pub fn normalize_gsmarena_battery_table_html(html: &str, url: &str) -> String {
    if html.is_empty() || !is_gsmarena_battery_test_url(url) {
        return html.to_string();
    }
    let lower = html.to_lowercase();
    let Some(marker) = lower.find("battery-table") else {
        return html.to_string();
    };
    let Some(tbody_open_rel) = lower[marker..].find("<tbody") else {
        return html.to_string();
    };
    let tbody_tag_start = marker + tbody_open_rel;
    let Some(tbody_content_start) = lower[tbody_tag_start..]
        .find('>')
        .map(|i| tbody_tag_start + i + 1)
    else {
        return html.to_string();
    };
    let Some(tbody_end_rel) = lower[tbody_content_start..].find("</tbody>") else {
        return html.to_string();
    };
    let tbody_end = tbody_content_start + tbody_end_rel;
    let inner = &html[tbody_content_start..tbody_end];
    let inner_lower = inner.to_lowercase();
    if inner_lower.contains("<table") {
        return html.to_string();
    }
    // Split the top-level rows.
    let mut rows: Vec<String> = Vec::new();
    let mut scan = 0;
    while let Some(rel) = inner_lower[scan..].find("<tr") {
        let row_start = scan + rel;
        let Some(end_rel) = inner_lower[row_start..].find("</tr>") else {
            return html.to_string();
        };
        let row_end = row_start + end_rel + "</tr>".len();
        rows.push(inner[row_start..row_end].to_string());
        scan = row_end;
    }
    if rows.is_empty() {
        return html.to_string();
    }
    let score_of = |row: &str| -> Option<i64> {
        // Last cell's text (Python reads cells[-1]).
        let row_lower = row.to_lowercase();
        let mut last_cell_start = None;
        for pat in ["<td", "<th"] {
            let mut s = 0;
            while let Some(rel) = row_lower[s..].find(pat) {
                last_cell_start = Some(s + rel);
                s += rel + pat.len();
            }
        }
        let cell = last_cell_start.map(|i| &row[i..]).unwrap_or(row);
        gsmarena_duration_minutes(&fragment_text(cell))
    };
    let mut scored: Vec<(Option<i64>, String)> =
        rows.into_iter().map(|r| (score_of(&r), r)).collect();
    if scored.iter().all(|(s, _)| s.is_none()) {
        return html.to_string();
    }
    // (has score, score) descending, stable — Python's sort.
    scored.sort_by(|a, b| {
        let ka = (a.0.is_some(), a.0.unwrap_or(-1));
        let kb = (b.0.is_some(), b.0.unwrap_or(-1));
        kb.cmp(&ka)
    });
    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..tbody_content_start]);
    for (_, row) in &scored {
        out.push_str(row);
        out.push('\n');
    }
    out.push_str(&html[tbody_end..]);
    out
}

pub fn extract_article(html_in: &str, url: &str, feed_text: Option<&str>) -> ExtractOutcome {
    extract_article_opts(html_in, url, feed_text, false)
}

/// extract_article with the image-alt option: when `keep_alt` is
/// set, the feed-content conversion keeps `[Image: alt]` markers
/// (the resolved show_image_alt / per-feed setting).
pub fn extract_article_opts(
    html_in: &str,
    url: &str,
    feed_text: Option<&str>,
    keep_alt: bool,
) -> ExtractOutcome {
    // CSS-only quizzes are removed before any extraction pass
    // reads the page (Python `_extract_text_any` does the same —
    // their prose otherwise vouches for itself everywhere).
    let quiz_stripped = strip_css_quizzes(html_in);
    let html_in = &quiz_stripped;
    // Figure captions are furniture, not prose (see
    // drop_figcaptions); no engine may see them. PhotoSwipe's
    // hidden caption twins go with them.
    let figcaption_stripped = drop_pswp_captions(&drop_figcaptions(html_in));
    let html_in = &figcaption_stripped;
    // GSMArena's battery table arrives in the wrong order
    // (its sorter is a script we do not run); normalize first.
    let gsm_normalized;
    let html_in = if is_gsmarena_battery_test_url(url) {
        gsm_normalized = normalize_gsmarena_battery_table_html(html_in, url);
        &gsm_normalized
    } else {
        html_in
    };
    // Bloomberg video pages carry their text in the JSON-LD
    // description; the generic cascade sees a video shell.
    if let Some(text) = bloomberg_video_text(html_in, url) {
        return ExtractOutcome::Article {
            title: String::new(),
            text: crate::postprocess::postprocess_extracted_text(
                &text,
                url,
                &crate::postprocess::published_date_label(html_in),
            ),
            source: "bloomberg",
        };
    }
    let structured = linearize_structure_html(html_in, structure_options());
    match extract_article_impl(&structured, url, feed_text, keep_alt) {
        ExtractOutcome::Article {
            title,
            text,
            source,
        } => ExtractOutcome::Article {
            title,
            text: crate::postprocess::postprocess_extracted_text(
                &text,
                url,
                &crate::postprocess::published_date_label(html_in),
            ),
            source,
        },
        other => other,
    }
}

fn extract_article_impl(
    html_in: &str,
    url: &str,
    feed_text: Option<&str>,
    keep_alt: bool,
) -> ExtractOutcome {
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
    let feed_owned = feed_text.map(|t| html_to_text_opts(t, keep_alt));
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
        (
            title,
            attempt_lead_recovery(html, url, &prec.1, &rec.1).unwrap_or_else(|| prec.1.clone()),
        )
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
    let site_text = if host_matches(url, "arstechnica.com") || host_matches(url, "zdnet.com") {
        publisher_body_text(html, url)
    } else if host.contains("axios.com") {
        axios_text(html)
    } else if host.contains("theregister.com") || host.contains("theregister.co.uk") {
        register_text(html)
    } else if host_matches(url, "reuters.com") {
        reuters_text(html)
    } else if host_matches(url, "toptechtidbits.com") {
        toptechtidbits_text(html)
    } else if host_matches(url, "gsmarena.com") {
        gsmarena_text(html)
    } else if host_matches(url, "thepostmillennial.com") {
        extract_without_dom_boilerplate(html, &["section.contributions-container"])
    } else if host_matches(url, "rebelnews.com") {
        extract_without_dom_boilerplate(html, &["section.posts-profile"])
    } else if host_matches(url, "simonwillison.net") {
        extract_without_dom_boilerplate(
            html,
            &[
                "div.recent-articles",
                "#sponsored-banner",
                "#secondary",
                "#ft",
                "#smallhead",
            ],
        )
    } else if crate::forum_sources::is_forum_thread_host(url)
        || !crate::forum_sources::forum_thread_text_for(url, html).is_empty()
    {
        // Forum threads (Python `_is_forum_thread_host` branch):
        // linearize every post instead of letting the generic engines
        // keep a single sibling block.
        crate::forum_sources::forum_thread_text_for(url, html)
    } else if is_socast_page(html) {
        socast_text(html)
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
        // JSON-LD is never trusted alone (Python `_extract_text_any`
        // step 1): some CMSes omit paragraphs from articleBody —
        // Wired/Conde Nast drops the lede when it uses styled
        // lead-in markup — so re-attach a missing lead from the
        // trafilatura text or the DOM paragraphs before serving it.
        let article_paras = article_paragraph_text(html);
        let patched = patch_json_ld_missing_lead(&jsonld, &[&base.1, &article_paras]);
        (base.0.clone(), patched, "jsonld")
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
    if structure_options().tables && html_lower.contains("<table") {
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
// Order (Python's, Audit 8 Task 24):
//   direct (with stored site cookies)
//   -> clearance refresh + one refetch (only when a gate
//      was seen; Task 25's recovery functions, wired here)
//   -> Sky News via Google Translate (news.sky.com only)
//   -> TLS-impersonated refetch (skipped when the site's
//      pinned session already forced that handshake on
//      the direct fetch, unless stale clearance was just
//      dropped; the transport itself is the pending B16
//      fidelity recorded in PARITY.md — the stage keeps
//      its position and target selection and resolves to
//      no body until a compatible transport lands)
//   -> Jina reader as text (only when a gate was seen)
//   -> Smry (always)
//   -> Wayback Machine (always)
//   -> the warm browser (only when a gate was seen, the
//      site holds no stored clearance, and the per-host
//      failure cooldown has lapsed)
// Every fallback body is re-checked against the gate markers
// before it is trusted, exactly like Python.
// ---------------------------------------------------------------------------

/// The config path the app configured for the cookie
/// store (feeds.rs keeps its own copy for the same
/// purpose); the Task 25 recovery functions key off it.
static CONFIG_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

pub fn configure_cookies(config_path: &std::path::Path) {
    *CONFIG_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(config_path.to_owned());
}

fn configured_config_path() -> Option<std::path::PathBuf> {
    CONFIG_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn now_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

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
    // Python's proxy path order: markdown to text, then the
    // trailing cookie-block cut, then the edge nav-run trim.
    let text = markdown_to_text(&text);
    let text = crate::postprocess::strip_proxy_trailing_boilerplate(&text);
    Some(crate::postprocess::strip_edge_nav_runs(&text))
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

// ---------------------------------------------------------------------------
// Audit 8 Task 24 stages: clearance refresh, impersonation,
// and the warm browser (Python `_try_fallbacks` head/tail).
// ---------------------------------------------------------------------------

/// Python `_retry_with_refreshed_clearance`: re-read the
/// browsers' current clearance for this site (Task 25's
/// `cookies::refresh_clearance_from_browsers`, rate
/// limited inside) and, when it changed, refetch the URL
/// once with the fresh stored identity. Returns the body
/// only for a 200..400 response that is not a bot
/// interstitial.
fn clearance_recovery_body(url: &str) -> Option<String> {
    let config_path = configured_config_path()?;
    if !crate::cookies::refresh_clearance_from_browsers(&config_path, url) {
        return None;
    }
    let (cookie, ua) = crate::cookies::site_headers_for(&config_path, url);
    let mut request = crate::http::status_agent()
        .get(url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9");
    if !cookie.is_empty() {
        request = request.header("Cookie", cookie);
    }
    if let Some(ua) = &ua {
        request = request.header("User-Agent", ua.clone());
        for (key, value) in crate::user_agents::client_hints(ua) {
            request = request.header(key, value);
        }
    }
    let mut response = request.call().ok()?;
    let status = response.status().as_u16();
    if !(200..400).contains(&status) {
        return None;
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = response.body_mut().read_to_vec().ok()?;
    let body = decode_bytes(&bytes, &content_type, "");
    if body.trim().is_empty() || looks_gated(&body) {
        return None;
    }
    Some(body)
}

/// Whether the jar currently holds clearance for `url`
/// (Python `_has_stored_clearance`).
fn has_stored_clearance(url: &str) -> bool {
    let Some(config_path) = configured_config_path() else {
        return false;
    };
    let jar = crate::cookies::load_jar_beside(&config_path);
    crate::cookies::has_clearance_for(&jar, url, now_epoch_seconds())
}

/// Python `user_agents.impersonate_target_for_ua` (with
/// `_CURL_TARGET_BY_ENGINE`): the impersonation target
/// whose handshake matches a User-Agent, or "" when the
/// engine is unknown. Safari identities map to "" — the
/// table has no Safari entry.
fn impersonate_target_for_ua(ua: &str) -> &'static str {
    if ua.contains("Firefox/") || ua.contains("Gecko/") {
        "firefox"
    } else if ua.contains("Edg/")
        || ua.contains("Edge/")
        || ua.contains("Chrome/")
        || ua.contains("Chromium/")
    {
        "chrome"
    } else {
        ""
    }
}

/// Python `utils._site_cookie_impersonation`: the target
/// a site's pinned-UA session forces onto every request,
/// or "" when the site holds no UA-paired session. A
/// non-empty result means the direct fetch conceptually
/// already went out on that handshake, so the fallback
/// stage would only repeat it.
pub fn site_impersonation_target(url: &str) -> String {
    let Some(config_path) = configured_config_path() else {
        return String::new();
    };
    let jar = crate::cookies::load_jar_beside(&config_path);
    let ua = crate::cookies::user_agent_for(&config_path, &jar, url, now_epoch_seconds());
    impersonate_target_for_ua(&ua).to_string()
}

/// Python `_IMPERSONATE_TARGETS` plus the pinned-session
/// restriction: (default Chrome target, "safari184"), or
/// the single pinned target when a session pins one.
/// Targets are fingerprint profile keys, never
/// User-Agent strings; the UA on the wire still comes
/// from the user_agents policy.
fn impersonation_target_list(pinned: &str) -> Vec<&'static str> {
    if pinned.is_empty() {
        vec!["", "safari184"]
    } else if pinned == "firefox" {
        vec!["firefox"]
    } else {
        vec!["chrome"]
    }
}

/// One impersonated request (Audit 8 B16, landed): the
/// transport is `crate::impersonate` — wreq/BoringSSL on a
/// per-call blocking bridge, with the target key selecting
/// the browser fingerprint profile. Certificate validation
/// is never weakened. The timeout is the configured feed
/// timeout, the same value the shared ureq agent carries.
/// Python's per-attempt timeout is its extraction timeout;
/// the cascade gives this stage no separate budget in
/// either implementation.
fn impersonated_fetch(url: &str, target: &str, cookie_header: &str) -> Option<String> {
    crate::impersonate::fetch(
        url,
        target,
        cookie_header,
        std::time::Duration::from_secs(crate::http::timeout_secs()),
    )
}

/// Python `_download_via_impersonation`: refetch the
/// live page on a real-browser TLS/HTTP fingerprint,
/// trying each target in order and trusting only a
/// 200..400 non-interstitial body (the successful-body
/// discipline lives in `impersonated_fetch`'s contract).
pub fn fetch_via_impersonation(url: &str, cookie_header: &str) -> Option<String> {
    let pinned = site_impersonation_target(url);
    for target in impersonation_target_list(&pinned) {
        if let Some(body) = impersonated_fetch(url, target, cookie_header)
            && !body.trim().is_empty()
            && !looks_gated(&body)
        {
            return Some(body);
        }
    }
    None
}

/// Per-host browser-failure cooldown (Python
/// `_BROWSER_GATE_FAILURES`): one failed launch covers
/// the host for ten minutes.
static BROWSER_GATE_FAILURES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
const BROWSER_GATE_FAILURE_COOLDOWN_SECONDS: u64 = 600;
const BROWSER_GATE_FAILURE_CACHE_LIMIT: usize = 256;

fn browser_gate_cooling_down(host: &str) -> bool {
    BROWSER_GATE_FAILURES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(host)
        .is_some_and(|at| at.elapsed().as_secs() < BROWSER_GATE_FAILURE_COOLDOWN_SECONDS)
}

fn record_browser_gate_failure(host: &str) {
    let mut map = BROWSER_GATE_FAILURES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if map.len() >= BROWSER_GATE_FAILURE_CACHE_LIMIT {
        map.clear();
    }
    map.insert(host.to_string(), Instant::now());
}

/// Python `_download_via_browser`: last-resort render of
/// the page in the warm browser, at the extractor's
/// timeout floor of 90 s, with failure memory on (the
/// browser stage records its own per-URL cooldown; the
/// per-host cooldown here is the extractor's own gate).
/// Callers apply Python's remaining gates (gate seen, no
/// stored clearance) before calling.
fn fetch_via_browser(url: &str) -> Option<String> {
    let host = host_of(url).to_lowercase();
    if browser_gate_cooling_down(&host) {
        return None;
    }
    let body = crate::browser::fetch_page_with_options(url, 90.0, true, "", None)
        .ok()
        .map(|response| response.html)
        .filter(|html| !html.trim().is_empty() && !looks_gated(html));
    if body.is_none() {
        record_browser_gate_failure(&host);
    }
    body
}

// ---------------------------------------------------------------------------
// Batch 35b: multi-page following, the metered-preview notice, the
// NPR text edition, Sky Flourish table expansion, ID3 show notes,
// and the Google News proxy path — the remaining pieces of Python's
// extract_full_article main loop.
// ---------------------------------------------------------------------------

const MEDIA_EXTS: [&str; 20] = [
    ".mp3", ".m4a", ".aac", ".wav", ".flac", ".ogg", ".opus", ".mp4", ".mkv", ".webm", ".mov",
    ".m4v", ".avi", ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".pdf",
];

fn url_path_lower(url: &str) -> String {
    // Windows file paths reach here as article "URLs" for local
    // enclosures; their backslash separators must split like '/'
    // or the path reads as empty and media detection fails.
    let url = url.replace('\\', "/");
    let after = url.split("://").nth(1).unwrap_or(&url);
    let path = after
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .split_once('/')
        .map(|(_, p)| format!("/{p}"))
        .unwrap_or_default();
    path.to_lowercase()
}

/// Python `_looks_like_media_url`: the URL path ends in a media
/// extension (a direct enclosure, not a webpage).
pub fn looks_like_media_url(url: &str) -> bool {
    let path = url_path_lower(url);
    MEDIA_EXTS.iter().any(|ext| path.ends_with(ext))
}

/// Python `_looks_like_id3_media_url`: an MP3 whose leading ID3 tag
/// can carry readable show notes.
pub fn looks_like_id3_media_url(url: &str) -> bool {
    url_path_lower(url).ends_with(".mp3")
}

// --- ID3v2 (bounded prefix read, Python utils.embedded_id3_metadata) ---

fn fetch_prefix_bytes(url: &str, range: &str, max: usize) -> Option<Vec<u8>> {
    // Local files read directly; remote media via a Range request.
    if !url.contains("://") || url.starts_with("file://") {
        let path = url.strip_prefix("file://").unwrap_or(url);
        let data = std::fs::read(path).ok()?;
        return Some(data.into_iter().take(max).collect());
    }
    let mut resp = crate::http::agent()
        .get(url)
        .header("Range", range)
        .call()
        .ok()?;
    let body = resp.body_mut().read_to_vec().ok()?;
    Some(body.into_iter().take(max).collect())
}

fn id3_decode_text(enc: u8, bytes: &[u8]) -> String {
    match enc {
        3 => String::from_utf8_lossy(bytes).to_string(),
        1 | 2 => {
            // UTF-16: BOM decides endianness for enc 1; enc 2 is BE.
            let (little, skip) = if enc == 1 && bytes.len() >= 2 {
                if bytes[0] == 0xFF && bytes[1] == 0xFE {
                    (true, 2)
                } else if bytes[0] == 0xFE && bytes[1] == 0xFF {
                    (false, 2)
                } else {
                    (false, 0)
                }
            } else {
                (false, 0)
            };
            let units: Vec<u16> = bytes[skip..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| {
                    if little {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => bytes.iter().map(|b| *b as char).collect(),
    }
}

/// Split a text-frame payload on its encoding's terminator.
fn id3_split_descriptor(enc: u8, bytes: &[u8]) -> Option<(String, &[u8])> {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0 && bytes[i + 1] == 0 {
                return Some((id3_decode_text(enc, &bytes[..i]), &bytes[i + 2..]));
            }
            i += 2;
        }
        None
    } else {
        let pos = bytes.iter().position(|b| *b == 0)?;
        Some((id3_decode_text(enc, &bytes[..pos]), &bytes[pos + 1..]))
    }
}

/// ID3 unsynchronisation decoding (mutagen `unsynch.decode`): a
/// 0x00 stuffed after a 0xFF is removed.
fn deunsynchronise(data: &[u8]) -> Vec<u8> {
    if !data.contains(&0xFF) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        out.push(data[i]);
        if data[i] == 0xFF && i + 1 < data.len() && data[i + 1] == 0x00 {
            i += 1;
        }
        i += 1;
    }
    out
}

/// zlib-decompress a compressed ID3 frame payload (mutagen
/// decompresses FLAG_COMPRESS frames the same way). Output is
/// capped: real show notes never approach it, and the cap keeps a
/// hostile tag from inflating without bound.
fn id3_zlib_decompress(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .take(4_000_001)
        .read_to_end(&mut out)
        .ok()?;
    if out.len() > 4_000_000 {
        return None;
    }
    Some(out)
}

/// Mutagen's extended-header heuristic: the frame IDs a real tag
/// body can start with — any Txxx text frame or Wxxx URL frame by
/// spec, plus the named non-text frames.
fn is_known_frame_id(id: &[u8]) -> bool {
    if id.len() != 4 || !id.iter().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    if id[0] == b'T' || id[0] == b'W' {
        return true;
    }
    const NAMED: &[&[u8]] = &[
        b"AENC", b"APIC", b"ASPI", b"CHAP", b"COMM", b"COMR", b"CTOC", b"ENCR", b"EQU2", b"ETCO",
        b"GEOB", b"GRID", b"LINK", b"MCDI", b"MLLT", b"OWNE", b"PCNT", b"POPM", b"POSS", b"PRIV",
        b"RBUF", b"RVA2", b"RVRB", b"SEEK", b"SIGN", b"SYLT", b"SYTC", b"UFID", b"USER", b"USLT",
    ];
    NAMED.contains(&id)
}

/// Walk the frames of the ID3v2 tag at `url` (a local path or a
/// remote URL read by bounded prefix), returning each frame's
/// id and payload after mutagen's flag handling: extended
/// header skipped, tag/frame unsynchronisation decoded,
/// compressed frames inflated, encrypted frames dropped.
/// Shared by the show-notes reader (`id3_metadata`) and the
/// chapter reader (`id3_chapters`) so both see the same frames.
fn id3_frame_list(url: &str) -> (u8, Vec<(String, Vec<u8>)>) {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let Some(hdr) = fetch_prefix_bytes(url, "bytes=0-9", 10) else {
        return (0, out);
    };
    if hdr.len() < 10 || &hdr[0..3] != b"ID3" {
        return (0, out);
    }
    let version = hdr[3];
    let flags = hdr[5];
    let ss = &hdr[6..10];
    let tag_size = (((ss[0] & 0x7F) as usize) << 21)
        | (((ss[1] & 0x7F) as usize) << 14)
        | (((ss[2] & 0x7F) as usize) << 7)
        | ((ss[3] & 0x7F) as usize);
    let total = tag_size + 10 + if flags & 0x10 != 0 { 10 } else { 0 };
    if total <= 10 || total > 1_000_000 {
        return (version, out);
    }
    let Some(tag) = fetch_prefix_bytes(url, &format!("bytes=0-{}", total - 1), total) else {
        return (version, out);
    };
    if tag.len() < total || &tag[0..3] != b"ID3" {
        return (version, out);
    }
    // Frame region: the tag body after any extended header.
    // Mutagen skips the extended header; starting the scan at
    // offset 10 unconditionally (the previous behaviour) landed
    // inside its size bytes and lost every frame of the tag.
    let tag_end = (10 + tag_size).min(tag.len());
    let mut region_start = 10usize;
    if flags & 0x40 != 0 {
        if tag_end < 14 {
            return (version, out);
        }
        // Mutagen heuristic: some taggers set the extended-header
        // flag without writing one — if the bytes at the frame
        // offset already decode as a known frame ID, the frames
        // follow immediately and there is nothing to skip.
        if !is_known_frame_id(&tag[10..14]) {
            let skip = if version == 4 {
                // Synchsafe size of the whole extended header,
                // including its 4 size bytes.
                if tag[10..14].iter().any(|b| b & 0x80 != 0) {
                    return (version, out);
                }
                (((tag[10] & 0x7F) as usize) << 21)
                    | (((tag[11] & 0x7F) as usize) << 14)
                    | (((tag[12] & 0x7F) as usize) << 7)
                    | ((tag[13] & 0x7F) as usize)
            } else {
                // Big-endian size excluding the 4 size bytes.
                4 + u32::from_be_bytes([tag[10], tag[11], tag[12], tag[13]]) as usize
            };
            if skip == 0 || 10 + skip > tag_end {
                return (version, out);
            }
            region_start = 10 + skip;
        }
    }
    // ID3v2.3 tag unsynchronisation: the frame region is stored
    // stuffed (0xFF 0x00 for 0xFF) and frame sizes describe the
    // decoded bytes, so decode before scanning, as mutagen does.
    // v2.4 never does this at tag level; it flags frames instead.
    let decoded_region;
    let frames: &[u8] = if version == 3 && flags & 0x80 != 0 {
        decoded_region = deunsynchronise(&tag[region_start..tag_end]);
        &decoded_region
    } else {
        &tag[region_start..tag_end]
    };
    let mut pos = 0usize;
    while pos + 10 <= frames.len() {
        let id = &frames[pos..pos + 4];
        if id.iter().all(|b| *b == 0) {
            break; // padding
        }
        let size = if version == 4 {
            (((frames[pos + 4] & 0x7F) as usize) << 21)
                | (((frames[pos + 5] & 0x7F) as usize) << 14)
                | (((frames[pos + 6] & 0x7F) as usize) << 7)
                | ((frames[pos + 7] & 0x7F) as usize)
        } else {
            u32::from_be_bytes([
                frames[pos + 4],
                frames[pos + 5],
                frames[pos + 6],
                frames[pos + 7],
            ]) as usize
        };
        let start = pos + 10;
        let end = (start + size).min(frames.len());
        if !id.iter().all(|b| b.is_ascii_alphanumeric()) {
            // Mutagen skips an unparseable frame and keeps
            // scanning; a hard stop would lose every later frame.
            pos = end.max(start);
            continue;
        }
        if size == 0 {
            pos = start; // mutagen drops empty frames
            continue;
        }
        if start >= end {
            break;
        }
        let flag2 = frames[pos + 9];
        // Per-frame flag handling, in mutagen's _fromData order:
        // data-length prefix, unsynchronisation, encryption
        // (unsupported — the frame is skipped), compression.
        let mut payload: &[u8] = &frames[start..end];
        let mut skip_frame = false;
        let mut unsynced: Option<Vec<u8>> = None;
        let mut inflated: Option<Vec<u8>> = None;
        if version == 4 {
            if flag2 & 0x09 != 0 && payload.len() >= 4 {
                // DATALEN (0x01) and COMPRESS (0x08) both prefix a
                // 4-byte data-length indicator.
                payload = &payload[4..];
            }
            if flag2 & 0x02 != 0 || flags & 0x80 != 0 {
                unsynced = Some(deunsynchronise(payload));
            }
            if let Some(u) = &unsynced {
                payload = u;
            }
            if flag2 & 0x04 != 0 {
                skip_frame = true; // encrypted
            } else if flag2 & 0x08 != 0 {
                match id3_zlib_decompress(payload) {
                    Some(data) => inflated = Some(data),
                    None => skip_frame = true,
                }
                if let Some(d) = &inflated {
                    payload = d;
                }
            }
        } else if version == 3 {
            if flag2 & 0x40 != 0 {
                skip_frame = true; // encrypted
            } else if flag2 & 0x80 != 0 {
                // COMPRESS: a 4-byte decompressed-size prefix,
                // then the zlib stream.
                if payload.len() < 4 {
                    skip_frame = true;
                } else {
                    match id3_zlib_decompress(&payload[4..]) {
                        Some(data) => inflated = Some(data),
                        None => skip_frame = true,
                    }
                    if let Some(d) = &inflated {
                        payload = d;
                    }
                }
            }
        }
        if skip_frame {
            pos = end;
            continue;
        }
        out.push((
            std::str::from_utf8(id).unwrap_or("").to_string(),
            payload.to_vec(),
        ));
        pos = end;
    }
    (version, out)
}

/// (title, author, content) from an MP3's embedded ID3 tag, or None
/// when the tag carries no show-note text. Content candidates are
/// USLT and COMM frames plus TXXX frames described as description /
/// summary / show notes / transcript; the longest wins (Python).
pub fn id3_metadata(url: &str) -> Option<(String, String, String)> {
    let mut title = String::new();
    let mut author = String::new();
    let mut author2 = String::new();
    let mut candidates: Vec<String> = Vec::new();
    for (id_str, payload) in id3_frame_list(url).1 {
        let payload: &[u8] = &payload;
        match id_str.as_str() {
            "TIT2" | "TPE1" | "TPE2" => {
                if !payload.is_empty() {
                    let text = id3_decode_text(payload[0], &payload[1..])
                        .replace('\0', " ")
                        .trim()
                        .to_string();
                    match id_str.as_str() {
                        "TIT2" if title.is_empty() => title = text,
                        "TPE1" if author.is_empty() => author = text,
                        "TPE2" if author2.is_empty() => author2 = text,
                        _ => {}
                    }
                }
            }
            "COMM" | "USLT" => {
                if payload.len() > 4 {
                    let enc = payload[0];
                    // Skip the 3-byte language, then the descriptor.
                    if let Some((_desc, rest)) = id3_split_descriptor(enc, &payload[4..]) {
                        let text = id3_decode_text(enc, rest).trim().to_string();
                        if !text.is_empty() && !candidates.contains(&text) {
                            candidates.push(text);
                        }
                    }
                }
            }
            "TXXX" if !payload.is_empty() => {
                let enc = payload[0];
                if let Some((desc, rest)) = id3_split_descriptor(enc, &payload[1..])
                    && [
                        "description",
                        "summary",
                        "show notes",
                        "shownotes",
                        "transcript",
                    ]
                    .contains(&desc.trim().to_lowercase().as_str())
                {
                    let text = id3_decode_text(enc, rest).trim().to_string();
                    if !text.is_empty() && !candidates.contains(&text) {
                        candidates.push(text);
                    }
                }
            }
            _ => {}
        }
    }
    let content = candidates.into_iter().max_by_key(|c| c.len())?;
    if content.trim().is_empty() {
        return None;
    }
    if author.is_empty() {
        author = author2;
    }
    Some((title, author, content))
}

/// Walk one CHAP frame's embedded sub-frames (same frame
/// header shape as the tag itself; sizes are synchsafe in
/// v2.4, big-endian in v2.3). Sub-frames are read raw: the
/// per-frame compression/encryption flags mutagen would apply
/// are not interpreted at this level — plain sub-frames, the
/// universal case in podcast tags, parse identically.
fn id3_sub_frames(version: u8, mut bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    while bytes.len() >= 10 {
        let id = &bytes[0..4];
        if id.iter().all(|b| *b == 0) || !id.iter().all(|b| b.is_ascii_alphanumeric()) {
            break;
        }
        let size = if version == 4 {
            if bytes[4..8].iter().any(|b| b & 0x80 != 0) {
                break;
            }
            (((bytes[4] & 0x7F) as usize) << 21)
                | (((bytes[5] & 0x7F) as usize) << 14)
                | (((bytes[6] & 0x7F) as usize) << 7)
                | ((bytes[7] & 0x7F) as usize)
        } else {
            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize
        };
        let end = (10 + size).min(bytes.len());
        out.push((
            std::str::from_utf8(id).unwrap_or("").to_string(),
            bytes[10..end].to_vec(),
        ));
        bytes = &bytes[end..];
    }
    out
}

/// The first text string of a text frame's payload (mutagen's
/// `text[0]`), or a URL frame's raw text (URL frames carry no
/// encoding byte; their text is Latin-1).
fn id3_first_text(payload: &[u8], is_url_frame: bool) -> String {
    if payload.is_empty() {
        return String::new();
    }
    if is_url_frame {
        let raw: String = payload.iter().map(|b| *b as char).collect();
        return raw.split('\0').next().unwrap_or("").to_string();
    }
    let text = id3_decode_text(payload[0], &payload[1..]);
    text.split('\0').next().unwrap_or("").to_string()
}

/// Chapters embedded in an MP3's ID3 tag (Python
/// `_chapters_from_id3`, Audit 8 M12): each CHAP frame's start
/// time, the first text of its first TIT2 sub-frame as the
/// title, and the first URL among its WXXX, WOAR, WCOM and WPUB
/// sub-frames consulted in that priority order — normalized
/// with Python's sort/dedupe-merge.
pub fn id3_chapters(url: &str) -> Vec<crate::Chapter> {
    let (version, frames) = id3_frame_list(url);
    let mut triples: Vec<(f64, String, Option<String>)> = Vec::new();
    for (id, payload) in frames {
        if id != "CHAP" {
            continue;
        }
        // Element id (NUL-terminated), then start time, end
        // time, start offset and end offset — 4-byte big-endian
        // each — then the sub-frames.
        let Some(nul) = payload.iter().position(|b| *b == 0) else {
            continue;
        };
        let rest = &payload[nul + 1..];
        if rest.len() < 16 {
            continue;
        }
        let start_ms = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let subs = id3_sub_frames(version, &rest[16..]);
        let title = subs
            .iter()
            .find(|(sid, _)| sid == "TIT2")
            .map(|(_, p)| id3_first_text(p, false))
            .unwrap_or_default();
        let mut href: Option<String> = None;
        for frame_name in ["WXXX", "WOAR", "WCOM", "WPUB"] {
            let Some((_, p)) = subs.iter().find(|(sid, _)| sid == frame_name) else {
                continue;
            };
            let candidate = if frame_name == "WXXX" {
                // Encoding byte, description, then the URL.
                if p.is_empty() {
                    String::new()
                } else {
                    match id3_split_descriptor(p[0], &p[1..]) {
                        Some((_desc, rest)) => {
                            let text = id3_decode_text(p[0], rest);
                            text.split('\0').next().unwrap_or("").to_string()
                        }
                        None => String::new(),
                    }
                }
            } else {
                id3_first_text(p, true)
            };
            if !candidate.is_empty() {
                href = Some(candidate);
                break;
            }
        }
        triples.push((start_ms as f64 / 1000.0, title, href));
    }
    crate::chapters::normalize_triples(triples)
}

// --- Metered preview (Python _looks_like_metered_preview) ---

pub fn looks_like_metered_preview(html: &str) -> bool {
    if html.is_empty() {
        return false;
    }
    let head: String = html.chars().take(400_000).collect();
    let lower = head.to_lowercase();
    METERED_MARKERS.iter().any(|marker| lower.contains(marker))
}

pub fn metered_preview_notice() -> &'static str {
    METERED_NOTICE
}

// --- Pagination (Python _find_next_page + _merge_texts) ---

const NO_PAGINATION_FOLLOW_HOSTS: [&str; 6] = [
    "wired.com",
    "ning.com",
    "neowin.net",
    "bloomberg.com",
    "slashdot.org",
    "github.com",
];

/// Find an article continuation, excluding next-story navigation and
/// cross-origin links. Cookies must never travel to an unrelated host.
pub fn find_next_page(html: &str, base_url: &str) -> Option<String> {
    let base = url::Url::parse(base_url).ok()?;
    let host = base.host_str()?;
    if NO_PAGINATION_FOLLOW_HOSTS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        return None;
    }
    let next_story =
        regex::Regex::new(r"(?i)next[\s_-]*(?:stor(?:y|ies)|articles?|posts?|read|up)\b").unwrap();
    let label = regex::Regex::new(
        r"(?i)^(?:(?:next|older)(?:\s+(?:page|pages|entries))?\s*[›»>]*|[›»>]+)$",
    )
    .unwrap();
    let tokens = tag_tokens(html);
    // Explicit rel=next takes precedence over heuristic navigation labels.
    for pass in 0..3 {
        for token in &tokens {
            if token.close || (token.name != "link" && token.name != "a") {
                continue;
            }
            let open = &html[token.start..token.end];
            let explicit = attr_value(open, "rel")
                .is_some_and(|v| v.split_whitespace().any(|r| r.eq_ignore_ascii_case("next")));
            if match pass {
                0 => token.name != "link" || !explicit,
                1 => token.name != "a" || !explicit,
                _ => token.name != "a" || explicit,
            } {
                continue;
            }
            let text = if token.name == "a" {
                let rest = &html[token.end..];
                let end = rest.to_ascii_lowercase().find("</a").unwrap_or(0);
                html_to_text(&rest[..end]).trim().to_lowercase()
            } else {
                String::new()
            };
            let class = attr_value(open, "class").unwrap_or_default().to_lowercase();
            let aria = attr_value(open, "aria-label")
                .unwrap_or_default()
                .to_lowercase();
            if next_story.is_match(&format!("{text} {class} {aria} {open}")) {
                continue;
            }
            if !explicit
                && !(label.is_match(&text) || class.contains("next") || aria.starts_with("next"))
            {
                continue;
            }
            let Some(href) = attr_value(open, "href") else {
                continue;
            };
            let href = unescape_entities(&href);
            if href.trim().is_empty() {
                continue;
            }
            let mut target = match base.join(href.trim()) {
                Ok(target) => target,
                Err(_) => continue,
            };
            target.set_fragment(None);
            let mut current = base.clone();
            current.set_fragment(None);
            if matches!(target.scheme(), "http" | "https")
                && target.origin() == base.origin()
                && target != current
            {
                return Some(target.to_string());
            }
        }
    }
    None
}

/// Merge page texts, de-duplicating repeated paragraphs (Python
/// `_merge_texts` for HTML-extracted text: short lines always kept
/// and stay out of the dedupe set; linearized table blocks are kept
/// or dropped as a unit; table/structure marker lines bypass both
/// the length floor and dedupe because the fixed markers repeat).
pub fn merge_texts(texts: &[String]) -> String {
    static TABLE_MARKER_RE: OnceLock<Regex> = OnceLock::new();
    let table_re = TABLE_MARKER_RE.get_or_init(|| {
        Regex::new(r"^(?:Table with \d+ rows? and \d+ columns?[:.]|Row \d+: .*\.|End of table\.)")
            .unwrap()
    });
    static STRUCTURE_MARKER_RE: OnceLock<Regex> = OnceLock::new();
    let structure_re = STRUCTURE_MARKER_RE.get_or_init(|| {
        Regex::new(r"^(?:Heading level [1-6]: .+|Quote:$|End of quote\.$|• .+|\d{1,3}\. .+)")
            .unwrap()
    });
    let key_of = |s: &str| -> String {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out_texts: Vec<String> = Vec::new();
    for t in texts {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        let paras: Vec<&str> = t
            .lines()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        let mut merged: Vec<String> = Vec::new();
        let mut i = 0;
        while i < paras.len() {
            let p = paras[i];
            if p.starts_with("Table with") && table_re.is_match(p) {
                // A linearized table block: marker lines only, kept
                // or dropped as a unit on the whole-block key.
                let mut block = vec![p];
                let mut j = i + 1;
                while j < paras.len() && table_re.is_match(paras[j]) {
                    block.push(paras[j]);
                    let last = paras[j];
                    j += 1;
                    if last == "End of table." {
                        break;
                    }
                }
                if seen.insert(key_of(&block.join(" "))) {
                    merged.extend(block.iter().map(|s| s.to_string()));
                }
                i = j;
                continue;
            }
            if table_re.is_match(p) || structure_re.is_match(p) {
                merged.push(p.to_string());
                i += 1;
                continue;
            }
            let key = key_of(p);
            // Short lines are always kept and stay out of the
            // dedupe set (the `||` short-circuit preserves that).
            if key.chars().count() < 25 || seen.insert(key) {
                merged.push(p.to_string());
            }
            i += 1;
        }
        if !merged.is_empty() {
            out_texts.push(merged.join("\n"));
        }
    }
    normalize_merged_whitespace(&out_texts.join("\n\n"))
}

/// Python `_normalize_whitespace` (the merge output pass): strip
/// zero-width marks, unify newlines, drop trailing spaces per line,
/// collapse 3+ newlines to a blank line, trim.
fn normalize_merged_whitespace(text: &str) -> String {
    let text = text
        .replace(['\u{200b}', '\u{2060}', '\u{feff}'], "")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut out = String::with_capacity(text.len());
    for line in text.split('\n') {
        out.push_str(line.trim_end_matches([' ', '\t']));
        out.push('\n');
    }
    // Collapse runs of 3+ newlines (2+ blank lines) to one blank line.
    let mut collapsed = String::with_capacity(out.len());
    let mut newline_run = 0usize;
    for ch in out.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                collapsed.push(ch);
            }
        } else {
            newline_run = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

// --- Sky Flourish tables (Python _expand_sky_flourish_tables) ---

const FLOURISH_DATE_MS_KEY: &str = "__blindrss_flourish_date_ms__";
const FLOURISH_ROWS_PER_TABLE: usize = 100;

/// Python `_extract_balanced_js_value`: the JSON-like object or
/// array assigned to a JavaScript variable. A non-greedy regex
/// cannot stop at the right nested closing brace, so this scanner
/// tracks brackets while ignoring them inside quoted strings.
fn extract_balanced_js_value(source: &str, variable: &str) -> String {
    let bytes = source.as_bytes();
    let mut search_from = 0usize;
    let start = loop {
        let Some(rel) = source[search_from..].find(variable) else {
            return String::new();
        };
        let name_start = search_from + rel;
        let before_ok = name_start == 0 || {
            let prev = bytes[name_start - 1];
            !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$')
        };
        let mut i = name_start + variable.len();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if before_ok && i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'{' || bytes[i] == b'[') {
                break i;
            }
        }
        search_from = name_start + variable.len();
    };
    let mut stack: Vec<u8> = Vec::new();
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            b'"' | b'\'' => quote = Some(ch),
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                if stack.pop() != Some(ch) {
                    return String::new();
                }
                if stack.is_empty() {
                    return source[start..=i].to_string();
                }
            }
            _ => {}
        }
        i += 1;
    }
    String::new()
}

/// Python `_flourish_json_value`: parse one embedded Flourish
/// data variable without executing JavaScript. Published datasets
/// are JSON apart from typed `new Date(...)` cells; only tokens
/// outside JSON strings are replaced, so authored cell text
/// containing those words stays untouched.
fn flourish_json_value(source: &str, variable: &str) -> Option<serde_json::Value> {
    let raw = extract_balanced_js_value(source, variable);
    if raw.is_empty() {
        return None;
    }
    static DATE_RE: OnceLock<Regex> = OnceLock::new();
    let date_re = DATE_RE.get_or_init(|| Regex::new(r"^new\s+Date\(\s*(-?\d+)\s*\)").unwrap());
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    while i < bytes.len() {
        // Delimiters are ASCII; UTF-8 continuation bytes never
        // equal them, so whole characters pass through untouched.
        let first = bytes[i];
        let ch_len = raw[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        if quoted {
            out.push_str(&raw[i..i + ch_len]);
            if escaped {
                escaped = false;
            } else if first == b'\\' {
                escaped = true;
            } else if first == b'"' {
                quoted = false;
            }
            i += ch_len;
            continue;
        }
        if first == b'"' {
            quoted = true;
            out.push_str(&raw[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        if first == b'n'
            && let Some(cap) = date_re.captures(&raw[i..])
        {
            out.push_str(&format!("{{\"{FLOURISH_DATE_MS_KEY}\":{}}}", &cap[1]));
            i += cap.get(0).unwrap().end();
            continue;
        }
        out.push_str(&raw[i..i + ch_len]);
        i += ch_len;
    }
    serde_json::from_str(&out).ok()
}

/// Days-from-epoch to (year, month, day) — Howard Hinnant's
/// civil-from-days algorithm, valid for pre-1970 dates too.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Python `_format_flourish_datetime`: format a Flourish typed
/// date using its published d3-style format. Epoch arithmetic is
/// used (not the platform clock) so pre-1970 time-only values
/// work on every OS.
fn format_flourish_datetime(milliseconds: i64, metadata: &serde_json::Value) -> String {
    let days = milliseconds.div_euclid(86_400_000);
    let rem = milliseconds.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600_000;
    let minute = (rem % 3_600_000) / 60_000;
    let second = (rem % 60_000) / 1000;
    let millis = rem % 1000;
    let format_id = metadata
        .get("output_format_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let date_format = format_id.split_once('$').map(|(_, f)| f).unwrap_or("");
    if date_format.is_empty() {
        let base = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
        return if millis != 0 {
            format!("{base}.{:06}Z", millis * 1000)
        } else {
            format!("{base}Z")
        };
    }
    let weekday = ((days % 7) + 7 + 4) % 7; // 1970-01-01 was a Thursday; 0 = Sunday
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const WEEKDAYS_FULL: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const MONTHS_FULL: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    let token_value = |token: &str| -> Option<String> {
        Some(match token {
            "Y" => format!("{year:04}"),
            "y" => format!("{:02}", year % 100),
            "m" => format!("{month:02}"),
            "d" => format!("{day:02}"),
            "e" => format!("{day:2}"),
            "H" => format!("{hour:02}"),
            "I" => format!("{hour12:02}"),
            "M" => format!("{minute:02}"),
            "S" => format!("{second:02}"),
            "L" => format!("{millis:03}"),
            "p" => if hour < 12 { "AM" } else { "PM" }.to_string(),
            "a" => WEEKDAYS[weekday as usize].to_string(),
            "A" => WEEKDAYS_FULL[weekday as usize].to_string(),
            "b" => MONTHS[(month - 1) as usize].to_string(),
            "B" => MONTHS_FULL[(month - 1) as usize].to_string(),
            "%" => "%".to_string(),
            _ => return None,
        })
    };
    static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    let token_re = TOKEN_RE.get_or_init(|| Regex::new(r"%(-?)([A-Za-z%])").unwrap());
    token_re
        .replace_all(date_format, |caps: &regex::Captures| {
            let token = &caps[2];
            match token_value(token) {
                Some(rendered) => {
                    if !caps[1].is_empty() && matches!(token, "m" | "d" | "H" | "I") {
                        let stripped = rendered.trim_start_matches('0');
                        if stripped.is_empty() {
                            "0".to_string()
                        } else {
                            stripped.to_string()
                        }
                    } else {
                        rendered
                    }
                }
                None => caps[0].to_string(),
            }
        })
        .to_string()
}

/// Python `_format_flourish_cell`.
fn format_flourish_cell(value: &serde_json::Value, metadata: &serde_json::Value) -> String {
    if let Some(obj) = value.as_object()
        && obj.len() == 1
        && let Some(ms) = obj.get(FLOURISH_DATE_MS_KEY)
    {
        let ms = ms
            .as_i64()
            .or_else(|| ms.as_f64().map(|f| f as i64))
            .unwrap_or(0);
        return format_flourish_datetime(ms, metadata);
    }
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string(value).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

/// Escape text-node content the way BeautifulSoup serializes it.
fn esc_html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Python `_flourish_table_nodes`, serialized: build the semantic
/// HTML tables of a published Flourish table embed from its
/// `_Flourish_data`, `_Flourish_data_column_names` and
/// `_Flourish_data_metadata` variables. Datasets with fewer than
/// two columns or no rows are skipped; tables split every 100
/// rows with a part caption. Returns "" when nothing decodes.
pub fn flourish_tables_html(embed_html: &str) -> String {
    let Some(data) = flourish_json_value(embed_html, "_Flourish_data") else {
        return String::new();
    };
    let Some(column_names) = flourish_json_value(embed_html, "_Flourish_data_column_names") else {
        return String::new();
    };
    let metadata = flourish_json_value(embed_html, "_Flourish_data_metadata")
        .unwrap_or(serde_json::Value::Null);
    let (Some(data), Some(column_names)) = (data.as_object(), column_names.as_object()) else {
        return String::new();
    };
    let mut out = String::new();
    for (dataset_name, raw_rows) in data {
        let Some(raw_rows) = raw_rows.as_array().filter(|r| !r.is_empty()) else {
            continue;
        };
        let Some(names) = column_names.get(dataset_name).and_then(|n| n.as_object()) else {
            continue;
        };
        let (headers, row_values): (Vec<serde_json::Value>, Vec<Vec<serde_json::Value>>) =
            if let Some(cols) = names.get("columns").and_then(|c| c.as_array()) {
                let mut rows_out = Vec::new();
                for row in raw_rows {
                    if let Some(cols) = row.get("columns").and_then(|c| c.as_array()) {
                        rows_out.push(cols.clone());
                    }
                }
                (cols.clone(), rows_out)
            } else {
                // Some templates expose rows as ordinary objects and
                // map binding keys to human-readable column names.
                let keys: Vec<&String> = names
                    .keys()
                    .filter(|k| names.get(*k).map(|v| v.is_string()).unwrap_or(false))
                    .collect();
                let headers_out = keys
                    .iter()
                    .map(|k| names.get(*k).cloned().unwrap_or(serde_json::Value::Null))
                    .collect();
                let mut rows_out = Vec::new();
                for row in raw_rows {
                    if row.is_object() {
                        rows_out.push(
                            keys.iter()
                                .map(|k| row.get(*k).cloned().unwrap_or(serde_json::Value::Null))
                                .collect(),
                        );
                    }
                }
                (headers_out, rows_out)
            };
        if headers.len() < 2 || row_values.is_empty() {
            continue;
        }
        let max_columns = row_values
            .iter()
            .map(|r| r.len())
            .max()
            .unwrap_or(0)
            .max(headers.len());
        let mut clean_headers: Vec<String> = headers
            .iter()
            .map(|h| match h {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string().trim_matches('"').trim().to_string(),
            })
            .collect();
        for index in clean_headers.len() + 1..=max_columns {
            clean_headers.push(format!("Column {index}"));
        }
        let empty_obj = serde_json::Value::Null;
        let dataset_metadata = metadata.get(dataset_name).unwrap_or(&empty_obj);
        let column_metadata = dataset_metadata
            .get("columns")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        let part_count = row_values.len().div_ceil(FLOURISH_ROWS_PER_TABLE);
        for part_index in 0..part_count {
            out.push_str("<table>");
            if part_count > 1 {
                out.push_str(&format!(
                    "<caption>Table, part {} of {}</caption>",
                    part_index + 1,
                    part_count
                ));
            }
            out.push_str("<thead><tr>");
            for heading in &clean_headers {
                out.push_str(&format!(
                    "<th scope=\"col\">{}</th>",
                    esc_html_text(heading)
                ));
            }
            out.push_str("</tr></thead><tbody>");
            let start = part_index * FLOURISH_ROWS_PER_TABLE;
            for row in &row_values[start..(start + FLOURISH_ROWS_PER_TABLE).min(row_values.len())] {
                out.push_str("<tr>");
                for column_index in 0..max_columns {
                    let cell_metadata = column_metadata
                        .get(column_index)
                        .filter(|m| m.is_object())
                        .unwrap_or(&empty_obj);
                    let cell = row.get(column_index).unwrap_or(&empty_obj);
                    out.push_str(&format!(
                        "<td>{}</td>",
                        esc_html_text(&format_flourish_cell(cell, cell_metadata))
                    ));
                }
                out.push_str("</tr>");
            }
            out.push_str("</tbody></table>");
        }
    }
    out
}

/// Replace Sky's script-hydrated Flourish tables with the semantic
/// tables decoded from the Flourish embed's published dataset, so
/// extraction can see the cells.
pub fn expand_sky_flourish_tables(html: &str, url: &str) -> String {
    if html.is_empty() || host_of(url) != "news.sky.com" {
        return html.to_string();
    }
    if !html.to_lowercase().contains("flourish-table") {
        return html.to_string();
    }
    static SRC_RE: OnceLock<Regex> = OnceLock::new();
    let src_re = SRC_RE.get_or_init(|| Regex::new(r"^/?visualisation/(\d+)(?:\?[^#]*)?$").unwrap());
    let widgets = find_elements(html, Some("div"), &|open: &str| {
        has_class(open, "flourish-embed") && has_class(open, "flourish-table")
    });
    if widgets.is_empty() {
        return html.to_string();
    }
    // Wrapper candidates for the sdc-article-widget replacement.
    let wrappers = find_elements(html, Some("div"), &|open: &str| {
        has_class(open, "sdc-article-widget")
    });
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for widget in widgets.iter().take(12) {
        let open = &html[widget.start..widget.inner_start];
        let Some(data_src) = attr_value(open, "data-src") else {
            continue;
        };
        let Some(cap) = src_re.captures(&data_src) else {
            continue;
        };
        let embed_url = format!("https://flo.uri.sh/visualisation/{}/embed", &cap[1]);
        // Python fetches the embed with the article as Referer.
        let Ok(mut resp) = crate::http::agent()
            .get(&embed_url)
            .header("Referer", url)
            .call()
        else {
            continue;
        };
        let Ok(bytes) = resp.body_mut().read_to_vec() else {
            continue;
        };
        if bytes.len() > 8_000_000 {
            continue;
        }
        let embed_html = String::from_utf8_lossy(&bytes).to_string();
        let tables = flourish_tables_html(&embed_html);
        if tables.is_empty() {
            continue;
        }
        let container = format!("<div>{tables}</div>");
        // Replace the sdc-article-widget wrapper when the widget
        // sits inside one (trafilatura discards that wrapper).
        let target = wrappers
            .iter()
            .filter(|w| w.start <= widget.start && widget.end <= w.end)
            .min_by_key(|w| w.end - w.start)
            .map(|w| (w.start, w.end))
            .unwrap_or((widget.start, widget.end));
        replacements.push((target.0, target.1, container));
    }
    if replacements.is_empty() {
        return html.to_string();
    }
    replacements.sort_by_key(|r| r.0);
    replacements.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    for (start, end, replacement) in replacements {
        if start < cursor {
            continue;
        }
        out.push_str(&html[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&html[cursor..]);
    out
}

// --- NPR text edition (Python core/npr.py download_text_only_html) ---

/// Fetch NPR's own text-only edition of a story and strip the
/// content-free "Related Story: NPR" insets it renders.
pub fn npr_text_edition_html(url: &str) -> Option<String> {
    let story_id = crate::sources::npr_story_id(url)?;
    let (bytes, _final) = fetch_page_bytes(&format!("https://text.npr.org/{story_id}"), "").ok()?;
    let html = String::from_utf8_lossy(&bytes).to_string();
    static STUB_RE: OnceLock<Regex> = OnceLock::new();
    let stub_re = STUB_RE.get_or_init(|| {
        Regex::new(r"(?is)<hr[^>]*>\s*Related Story:\s*<a[^>]*>.*?</a>\s*<hr[^>]*>").unwrap()
    });
    Some(stub_re.replace_all(&html, "").to_string())
}

pub(crate) fn is_google_news_url(url: &str) -> bool {
    let host = host_of(url);
    if host != "news.google.com" && host != "www.news.google.com" {
        return false;
    }
    let path = url_path_lower(url);
    path.starts_with("/articles/") || path.starts_with("/read/")
}

/// Rebuilt source documents retain replies, headings and links that a generic scraper drops.
pub(crate) fn reconstructed_article_html(url: &str) -> Option<(String, String)> {
    crate::threads::github_html(url)
        .or_else(|| crate::forum_sources::topic_html(url))
        .or_else(|| crate::forum_sources::web_thread_html(url))
        .or_else(|| crate::threads::reddit_html(url))
        .or_else(|| crate::threads::lemmy_html(url))
}

/// Process one fetched page body the way the direct
/// fetch is processed: Sky Flourish expansion, the
/// extraction cascade, and multi-page following with the
/// metered-preview notice (Python's extract_full_article
/// main loop). Ok is an Article win; Err is the
/// non-article outcome for the caller to remember.
fn process_fetched_page(
    html: &str,
    url: &str,
    feed_text: Option<&str>,
    encoding_override: &str,
    keep_alt: bool,
    cookie_header: &str,
) -> Result<ExtractOutcome, ExtractOutcome> {
    let expanded = expand_sky_flourish_tables(html, url);
    let outcome = extract_article_opts(&expanded, url, feed_text, keep_alt);
    let ExtractOutcome::Article {
        title,
        text,
        source,
    } = outcome
    else {
        return Err(outcome);
    };
    let mut texts = vec![text];
    let mut cur_html = expanded;
    let mut cur_url = url.to_string();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(url.to_string());
    for _ in 0..5 {
        let Some(next) = find_next_page(&cur_html, &cur_url) else {
            break;
        };
        if !visited.insert(next.clone()) {
            break;
        }
        let Ok((bytes, ct)) = fetch_page_bytes(&next, cookie_header) else {
            break;
        };
        let page_html = decode_bytes(&bytes, &ct, encoding_override);
        let page_html = expand_sky_flourish_tables(&page_html, &next);
        match extract_article_opts(&page_html, &next, None, keep_alt) {
            ExtractOutcome::Article { text: t2, .. } if !t2.trim().is_empty() => {
                texts.push(t2);
                cur_html = page_html;
                cur_url = next;
            }
            _ => break,
        }
    }
    // Each page's extractor adds its own notice; keep one at the end of the merged story.
    for text in &mut texts {
        if let Some(body) = text.trim_end().strip_suffix(METERED_NOTICE) {
            *text = body.trim_end().to_string();
        }
    }
    let mut merged = if texts.len() > 1 {
        merge_texts(&texts)
    } else {
        texts.into_iter().next().unwrap_or_default()
    };
    if source != "feed" && looks_like_metered_preview(html) {
        merged = format!("{}\n\n{}", merged.trim_end(), metered_preview_notice());
    }
    Ok(ExtractOutcome::Article {
        title,
        text: merged,
        source,
    })
}

/// HTTP status codes Python's `_fetch_page` treats as a
/// gate (`_GATE_STATUS_CODES`): 401 is deliberately absent
/// — a genuine credential prompt a browser cannot satisfy.
const GATE_STATUS_CODES: &[u16] = &[402, 403, 429, 451, 503];

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
    fetch_full_text_opts(url, cookie_header, feed_text, encoding_override, false)
}

/// fetch_full_text with the image-alt option threaded into the
/// cascade's feed-content conversion.
pub fn fetch_full_text_opts(
    url: &str,
    cookie_header: &str,
    feed_text: Option<&str>,
    encoding_override: &str,
    keep_alt: bool,
) -> (ExtractOutcome, Option<String>) {
    let url = url.trim();
    if EMPTY_EXTRACTIONS
        .get_or_init(|| Mutex::new(EmptyExtractions::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(url, encoding_override, Instant::now())
    {
        let text = feed_text
            .map(|text| html_to_text_opts(text, keep_alt))
            .unwrap_or_default();
        return (
            if text.trim().is_empty() {
                ExtractOutcome::Empty
            } else {
                ExtractOutcome::Article {
                    title: String::new(),
                    text,
                    source: "feed",
                }
            },
            None,
        );
    }
    if let Some((title, html)) = reconstructed_article_html(url) {
        let mut text = html_to_text_opts(&html, keep_alt);
        if !crate::postprocess::is_github_page_url(url)
            && let Some(feed) = feed_text.map(|feed| html_to_text_opts(feed, keep_alt))
            && feed.chars().count() > text.chars().count()
        {
            text = feed;
        }
        if !text.trim().is_empty() {
            return (
                ExtractOutcome::Article {
                    title,
                    text,
                    source: "thread",
                },
                Some(html),
            );
        }
    }
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
    // Direct media URLs (Batch 35b, Python extract_full_article):
    // an MP3's show notes live in its ID3 tag; other media has no
    // webpage text at all, so callers fall back to feed content.
    if looks_like_media_url(url) {
        if looks_like_id3_media_url(url)
            && let Some((title, _author, content)) = id3_metadata(url)
        {
            let text = html_to_text(&content);
            if !text.trim().is_empty() {
                return (
                    ExtractOutcome::Article {
                        title,
                        text,
                        source: "id3",
                    },
                    None,
                );
            }
        }
        return (ExtractOutcome::Empty, None);
    }
    // YouTube watch pages: the structured reconstruction beats the
    // progressively-hydrated HTML (Python youtube_fulltext); a
    // failure is reported, not scraped boilerplate.
    if crate::ytfull::is_youtube_video_url(url) {
        return match crate::ytfull::extract_article(url) {
            Some((title, _author, text)) => (
                ExtractOutcome::Article {
                    title,
                    text,
                    source: "youtube",
                },
                None,
            ),
            None => (
                ExtractOutcome::Gate {
                    reason: "Could not retrieve YouTube video details.".to_string(),
                },
                None,
            ),
        };
    }
    // Google News links are signed redirects: resolve through the
    // Jina proxy, which renders the redirect server-side (Python's
    // local decoder rides the curl_cffi impersonation transport —
    // a documented non-port).
    if is_google_news_url(url) {
        if let Some(text) = fetch_via_jina_text(url)
            && text.chars().count() >= 200
        {
            return (
                ExtractOutcome::Article {
                    title: String::new(),
                    text,
                    source: "jina",
                },
                None,
            );
        }
        return (
            ExtractOutcome::Gate {
                reason: "Could not resolve the Google News link to its publisher page.".to_string(),
            },
            None,
        );
    }
    let mut first_html: Option<String> = None;
    // NPR stories: NPR's own text edition is the better source and
    // sits outside the TollBit gate (Python routes fetches to it).
    if let Some(html) = npr_text_edition_html(url) {
        let outcome = extract_article_opts(&html, url, feed_text, keep_alt);
        if matches!(outcome, ExtractOutcome::Article { .. }) {
            let outcome = match outcome {
                ExtractOutcome::Article { title, text, .. } => ExtractOutcome::Article {
                    title,
                    text,
                    source: "npr",
                },
                other => other,
            };
            return (outcome, Some(html));
        }
    }
    // The direct fetch is status-aware (Python `_fetch_page`
    // inspects error-status bodies too): gate statuses and
    // interstitial bodies mark the gate even when no usable
    // page came back, which is what unlocks the gate-only
    // fallback stages below.
    let direct = (|| -> Result<(u16, String), String> {
        let request = crate::http::status_agent().get(url);
        let request = if cookie_header.is_empty() {
            request
        } else {
            request.header("Cookie", cookie_header)
        };
        let mut response = request.call().map_err(|e| format!("fetch failed: {e}"))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|e| format!("read failed: {e}"))?;
        Ok((
            status,
            decode_bytes(&bytes, &content_type, encoding_override),
        ))
    })();
    let mut direct_outcome: Option<ExtractOutcome> = None;
    let mut gate_seen = false;
    let mut direct_error: Option<String> = None;
    match &direct {
        Ok((status, html)) => {
            first_html = Some(html.clone());
            if (200..400).contains(status) {
                match process_fetched_page(
                    html,
                    url,
                    feed_text,
                    encoding_override,
                    keep_alt,
                    cookie_header,
                ) {
                    Ok(win) => return (win, first_html),
                    Err(outcome) => {
                        gate_seen = matches!(outcome, ExtractOutcome::Gate { .. });
                        direct_outcome = Some(outcome);
                    }
                }
            } else {
                gate_seen = GATE_STATUS_CODES.contains(status) || looks_gated(html);
                direct_outcome = Some(ExtractOutcome::Gate {
                    reason: format!("fetch failed: http status: {status}"),
                });
            }
        }
        Err(error) => {
            direct_error = Some(error.clone());
        }
    }
    let relabel = |outcome: ExtractOutcome, label: &'static str| match outcome {
        ExtractOutcome::Article { title, text, .. } => ExtractOutcome::Article {
            title,
            text,
            source: label,
        },
        other => other,
    };
    // Python `_try_fallbacks`, in order. The clearance
    // refresh runs first when a gate was seen: a current
    // cookie fetches the page outright, beating the whole
    // chain below.
    let mut dropped_stale_clearance = false;
    if gate_seen {
        if let Some(body) = clearance_recovery_body(url) {
            first_html = Some(body.clone());
            // The recovered body IS the fetch result
            // (Python returns it from `_try_fallbacks`);
            // its extraction outcome is the answer either
            // way.
            return (
                match process_fetched_page(
                    &body,
                    url,
                    feed_text,
                    encoding_override,
                    keep_alt,
                    cookie_header,
                ) {
                    Ok(win) => win,
                    Err(outcome) => outcome,
                },
                first_html,
            );
        }
        if has_stored_clearance(url) {
            let config_path = configured_config_path();
            if let Some(config_path) = config_path {
                dropped_stale_clearance = crate::cookies::forget_clearance_for(&config_path, url);
            }
        }
    }
    // Sky via Google Translate (host-gated inside the fn).
    if let Some(html) = fetch_via_sky_translate(url) {
        let outcome = extract_article_opts(&html, url, feed_text, keep_alt);
        if matches!(outcome, ExtractOutcome::Article { .. }) {
            return (relabel(outcome, "sky"), first_html);
        }
    }
    // Impersonated refetch: skipped when the site's
    // pinned session already forced that handshake on the
    // direct fetch, unless stale clearance was just
    // dropped (Python's rule). The transport is
    // crate::impersonate (B16, landed).
    if (site_impersonation_target(url).is_empty() || dropped_stale_clearance)
        && let Some(html) = fetch_via_impersonation(url, cookie_header)
        && let Ok(win) = process_fetched_page(
            &html,
            url,
            feed_text,
            encoding_override,
            keep_alt,
            cookie_header,
        )
    {
        return (win, first_html);
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
    // Wayback (always tried).
    if let Some(html) = fetch_via_wayback(url, cookie_header) {
        let outcome = extract_article_opts(&html, url, feed_text, keep_alt);
        if matches!(outcome, ExtractOutcome::Article { .. }) {
            return (relabel(outcome, "wayback"), first_html);
        }
    }
    // The warm browser: genuinely last, only when a gate
    // was actually seen and the site holds no stored
    // clearance (a session the browser itself won would
    // have been tried by the clearance stages already).
    if gate_seen
        && !has_stored_clearance(url)
        && let Some(html) = fetch_via_browser(url)
    {
        return (
            match process_fetched_page(
                &html,
                url,
                feed_text,
                encoding_override,
                keep_alt,
                cookie_header,
            ) {
                Ok(win) => relabel(win, "browser"),
                Err(outcome) => outcome,
            },
            first_html,
        );
    }
    // Nothing worked: report the direct outcome (Gate/Empty)
    // or the fetch failure as a Gate with its reason.
    if matches!(direct_outcome, Some(ExtractOutcome::Empty)) {
        EMPTY_EXTRACTIONS
            .get_or_init(|| Mutex::new(EmptyExtractions::default()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remember(url, encoding_override, Instant::now());
    }
    match direct_outcome {
        Some(outcome) => (outcome, first_html),
        None => (
            ExtractOutcome::Gate {
                reason: direct_error.unwrap_or_else(|| "fetch failed".to_string()),
            },
            first_html,
        ),
    }
}

// ---------------------------------------------------------------------------
// Site-specific text cleaners: Python's whole postprocess stage
// lives in crate::postprocess (ported 2026-10-31, Audit 4); this
// stays as the cascade's entry point.
// ---------------------------------------------------------------------------

fn host_matches(url: &str, domain: &str) -> bool {
    let host = host_of(url);
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// Apply Python's postprocess stage for a URL to extracted text
/// (no published-date label available on this path).
pub fn apply_site_cleaners(text: &str, url: &str) -> String {
    crate::postprocess::postprocess_extracted_text(text, url, "")
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

/// Python `_extract_title_author_from_meta`
/// (article_extractor.py): the title is the `<title>`
/// element's text only (never og:title or a heading — forum
/// and thread pages put the first post's heading where
/// trafilatura looks), and the author is the first non-empty
/// candidate from the author-meta priority list, with a
/// leading "By " stripped and profile URLs rejected (a URL
/// is never a byline).
pub fn title_author_from_meta(html: &str) -> (String, String) {
    let lower = html.to_lowercase();
    let mut title = String::new();
    if let Some(pos) = lower.find("<title")
        && let Some(gt) = lower[pos..].find('>')
    {
        let start = pos + gt + 1;
        if let Some(end) = lower[start..].find("</title") {
            title = unescape_entities(&html[start..start + end])
                .trim()
                .to_string();
        }
    }
    let mut author = String::new();
    for name in [
        "byl",
        "author",
        "article:author",
        "parsely-author",
        "sailthru.author",
    ] {
        if let Some(raw) = meta_content(html, name) {
            let value = unescape_entities(&raw);
            if !value.trim().is_empty() {
                author = value.trim().to_string();
                break;
            }
        }
    }
    if !author.is_empty() {
        author = strip_by_prefix(&author);
        let lowered = author.to_lowercase();
        if lowered.starts_with("//")
            || lowered.starts_with("http://")
            || lowered.starts_with("https://")
        {
            author = String::new();
        }
    }
    (title, author)
}

/// Python's `(?i)^\s*by[\s:]+` prefix strip for bylines.
fn strip_by_prefix(author: &str) -> String {
    let s = author.trim_start();
    let bytes = s.as_bytes();
    if bytes.len() >= 3
        && bytes[0].eq_ignore_ascii_case(&b'b')
        && bytes[1].eq_ignore_ascii_case(&b'y')
        && (bytes[2] == b':' || bytes[2].is_ascii_whitespace())
    {
        return s[2..]
            .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
            .to_string();
    }
    s.to_string()
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

#[cfg(test)]
mod parity_recovery_tests {
    use super::*;

    #[test]
    fn media_url_detection_accepts_windows_file_paths() {
        // Regression (Wave 3 batch 1 Windows CI failure): a local
        // enclosure path with backslash separators produced an
        // empty path here, so looks_like_id3_media_url was false
        // and render_rich_article never read the ID3 notes.
        let win = r"C:\Users\brandon\AppData\Local\Temp\episode.MP3";
        assert!(looks_like_id3_media_url(win));
        assert!(looks_like_media_url(win));
        assert!(looks_like_id3_media_url("/tmp/episode.mp3"));
        assert!(looks_like_id3_media_url(
            "https://example.test/a/episode.mp3"
        ));
        assert!(!looks_like_id3_media_url(r"C:\podcasts\episode.m4a"));
        assert!(!looks_like_media_url("https://example.test/story"));
    }

    #[test]
    fn empty_extractions_expire_and_include_encoding_in_the_key() {
        let now = Instant::now();
        let mut cache = EmptyExtractions::default();
        cache.remember("https://example.test/story", "utf-8", now);
        assert!(!cache.contains("https://example.test/story", "windows-1252", now));
        assert!(cache.contains(
            "https://example.test/story",
            "utf-8",
            now + Duration::from_secs(899)
        ));
        assert!(!cache.contains(
            "https://example.test/story",
            "utf-8",
            now + EMPTY_EXTRACTION_TTL
        ));
    }

    #[test]
    fn empty_extractions_evict_the_least_recently_used_url() {
        let now = Instant::now();
        let mut cache = EmptyExtractions::default();
        for i in 0..256 {
            cache.remember(&format!("https://example.test/{i}"), "", now);
        }
        assert!(cache.contains("https://example.test/0", "", now));
        cache.remember("https://example.test/256", "", now);
        assert_eq!(cache.0.len(), 256);
        assert!(cache.contains("https://example.test/0", "", now));
        assert!(!cache.contains("https://example.test/1", "", now));
    }

    #[test]
    fn cached_empty_extraction_keeps_feed_fallback_available() {
        let url = "https://cached-empty.invalid/story";
        EMPTY_EXTRACTIONS
            .get_or_init(|| Mutex::new(EmptyExtractions::default()))
            .lock()
            .unwrap()
            .remember(url, "", Instant::now());
        assert_eq!(
            fetch_full_text(url, "", None, ""),
            (ExtractOutcome::Empty, None)
        );
        let (outcome, html) = fetch_full_text(url, "", Some("<p>Authored feed content.</p>"), "");
        assert!(
            matches!(outcome, ExtractOutcome::Article { text, source: "feed", .. } if text == "Authored feed content.")
        );
        assert!(html.is_none());
    }

    #[test]
    fn json_ld_decodes_escaped_block_markup_but_preserves_prose_tags() {
        let prose = "The researchers carefully checked the evidence and published their detailed findings for independent review. ";
        let body = format!(
            "&lt;h2&gt;Findings&lt;/h2&gt;&lt;p&gt;{prose}&lt;/p&gt;&lt;p&gt;{prose}&lt;/p&gt;"
        );
        let page = format!(
            "<script type='application/ld+json'>{}</script>",
            serde_json::json!({"articleBody": body})
        );
        let text = json_ld_text(&page);
        assert!(text.contains("Findings"));
        assert!(text.contains(prose.trim()));
        assert!(!text.contains("<p>"));
        assert!(!text.contains("<h2>"));
        let body = format!("{prose}Use &lt;/p&gt; to close a paragraph. {prose}");
        let page = format!(
            "<script type='application/ld+json'>{}</script>",
            serde_json::json!({"articleBody": body})
        );
        assert!(json_ld_text(&page).contains("Use </p> to close a paragraph."));
    }

    #[test]
    fn json_ld_candidate_strips_leaked_script_code() {
        // Task 14 (R08): the candidate is cleaned — leaked script
        // source stripped — before it competes on length, exactly
        // as Python's _collect_json_ld_text does; the code must
        // not survive into the candidate however long it is.
        let prose = "This is a substantial paragraph of article body text that goes on for a while to make the point clearly and completely for the reader who is following along at home today.";
        let code = "let cbeScripts = {};\ndocument.querySelectorAll('.cbe').forEach(function (item) {\n  new cbeScriptObserver(item, cbeScripts[item]);\n});\nwindow.addEventListener('load', function () { console.log('ready'); });";
        let body = format!("{prose}\n{prose}\n{code}");
        let page = format!(
            "<script type='application/ld+json'>{}</script>",
            serde_json::json!({"@type": "NewsArticle", "articleBody": body})
        );
        let text = json_ld_text(&page);
        assert!(text.contains("substantial paragraph"), "{text}");
        assert!(!text.contains("cbeScriptObserver"), "JS leaked: {text}");
        assert!(!text.contains("querySelectorAll"), "JS leaked: {text}");
    }
}
