//! Post-extraction boilerplate stripping, ported from Python
//! `core/article_extractor.py`'s postprocess stage
//! (`_postprocess_extracted_text` and the strip functions it
//! dispatches to). Python applies this to the final extracted text
//! on every path (download, feed HTML, render); the Rust cascade
//! applies it in `extract_article`'s public wrapper so every
//! outcome passes through it once, plus the two proxy-only strips
//! on the Jina path.
//!
//! Patterns are Python's, translated to the `regex` crate (no
//! look-around is used by any of them). Host dispatch mirrors
//! Python's netloc checks exactly, including their substring /
//! suffix looseness.

use regex::Regex;
use std::sync::LazyLock;

/// Python `_normalize_whitespace`: drop zero-width characters,
/// unify newlines, trim trailing spaces per line, cap blank-line
/// runs at one, strip the ends. (Deliberately NOT the same as
/// extract.rs's `normalize_text`, which also collapses spaces
/// inside lines.)
pub fn normalize_whitespace(text: &str) -> String {
    let text = text.replace(['\u{200B}', '\u{2060}', '\u{FEFF}'], "");
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    static TRAILING_WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+\n").unwrap());
    let text = TRAILING_WS.replace_all(&text, "\n");
    static BLANK_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());
    let text = BLANK_RUN.replace_all(&text, "\n\n");
    text.trim().to_string()
}

/// Python `_split_paragraphs`: split by blank lines first, then by
/// single newlines, keeping non-empty trimmed lines as paragraphs.
pub(crate) fn split_paragraphs(text: &str) -> Vec<String> {
    let t = normalize_whitespace(text);
    if t.is_empty() {
        return Vec::new();
    }
    static BLANK_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());
    let mut out = Vec::new();
    for block in BLANK_LINE.split(&t) {
        for p in block.split('\n') {
            let p = p.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// Lowercased URL authority (`host[:port]`, userinfo dropped) —
/// Python's `urlsplit(url).netloc.lower()`.
fn netloc_of(url: &str) -> String {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    match authority.rfind('@') {
        Some(i) => authority[i + 1..].to_string(),
        None => authority,
    }
}

/// Hostname only (no port, no userinfo), lowercased.
fn hostname_of(url: &str) -> String {
    let authority = netloc_of(url);
    match authority.rfind(':') {
        // Keep IPv6 literals (contain brackets) whole.
        Some(i) if !authority.contains('[') => authority[..i].to_string(),
        _ => authority,
    }
}

/// Python `_host_matches`: hostname equals the domain or is a
/// subdomain of it.
pub fn host_matches(url: &str, domain: &str) -> bool {
    let host = hostname_of(url);
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// Path of a URL (no query/fragment), for the GitHub page check.
fn path_of(url: &str) -> String {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => return String::new(),
    };
    match after_scheme.find('/') {
        Some(i) => {
            let rest = &after_scheme[i..];
            rest.split(['?', '#']).next().unwrap_or("").to_string()
        }
        None => String::new(),
    }
}

const GITHUB_RESERVED_OWNERS: &[&str] = &[
    "about",
    "apps",
    "blog",
    "collections",
    "contact",
    "customer-stories",
    "enterprise",
    "events",
    "explore",
    "features",
    "issues",
    "login",
    "marketplace",
    "notifications",
    "orgs",
    "pricing",
    "pulls",
    "search",
    "security",
    "settings",
    "site",
    "sponsors",
    "stars",
    "topics",
    "trending",
    "users",
    "watching",
];

/// Python `core/github_source.py::is_github_page_url`: whether the
/// URL is a GitHub page the source-feed module rebuilds from the
/// API (pull / issue / commit / release / discussion / org
/// discussion). Such pages' bodies ARE code, so the leaked-script
/// strip must never run on them.
pub fn is_github_page_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    let host = hostname_of(url);
    if host != "github.com" && host != "www.github.com" {
        return false;
    }
    let path = path_of(url);
    static ORG_DISCUSSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^/orgs/([A-Za-z0-9][A-Za-z0-9._-]{0,99})/discussions/(\d+)/?$").unwrap()
    });
    if ORG_DISCUSSION.is_match(&path) {
        return true;
    }
    static OWNER_REPO: &str =
        r"([A-Za-z0-9](?:[A-Za-z0-9._-]{0,98}[A-Za-z0-9])?)/([A-Za-z0-9._-]{1,100})";
    for pat in [
        format!(
            r"^/{OWNER_REPO}/pull/(\d+)(?:/(?:files|commits|checks|conversation)(?:/[^/]*)?)?/?$"
        ),
        format!(r"^/{OWNER_REPO}/issues/(\d+)/?$"),
        format!(r"^/{OWNER_REPO}/commit/([0-9a-fA-F]{{7,40}})(?:\.patch|\.diff)?/?$"),
        format!(r"^/{OWNER_REPO}/releases/tag/([^/]+)/?$"),
        format!(r"^/{OWNER_REPO}/discussions/(\d+)/?$"),
    ] {
        let re = Regex::new(&pat).unwrap();
        if let Some(caps) = re.captures(&path) {
            let owner = caps.get(1).map(|m| m.as_str().to_lowercase());
            if let Some(owner) = owner
                && GITHUB_RESERVED_OWNERS.contains(&owner.as_str())
            {
                return false;
            }
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------
// Host strips
// ---------------------------------------------------------------------

static ZDNET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^\s*ZDNET\s+Recommends\b",
        r"^\s*What\s+exactly\s+does\s+it\s+mean\?\s*$",
        r"\bZDNET's\s+recommendations\s+are\s+based\s+on\b",
        r"\bhours\s+of\s+testing\b",
        r"\bcomparison\s+shopping\b",
        r"\bvendor\s+and\s+retailer\s+listings\b",
        r"\baffiliate\s+commissions\b",
        r"\bdoes\s+not\s+affect\s+the\s+price\s+you\s+pay\b",
        r"\bstrict\s+guidelines\b",
        r"\beditorial\s+content\b.*\badvertisers\b",
        r"\bOur\s+goal\s+is\s+to\s+deliver\b",
        r"\bfact-?check\b",
        r"\breport\s+the\s+mistake\b",
        r"^\s*Follow\s+ZDNET\b",
        r"\bAdd\s+us\s+as\s+a\s+preferred\s+source\s+on\s+Google\b",
        r"\bpreferred\s+source\s+on\s+Google\b",
        r"\bFollow\s+ZDNET\b",
    ]
    .iter()
    .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
    .collect()
});

