//! Embed-preserving article HTML cleaning for the opt-in rich reader.
//! Port of Python `core/article_html.py` (Batch 26; see docs/PARITY.md).
//!
//! The plain-text reader strips everything that is not prose — including
//! iframe video widgets and embedded tweets. The rich reader renders real
//! HTML in a WebView instead, so this module produces a *sanitized
//! article-body fragment* that keeps prose, links, images, tables, and
//! media embeds while dropping page chrome (nav / sidebars / tickers /
//! scripts / ads). Sanitization runs through ammonia (the Rust HTML
//! sanitizer) with Python's allowlists; the DOM passes ammonia cannot
//! express (main-node pick, chrome-class drops, boilerplate/byline text
//! rules, the trafilatura-guided prune, social-embed conversion) run on
//! the balanced-element scanner from `extract` before/after it.
//!
//! Deliberate divergences from Python, kept small and listed in
//! PARITY.md: `data:` URLs are dropped by the scheme allowlist (Python
//! passes them through; in practice they are tracking pixels and
//! placeholders), and the socast/forum/publisher reconstructions feed
//! the text reader only — the rich node pick is the generic one.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::article_lang::{ContentLangSources, resolve_content_language};
use crate::extract::{
    attr_value, extract_focus, fetch_page_bytes, has_class, html_to_text, leaf_text,
    remove_elements, strip_zero_width, tag_tokens,
};
use crate::postprocess::is_github_page_url;

// ---------------------------------------------------------------------------
// Python's allowlists
// ---------------------------------------------------------------------------

const ALLOWED_TAGS: &[&str] = &[
    "p",
    "br",
    "hr",
    "span",
    "div",
    "section",
    "article",
    "main",
    "figure",
    "figcaption",
    "picture",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "blockquote",
    "q",
    "cite",
    "pre",
    "code",
    "em",
    "strong",
    "b",
    "i",
    "u",
    "s",
    "sub",
    "sup",
    "mark",
    "small",
    "abbr",
    "time",
    "a",
    "img",
    "source",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "caption",
    "colgroup",
    "col",
    "iframe",
    "video",
    "audio",
    "track",
];

const DROP_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "form", "input", "button",
    "select", "textarea", "label", "nav", "aside", "header", "footer", "object", "embed", "ins",
    "link", "meta", "base", "dialog",
];

fn attr_keep() -> HashMap<&'static str, HashSet<&'static str>> {
    let mut m: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();
    let mut add = |tag: &'static str, attrs: &[&'static str]| {
        m.insert(tag, attrs.iter().copied().collect());
    };
    add("a", &["href", "title"]);
    add("img", &["src", "srcset", "alt", "title"]);
    add("source", &["src", "srcset", "type", "media"]);
    add(
        "iframe",
        &[
            "src",
            "title",
            "allow",
            "allowfullscreen",
            "width",
            "height",
        ],
    );
    add("video", &["src", "poster", "controls", "width", "height"]);
    add("audio", &["src", "controls"]);
    add("track", &["src", "kind", "srclang", "label", "default"]);
    add("time", &["datetime"]);
    add("th", &["colspan", "rowspan", "scope"]);
    add("td", &["colspan", "rowspan"]);
    m
}

// ---------------------------------------------------------------------------
// Python's text rules (regexes ported verbatim; CHROME loses its one
// lookahead — see chrome_match)
// ---------------------------------------------------------------------------

static BOILERPLATE_TEXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(affiliate links?\b.{0,80}\bcommission|may earn\b.{0,40}\bcommission|earns?\s+(?:us\s+)?an?\s+(?:small\s+)?commission|we may (?:earn|receive|get)\b.{0,40}(?:commission|compensat)|contains? affiliate links?|this (?:post|article|page)\b.{0,30}\baffiliate links?|sign up (?:for|to)\b.{0,40}\bnewsletter|subscribe (?:to|today|now|for)\b|unlimited access\b|subscriber[- ](?:only|content|exclusive)|best-in-class reporting|delivered (?:to|straight to) your inbox|get (?:the|our)\b.{0,30}\bnewsletter|join (?:our|the)\b.{0,30}\bnewsletter|(?:please )?log ?in to (?:bookmark|read|continue|save)|create (?:a )?free account|already a (?:subscriber|member)|open this photo in gallery|follow us on\b.{0,20}\b(?:google|discover|flipboard|twitter|facebook|instagram|threads|youtube|tiktok|linkedin|mastodon|bluesky|whatsapp|telegram)|add us as (?:a )?preferred source|thank you for being part of our community|read our comment policy|comment policy before posting)",
    )
    .unwrap()
});

static WIDGET_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\W*((?:zdnet|editor'?s?|our)\s+(?:recommends?|picks?|choice)|recommended (?:stories|reading|for you|videos?)|(?:related|more|popular|trending|latest)\s+(?:stories|articles|reading|coverage|posts?|news|videos?)|read (?:more|next|also)|(?:up|coming) next|you (?:might|may) (?:also )?like|most (?:popular|read)|trending now|(?:see|show) more|advertisement|sponsored( content| links?)?)\W*$",
    )
    .unwrap()
});

static BYLINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^by$|^by\s|\b\d{1,2}:\d{2}\s*(?:a\.?m\.?|p\.?m\.?)\b|\b\d{1,2}:\d{2}\b\s*(?:et|est|edt|pt|pst|pdt|ct|cst|mt|gmt|utc)\b|^(?:updated|published|posted|last updated)\b\s*:?)",
    )
    .unwrap()
});

static CHROME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[-_ ])(?:sidebar|advert|advertisement|\bads?\b|promo|newsletter|subscribe|signup|related|recommend|trending|sharethis|share-|social|comments?|disqus|cookie|consent|ticker|breaking-?bar|popup|modal|paywall|taboola|outbrain|more-from|read-?more|back-to-top|breadcrumb|post-?meta|entry-?meta|post-?tags?|tags?-list|tag-?cloud|cat-?links|post-?categories|author-?bio|author-?box|byline|most-?popular|up-?next|read-?next|editors?-?picks?|sponsor|partner|follow-?us|newsletter-?signup|email-?signup|jump-?link|toc|inline-?promo|visually-?hidden|sr-?only|screen-?reader|toggletip|popover|tooltip|discover|preferred-?source|bookmark|save-?story|gallery-?caption)",
    )
    .unwrap()
});

static METER_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[-_ ])meter(?:$|[-_ ])").unwrap());

/// Python `_CHROME_RE.search(ident)`. The Python pattern's `meter(?!ed)`
/// (paywall meters, but not NYT's `meteredContent` body wrapper) uses a
/// lookahead the Rust regex crate lacks; it is checked separately here:
/// a `meter` token bounded by start/`-_ `/end that is not `metered`.
fn chrome_match(ident: &str) -> bool {
    if CHROME_RE.is_match(ident) {
        return true;
    }
    if METER_TOKEN_RE.is_match(ident) {
        return true;
    }
    // `meter` at the very end after a separator, or before punctuation
    // the class list may carry — the bounded regex above needs a
    // trailing separator or end; also accept `meter` followed by a
    // non-letter that is not `e` of `ed`.
    let low = ident.to_lowercase();
    for (i, _) in low.match_indices("meter") {
        let before_ok = i == 0 || matches!(low.as_bytes()[i - 1], b'-' | b'_' | b' ');
        if !before_ok {
            continue;
        }
        let after = &low[i + 5..];
        if after.is_empty() || !after.starts_with("ed") {
            return true;
        }
    }
    false
}

const BOILERPLATE_MAX_LEN: usize = 240;

static RE_TWITTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:twitter\.com|x\.com)/[^/\s]+/status(?:es)?/(\d+)").unwrap()
});
static RE_INSTAGRAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)instagram\.com/(?:p|reel|tv)/([A-Za-z0-9_-]+)").unwrap());
static RE_TIKTOK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)tiktok\.com/@[^/\s]+/video/(\d+)").unwrap());
static RE_TIKTOK_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{6,})").unwrap());
static RE_BSKY_ATURI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(did:[^/\s]+)/app\.bsky\.feed\.post/([A-Za-z0-9]+)").unwrap()
});
static RE_BSKY_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)bsky\.app/profile/([^/\s]+)/post/([A-Za-z0-9]+)").unwrap());
static RE_THREADS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)threads\.(?:net|com)/(@[^/\s]+/post/[A-Za-z0-9_-]+)").unwrap()
});