/// Python `_strip_zdnet_recommends_block`: drop boilerplate
/// paragraphs from the top of ZDNet text (first 25 paragraphs).
pub fn strip_zdnet_recommends_block(text: &str) -> String {
    let paras = split_paragraphs(text);
    if paras.is_empty() {
        return String::new();
    }
    static ZDNET_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bZDNET\b").unwrap());
    static ZDNET_CHUNK: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        [r"\brecommend", r"\bpreferred\s+source\b", r"\bfollow\b"]
            .iter()
            .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
            .collect()
    });
    let max_scan = paras.len().min(25);
    let mut i = 0;
    while i < max_scan {
        let p = paras[i].trim();
        if p.is_empty() {
            i += 1;
            continue;
        }
        if ZDNET_PATTERNS.iter().any(|rx| rx.is_match(p)) {
            i += 1;
            continue;
        }
        // A few pages split disclosure headings into tiny chunks.
        if i < 10 && ZDNET_WORD.is_match(p) && ZDNET_CHUNK.iter().any(|rx| rx.is_match(p)) {
            i += 1;
            continue;
        }
        break;
    }
    paras[i..].join("\n\n").trim().to_string()
}

/// Python `_strip_thetyee_boilerplate`.
pub fn strip_thetyee_boilerplate(text: &str) -> String {
    let t = text.trim();
    static TOP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?si)Our\s+[Jj]ournalism\s+is\s+supported\s+by\s+(?:readers|Tyee\s+Builders)\s+like\s+you.*?\nSupport\s+Us\s+Now\s*",
        )
        .unwrap()
    });
    let t = TOP.replacen(t, 1, "");
    static BOTTOM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?si)Subscribe\s+now\s+Privacy\s+policy.*?Subscribe\s+now\s+Privacy\s+policy\s*",
        )
        .unwrap()
    });
    let t = BOTTOM.replace_all(&t, "");
    t.trim().to_string()
}

/// Python `_strip_9to5mac_boilerplate`.
pub fn strip_9to5mac_boilerplate(text: &str) -> String {
    static FTC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)FTC:\s*We\s+use\s+income\s+earning\s+auto\s+affiliate\s+links\..*?More\.")
            .unwrap()
    });
    let t = FTC.replace_all(text, "");
    static READING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)You(?:’|')re\s+reading\s+9to5Mac\s*(?:—|-).*?(?:loop\.|channel)").unwrap()
    });
    let t = READING.replace_all(&t, "");
    static EXCLUSIVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)Check\s+out\s+our\s+exclusive\s+stories,.*?(?:channel|loop\.)").unwrap()
    });
    let t = EXCLUSIVE.replace_all(&t, "");
    static WORTH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)\nWorth\s+checking\s+out\s+on\s+Amazon\b.*$").unwrap());
    let t = WORTH.replace_all(&t, "");
    static FOLLOW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)\nFollow\s+[A-Z][^:\n]{0,60}:\s*(?:Threads|Bluesky|Instagram|Mastodon)\b.*$",
        )
        .unwrap()
    });
    let t = FOLLOW.replace_all(&t, "");
    t.into_owned()
}

/// Python `_strip_9to5google_boilerplate`.
pub fn strip_9to5google_boilerplate(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)\nJoin\s+9to5Google\s+Pro\s+to\s+get\s+more\b.*$").unwrap()
    });
    RE.replace_all(text, "").into_owned()
}

/// Python `_strip_9to5toys_boilerplate`.
pub fn strip_9to5toys_boilerplate(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)\nYou(?:’|')re\s+reading\s+9to5Toys\s*(?:—|-).*$").unwrap()
    });
    RE.replace_all(text, "").into_owned()
}

/// Python `_strip_postmillennial_boilerplate`.
pub fn strip_postmillennial_boilerplate(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)\nJoin\s+and\s+support\s+independent\s+free\s+thinkers!.*$").unwrap()
    });
    RE.replace_all(text, "").into_owned()
}

/// Python `_strip_globalnews_boilerplate`.
pub fn strip_globalnews_boilerplate(text: &str) -> String {
    static BYLINE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?im)^By\s+Staff\s+The\s+Canadian\s+Press").unwrap());
    let t = BYLINE.replace_all(text, "");
    static POSTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?im)^Posted\s+\w+\s+\d+,\s+\d+\s+\d+:\d+\s+[ap]m").unwrap());
    let t = POSTED.replace_all(&t, "");
    static MINREAD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?im)^\d+\s+min\s+read").unwrap());
    let t = MINREAD.replace_all(&t, "");
    static SOCIAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)If\s+you\s+get\s+Global\s+News\s+from\s+Instagram\s+or\s+Facebook.*?(?:connect\s+with\s+us\.)",
        )
        .unwrap()
    });
    let t = SOCIAL.replace_all(&t, "");
    static FONTBAR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)Hide\s+message\s+barDescrease\s+article\s+font\s+size\s*Increase\s+article\s+font\s+size")
            .unwrap()
    });
    let t = FONTBAR.replace_all(&t, "");
    t.into_owned()
}

static AJ_RELATED_LIST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)^(?:Recommended\s+Stories\s*\n)?list\s+of\s+\d+\s+items-\s*list\s+\d+\s+of\s+\d+[^\n]*(?:\n-\s*list\s+\d+\s+of\s+\d+[^\n]*)*\n?",
    )
    .unwrap()
});

/// Python `_strip_aljazeera_boilerplate`.
pub fn strip_aljazeera_boilerplate(text: &str) -> String {
    static PUBLISHED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)Published\s+On\s+\d+\s+\w+\s+\d+.*?(?:20\d\d)").unwrap());
    let t = PUBLISHED.replace_all(text, "");
    static SHARE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)Click\s+here\s+to\s+share\s+on\s+social\s+media").unwrap()
    });
    let t = SHARE.replace_all(&t, "");
    static SAVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)share\d+Save").unwrap());
    let t = SAVE.replace_all(&t, "");
    let t = AJ_RELATED_LIST_RE.replace_all(&t, "\n");
    t.into_owned()
}

static AXIOS_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^Add\s+Axios\s+as\s+(?:a|your)\s+preferred\s+source\b.*\bGoogle\.?$",
        r"^(?:\d+\s+(?:mins?|minutes?|hours?|days?)|moments?)\s+ago\s*[-–—]\s*\S",
        r"^Go\s+deeper(?:\s*\(\s*\d+\s+min(?:\.|utes)?\s+read\s*\))?$",
    ]
    .iter()
    .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
    .collect()
});

/// Python `_strip_axios_boilerplate`.
pub fn strip_axios_boilerplate(text: &str) -> String {
    let paras = split_paragraphs(text);
    if paras.is_empty() {
        return String::new();
    }
    let kept: Vec<&str> = paras
        .iter()
        .map(|p| p.as_str())
        .filter(|p| !AXIOS_PATTERNS.iter().any(|rx| rx.is_match(p)))
        .collect();
    kept.join("\n\n").trim().to_string()
}

/// Python `_published_date_label`: '30 October 2026' from a page's
/// JSON-LD `datePublished` or `article:published_time` meta tag.
pub fn published_date_label(html_text: &str) -> String {
    if html_text.is_empty() {
        return String::new();
    }
    static JSONLD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""datePublished"\s*:\s*"([^"]+)""#).unwrap());
    static META: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"<meta[^>]+(?:property|name)=["']article:published_time["'][^>]+content=["']([^"']+)"#,
        )
        .unwrap()
    });
    let raw = JSONLD
        .captures(html_text)
        .and_then(|c| c.get(1))
        .or_else(|| META.captures(html_text).and_then(|c| c.get(1)))
        .map(|m| m.as_str().to_string());
    let Some(raw) = raw else {
        return String::new();
    };
    // datetime.fromisoformat(raw.replace("Z", "+00:00")) then
    // f"{day} {Month} {year}". Parse the leading date portion.
    static DATE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\d{4})-(\d{1,2})-(\d{1,2})").unwrap());
    let Some(caps) = DATE.captures(raw.trim()) else {
        return String::new();
    };
    let year: i64 = caps[1].parse().unwrap_or(0);
    let month: usize = caps[2].parse().unwrap_or(0);
    let day: i64 = caps[3].parse().unwrap_or(0);
    const MONTHS: [&str; 12] = [
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
    if !(1..=12).contains(&month) || day == 0 {
        return String::new();
    }
    format!("{day} {} {year}", MONTHS[month - 1])
}

static BBC_LEAD_PUBLISHED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\s*-\s*Published\s*\n").unwrap());

/// Python `_strip_bbc_boilerplate`.
pub fn strip_bbc_boilerplate(text: &str, published: &str) -> String {
    static SHARESAVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)ShareSave").unwrap());
    let t = SHARESAVE.replace_all(text, "");
    // Complete the dangling byline with the publish date when the
    // page carried one; drop the line when it did not.
    if BBC_LEAD_PUBLISHED_RE.is_match(&t) {
        let lead = if published.is_empty() {
            String::new()
        } else {
            format!("- Published {published}\n")
        };
        let t2 = BBC_LEAD_PUBLISHED_RE.replacen(&t, 1, |_: &regex::Captures| lead.clone());
        return t2.into_owned();
    }
    t.into_owned()
}

/// Python `_strip_wsj_boilerplate`.
pub fn strip_wsj_boilerplate(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)\s*Copyright\s*©?\s*\d{4}\s*Dow\s+Jones\s*&\s*Company.*?All\s+Rights\s+Reserved\.\s*[0-9a-f]{16,}\s*$",
        )
        .unwrap()
    });
    RE.replace_all(text, "").trim().to_string()
}