static BR_RUN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>(\s*<br\s*/?>)+").unwrap());
static BR_AFTER_OPEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(<(?:p|div|section|article|li|blockquote|figure|figcaption|td|th|h[1-6])[^>]*>)\s*<br\s*/?>").unwrap()
});
static BR_BEFORE_CLOSE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<br\s*/?>\s*(</(?:p|div|section|article|li|blockquote|figure|figcaption|td|th|h[1-6])>)").unwrap()
});

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Python `_norm`: lowercase, collapse to alphanumerics (ASCII), single
/// spaces — for robust text-overlap matching.
fn norm(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = true; // trims the leading edge like strip()
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The first `n` chars of a normalized probe (Python `probe[:60]`).
fn probe_head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn esc_attr(s: &str) -> String {
    esc_text(s).replace('"', "&quot;")
}

/// Python `urllib.parse.quote(value, safe="")`.
fn quote_all(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn absolutize(value: &str, base_url: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with("data:")
        || value.starts_with("mailto:")
        || value.starts_with("tel:")
        || value.starts_with('#')
    {
        return value.to_string();
    }
    if let Some(rest) = value.strip_prefix("//") {
        return format!("https://{rest}");
    }
    if !base_url.is_empty()
        && url::Url::parse(value).is_err()
        && let Ok(base) = url::Url::parse(base_url)
        && let Ok(joined) = base.join(value)
    {
        return joined.to_string();
    }
    value.to_string()
}

/// Turn a known player/embed URL into a page a browser can open directly
/// (Python `_normalize_media_url`).
fn normalize_media_url(value: &str) -> String {
    let value = value.trim();
    let value = if let Some(rest) = value.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        value.to_string()
    };
    let Ok(parts) = url::Url::parse(&value) else {
        return value;
    };
    let host = parts.host_str().unwrap_or("").to_lowercase();
    let path = parts.path();
    if (host.ends_with("youtube.com") || host.ends_with("youtube-nocookie.com"))
        && let Some(id) = path.strip_prefix("/embed/")
    {
        let id: String = id
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if id.len() >= 6 {
            return format!("https://www.youtube.com/watch?v={id}");
        }
    }
    if host.ends_with("player.vimeo.com")
        && let Some(id) = path.strip_prefix("/video/")
    {
        let id: String = id.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !id.is_empty() {
            return format!("https://vimeo.com/{id}");
        }
    }
    value
}

// ---------------------------------------------------------------------------
// Balanced-element scanning (all elements, nested included — the
// BeautifulSoup-shaped view the cleaning passes need)
// ---------------------------------------------------------------------------

const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

#[derive(Clone)]
struct Elem {
    name: String,
    start: usize,
    open_end: usize,
    inner_start: usize,
    inner_end: usize,
    end: usize,
}

fn all_elements(html: &str) -> Vec<Elem> {
    let toks = tag_tokens(html);
    let mut out: Vec<Elem> = Vec::new();
    let mut stack: Vec<Elem> = Vec::new();
    for tok in &toks {
        if tok.close {
            // Find the nearest matching open element; anything above it
            // is implicitly closed where this tag starts.
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
        let e = Elem {
            name: tok.name.clone(),
            start: tok.start,
            open_end: tok.end,
            inner_start: tok.end,
            inner_end: tok.end,
            end: tok.end,
        };
        if tok.self_closing || VOID_TAGS.contains(&tok.name.as_str()) {
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

impl Elem {
    fn open<'a>(&self, html: &'a str) -> &'a str {
        &html[self.start..self.open_end]
    }
    fn inner<'a>(&self, html: &'a str) -> &'a str {
        &html[self.inner_start..self.inner_end]
    }
}

/// Apply (start, end, replacement) edits left to right; edits nested
/// inside an earlier edit are dropped (the outer edit wins, matching
/// BeautifulSoup's decompose/replace semantics).
fn apply_edits(html: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    if edits.is_empty() {
        return html.to_string();
    }
    edits.sort_by_key(|(s, e, _)| (*s, std::cmp::Reverse(*e)));
    let mut out = String::with_capacity(html.len());
    let mut pos = 0usize;
    for (s, e, rep) in edits {
        if s < pos {
            continue;
        }
        out.push_str(&html[pos..s]);
        out.push_str(&rep);
        pos = e;
    }
    out.push_str(&html[pos..]);
    out
}

fn apply_removals(html: &str, spans: Vec<(usize, usize)>) -> String {
    apply_edits(
        html,
        spans
            .into_iter()
            .map(|(s, e)| (s, e, String::new()))
            .collect(),
    )
}

/// Remove every element (subtree included) for which `drop` returns
/// true. The predicate sees the open-tag source and the inner HTML.
fn drop_elements(html: &str, drop: &dyn Fn(&str, &str, &Elem) -> bool) -> String {
    let elems = all_elements(html);
    let spans: Vec<(usize, usize)> = elems
        .iter()
        .filter(|e| drop(e.open(html), e.inner(html), e))
        .map(|e| (e.start, e.end))
        .collect();
    apply_removals(html, spans)
}

fn has_media(inner: &str) -> bool {
    let low = inner.to_lowercase();
    low.contains("<iframe") || low.contains("<video") || low.contains("<audio")
}

fn has_any_tag(inner: &str, names: &[&str]) -> bool {
    tag_tokens(inner)
        .iter()
        .any(|t| !t.close && names.contains(&t.name.as_str()))
}

/// True if the fragment contains a real content image (not a decorative
/// SVG/icon used by share/follow/promo widgets).
fn has_content_image(inner: &str) -> bool {
    for tok in tag_tokens(inner) {
        if tok.close || tok.name != "img" {
            continue;
        }
        let open = &inner[tok.start..tok.end];
        let alt = attr_value(open, "alt").unwrap_or_default();
        if alt.trim().is_empty() {
            continue;
        }
        let src = attr_value(open, "src")
            .or_else(|| attr_value(open, "data-src"))
            .unwrap_or_default()
            .to_lowercase();
        if src.contains(".svg") || src.starts_with("data:") {
            continue;
        }
        return true;
    }
    false
}

/// True if the fragment wraps a block-level prose element (so it's a
/// container, not a leaf we can safely drop wholesale).
fn has_prose_child(inner: &str) -> bool {
    has_any_tag(
        inner,
        &[
            "p",
            "h1",
            "h2",
            "h3",
            "ul",
            "ol",
            "blockquote",
            "table",
            "figure",
        ],
    )
}

// ---------------------------------------------------------------------------
// Social-media embeds (Python _social_embed_for / _convert_social_embeds)
// ---------------------------------------------------------------------------

fn first_href(inner: &str) -> String {
    for tok in tag_tokens(inner) {
        if tok.close || tok.name != "a" {
            continue;
        }
        if let Some(href) = attr_value(&inner[tok.start..tok.end], "href") {
            let href = href.trim().to_string();
            if href.to_lowercase().starts_with("http://")
                || href.to_lowercase().starts_with("https://")
            {
                return href;
            }
        }
    }
    String::new()
}

/// (iframe_src, permalink, platform_label) for a social embed element.
fn social_embed_for(
    open: &str,
    inner: &str,
    base_url: &str,
) -> Option<(String, String, &'static str)> {
    let classes = attr_value(open, "class").unwrap_or_default().to_lowercase();
    let mut data_href = String::new();
    for attr in [
        "data-href",
        "data-instgrm-permalink",
        "data-embed-url",
        "cite",
        "data-url",
    ] {
        if let Some(v) = attr_value(open, attr)
            && !v.trim().is_empty()
        {
            data_href = absolutize(v.trim(), base_url);
            break;
        }
    }
    let href = if data_href.is_empty() {
        absolutize(&first_href(inner), base_url)
    } else {
        data_href
    };
    let at_uri = attr_value(open, "data-bluesky-uri").unwrap_or_default();
    let embed_url_attr = attr_value(open, "data-embed-url").unwrap_or_default();
    let hay = format!("{classes} {href} {at_uri} {embed_url_attr}");

    // Twitter / X
    let m = RE_TWITTER.captures(&hay);
    if classes.contains("twitter-tweet") || classes.contains("twitter-video") || m.is_some() {
        if let Some(cap) = m {
            return Some((
                format!(
                    "https://platform.twitter.com/embed/Tweet.html?id={}",
                    &cap[1]
                ),
                href,
                "X",
            ));
        }
        return Some((String::new(), href, "X"));
    }

    // Bluesky
    if classes.contains("bluesky-embed") || hay.contains("bsky.app") || !at_uri.is_empty() {
        if let Some(cap) = RE_BSKY_ATURI
            .captures(&at_uri)
            .or_else(|| RE_BSKY_ATURI.captures(&hay))
        {
            return Some((
                format!(
                    "https://embed.bsky.app/embed/{}/app.bsky.feed.post/{}",
                    &cap[1], &cap[2]
                ),
                href,
                "Bluesky",
            ));
        }
        if let Some(cap) = RE_BSKY_URL.captures(&hay)
            && cap[1].starts_with("did:")
        {
            return Some((
                format!(
                    "https://embed.bsky.app/embed/{}/app.bsky.feed.post/{}",
                    &cap[1], &cap[2]
                ),
                href,
                "Bluesky",
            ));
        }
        return Some((String::new(), href, "Bluesky"));
    }

    // Instagram
    let m = RE_INSTAGRAM.captures(&hay);
    if classes.contains("instagram-media") || m.is_some() {
        if let Some(cap) = m {
            return Some((
                format!("https://www.instagram.com/p/{}/embed", &cap[1]),
                href,
                "Instagram",
            ));
        }
        return Some((String::new(), href, "Instagram"));
    }

    // TikTok
    let m = RE_TIKTOK.captures(&hay);
    if classes.contains("tiktok-embed") || m.is_some() {
        let mut vid = m.map(|cap| cap[1].to_string()).unwrap_or_default();
        if vid.is_empty() {
            vid = attr_value(open, "data-video-id")
                .unwrap_or_default()
                .trim()
                .to_string();
        }
        if vid.is_empty()
            && let Some(cap) = RE_TIKTOK_ID.captures(&hay)
        {
            vid = cap[1].to_string();
        }
        if !vid.is_empty() {
            return Some((
                format!("https://www.tiktok.com/embed/v2/{vid}"),
                href,
                "TikTok",
            ));
        }
        return Some((String::new(), href, "TikTok"));
    }

    // Facebook
    if classes.contains("fb-post")
        || classes.contains("fb-video")
        || hay.contains("facebook.com/plugins")
    {
        if !href.is_empty() {
            let src = format!(
                "https://www.facebook.com/plugins/post.php?href={}&show_text=true",
                quote_all(&href)
            );
            return Some((src, href, "Facebook"));
        }
        return Some((String::new(), href, "Facebook"));
    }

    // Threads
    let m = RE_THREADS.captures(&hay);
    if classes.contains("text-post-media") || classes.contains("threads-embed") || m.is_some() {
        if let Some(cap) = m {
            return Some((
                format!("https://www.threads.net/{}/embed", &cap[1]),
                href,
                "Threads",
            ));
        }
        return Some((String::new(), href, "Threads"));
    }

    // Mastodon
    if classes.contains("mastodon-embed") {
        let embed = attr_value(open, "data-embed-url")
            .unwrap_or_default()
            .trim()
            .to_string();
        if !embed.is_empty() {
            return Some((absolutize(&embed, base_url), href, "Mastodon"));
        }
        return Some((String::new(), href, "Mastodon"));
    }

    None
}

/// Rewrite known social-embed blockquotes/divs into iframes/links.
fn convert_social_embeds(html: &str, base_url: &str) -> String {
    let elems = all_elements(html);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for e in &elems {
        if e.name != "blockquote" && e.name != "div" {
            continue;
        }
        let Some((iframe_src, permalink, label)) =
            social_embed_for(e.open(html), e.inner(html), base_url)
        else {
            continue;
        };
        let replacement = if !iframe_src.is_empty() {
            format!(
                "<iframe src=\"{}\" title=\"{label} post\" allowfullscreen=\"true\"></iframe>",
                esc_attr(&iframe_src)
            )
        } else if !permalink.is_empty() {
            let quoted = leaf_text(e.inner(html));
            let mut p = String::from("<p>");
            if !quoted.is_empty() {
                p.push_str(&esc_text(&quoted));
                p.push_str(" — ");
            }
            p.push_str(&format!(
                "<a href=\"{}\">View this post on {label}</a></p>",
                esc_attr(&permalink)
            ));
            p
        } else {
            continue;
        };
        edits.push((e.start, e.end, replacement));
    }
    apply_edits(html, edits)
}

// ---------------------------------------------------------------------------
// Main-node pick (Python _pick_main_node)
// ---------------------------------------------------------------------------

fn pick_main_fragment(html: &str) -> String {
    let elems = all_elements(html);
    let pick = |pred: &dyn Fn(&Elem) -> bool| -> Option<String> {
        for e in &elems {
            if pred(e) && char_len(&leaf_text(e.inner(html))) > 200 {
                return Some(e.inner(html).to_string());
            }
        }
        None
    };
    for tag in ["article", "main"] {
        if let Some(f) = pick(&|e: &Elem| e.name == tag) {
            return f;
        }
    }
    if let Some(f) = pick(&|e: &Elem| {
        attr_value(e.open(html), "role")
            .map(|v| v == "main")
            .unwrap_or(false)
    }) {
        return f;
    }
    if let Some(f) = pick(&|e: &Elem| {
        attr_value(e.open(html), "itemprop")
            .map(|v| v == "articleBody")
            .unwrap_or(false)
    }) {
        return f;
    }
    for class in [
        "entry-content",
        "post-content",
        "article-content",
        "article-body",
        "entry-body",
        "story-body",
        "post-body",
        "entry",
    ] {
        if let Some(f) = pick(&|e: &Elem| has_class(e.open(html), class)) {
            return f;
        }
    }
    for e in &elems {
        if e.name == "body" {
            return e.inner(html).to_string();
        }
    }
    html.to_string()
}

// ---------------------------------------------------------------------------
// The cleaner (Python clean_article_html)
// ---------------------------------------------------------------------------

/// Normalized recall-mode trafilatura extraction — the article's real
/// text, used as ground truth to prune blocks the extractor excluded.
fn article_keep_text(html: &str, _url: &str) -> String {
    let (_, text) = extract_focus(html, rust_trafilatura::ExtractionFocus::FavorRecall);
    norm(&text)
}

/// Return a sanitized article-body HTML fragment, or "" if none is
/// usable. Keeps prose, links, images, tables, and media embeds; drops
/// page chrome, scripts, ad/ticker widgets, and (when `use_traf_prune`)
/// any block the trafilatura extractor judged to be outside the article
/// body. URLs are absolutized against `url`.
pub fn clean_article_html(html: &str, url: &str, use_traf_prune: bool) -> String {
    if html.is_empty() {
        return String::new();
    }
    let keep_text = if use_traf_prune {
        article_keep_text(html, url)
    } else {
        String::new()
    };
    let mut frag = pick_main_fragment(html);

    // Social embeds first, so the passes below keep the resulting
    // iframe/anchor nodes.
    frag = convert_social_embeds(&frag, url);

    // Category/tag links (WordPress rel="tag"/"category tag").
    frag = remove_elements(&frag, &|name, open| {
        if name != "a" {
            return false;
        }
        let rel = attr_value(open, "rel").unwrap_or_default().to_lowercase();
        rel.split_whitespace()
            .any(|r| r == "tag" || r == "category")
    })
    .0;

    // Class/id furniture (kept-tag containers like <div
    // class="related">). Never drop a container holding a media embed.
    // Python only chrome-checks tags in the drop/allow lists.
    frag = drop_elements(&frag, &|open, inner, e| {
        if !ALLOWED_TAGS.contains(&e.name.as_str()) && !DROP_TAGS.contains(&e.name.as_str()) {
            return false;
        }
        let ident = format!(
            "{} {}",
            attr_value(open, "class").unwrap_or_default(),
            attr_value(open, "id").unwrap_or_default()
        );
        let ident = ident.trim();
        !ident.is_empty() && chrome_match(ident) && !has_media(inner)
    });

    // Stock affiliate/subscribe disclosures and label-only widget
    // headers (short blocks, matched by text).
    frag = drop_elements(&frag, &|_, inner, e| {
        if !matches!(
            e.name.as_str(),
            "p" | "div"
                | "aside"
                | "small"
                | "figcaption"
                | "span"
                | "section"
                | "h2"
                | "h3"
                | "h4"
        ) {
            return false;
        }
        if has_media(inner) || has_content_image(inner) {
            return false;
        }
        let txt = leaf_text(inner);
        if txt.is_empty() || char_len(&txt) > BOILERPLATE_MAX_LEN {
            return false;
        }
        BOILERPLATE_TEXT_RE.is_match(&txt)
            || (char_len(&txt) <= 70 && WIDGET_HEADER_RE.is_match(&txt))
    });

    // Trafilatura-guided prune (Python's three passes), only when the
    // ground-truth text is substantial enough to trust.
    if !keep_text.is_empty() && char_len(&keep_text) >= 400 {
        // Prose blocks whose text the extractor excluded.
        frag = drop_elements(&frag, &|_, inner, e| {
            if !matches!(e.name.as_str(), "p" | "li" | "blockquote" | "dd") {
                return false;
            }
            if has_media(inner) || has_any_tag(inner, &["img"]) {
                return false;
            }
            let probe = norm(&leaf_text(inner));
            if char_len(&probe) < 25 {
                return false;
            }
            !keep_text.contains(&probe_head(&probe, 60))
        });
        // Pure link clusters (breadcrumbs, tag rows, related lists).
        frag = drop_elements(&frag, &|_, inner, e| {
            if !matches!(
                e.name.as_str(),
                "nav" | "div" | "ul" | "ol" | "section" | "header" | "p"
            ) {
                return false;
            }
            if has_media(inner) || has_content_image(inner) {
                return false;
            }
            let anchors: Vec<Elem> = all_elements(inner)
                .into_iter()
                .filter(|a| a.name == "a")
                .collect();
            if anchors.len() < 2 {
                return false;
            }
            let full = norm(&leaf_text(inner));
            if full.is_empty() || keep_text.contains(&probe_head(&full, 60)) {
                return false;
            }
            let link_txt = norm(
                &anchors
                    .iter()
                    .map(|a| leaf_text(a.inner(inner)))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            char_len(&full) as i64 - char_len(&link_txt) as i64 <= 15
        });
        // Short non-prose leaf blocks the extractor excluded.
        frag = drop_elements(&frag, &|_, inner, e| {
            if !matches!(
                e.name.as_str(),
                "p" | "div" | "span" | "figcaption" | "small" | "li" | "h4" | "h5" | "h6"
            ) {
                return false;
            }
            if has_media(inner) || has_content_image(inner) || has_prose_child(inner) {
                return false;
            }
            let probe = norm(&leaf_text(inner));
            let n = char_len(&probe);
            if !(3..=120).contains(&n) {
                return false;
            }
            !keep_text.contains(&probe_head(&probe, 60))
        });
    }

    // Separator/bullet-only leaves.
    frag = drop_elements(&frag, &|_, inner, e| {
        if !matches!(e.name.as_str(), "p" | "div" | "span" | "li") {
            return false;
        }
        if has_any_tag(inner, &["iframe", "video", "audio", "img", "a"]) {
            return false;
        }
        let raw = leaf_text(inner);
        !raw.is_empty() && norm(&raw).is_empty()
    });

    // Byline / timestamp leaf lines, redundant with the reader header.
    frag = drop_elements(&frag, &|_, inner, e| {
        if !matches!(
            e.name.as_str(),
            "p" | "div" | "span" | "time" | "small" | "li"
        ) {
            return false;
        }
        if has_media(inner) || has_content_image(inner) || has_prose_child(inner) {
            return false;
        }
        let raw = leaf_text(inner);
        !raw.is_empty() && char_len(&raw) <= 120 && BYLINE_RE.is_match(&raw)
    });

    // Media src normalization (data-src fallbacks + player-URL
    // normalization), before ammonia drops the data-* attributes.
    frag = normalize_media_srcs(&frag, url);

    // Sanitize (tags, attributes, URL schemes, relative-URL rewrite).
    let mut body = sanitize(&frag, url);

    // Blank-line control: collapse <br> runs, drop edge <br>s.
    body = BR_RUN_RE.replace_all(&body, "<br>").to_string();
    body = BR_AFTER_OPEN_RE.replace_all(&body, "$1").to_string();
    body = BR_BEFORE_CLOSE_RE.replace_all(&body, "$1").to_string();

    // Drop empty media-less containers left behind by chrome removal.
    for _ in 0..4 {
        let next = drop_elements(&body, &|_, inner, e| {
            if !matches!(
                e.name.as_str(),
                "p" | "div" | "span" | "section" | "figure" | "li" | "ul" | "ol" | "dl"
            ) {
                return false;
            }
            if has_any_tag(inner, &["img", "iframe", "video", "audio", "a", "source"]) {
                return false;
            }
            leaf_text(inner).is_empty()
        });
        if next == body {
            break;
        }
        body = next;
    }

    body = strip_zero_width(body.trim());

    // Paywall / mis-picked node: the sanitized body has far less text
    // than the extractor found — rebuild from the extractor's text and
    // append any media embeds salvaged from the body.
    if !keep_text.is_empty() && char_len(&keep_text) >= 1200 {
        let body_len = char_len(&norm(&leaf_text(&body)));
        if (body_len as f64) < 0.4 * char_len(&keep_text) as f64 {
            let rebuilt = traf_body_html(html);
            if !rebuilt.is_empty() {
                let embeds = harvest_embeds(&body);
                body = format!("{rebuilt}{embeds}");
            }
        }
    }

    if leaf_text(&body).is_empty() && !body.contains("<iframe") {
        return String::new();
    }
    body
}

/// The sanitized HTML of the media embeds inside a fragment, in order.
fn harvest_embeds(html: &str) -> String {
    all_elements(html)
        .iter()
        .filter(|e| matches!(e.name.as_str(), "iframe" | "video" | "audio"))
        .map(|e| &html[e.start..e.end])
        .collect::<Vec<_>>()
        .join("")
}

/// Build an HTML body from trafilatura's extracted text (paragraphs) —
/// Python `_traf_body_html`.
fn traf_body_html(html: &str) -> String {
    let (_, text) = extract_focus(html, rust_trafilatura::ExtractionFocus::FavorRecall);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    text.split('\n')
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| format!("<p>{}</p>", esc_text(b)))
        .collect()
}

/// Rewrite media `src` attributes in place: fall back to data-src /
/// data-lazy-src, absolutize, and normalize player URLs (Python's
/// attribute pass, media half — ammonia does the attr allowlist and
/// the remaining URL rewriting).
fn normalize_media_srcs(html: &str, base_url: &str) -> String {
    let toks = tag_tokens(html);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for tok in &toks {
        if tok.close
            || !matches!(
                tok.name.as_str(),
                "iframe" | "video" | "audio" | "source" | "img"
            )
        {
            continue;
        }
        let open = &html[tok.start..tok.end];
        let mut src = attr_value(open, "src");
        if src.as_deref().unwrap_or("").is_empty() {
            src = attr_value(open, "data-src");
        }
        if src.as_deref().unwrap_or("").is_empty() && tok.name == "img" {
            src = attr_value(open, "data-lazy-src");
        }
        let Some(src) = src.filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let new_src = normalize_media_url(&absolutize(&src, base_url));
        let new_open = set_attr(open, "src", &new_src);
        if new_open != open {
            edits.push((tok.start, tok.end, new_open));
        }
    }
    apply_edits(html, edits)
}

/// Set attribute `name` to `value` in an open tag's source (replacing
/// the existing value, or inserting before the closing bracket).
fn set_attr(open_tag: &str, name: &str, value: &str) -> String {
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
            let v_start = after_eq + (open_tag[after_eq..].len() - v.len());
            if let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let val_len = v[1..].find(q).unwrap_or_else(|| v.len().saturating_sub(2));
                let val_span = (v_start + 1, v_start + 1 + val_len);
                return format!(
                    "{}{}{}",
                    &open_tag[..val_span.0],
                    esc_attr(value),
                    &open_tag[val_span.1..]
                );
            }
            let end = v
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(v.len());
            return format!(
                "{}{}{}",
                &open_tag[..v_start],
                esc_attr(value),
                &open_tag[v_start + end..]
            );
        }
        idx = start + name.len();
    }
    // Insert before the closing '>' (keeping a trailing '/' intact).
    let trimmed = open_tag.trim_end();
    if let Some(stripped) = trimmed.strip_suffix("/>") {
        format!("{stripped} {name}=\"{}\"/>", esc_attr(value))
    } else if let Some(stripped) = trimmed.strip_suffix('>') {
        format!("{stripped} {name}=\"{}\">", esc_attr(value))
    } else {
        open_tag.to_string()
    }
}