static TECHRADAR_GATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^You\s+must\s+confirm\s+your\s+public\s+display\s+name\s+before\s+commenting\b",
    )
    .unwrap()
});
static TECHRADAR_FOLLOW_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^Follow\s+TechRadar\b.*\b(?:Google\s+News|preferred\s+source)\b").unwrap()
});
static TECHRADAR_SIGNUP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^Sign\s+up\s+for\s+breaking\s+news\b.*\btop\s+tech\s+deals\b").unwrap()
});

/// Python `_strip_techradar_boilerplate`.
pub fn strip_techradar_boilerplate(text: &str) -> String {
    let paras = split_paragraphs(text);
    if paras.is_empty() {
        return String::new();
    }
    let is_promo = |p: &str| TECHRADAR_FOLLOW_RE.is_match(p) || TECHRADAR_SIGNUP_RE.is_match(p);
    let mut cut = paras.len();
    let mut gated = false;
    for (i, p) in paras.iter().enumerate() {
        if TECHRADAR_GATE_RE.is_match(p) {
            cut = i;
            // The author bio sits directly above the comment gate.
            if cut >= 1 {
                cut -= 1;
            }
            // A promo / sign-up line sits directly above the bio.
            if cut >= 1 && is_promo(&paras[cut - 1]) {
                cut -= 1;
            }
            gated = true;
            break;
        }
    }
    if !gated {
        // No comment gate: fall back to the always-trailing
        // "Follow ... Google News" promo.
        for (i, p) in paras.iter().enumerate() {
            if TECHRADAR_FOLLOW_RE.is_match(p) {
                cut = i;
                break;
            }
        }
    }
    let kept: Vec<&str> = paras[..cut]
        .iter()
        .map(|p| p.as_str())
        .filter(|p| !is_promo(p))
        .collect();
    kept.join("\n\n").trim().to_string()
}

/// Python `_strip_canada_boilerplate`.
pub fn strip_canada_boilerplate(text: &str) -> String {
    static AD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)Advertisement\s+\d+").unwrap());
    let t = AD.replace_all(text, "");
    static NOTLOADED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?is)This\s+advertisement\s+has\s+not\s+loaded\s+yet.*?(?:continues\s+below\.)",
        )
        .unwrap()
    });
    let t = NOTLOADED.replace_all(&t, "");
    static AUTHOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)Author\s+of\s+the\s+article:.*?(?:read)").unwrap());
    let t = AUTHOR.replace_all(&t, "");
    static JOIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)Join\s+the\s+conversation").unwrap());
    let t = JOIN.replace_all(&t, "");
    static READMORE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)Read\s+More.*?(?:Article\s+content)").unwrap());
    let t = READMORE.replace_all(&t, "");
    static SHARE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)Share\s+this\s+article\s+in\s+your\s+social\s+network").unwrap()
    });
    let t = SHARE.replace_all(&t, "");
    static TRENDING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)Trending\s+Latest\s+National\s+Stories").unwrap());
    let t = TRENDING.replace_all(&t, "");
    t.into_owned()
}

/// Python `_strip_slashdot_boilerplate`.
pub fn strip_slashdot_boilerplate(text: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?is)\n*#*\s*YOUR\s+PRIVACY\s+CHOICES\s*\(DO\s+NOT\s+SELL/SHARE/TARGET\).*$")
            .unwrap()
    });
    RE.replace_all(text, "").trim().to_string()
}

/// Python `_strip_castanet_boilerplate`.
pub fn strip_castanet_boilerplate(text: &str) -> String {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)-\s+.*?\s+-\s+\d+:\d+\s+[ap]m").unwrap());
    RE.replace_all(text, "").into_owned()
}

fn bloomberg_is_author_line(p: &str) -> bool {
    static MD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^By\s+\[[^\]]+\]\(").unwrap());
    if MD.is_match(p) {
        return true;
    }
    static PLAIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^By\s+[A-Z][A-Za-z .,'’\-]{1,100}$").unwrap());
    PLAIN.is_match(p)
}

fn bloomberg_is_meta_or_control(p: &str) -> bool {
    let p = p.trim();
    if p.is_empty() {
        return true;
    }
    static CONTROLS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(Gift this article|Add us on Google|Save|Translate|Listen)$").unwrap()
    });
    if CONTROLS.is_match(p) {
        return true;
    }
    static STAMPS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(Updated on|Published on)$").unwrap());
    if STAMPS.is_match(p) {
        return true;
    }
    static DATETIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^[A-Z][a-z]+\s+\d{1,2},\s+\d{4}\s+at\s+\d{1,2}:\d{2}\s+[AP]M\s+UTC$")
            .unwrap()
    });
    if DATETIME.is_match(p) {
        return true;
    }
    if p.starts_with("[Contact us: Provide news feedback or report an error](")
        || p.starts_with("[Confidential tip? Send a tip to our reporters](")
        || p.starts_with("[Site feedback: Take our Survey](")
        || p.starts_with("### **Takeaways** by Bloomberg AI")
        || p.starts_with("### Takeaways by Bloomberg AI")
        || p.starts_with("Takeaways by Bloomberg AI")
    {
        return true;
    }
    false
}

fn bloomberg_is_end_marker(p: &str) -> bool {
    let p = p.trim();
    if p.is_empty() {
        return false;
    }
    p.starts_with("[Before it's here, it's on the Bloomberg Terminal")
        || p.starts_with("### More From Bloomberg")
        || p.starts_with("### Top Reads")
        || p.starts_with("[Home](https://www.bloomberg.com/)[BTV+]")
        || p.starts_with("[Terms of Service](")
        || p.contains("Bloomberg L.P. All Rights Reserved")
        || p.starts_with("Explore live news and interviews")
        || p.starts_with("Get unlimited access for just")
        || p.starts_with("By accepting, you agree to our updated [Terms of Service")
}

/// Python `_strip_bloomberg_boilerplate`.
pub fn strip_bloomberg_boilerplate(text: &str) -> String {
    let original = normalize_whitespace(text);
    let paras = split_paragraphs(&original);
    if paras.is_empty() {
        return String::new();
    }
    let is_header_line = |p: &str| bloomberg_is_author_line(p) || bloomberg_is_meta_or_control(p);

    let mut start = 0usize;
    let header_anchor_idx = paras
        .iter()
        .position(|p| bloomberg_is_author_line(p))
        .or_else(|| paras.iter().position(|p| bloomberg_is_meta_or_control(p)));
    if let Some(anchor) = header_anchor_idx {
        let mut seq_start = anchor;
        while seq_start > 0 && is_header_line(&paras[seq_start - 1]) {
            seq_start -= 1;
        }
        let mut i = seq_start;
        while i < paras.len() && is_header_line(&paras[i]) {
            i += 1;
        }
        if i < paras.len() {
            start = i;
        }
    }

    let mut end = paras.len();
    for (i, p) in paras.iter().enumerate().skip(start) {
        if bloomberg_is_end_marker(p) {
            end = i;
            break;
        }
    }

    let cleaned = paras[start..end].join("\n\n").trim().to_string();
    if cleaned.is_empty() {
        original
    } else {
        cleaned
    }
}

static NING_ACTION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"^posted\s+a\s+\w[\w -]*$",
        r"^posted\s+blog\s+posts?$",
        r"^updated\s+their$",
        r"^replied(?:\s+to)?$",
        r"^commented(?:\s+on)?$",
        r"^liked$",
        r"^shared$",
    ]
    .iter()
    .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
    .collect()
});

/// Python `_strip_ning_activity_noise`.
pub fn strip_ning_activity_noise(text: &str) -> String {
    let original = normalize_whitespace(text);
    let paras = split_paragraphs(&original);
    if paras.is_empty() {
        return String::new();
    }
    static SEE_MORE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^see\s+more$").unwrap());
    static N_MORE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^\d+\s+more(?:\.{3}|…)?$").unwrap());
    let is_more_link_line = |p: &str| {
        let s = p.trim();
        s.is_empty() || SEE_MORE.is_match(s) || N_MORE.is_match(s)
    };
    let is_action_line = |p: &str| NING_ACTION_PATTERNS.iter().any(|rx| rx.is_match(p.trim()));
    static NAMEISH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\w .,'’()&+\-†]+$").unwrap());
    let looks_like_actor_name = |p: &str| {
        let s = p.trim();
        if s.is_empty() || s.chars().count() > 120 {
            return false;
        }
        let low = s.to_lowercase();
        if [
            "http://",
            "https://",
            "posted ",
            "replied",
            "commented",
            "updated ",
        ]
        .iter()
        .any(|tok| low.contains(tok))
        {
            return false;
        }
        NAMEISH.is_match(s)
    };

    let mut cleaned: Vec<String> = paras
        .iter()
        .filter(|p| !is_more_link_line(p))
        .cloned()
        .collect();

    // Remove a common Ning wrapper prefix (actor name + action
    // line) only when there is meaningful content after it.
    if cleaned.len() >= 4 && looks_like_actor_name(&cleaned[0]) && is_action_line(&cleaned[1]) {
        let tail = cleaned[2..].to_vec();
        let meaningful: Vec<&String> = tail
            .iter()
            .filter(|p| p.trim().chars().count() >= 12)
            .collect();
        if meaningful.len() >= 2 || meaningful.iter().any(|p| p.trim().chars().count() >= 40) {
            cleaned = tail;
        }
    }

    if cleaned.len() > 1 && cleaned[0].trim().to_lowercase() == "profile" {
        cleaned.remove(0);
    }

    let out = cleaned.join("\n\n").trim().to_string();
    if out.is_empty() { original } else { out }
}

// ---------------------------------------------------------------------
// Generic strips
// ---------------------------------------------------------------------

static CODE_STRONG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"document\.(?:querySelector|querySelectorAll|getElementById|getElementsBy\w+|createElement|head|body|cookie|write)\b|window\.(?:addEventListener|attachEvent|dataLayer|location\.href|onload)\b|window\[[^\]]{1,60}\]\s*=|\.addEventListener\s*\(|\bnew\s+(?:IntersectionObserver|MutationObserver|XMLHttpRequest|ResizeObserver)\s*\(|\bconsole\.(?:log|warn|error|info|debug)\s*\(|=>\s*\{|\bfunction\s*\*?\s*\(|\bJSON\.(?:parse|stringify)\s*\(|\bdataLayer\.push\s*\(|\bgoogletag\b|\bgtag\s*\(",
    )
    .unwrap()
});
static CODE_STRUCT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:let|const|var)\s+[A-Za-z_$][\w$]*\s*[=;]|^(?:function|if|for|while|switch|try|catch|else|return|throw|import|export|await)\b\s*[({;]|^new\s+[A-Za-z_$][\w$]*\s*\(|^[\[\]{}();,]+$|^[}\])][\s)\]};,]*(?:\{|\(|,\s*\{)|^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)+\s*(?:=[^=]|\()",
    )
    .unwrap()
});