/// The ammonia pass: Python's tag/attribute allowlists, drop-with-
/// contents tags, http/https/mailto/tel URL schemes, relative URLs
/// rewritten against the article URL.
fn sanitize(fragment: &str, base_url: &str) -> String {
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(ALLOWED_TAGS.iter().copied().collect::<HashSet<_>>())
        .clean_content_tags(DROP_TAGS.iter().copied().collect::<HashSet<_>>())
        .tag_attributes(attr_keep())
        .generic_attributes(HashSet::new())
        .url_schemes(["http", "https", "mailto", "tel"].into_iter().collect())
        .link_rel(None)
        .strip_comments(true);
    match url::Url::parse(base_url) {
        Ok(base) => {
            builder.url_relative(ammonia::UrlRelative::RewriteWithBase(base));
        }
        Err(_) => {
            builder.url_relative(ammonia::UrlRelative::PassThrough);
        }
    }
    builder.clean(fragment).to_string()
}

// ---------------------------------------------------------------------------
// Full render (Python render_full_article_html, standard path)
// ---------------------------------------------------------------------------

fn feed_fallback_html(fallback_html: &str, url: &str) -> String {
    let cleaned = clean_article_html(fallback_html, url, false);
    if !cleaned.is_empty() {
        return cleaned;
    }
    let text = html_to_text(fallback_html);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    text.split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| format!("<p>{}</p>", esc_text(b)))
        .collect()
}