/// A run must be this many code lines, with this many strong hits,
/// before it is dropped (Python `_CODE_RUN_MIN_LINES` / `_STRONG`).
const CODE_RUN_MIN_LINES: usize = 4;
const CODE_RUN_MIN_STRONG: usize = 2;

/// Python `_classify_code_line`: (is_code_like, is_strong_marker).
fn classify_code_line(line: &str) -> (bool, bool) {
    let s = line.trim();
    if s.is_empty() {
        return (false, false);
    }
    if CODE_STRONG_RE.is_match(s) {
        return (true, true);
    }
    (CODE_STRUCT_RE.is_match(s), false)
}

/// Python `_strip_embedded_script_code`: drop runs of leaked
/// <script>/<style> source from otherwise-clean article text.
pub fn strip_embedded_script_code(text: &str) -> String {
    if text.is_empty() || !CODE_STRONG_RE.is_match(text) {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let flags: Vec<(bool, bool)> = lines.iter().map(|ln| classify_code_line(ln)).collect();
    let mut keep = vec![true; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        if !flags[i].0 {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut last = i;
        let mut count = 0usize;
        let mut strong = 0usize;
        while j < lines.len() {
            if flags[j].0 {
                last = j;
                count += 1;
                if flags[j].1 {
                    strong += 1;
                }
                j += 1;
            } else if lines[j].trim().is_empty() {
                j += 1; // a blank line inside a code block does not end the run
            } else {
                break;
            }
        }
        if count >= CODE_RUN_MIN_LINES && strong >= CODE_RUN_MIN_STRONG {
            for item in keep.iter_mut().take(last + 1).skip(i) {
                *item = false;
            }
        }
        i = j.max(i + 1);
    }
    if keep.iter().all(|k| *k) {
        return text.to_string();
    }
    let out = normalize_whitespace(
        &lines
            .iter()
            .zip(keep.iter())
            .filter(|(_, k)| **k)
            .map(|(ln, _)| *ln)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    if out.is_empty() {
        text.to_string()
    } else {
        out
    }
}

static SKY_RELATED_SECTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*more\s+from\s+sky\s+news(?:\s+entertainment)?\s*[:–—-]?\s*$").unwrap()
});

/// Python `_strip_sky_related_sections`.
pub fn strip_sky_related_sections(text: &str) -> String {
    let Some(m) = SKY_RELATED_SECTION_RE.find(text) else {
        return text.to_string();
    };
    let cleaned = text[..m.start()].trim_end().to_string();
    if cleaned.is_empty() {
        text.to_string()
    } else {
        cleaned
    }
}

static RECIRCULATION_LABEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(?:you\s+may\s+also\s+like|you\s+might\s+also\s+like|(?:see|read|watch)\s+also|related(?:\s+(?:stor(?:y|ies)|articles?|posts?|reading|coverage|content))?|more\s+(?:like\s+this|from\s+\w+)|recommended(?:\s+for\s+you)?|most\s+popular|editor'?s?\s+picks?)\s*[:—-]?\s*$",
    )
    .unwrap()
});
const RECIRCULATION_LABEL_MAX_LEN: usize = 32;

/// Python `_strip_recirculation_labels`.
pub fn strip_recirculation_labels(text: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let kept: Vec<&str> = lines
        .iter()
        .filter(|ln| {
            !(ln.trim().chars().count() <= RECIRCULATION_LABEL_MAX_LEN
                && RECIRCULATION_LABEL_RE.is_match(ln))
        })
        .copied()
        .collect();
    if kept.len() == lines.len() {
        return text.to_string();
    }
    let out = normalize_whitespace(&kept.join("\n"));
    if out.is_empty() {
        text.to_string()
    } else {
        out
    }
}

static SOCIAL_SHARE_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    let networks = r"facebook|twitter|x|blue ?sky|linked ?in|reddit|whats ?app|threads|pinterest|telegram|mastodon|flipboard|tumblr|e-?mail|messenger|gab|truth\s*social|parler|line|viber|kakao";
    Regex::new(&format!(
        r"(?i)^\s*(?:share(?:\s+(?:on|to|via)\s+(?:{networks}))?|(?:{networks})|copy\s+link|link\s+copied|copied|print(?:\s+article)?|save(?:\s+article)?|gift(?:\s+article)?|comments?|tweet|listen(?:\s+to\s+(?:this\s+)?article)?)\s*$"
    ))
    .unwrap()
});
static LEADING_JUNK_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(?:advertisement(?:\s*\d+)?|this\s+advertisement\s+has\s+not\s+loaded\s+yet\.?|(?:the\s+)?story\s+continues\s+below(?:\s+advertisement)?\.?|article\s+content|open\s+this\s+photo\s+in\s+gallery:?|we\s+apologi[sz]e,?\s+but\s+this\s+video\s+has\s+failed\s+to\s+load\.?|we\s+have\s+been\s+unable\s+to\s+load[^\n]{0,60})\s*$",
    )
    .unwrap()
});

/// Python `_strip_leading_boilerplate`: drop a leading run of
/// toolbar/ad/gallery chrome lines.
pub fn strip_leading_boilerplate(text: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0usize;
    let mut removed = 0usize;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() {
            i += 1;
            continue;
        }
        let is_share = line.chars().count() <= 30 && SOCIAL_SHARE_LINE_RE.is_match(line);
        let is_junk = LEADING_JUNK_LINE_RE.is_match(line);
        if is_share || is_junk {
            i += 1;
            removed += 1;
            continue;
        }
        break;
    }
    if removed > 0 && i < lines.len() {
        return lines[i..].join("\n").trim_start_matches('\n').to_string();
    }
    text.to_string()
}