/// Fetch `url` and return a full rich-reader HTML fragment (header +
/// body), falling back to cleaned feed content when the page can't be
/// fetched or yields no usable body. Returns None only when there is
/// nothing at all to show. The returned `<article>` carries a `lang`
/// attribute resolved by issue #72's priority order.
///
/// Not ported from Python's render (PARITY residuals): the YouTube
/// structured-text special case, the ID3 show-notes path, the Google
/// News proxy path, the metered-preview notice, and multi-page
/// following (the Rust text cascade is single-page too).
#[allow(clippy::too_many_arguments)]
pub fn render_rich_article(
    url: &str,
    fallback_html: &str,
    fallback_title: &str,
    fallback_author: &str,
    date: &str,
    cookie_header: &str,
    feed_lang: &str,
    feed_item_lang: &str,
    ui_lang: &str,
) -> Option<String> {
    let url = url.trim();
    let mut title = fallback_title.to_string();
    let author = fallback_author.to_string();
    let mut body = String::new();
    let mut page_lang: Option<String> = None;

    if !url.is_empty()
        && let Ok((bytes, content_type)) = fetch_page_bytes(url, cookie_header)
    {
        let page_html = crate::extract::decode_bytes(&bytes, &content_type, "");
        page_lang = crate::article_lang::lang_from_page_html(&page_html);
        if title.is_empty() {
            let (t, _) = extract_focus(
                &page_html,
                rust_trafilatura::ExtractionFocus::FavorPrecision,
            );
            title = t;
        }
        body = clean_article_html(&page_html, url, true);
    }

    // Some publishers serve a truncated page but syndicate the complete
    // story in the feed: prefer whichever body is fuller, never
    // downgrading below the feed content. Never for a GitHub page.
    if !fallback_html.is_empty() && !is_github_page_url(url) {
        let feed_body = clean_article_html(fallback_html, url, false);
        if !feed_body.is_empty() {
            let body_len = char_len(&norm(&leaf_text(&body)));
            let feed_len = char_len(&norm(&leaf_text(&feed_body)));
            if (feed_len as f64) > body_len as f64 * 1.25 {
                let embeds = harvest_embeds(&body);
                body = format!("{feed_body}{embeds}");
            }
        }
    }

    if body.is_empty() {
        body = feed_fallback_html(fallback_html, url);
    }
    assemble_rich(
        &body,
        url,
        &title,
        &author,
        date,
        page_lang,
        feed_lang,
        feed_item_lang,
        ui_lang,
    )
}

/// The rich fragment for an article shown from its stored feed body
/// alone (no fetch): the reader's immediate rendering on selection,
/// before the full rich article arrives. None when the feed body
/// yields nothing renderable — the caller falls back to plain text.
#[allow(clippy::too_many_arguments)]
pub fn rich_article_from_feed(
    url: &str,
    fallback_html: &str,
    fallback_title: &str,
    fallback_author: &str,
    date: &str,
    feed_lang: &str,
    feed_item_lang: &str,
    ui_lang: &str,
) -> Option<String> {
    let body = feed_fallback_html(fallback_html, url.trim());
    assemble_rich(
        &body,
        url.trim(),
        fallback_title,
        fallback_author,
        date,
        None,
        feed_lang,
        feed_item_lang,
        ui_lang,
    )
}

/// Header + language wrapper shared by the full and feed-only renders
/// (Python's render tail).
#[allow(clippy::too_many_arguments)]
fn assemble_rich(
    body: &str,
    url: &str,
    title: &str,
    author: &str,
    date: &str,
    page_lang: Option<String>,
    feed_lang: &str,
    feed_item_lang: &str,
    ui_lang: &str,
) -> Option<String> {
    if body.is_empty() {
        return None;
    }

    let mut header = format!(
        "<h1>{}</h1>",
        esc_text(if title.trim().is_empty() {
            url
        } else {
            title.trim()
        })
    );
    let meta_line = [date.trim(), author.trim()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if !meta_line.is_empty() {
        header.push_str(&format!(
            "<p class=\"awv-meta\">{}</p>",
            esc_text(&meta_line)
        ));
    }
    if !url.is_empty() {
        header.push_str(&format!(
            "<p class=\"awv-source\"><a href=\"{}\">{}</a></p>",
            esc_attr(url),
            esc_text(url)
        ));
    }
    let lang = resolve_content_language(
        &ContentLangSources {
            page_lang,
            feed_item_lang: Some(feed_item_lang.to_string()).filter(|s| !s.is_empty()),
            feed_lang: Some(feed_lang.to_string()).filter(|s| !s.is_empty()),
            ..Default::default()
        },
        ui_lang,
    );
    Some(format!(
        "<article lang=\"{}\">{header}<hr>{body}</article>",
        esc_attr(&lang)
    ))
}