static PROXY_COOKIE_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[^\n]{0,160}\b(?:uses?|stores?|places?)\s+cookies\b.*$").unwrap()
});

/// Python `_strip_proxy_trailing_boilerplate` (proxy renderings
/// only): cut a trailing cookie-consent block.
pub fn strip_proxy_trailing_boilerplate(text: &str) -> String {
    if let Some(m) = PROXY_COOKIE_BLOCK_RE.find(text)
        && m.start() as f64 > text.len() as f64 * 0.5
    {
        return text[..m.start()].trim_end().to_string();
    }
    text.to_string()
}

static LINK_LIST_SENTENCE_END_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.!?…](\s|$)").unwrap());
/// Python `_LINK_LIST_MAX_LINE_LEN`.
const LINK_LIST_MAX_LINE_LEN: usize = 90;

/// Python `_strip_edge_nav_runs` (proxy renderings only): drop the
/// header/footer navigation runs from a whole-page rendering.
pub fn strip_edge_nav_runs(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let is_nav_like = |line: &str| {
        let line = line.trim();
        if line.is_empty() {
            return true;
        }
        if line.chars().count() > LINK_LIST_MAX_LINE_LEN {
            return false;
        }
        line.chars().count() <= 40 || !LINK_LIST_SENTENCE_END_RE.is_match(line)
    };
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && is_nav_like(lines[start]) {
        start += 1;
    }
    while end > start && is_nav_like(lines[end - 1]) {
        end -= 1;
    }
    lines[start..end].join("\n")
}

// ---------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------

/// Python `_postprocess_extracted_text`: host strip first, then
/// the generic strips, then a final whitespace normalization.
/// `published` is the page's published-date label
/// (`published_date_label`), used only by the BBC strip.
pub fn postprocess_extracted_text(text: &str, url: &str, published: &str) -> String {
    let t = normalize_whitespace(text);
    if t.is_empty() {
        return String::new();
    }
    let netloc = netloc_of(url);
    let mut t = if netloc.ends_with("zdnet.com") {
        strip_zdnet_recommends_block(&t)
    } else if netloc.ends_with("thetyee.ca") {
        strip_thetyee_boilerplate(&t)
    } else if netloc.contains("9to5mac.com") {
        strip_9to5mac_boilerplate(&t)
    } else if netloc.contains("9to5google.com") {
        strip_9to5google_boilerplate(&t)
    } else if netloc.contains("9to5toys.com") {
        strip_9to5toys_boilerplate(&t)
    } else if netloc.contains("thepostmillennial.com") {
        strip_postmillennial_boilerplate(&t)
    } else if netloc.contains("globalnews.ca") {
        strip_globalnews_boilerplate(&t)
    } else if netloc.contains("aljazeera.com") {
        strip_aljazeera_boilerplate(&t)
    } else if netloc.contains("axios.com") {
        strip_axios_boilerplate(&t)
    } else if netloc.contains("bbc.com") || netloc.contains("bbc.co.uk") {
        strip_bbc_boilerplate(&t, published)
    } else if netloc.contains("wsj.com") {
        strip_wsj_boilerplate(&t)
    } else if netloc.contains("techradar.com") {
        strip_techradar_boilerplate(&t)
    } else if netloc.contains("o.canada.com") || netloc.contains("canada.com") {
        strip_canada_boilerplate(&t)
    } else if netloc.contains("castanet.net") {
        strip_castanet_boilerplate(&t)
    } else if netloc.contains("slashdot.org") {
        strip_slashdot_boilerplate(&t)
    } else if netloc.contains("bloomberg.com") {
        strip_bloomberg_boilerplate(&t)
    } else if host_matches(url, "news.sky.com") {
        strip_sky_related_sections(&t)
    } else if netloc.ends_with(".ning.com") || netloc == "ning.com" {
        strip_ning_activity_noise(&t)
    } else {
        t
    };

    // Generic (all hosts): leading toolbar/ad/gallery chrome.
    t = strip_leading_boilerplate(&t);

    // Generic (all hosts): leaked script/style source. Never on a
    // GitHub page: its diff hunks and code blocks ARE the content.
    if !is_github_page_url(url) {
        t = strip_embedded_script_code(&t);
    }

    // Generic (all hosts): bare recirculation headings left behind
    // mid-body by the widgets they labelled.
    t = strip_recirculation_labels(&t);

    normalize_whitespace(&t)
}

/// Python `utils.collapse_blank_lines`: one paragraph per line,
/// no blank lines in between. Trafilatura output already reads
/// that way; HTML-to-text conversion and the site strippers emit
/// blank-line-separated paragraphs, and a screen reader stops on
/// every blank line, so every reader-bound body is normalized to
/// the one-line convention. Newlines unify first, lines that are
/// empty after trimming are dropped, surviving lines are
/// right-trimmed, and the whole result is trimmed.
pub fn collapse_blank_lines(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim_end())
        .collect();
    lines.join("\n").trim().to_string()
}

/// Days from civil date (Howard Hinnant's algorithm) — the
/// inverse of the civil-from-days conversion the app uses to
/// format timestamps.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date/time from unix seconds (UTC), Hinnant's algorithm.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 { y + 1 } else { y },
        m as u32,
        d as u32,
        h as u32,
        mi as u32,
        s as u32,
    )
}

/// Parse the date shapes the apps store (RFC 3339 / ISO 8601,
/// `Z` or numeric offset, fractional seconds optional, a space
/// may separate date and time, date-only means midnight UTC)
/// into unix seconds. Python's `parse_datetime_utc` accepts more
/// shapes (RFC 2822 feed dates); stored article dates in both
/// apps' databases are normalized to this family at ingest.
pub fn parse_stored_date_unix(text: &str) -> Option<i64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<i64> { text.get(from..from + len)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86_400;
    if bytes.len() == 10 {
        return Some(secs);
    }
    let (hour, min, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2).unwrap_or(0));
    secs += hour * 3600 + min * 60 + sec;
    // Offset: 'Z'/'z' or ±HH:MM / ±HHMM after the seconds (and any
    // fractional part). No offset means the value is already UTC
    // (Python treats naive values as UTC in parse_datetime_utc).
    let rest = &text[19.min(text.len())..];
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if rest.is_empty() || rest.starts_with(['Z', 'z']) {
        return Some(secs);
    }
    let sign = match rest.as_bytes().first() {
        Some(b'+') => 1i64,
        Some(b'-') => -1i64,
        _ => return Some(secs),
    };
    let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Some(secs);
    }
    let oh: i64 = digits[..2].parse().ok()?;
    let om: i64 = digits[2..4].parse().ok()?;
    Some(secs - sign * (oh * 3600 + om * 60))
}

/// Python `utils.humanize_article_date`: relative wording for
/// the first 24 hours ("Just now", "N minutes ago", "N hours
/// ago"), then the absolute local time `YYYY-MM-DD HH:MM`.
/// Unparseable input yields "". `now_unix` and
/// `local_offset_secs` are parameters so the rule is testable;
/// the caller supplies the offset for the article date's own
/// timezone (see the app-side note on how it is derived).
pub fn humanize_article_date(date_str: &str, now_unix: i64, local_offset_secs: i64) -> String {
    let Some(dt) = parse_stored_date_unix(date_str) else {
        return String::new();
    };
    let delta = (now_unix - dt).max(0);
    if delta <= 24 * 3600 {
        if delta < 60 {
            return "Just now".to_string();
        }
        let mins = delta / 60;
        if mins < 60 {
            return if mins == 1 {
                "1 minute ago".to_string()
            } else {
                format!("{mins} minutes ago")
            };
        }
        let hours = mins / 60;
        return if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        };
    }
    let (y, mo, d, h, mi, _s) = civil_from_unix(dt + local_offset_secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

#[cfg(test)]
mod reader_text_tests {
    use super::{collapse_blank_lines, humanize_article_date, parse_stored_date_unix};

    #[test]
    fn collapse_drops_blank_lines_and_unifies_endings() {
        let input = "First para.\r\n\r\nSecond para.  \n\n\nThird para.\rTail";
        assert_eq!(
            collapse_blank_lines(input),
            "First para.\nSecond para.\nThird para.\nTail"
        );
    }

    #[test]
    fn collapse_keeps_indentation_and_content_lines() {
        assert_eq!(
            collapse_blank_lines("  indented\n\n\ttabbed"),
            "indented\n\ttabbed"
        );
        assert_eq!(collapse_blank_lines(""), "");
        assert_eq!(collapse_blank_lines("\n\n  \n"), "");
    }

    #[test]
    fn parse_stored_dates() {
        // 2026-10-30T10:00:00Z
        assert_eq!(
            parse_stored_date_unix("2026-10-30T10:00:00Z"),
            Some(1_793_354_400)
        );
        assert_eq!(
            parse_stored_date_unix("2026-10-30T10:00:00+01:00"),
            Some(1_793_350_800)
        );
        assert_eq!(
            parse_stored_date_unix("2026-10-30 10:00:00Z"),
            Some(1_793_354_400)
        );
        assert_eq!(parse_stored_date_unix("2026-10-30"), Some(1_793_318_400));
        assert_eq!(parse_stored_date_unix("not a date"), None);
        assert_eq!(parse_stored_date_unix(""), None);
    }

    #[test]
    fn humanize_relative_and_absolute() {
        let t0 = 1_793_354_400; // 2026-10-30T10:00:00Z
        let stamp = "2026-10-30T10:00:00Z";
        assert_eq!(humanize_article_date(stamp, t0 + 30, 0), "Just now");
        assert_eq!(humanize_article_date(stamp, t0 + 60, 0), "1 minute ago");
        assert_eq!(
            humanize_article_date(stamp, t0 + 5 * 60, 0),
            "5 minutes ago"
        );
        assert_eq!(humanize_article_date(stamp, t0 + 3600, 0), "1 hour ago");
        assert_eq!(
            humanize_article_date(stamp, t0 + 3 * 3600, 0),
            "3 hours ago"
        );
        // Exactly 24h is still relative in Python (delta <= 24h).
        assert_eq!(
            humanize_article_date(stamp, t0 + 24 * 3600, 0),
            "24 hours ago"
        );
        // Past 24h: absolute local time (offset -8h here).
        assert_eq!(
            humanize_article_date(stamp, t0 + 25 * 3600, -8 * 3600),
            "2026-10-30 02:00"
        );
        assert_eq!(humanize_article_date("garbage", t0, 0), "");
        // Future dates clamp to zero delta, like Python.
        assert_eq!(humanize_article_date(stamp, t0 - 500, 0), "Just now");
    }
}
