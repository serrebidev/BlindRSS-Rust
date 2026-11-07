//! Thread reconstructions (Python's article-extractor thread
//! paths + core/github_source.py, core/forum_sources.py).
//!
//! When an article URL is a thread on a platform with a real
//! API, Python rebuilds the thread as a synthetic document —
//! the opening post followed by every comment in order —
//! instead of scraping the HTML shell. This module does the
//! same for the API-driven platforms. Reddit and Lemmy (like
//! GitHub) build ONE semantic HTML document per thread that
//! both readers consume — the plain reader via `html_to_text`,
//! the rich reader via the article_html cleaner — so the post
//! and reply sets are identical by construction (Audit 8 R04).
//! Discourse still returns its spoken plain-text shape:
//!
//!   Post 1 - <author> - <date>
//!   <body>
//!
//!   Post 2 - <author> - <date> (reply level 1)
//!   <body>
//!
//! Detection fails closed: anything that does not confirm
//! returns None and normal extraction runs. The HTML-selector
//! forum layouts (Invision, vBulletin, XenForo...) live in
//! `forum_sources.rs`, which carries its own small selector
//! matcher; this module owns the API-shaped sources.

use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
mod github;

static CONFIG_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

pub fn configure(_cfg: &crate::config::Config, path: &std::path::Path) {
    *CONFIG_PATH.lock().unwrap_or_else(|p| p.into_inner()) = Some(path.to_owned());
}

fn resolve_source_key(
    names: &[&str],
    key: &str,
    cfg: &crate::config::Config,
    mut environment: impl FnMut(&str) -> Option<String>,
) -> String {
    names
        .iter()
        .filter_map(|name| environment(name))
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| cfg.get_str(key, "").trim().to_string())
}

pub(crate) fn escape_source_html(value: &str) -> String {
    github::escape(value)
}

pub(crate) fn source_key(names: &[&str], key: &str) -> String {
    let path = CONFIG_PATH
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let cfg = path
        .and_then(|p| crate::config::Config::load(&p).ok())
        .unwrap_or_default();
    resolve_source_key(names, key, &cfg, |name| std::env::var(name).ok())
}

pub(crate) fn source_api_json(url: &str, host: &str, token: &str, github: bool) -> Option<Value> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(host)
        || parsed.port_or_known_default() != Some(443)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    // Never forward source credentials through redirects or imported browser cookies.
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .user_agent(crate::http::user_agent())
            .timeout_global(Some(std::time::Duration::from_secs(
                crate::http::timeout_secs(),
            )))
            .max_redirects(0)
            .build(),
    );
    let mut request = agent.get(url).header(
        "Accept",
        if github {
            "application/vnd.github.full+json"
        } else {
            "application/json"
        },
    );
    if github {
        request = request.header("X-GitHub-Api-Version", "2022-11-28");
    }
    if !token.is_empty() {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut response = request.call().ok()?;
    if response.status() != 200 {
        return None;
    }
    serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()
}

/// Try every thread reconstructor for this URL.
pub fn thread_text(url: &str) -> Option<(String, String)> {
    lemmy_thread(url)
        .or_else(|| github_thread(url))
        .or_else(|| github_page_text(url))
        .or_else(|| reddit_thread(url))
        .or_else(|| discourse_thread(url))
}

fn get_json(url: &str) -> Option<Value> {
    let mut resp = crate::http::agent().get(url).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    serde_json::from_str(&text).ok()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Semantic thread documents (Reddit/Lemmy): one block per post
// or comment, rendered into a single HTML document by the
// builders below (Python _reddit_thread_document /
// _lemmy_thread_document).
// ---------------------------------------------------------------------------

struct ThreadBlock {
    author: String,
    when: String,
    depth: Option<usize>,
    body_html: String,
}

/// Python truthiness for a JSON value (None/false/0/"" are false).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// Python `str(value or "")` for the scalar shapes an API id or
/// child list carries.
fn value_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// An API identity value under Python's `value or fallback`
/// rule: numbers count only when non-zero, strings when
/// non-empty.
fn id_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::Number(n)) => {
            if n.as_f64().map(|f| f != 0.0).unwrap_or(false) {
                n.to_string()
            } else {
                String::new()
            }
        }
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Python `html.escape(value, quote=False)`: &, < and > only.
fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Split on runs of 2+ newlines (Python `re.split(r"\n{2,}", …)`),
/// keeping only blocks with non-whitespace content.
fn paragraph_blocks(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b'\n' {
                j += 1;
            }
            if j - i >= 2 {
                blocks.push(&text[start..i]);
                start = j;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    blocks.push(&text[start..]);
    blocks
        .into_iter()
        .filter(|b| !b.trim().is_empty())
        .map(|b| b.to_string())
        .collect()
}

/// Render Markdown the way Python's `_lemmy_markdown_html` does:
/// escape the source first (raw HTML becomes literal text),
/// render with the "extra"-class feature set, and fall back to
/// plain paragraph wrapping when the render is empty.
fn markdown_html(value: &str) -> String {
    let source = value.trim();
    if source.is_empty() {
        return String::new();
    }
    let escaped = escape_text(source);
    let options = pulldown_cmark::Options::ENABLE_TABLES
        | pulldown_cmark::Options::ENABLE_FOOTNOTES
        | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
        | pulldown_cmark::Options::ENABLE_TASKLISTS;
    let parser = pulldown_cmark::Parser::new_ext(&escaped, options);
    let mut rendered = String::new();
    pulldown_cmark::html::push_html(&mut rendered, parser);
    if !rendered.trim().is_empty() {
        return rendered;
    }
    paragraph_blocks(&escaped)
        .iter()
        .map(|b| format!("<p>{b}</p>"))
        .collect()
}

// Civil-date conversions (Howard Hinnant's algorithms) for the
// "%Y-%m-%d %H:%M UTC" labels Python formats from timestamps.

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn utc_label_from_unix(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// Python `_reddit_time_label`: epoch seconds -> UTC label,
/// empty for missing/out-of-range values.
fn reddit_time_label(value: Option<&Value>) -> String {
    let ts = match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    };
    if ts > 0.0 && ts < 253_402_300_800.0 {
        utc_label_from_unix(ts as i64)
    } else {
        String::new()
    }
}

/// Parse an ISO-8601 timestamp to Unix seconds (Python
/// `datetime.fromisoformat` with Z handling; naive = UTC).
fn parse_iso_utc(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if bytes[10] != b'T' && bytes[10] != b' ' {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let num = |a: usize, b: usize| raw.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let mut idx = 19;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        while bytes.get(idx).map(|c| c.is_ascii_digit()) == Some(true) {
            idx += 1;
        }
    }
    let rest = raw.get(idx..)?;
    let offset: i64 = if rest.is_empty() || rest == "Z" || rest == "z" {
        0
    } else {
        let sign: i64 = match rest.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return None,
        };
        let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
        if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let oh: i64 = digits[..2].parse().ok()?;
        let om: i64 = digits[2..].parse().ok()?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s - offset)
}

/// Python `_lemmy_time_label`: ISO timestamp -> UTC label;
/// unparseable values pass through raw.
fn lemmy_time_label(value: Option<&Value>) -> String {
    let raw = match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    match parse_iso_utc(raw) {
        Some(seconds) => utc_label_from_unix(seconds),
        None => raw.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Lemmy: /post/<id> on any instance -> api/v3 post + comments.
// ---------------------------------------------------------------------------

const LEMMY_COMMENT_PAGE_SIZE: usize = 50;
const LEMMY_MAX_COMMENT_REQUESTS: usize = 250;
const LEMMY_MAX_COMMENTS: usize = 10_000;

/// `(instance origin, post id)` for a possible Lemmy post URL
/// (Python `_lemmy_thread_parts`). The strict numeric route is
/// only a candidate; the API request later confirms the
/// instance and fails closed. Query/fragment suffixes are not
/// part of the id (Audit 8 R03).
fn lemmy_parts(url: &str) -> Option<(String, i64)> {
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    let re = PATH_RE.get_or_init(|| Regex::new(r"(?i)^/post/([0-9]+)/?$").unwrap());
    let parsed = url::Url::parse(url.trim()).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = parsed.host_str()?;
    if host.is_empty() {
        return None;
    }
    let id: i64 = re.captures(parsed.path())?.get(1)?.as_str().parse().ok()?;
    let host_part = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let origin = match parsed.port() {
        Some(port) => format!("{scheme}://{host_part}:{port}"),
        None => format!("{scheme}://{host_part}"),
    };
    Some((origin, id))
}

/// Python `_lemmy_actor_name`: display name wins over the
/// handle; a missing creator is "[deleted]".
fn lemmy_actor_name(view: &Value) -> String {
    let Some(creator) = view.get("creator").filter(|c| c.is_object()) else {
        return "[deleted]".to_string();
    };
    let pick = ["display_name", "name"]
        .iter()
        .filter_map(|k| creator.get(*k).and_then(|v| v.as_str()))
        .find(|s| !s.is_empty())
        .unwrap_or("[deleted]");
    pick.trim().to_string()
}

/// Python `_lemmy_comment_depth`: the path "0.123.456" is
/// depth 2 — the count of id segments after the root marker.
fn lemmy_comment_depth(comment: &Value) -> usize {
    let path = str_field(comment, "path");
    let parts = path
        .trim_matches('.')
        .split('.')
        .filter(|p| !p.is_empty() && *p != "0")
        .count();
    parts.saturating_sub(1)
}

/// Python `_lemmy_post_body_html`.
fn lemmy_post_body_html(post: &Value) -> String {
    if truthy(post.get("removed")) {
        return "<p>[removed]</p>".to_string();
    }
    if truthy(post.get("deleted")) {
        return "<p>[deleted]</p>".to_string();
    }
    let mut chunks = markdown_html(&str_field(post, "body"));
    let target = str_field(post, "url");
    let target = target.trim();
    let ap_id = str_field(post, "ap_id");
    if !target.is_empty() && target != ap_id.trim() {
        chunks.push_str(&format!(
            "<p>Linked content: <a href=\"{}\">{}</a></p>",
            github::escape(target),
            github::escape(target)
        ));
    }
    if chunks.is_empty() {
        "<p>[no text available]</p>".to_string()
    } else {
        chunks
    }
}

/// Python `_lemmy_comment_body_html`.
fn lemmy_comment_body_html(comment: &Value) -> String {
    if truthy(comment.get("removed")) {
        return "<p>[removed]</p>".to_string();
    }
    if truthy(comment.get("deleted")) {
        return "<p>[deleted]</p>".to_string();
    }
    let rendered = markdown_html(&str_field(comment, "content"));
    if rendered.is_empty() {
        "<p>[no text available]</p>".to_string()
    } else {
        rendered
    }
}

/// Build the semantic Lemmy document (Python
/// `_lemmy_thread_document`): the opening post plus the
/// comments in the order supplied (the API's Old sort with
/// stable depth paths), one `<section>` per block. Returns
/// the document title with the document.
pub fn lemmy_thread_document(post_view: &Value, comments: &[Value]) -> Option<(String, String)> {
    let post = post_view.get("post").filter(|p| p.is_object())?;
    if !truthy(post.get("id")) {
        return None;
    }
    let name = str_field(post, "name");
    let title = if name.trim().is_empty() {
        "Lemmy thread".to_string()
    } else {
        name.trim().to_string()
    };
    let author = lemmy_actor_name(post_view);
    let mut blocks = vec![ThreadBlock {
        author: author.clone(),
        when: lemmy_time_label(post.get("published")),
        depth: None,
        body_html: lemmy_post_body_html(post),
    }];
    let empty = Value::Object(serde_json::Map::new());
    for view in comments {
        let comment = view
            .get("comment")
            .filter(|c| c.is_object())
            .unwrap_or(&empty);
        blocks.push(ThreadBlock {
            author: lemmy_actor_name(view),
            when: lemmy_time_label(comment.get("published")),
            depth: Some(lemmy_comment_depth(comment)),
            body_html: lemmy_comment_body_html(comment),
        });
    }
    let mut sections = String::new();
    for (i, block) in blocks.iter().enumerate() {
        let index = i + 1;
        let role = if index == 1 { "Posted" } else { "Comment" };
        let mut details = format!("#{index} {role} by @{}", block.author);
        if !block.when.is_empty() {
            details.push_str(" — ");
            details.push_str(&block.when);
        }
        if index > 1 {
            details.push_str(&format!(" — Reply level {}", block.depth.unwrap_or(0) + 1));
        }
        sections.push_str(&format!(
            "<section class=\"blindrss-lemmy-post\"><h2>{}</h2><div class=\"blindrss-lemmy-body\">{}</div></section>",
            github::escape(&details),
            block.body_html
        ));
    }
    let document = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title><meta name=\"author\" content=\"{}\"></head><body><article data-blindrss-lemmy-thread=\"1\">{}</article></body></html>",
        github::escape(&title),
        github::escape(&author),
        sections
    );
    Some((title, document))
}

/// Fetch one API version's complete comment set (Python
/// `_lemmy_api_thread_html`): page (v3) or cursor (v4)
/// pagination at full depth, deduplicated by comment id,
/// capped at 10,000 comments / 250 requests. A first-page
/// failure fails the whole version; a later page failure
/// keeps the comments already gathered.
fn lemmy_api_html(
    origin: &str,
    post_id: i64,
    api_version: u32,
    get: &mut impl FnMut(&str) -> Option<Value>,
) -> Option<(String, String)> {
    let api_root = format!("{origin}/api/v{api_version}");
    let post_payload = get(&format!("{api_root}/post?id={post_id}"))?;
    let post_view = post_payload
        .get("post_view")
        .filter(|v| v.is_object())?
        .clone();
    let mut comments: Vec<Value> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut cursor = String::new();
    for page in 1..=LEMMY_MAX_COMMENT_REQUESTS {
        let mut url = format!(
            "{api_root}/comment/list?post_id={post_id}&sort=Old&max_depth=999&limit={LEMMY_COMMENT_PAGE_SIZE}"
        );
        if api_version >= 4 {
            if !cursor.is_empty() {
                let encoded: String =
                    url::form_urlencoded::byte_serialize(cursor.as_bytes()).collect();
                url.push_str(&format!("&page_cursor={encoded}"));
            }
        } else {
            url.push_str(&format!("&page={page}"));
        }
        let payload = get(&url);
        let page_comments = payload
            .as_ref()
            .and_then(|p| p.get("comments"))
            .and_then(|c| c.as_array())
            .cloned();
        let Some(page_comments) = page_comments else {
            if page == 1 {
                return None;
            }
            return lemmy_thread_document(&post_view, &comments);
        };
        let page_len = page_comments.len();
        let mut new_count = 0usize;
        for view in &page_comments {
            if !view.is_object() {
                continue;
            }
            let comment_id = view
                .get("comment")
                .map(|c| {
                    let id = id_string(c.get("id"));
                    if id.is_empty() {
                        id_string(c.get("ap_id"))
                    } else {
                        id
                    }
                })
                .unwrap_or_default()
                .trim()
                .to_string();
            if comment_id.is_empty() || !seen_ids.insert(comment_id) {
                continue;
            }
            comments.push(view.clone());
            new_count += 1;
            if comments.len() >= LEMMY_MAX_COMMENTS {
                break;
            }
        }
        if comments.len() >= LEMMY_MAX_COMMENTS || page_len == 0 || new_count == 0 {
            break;
        }
        if api_version >= 4 {
            let next_cursor = payload
                .as_ref()
                .and_then(|p| p.get("next_page"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if next_cursor.is_empty() || next_cursor == cursor {
                break;
            }
            cursor = next_cursor;
        } else if page_len < LEMMY_COMMENT_PAGE_SIZE {
            break;
        }
    }
    lemmy_thread_document(&post_view, &comments)
}

/// Python `_download_lemmy_thread_html` with the fetch seam
/// exposed for tests: v3 (current 0.19 instances) first, then
/// v4 (Lemmy 1.0, cursor pagination).
pub fn lemmy_thread_html_from(
    origin: &str,
    post_id: i64,
    get: &mut impl FnMut(&str) -> Option<Value>,
) -> Option<(String, String)> {
    lemmy_api_html(origin, post_id, 3, get).or_else(|| lemmy_api_html(origin, post_id, 4, get))
}

/// The complete Lemmy document for both readers (title, HTML).
pub fn lemmy_html(url: &str) -> Option<(String, String)> {
    let (origin, post_id) = lemmy_parts(url)?;
    lemmy_thread_html_from(&origin, post_id, &mut |u| get_json(u))
}

pub fn lemmy_thread(url: &str) -> Option<(String, String)> {
    lemmy_html(url).map(|(title, html)| (title, crate::extract::html_to_text(&html)))
}

// ---------------------------------------------------------------------------
// GitHub: issue / PR / commit / release pages -> REST API.
// ---------------------------------------------------------------------------

/// Read issue and pull-request threads through the complete GitHub renderer.
pub fn github_thread(url: &str) -> Option<(String, String)> {
    let target = github::parse_target(url)?;
    if !matches!(target.kind.as_str(), "issue" | "pull") {
        return None;
    }
    github_html(url).map(|(title, html)| (title, crate::extract::html_to_text(&html)))
}

// ---------------------------------------------------------------------------
// GitHub page documents (Batch 35b): commit, release and discussion
// pages rebuilt from the REST API, porting the remaining
// core/github_source.py renderers (issue/PR live in github_thread
// above). Org discussions have no REST endpoint and are rebuilt
// from the server-rendered page, as in Python.
// ---------------------------------------------------------------------------

const GITHUB_RESERVED_OWNERS: [&str; 12] = [
    "settings",
    "login",
    "join",
    "explore",
    "marketplace",
    "notifications",
    "search",
    "topics",
    "collections",
    "sponsors",
    "orgs",
    "new",
];

#[derive(Debug, PartialEq, Eq)]
pub enum GithubPage {
    Commit(String, String, String),
    Release(String, String, String),
    Discussion(String, String, String),
    OrgDiscussion(String, String),
}

pub fn github_page_parts(url: &str) -> Option<GithubPage> {
    let rest = url.strip_prefix("https://github.com/")?;
    let clean = rest.split(['?', '#']).next().unwrap_or(rest);
    let parts: Vec<&str> = clean.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() == 4 && parts[0] == "orgs" && parts[2] == "discussions" {
        if parts[3].chars().all(|c| c.is_ascii_digit()) && !parts[3].is_empty() {
            return Some(GithubPage::OrgDiscussion(
                parts[1].to_string(),
                parts[3].to_string(),
            ));
        }
        return None;
    }
    if parts.len() < 4 {
        return None;
    }
    let (owner, repo) = (parts[0], parts[1]);
    if GITHUB_RESERVED_OWNERS.contains(&owner.to_lowercase().as_str()) {
        return None;
    }
    match parts[2] {
        "commit" if parts.len() == 4 => {
            let sha = parts[3];
            if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(GithubPage::Commit(
                    owner.to_string(),
                    repo.to_string(),
                    sha.to_string(),
                ));
            }
            None
        }
        "releases" if parts.len() >= 5 && parts[3] == "tag" => Some(GithubPage::Release(
            owner.to_string(),
            repo.to_string(),
            parts[4..].join("/"),
        )),
        "discussions" if parts.len() == 4 => {
            if parts[3].chars().all(|c| c.is_ascii_digit()) && !parts[3].is_empty() {
                return Some(GithubPage::Discussion(
                    owner.to_string(),
                    repo.to_string(),
                    parts[3].to_string(),
                ));
            }
            None
        }
        _ => None,
    }
}

/// Semantic GitHub document for both reader renderings.
pub fn github_html(url: &str) -> Option<(String, String)> {
    let mut target = github::parse_target(url)?;
    if target.kind == "org_discussion" {
        target.kind = "discussion".into();
        target.repo = ".github".into();
    }
    let token = source_key(
        &["BLINDRSS_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"],
        "github_token",
    );
    github::document(&target, &mut |path| {
        source_api_json(
            &format!("https://api.github.com{path}"),
            "api.github.com",
            &token,
            true,
        )
    })
}

pub fn github_page_text(url: &str) -> Option<(String, String)> {
    let page = github_page_parts(url)?;
    github_html(url)
        .map(|(title, html)| (title, crate::extract::html_to_text(&html)))
        .or_else(|| match page {
            GithubPage::Discussion(owner, _, num) | GithubPage::OrgDiscussion(owner, num) => {
                github_org_discussion_text(&owner, &num, url)
            }
            _ => None,
        })
}

fn github_org_discussion_text(owner: &str, num: &str, url: &str) -> Option<(String, String)> {
    // No REST endpoint: rebuild from the server-rendered page's
    // timeline comment groups (Python's page-scrape fallback).
    let (bytes, _final) = crate::extract::fetch_page_bytes(url, "").ok()?;
    let html = String::from_utf8_lossy(&bytes).to_string();
    let mut title = String::new();
    for sp in crate::extract::find_elements(&html, Some("title"), &|_| true) {
        title = crate::extract::leaf_text(&html[sp.inner_start..sp.inner_end]);
        if !title.is_empty() {
            break;
        }
    }
    title = title.split(" · ").next().unwrap_or("").trim().to_string();
    if title.is_empty() {
        title = format!("Discussion #{num}");
    }
    let groups = crate::extract::find_elements(&html, Some("div"), &|open: &str| {
        crate::extract::has_class(open, "timeline-comment-group")
    });
    let mut out: Vec<String> = Vec::new();
    let mut author = String::new();
    for (i, g) in groups.iter().enumerate() {
        let frag = &html[g.inner_start..g.inner_end];
        let bodies = crate::extract::find_elements(frag, None, &|open: &str| {
            crate::extract::has_class(open, "comment-body")
                || crate::extract::has_class(open, "markdown-body")
        });
        let Some(first) = bodies.first() else {
            continue;
        };
        let body_html = &frag[first.start..first.end];
        let text = crate::extract::html_to_text(body_html);
        if text.trim().is_empty() {
            continue;
        }
        let who = crate::extract::find_elements(frag, None, &|open: &str| {
            crate::extract::has_class(open, "author")
        })
        .first()
        .map(|sp| crate::extract::leaf_text(&frag[sp.inner_start..sp.inner_end]))
        .unwrap_or_default();
        if author.is_empty() {
            author = who.clone();
        }
        let label = if i == 0 {
            "Discussion opened by"
        } else {
            "Comment by"
        };
        out.push(format!("{label} {who}"));
        out.push(text.trim().to_string());
    }
    if out.is_empty() {
        return None;
    }
    let _ = author;
    Some((format!("{title} ({owner}#{num})"), out.join("\n\n")))
}

// ---------------------------------------------------------------------------
// Reddit: thread page -> comments JSON, with morechildren and
// "_" continuation expansion, rebuilt as one semantic
// document (Python _reddit_thread_parts /
// _download_reddit_thread_html, Audit 8 R02).
// ---------------------------------------------------------------------------

const REDDIT_MORE_BATCH_SIZE: usize = 100;
const REDDIT_MAX_MORE_REQUESTS: usize = 100;
const REDDIT_MAX_COMMENTS: usize = 10_000;

/// `(subreddit, article id)` for a Reddit thread permalink
/// (Python `_reddit_thread_parts`).
fn reddit_parts(url: &str) -> Option<(String, String)> {
    static PATH_RE: OnceLock<Regex> = OnceLock::new();
    let re = PATH_RE.get_or_init(|| {
        Regex::new(r"(?i)^/r/([A-Za-z0-9_]{2,21})/comments/([A-Za-z0-9]+)(?:/[^/?#]*)?/?$").unwrap()
    });
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    if host != "reddit.com" && !host.ends_with(".reddit.com") {
        return None;
    }
    let caps = re.captures(parsed.path())?;
    Some((
        caps.get(1)?.as_str().to_string(),
        caps.get(2)?.as_str().to_string(),
    ))
}

/// Python's fullname derivation: the stored `name`, else
/// `<prefix>_<id>`, stripped either way.
fn reddit_fullname(data: &Value, prefix: &str) -> String {
    let name = str_field(data, "name");
    if !name.is_empty() {
        return name.trim().to_string();
    }
    format!("{prefix}_{}", str_field(data, "id"))
        .trim()
        .to_string()
}

/// Python `_reddit_body_html`: Reddit's rendered fragment
/// when present (unescaping the legacy entity-escaped form),
/// else the plain body wrapped in paragraphs, else — for the
/// opening post — the linked-content line.
fn reddit_body_html(data: &Value, opening: bool) -> String {
    let html_key = if opening {
        "selftext_html"
    } else {
        "body_html"
    };
    let plain_key = if opening { "selftext" } else { "body" };
    let fragment = str_field(data, html_key);
    let fragment = fragment.trim();
    if !fragment.is_empty() {
        if fragment.starts_with("&lt;") {
            return crate::extract::unescape_entities(fragment);
        }
        return fragment.to_string();
    }
    let plain = str_field(data, plain_key);
    let plain = plain.trim();
    if !plain.is_empty() {
        return paragraph_blocks(plain)
            .iter()
            .map(|b| format!("<p>{}</p>", github::escape(b)))
            .collect();
    }
    if opening {
        let target_raw = str_field(data, "url_overridden_by_dest");
        let target = if !target_raw.is_empty() {
            target_raw
        } else {
            str_field(data, "url")
        };
        let target = target.trim();
        let permalink = str_field(data, "permalink");
        let permalink = permalink.trim();
        if !target.is_empty()
            && !permalink.is_empty()
            && target != permalink
            && !target.ends_with(permalink)
        {
            return format!(
                "<p>Linked content: <a href=\"{}\">{}</a></p>",
                github::escape(target),
                github::escape(target)
            );
        }
    }
    "<p>[no text available]</p>".to_string()
}

/// Python `_reddit_thread_document`: one `<section>` per
/// block under a single `<article>`, headings carrying the
/// sequential number, author, UTC date and reply level.
fn reddit_thread_document(title: &str, author: &str, blocks: &[ThreadBlock]) -> String {
    if blocks.is_empty() {
        return String::new();
    }
    let title = if title.trim().is_empty() {
        "Reddit thread"
    } else {
        title.trim()
    };
    let author = author.trim();
    let mut sections = String::new();
    for (i, block) in blocks.iter().enumerate() {
        let index = i + 1;
        let block_author = {
            let a = block.author.trim();
            if a.is_empty() { "[deleted]" } else { a }
        };
        let role = if index == 1 { "Posted" } else { "Comment" };
        let mut details = format!("#{index} {role} by u/{block_author}");
        if !block.when.is_empty() {
            details.push_str(" — ");
            details.push_str(&block.when);
        }
        if index > 1 {
            details.push_str(&format!(" — Reply level {}", block.depth.unwrap_or(0) + 1));
        }
        let body = if block.body_html.trim().is_empty() {
            "<p>[no text available]</p>"
        } else {
            block.body_html.trim()
        };
        sections.push_str(&format!(
            "<section class=\"blindrss-reddit-post\"><h2>{}</h2><div class=\"blindrss-reddit-body\">{}</div></section>",
            github::escape(&details),
            body
        ));
    }
    let meta_author = if author.is_empty() {
        String::new()
    } else {
        format!(
            "<meta name=\"author\" content=\"{}\">",
            github::escape(author)
        )
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title>{}</head><body><article data-blindrss-reddit-thread=\"1\">{}</article></body></html>",
        github::escape(title),
        meta_author,
        sections
    )
}

/// Collects the comments of one Reddit payload: inline `t1`
/// trees in order, `more` nodes queued for expansion
/// (Python `_visit` / `_queue_more` in
/// `_reddit_json_to_thread_html`).
struct RedditCollector {
    comments: Vec<Value>,
    seen: HashSet<String>,
    pending_ids: VecDeque<String>,
    queued_ids: HashSet<String>,
    pending_continue: VecDeque<String>,
    queued_continue: HashSet<String>,
}

impl RedditCollector {
    fn new() -> Self {
        RedditCollector {
            comments: Vec::new(),
            seen: HashSet::new(),
            pending_ids: VecDeque::new(),
            queued_ids: HashSet::new(),
            pending_continue: VecDeque::new(),
            queued_continue: HashSet::new(),
        }
    }

    fn queue_more(&mut self, data: &Value) {
        let child_ids: Vec<String> = data
            .get("children")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .map(|v| value_string(v).trim().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let parent_id = str_field(data, "parent_id");
        let parent_id = parent_id.trim();
        // At Reddit's maximum inline depth, a special `id: _`
        // node means "continue this thread": morechildren
        // cannot expand it, so the comment-root route is
        // queued instead.
        if str_field(data, "id").trim() == "_" || child_ids.iter().any(|c| c == "_") {
            let parent_comment_id = parent_id.strip_prefix("t1_").unwrap_or(parent_id);
            if !parent_comment_id.is_empty()
                && self.queued_continue.insert(parent_comment_id.to_string())
            {
                self.pending_continue
                    .push_back(parent_comment_id.to_string());
            }
        }
        for child_id in &child_ids {
            if !child_id.is_empty()
                && child_id != "_"
                && !self.seen.contains(&format!("t1_{child_id}"))
                && self.queued_ids.insert(child_id.clone())
            {
                self.pending_ids.push_back(child_id.clone());
            }
        }
    }

    fn visit(&mut self, node: &Value) {
        if self.comments.len() >= REDDIT_MAX_COMMENTS {
            return;
        }
        let Some(data) = node.get("data").filter(|d| d.is_object()) else {
            return;
        };
        match node.get("kind").and_then(|k| k.as_str()) {
            Some("more") => self.queue_more(data),
            Some("t1") => {
                let name = reddit_fullname(data, "t1");
                if name.is_empty() {
                    return;
                }
                if self.seen.insert(name) {
                    self.comments.push(data.clone());
                }
                if let Some(children) = data
                    .get("replies")
                    .and_then(|r| r.get("data"))
                    .and_then(|d| d.get("children"))
                    .and_then(|c| c.as_array())
                {
                    for child in children {
                        self.visit(child);
                    }
                }
            }
            _ => {}
        }
    }
}

fn reddit_stored_depth(item: &Value) -> usize {
    item.get("depth")
        .and_then(|d| d.as_f64())
        .map(|f| if f > 0.0 { f as usize } else { 0 })
        .unwrap_or(0)
}

fn reddit_emit(
    parent: &str,
    depth: usize,
    comments: &[Value],
    children_by_parent: &HashMap<String, Vec<usize>>,
    emitted: &mut HashSet<String>,
    ordered: &mut Vec<(usize, usize)>,
) {
    if let Some(children) = children_by_parent.get(parent) {
        for &idx in children {
            let name = reddit_fullname(&comments[idx], "t1");
            if name.is_empty() || !emitted.insert(name.clone()) {
                continue;
            }
            ordered.push((idx, depth));
            reddit_emit(
                &name,
                depth + 1,
                comments,
                children_by_parent,
                emitted,
                ordered,
            );
        }
    }
}

/// Expand a Reddit thread payload into the semantic document
/// (Python `_reddit_json_to_thread_html`): every `more` node
/// is exhausted through morechildren (batches of 100, at most
/// 100 requests) and `_` continuations through the
/// comment-root route, duplicates are dropped, and the flat
/// result is rebuilt into parent/child order with orphans
/// appended in arrival order. The fetch seam is exposed for
/// tests.
pub fn reddit_thread_html_from_payload(
    payload: &Value,
    thread_url: &str,
    get: &mut impl FnMut(&str) -> Option<Value>,
) -> Option<(String, String)> {
    let (subreddit, article_id) = reddit_parts(thread_url)?;
    let listings = payload.as_array().filter(|a| a.len() >= 2)?;
    let post = listings[0]
        .get("data")?
        .get("children")?
        .as_array()?
        .iter()
        .find(|n| n.get("kind").and_then(|k| k.as_str()) == Some("t3"))
        .and_then(|n| n.get("data"))
        .filter(|d| d.is_object())?
        .clone();
    let comment_nodes = listings[1]
        .get("data")?
        .get("children")?
        .as_array()?
        .clone();
    let link_id = reddit_fullname(&post, "t3");

    let mut collector = RedditCollector::new();
    for node in &comment_nodes {
        collector.visit(node);
    }
    let mut requested: HashSet<String> = HashSet::new();
    let mut request_count = 0usize;
    while (!collector.pending_ids.is_empty() || !collector.pending_continue.is_empty())
        && request_count < REDDIT_MAX_MORE_REQUESTS
        && collector.comments.len() < REDDIT_MAX_COMMENTS
    {
        if collector.pending_ids.is_empty() {
            let Some(parent_comment_id) = collector.pending_continue.pop_front() else {
                break;
            };
            request_count += 1;
            let url = format!(
                "https://www.reddit.com/r/{subreddit}/comments/{article_id}/_/{parent_comment_id}.json?raw_json=1&limit=500&depth=10&sort=confidence"
            );
            if let Some(nodes) = get(&url).and_then(|c| {
                c.get(1)
                    .and_then(|l| l.get("data"))
                    .and_then(|d| d.get("children"))
                    .and_then(|c| c.as_array())
                    .cloned()
            }) {
                for thing in &nodes {
                    collector.visit(thing);
                }
            }
            continue;
        }
        let mut batch: Vec<String> = Vec::new();
        while batch.len() < REDDIT_MORE_BATCH_SIZE {
            let Some(child_id) = collector.pending_ids.pop_front() else {
                break;
            };
            if !requested.insert(child_id.clone()) {
                continue;
            }
            batch.push(child_id);
        }
        if batch.is_empty() {
            continue;
        }
        request_count += 1;
        let children: String =
            url::form_urlencoded::byte_serialize(batch.join(",").as_bytes()).collect();
        let url = format!(
            "https://www.reddit.com/api/morechildren.json?api_type=json&link_id={link_id}&children={children}&limit_children=false&sort=confidence&raw_json=1"
        );
        let things = get(&url).and_then(|p| {
            p.get("json")
                .and_then(|j| j.get("data"))
                .and_then(|d| d.get("things"))
                .and_then(|t| t.as_array())
                .cloned()
        });
        let Some(things) = things else { break };
        for thing in &things {
            collector.visit(thing);
        }
    }

    // Rebuild parent/child order after morechildren's flat
    // response. Initial ordering is preserved within each
    // parent, and inaccessible/orphaned nodes are still
    // appended so available comments are never discarded.
    let comments = collector.comments;
    let mut children_by_parent: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, item) in comments.iter().enumerate() {
        let parent = str_field(item, "parent_id");
        let parent = if parent.is_empty() {
            link_id.clone()
        } else {
            parent
        };
        children_by_parent.entry(parent).or_default().push(idx);
    }
    let mut ordered: Vec<(usize, usize)> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    reddit_emit(
        &link_id,
        0,
        &comments,
        &children_by_parent,
        &mut emitted,
        &mut ordered,
    );
    for (idx, item) in comments.iter().enumerate() {
        let name = reddit_fullname(item, "t1");
        if emitted.insert(name) {
            ordered.push((idx, reddit_stored_depth(item)));
        }
    }

    let mut blocks = vec![ThreadBlock {
        author: str_field(&post, "author"),
        when: reddit_time_label(post.get("created_utc")),
        depth: None,
        body_html: reddit_body_html(&post, true),
    }];
    for (idx, depth) in &ordered {
        let item = &comments[*idx];
        blocks.push(ThreadBlock {
            author: str_field(item, "author"),
            when: reddit_time_label(item.get("created_utc")),
            depth: Some(*depth),
            body_html: reddit_body_html(item, false),
        });
    }
    let title_raw = str_field(&post, "title");
    let title = if title_raw.trim().is_empty() {
        "Reddit thread".to_string()
    } else {
        title_raw.trim().to_string()
    };
    let document = reddit_thread_document(&title, &str_field(&post, "author"), &blocks);
    if document.is_empty() {
        None
    } else {
        Some((title, document))
    }
}

/// The comments RSS fallback as a thread document (Python
/// `_reddit_rss_thread_html`): flat blocks in feed order,
/// deduplicated by entry id, entries without content skipped.
pub fn reddit_rss_thread_document(xml: &[u8]) -> Option<(String, String)> {
    let feed = feed_rs::parser::parse(std::io::Cursor::new(xml)).ok()?;
    let mut blocks: Vec<ThreadBlock> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for entry in &feed.entries {
        let entry_id = if !entry.id.is_empty() {
            entry.id.clone()
        } else {
            entry
                .links
                .first()
                .map(|l| l.href.clone())
                .unwrap_or_default()
        };
        if !entry_id.is_empty() && !seen.insert(entry_id) {
            continue;
        }
        let content = entry
            .content
            .as_ref()
            .and_then(|c| c.body.clone())
            .filter(|b| !b.trim().is_empty())
            .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()))
            .unwrap_or_default();
        if content.trim().is_empty() {
            continue;
        }
        let created = entry
            .published
            .or(entry.updated)
            .map(|d| d.timestamp())
            .unwrap_or(0);
        let author = entry
            .authors
            .first()
            .and_then(|a| a.name.clone())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "[deleted]".to_string());
        blocks.push(ThreadBlock {
            author,
            when: if created > 0 {
                utc_label_from_unix(created)
            } else {
                String::new()
            },
            depth: Some(0),
            body_html: content,
        });
    }
    if blocks.is_empty() {
        return None;
    }
    let title = feed
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Reddit thread".to_string());
    let document = reddit_thread_document(&title, &blocks[0].author.clone(), &blocks);
    if document.is_empty() {
        None
    } else {
        Some((title, document))
    }
}

/// Python `_download_reddit_thread_html`: the comments JSON
/// first, the RSS rendering as the failure fallback.
pub fn reddit_html(url: &str) -> Option<(String, String)> {
    let (subreddit, article_id) = reddit_parts(url)?;
    let json_url = format!(
        "https://www.reddit.com/r/{subreddit}/comments/{article_id}.json?raw_json=1&limit=500&depth=10&sort=confidence"
    );
    if let Some(payload) = get_json(&json_url)
        && let Some(pair) = reddit_thread_html_from_payload(&payload, url, &mut |u| get_json(u))
    {
        return Some(pair);
    }
    let rss_url =
        format!("https://www.reddit.com/r/{subreddit}/comments/{article_id}/.rss?limit=500");
    let mut resp = crate::http::agent().get(&rss_url).call().ok()?;
    let bytes = resp.body_mut().read_to_vec().ok()?;
    reddit_rss_thread_document(&bytes)
}

pub fn reddit_thread(url: &str) -> Option<(String, String)> {
    reddit_html(url).map(|(title, html)| (title, crate::extract::html_to_text(&html)))
}

// ---------------------------------------------------------------------------
// Discourse: /t/<slug>/<id> -> <origin>/t/<id>.json post stream.
// ---------------------------------------------------------------------------

fn discourse_parts(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    let segments: Vec<&str> = path.split('/').collect();
    // t/<slug>/<id>[/<post_number>]
    if segments.len() >= 3 && segments[0] == "t" {
        let id = segments[2];
        if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
            return Some((format!("{scheme}://{host}"), id.to_string()));
        }
    }
    None
}

const DISCOURSE_MAX_POSTS: usize = 10_000;
const DISCOURSE_POST_BATCH: usize = 100;

pub fn discourse_thread(url: &str) -> Option<(String, String)> {
    let (origin, id) = discourse_parts(url)?;
    discourse_thread_from(&origin, &id, &mut |url| get_json(url))
}

/// Discourse reconstruction with the fetch seam exposed for tests
/// (Python forum_sources `download_discourse_thread_html`): the
/// `print=true` topic document carries the first posts plus the full
/// post-id stream; the missing ids are fetched in batches of 100
/// through /t/<id>/posts.json, everything is sorted by post_number
/// and capped at 10,000 posts. Author is the display name when the
/// topic carries one, else @username.
fn discourse_thread_from(
    origin: &str,
    id: &str,
    get: &mut impl FnMut(&str) -> Option<Value>,
) -> Option<(String, String)> {
    let topic = get(&format!("{origin}/t/{id}.json?print=true"))?;
    let title = str_field(&topic, "title");
    if title.is_empty() {
        return None; // not a Discourse site: fail closed
    }
    let stream = topic.get("post_stream")?;
    let mut raw_posts: Vec<Value> = stream
        .get("posts")?
        .as_array()?
        .iter()
        .filter(|p| p.is_object())
        .cloned()
        .collect();
    let mut seen_ids: std::collections::HashSet<String> = raw_posts
        .iter()
        .filter_map(|p| p.get("id").map(|v| v.to_string()))
        .collect();
    let wanted: Vec<String> = stream
        .get("stream")
        .and_then(|v| v.as_array())
        .map(|ids| {
            ids.iter()
                .take(DISCOURSE_MAX_POSTS)
                .map(|v| v.to_string())
                .filter(|id| !seen_ids.contains(id))
                .collect()
        })
        .unwrap_or_default();
    for batch in wanted.chunks(DISCOURSE_POST_BATCH) {
        let query = batch
            .iter()
            .map(|id| format!("post_ids%5B%5D={id}"))
            .collect::<Vec<_>>()
            .join("&");
        let Some(page) = get(&format!("{origin}/t/{id}/posts.json?{query}")) else {
            break;
        };
        let page_posts = page
            .get("post_stream")
            .and_then(|s| s.get("posts"))
            .or_else(|| page.get("posts"))
            .and_then(|v| v.as_array())
            .cloned();
        let Some(page_posts) = page_posts else { break };
        for post in page_posts {
            if !post.is_object() {
                continue;
            }
            let identity = post.get("id").map(|v| v.to_string()).unwrap_or_default();
            if !identity.is_empty() && seen_ids.insert(identity) {
                raw_posts.push(post);
            }
        }
        if raw_posts.len() >= DISCOURSE_MAX_POSTS {
            break;
        }
    }
    raw_posts.sort_by_key(|p| {
        p.get("post_number")
            .and_then(|v| v.as_i64())
            .unwrap_or(i64::MAX)
    });
    let mut out = Vec::new();
    for p in raw_posts.iter().take(DISCOURSE_MAX_POSTS) {
        let cooked = str_field(p, "cooked");
        if cooked.trim().is_empty() {
            continue;
        }
        let number = p
            .get("post_number")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| (out.len() / 2 + 1).to_string());
        let username = str_field(p, "username");
        let display = str_field(p, "name");
        let author = if !display.is_empty() {
            display
        } else if !username.is_empty() {
            format!("@{username}")
        } else {
            "Unknown author".to_string()
        };
        let mut heading = format!(
            "Post {} - {} - {}",
            number,
            author,
            str_field(p, "created_at")
        );
        if let Some(reply_to) = p.get("reply_to_post_number").and_then(|v| v.as_i64()) {
            heading.push_str(&format!(" (reply to post {reply_to})"));
        }
        out.push(heading);
        out.push(crate::extract::html_to_text(&cooked));
    }
    if out.is_empty() {
        return None;
    }
    Some((title, out.join("\n\n")))
}
#[cfg(test)]
mod discourse_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discourse_fetches_missing_posts_in_batches_and_sorts_by_number() {
        let topic = json!({
            "title": "Big topic",
            "post_stream": {
                "posts": [
                    {"id": 11, "post_number": 2, "username": "bob", "name": "Bob B", "created_at": "2026-01-02", "cooked": "<p>Second</p>", "reply_to_post_number": 1},
                    {"id": 10, "post_number": 1, "username": "alice", "created_at": "2026-01-01", "cooked": "<p>First</p>"}
                ],
                "stream": [10, 11, 12]
            }
        });
        let page = json!({
            "post_stream": {"posts": [
                {"id": 12, "post_number": 3, "username": "carol", "created_at": "2026-01-03", "cooked": "<p>Third</p>"}
            ]}
        });
        let mut urls = Vec::new();
        let (title, text) = discourse_thread_from("https://forum.example", "42", &mut |url| {
            urls.push(url.to_string());
            Some(if urls.len() == 1 {
                topic.clone()
            } else {
                page.clone()
            })
        })
        .unwrap();
        assert_eq!(title, "Big topic");
        assert!(urls[0].ends_with("/t/42.json?print=true"));
        assert!(
            urls[1].contains("/t/42/posts.json?post_ids%5B%5D=12"),
            "{}",
            urls[1]
        );
        let first = text.find("Post 1 - @alice - 2026-01-01").unwrap();
        let second = text
            .find("Post 2 - Bob B - 2026-01-02 (reply to post 1)")
            .unwrap();
        let third = text.find("Post 3 - @carol - 2026-01-03").unwrap();
        assert!(first < second && second < third, "{text}");
        assert!(text.contains("First") && text.contains("Second") && text.contains("Third"));
    }

    #[test]
    fn discourse_fails_closed_without_a_topic_title() {
        assert!(
            discourse_thread_from("https://example.com", "1", &mut |_| Some(
                json!({"post_stream": {"posts": []}})
            ))
            .is_none()
        );
    }
}

#[cfg(test)]
mod source_key_tests {
    #[test]
    fn source_credentials_reject_other_origins_before_requesting() {
        for url in [
            "https://api.github.com.evil.test/repos/o/r",
            "http://api.github.com/repos/o/r",
            "https://api.github.com:444/repos/o/r",
            "https://name@api.github.com/repos/o/r",
        ] {
            assert!(
                super::source_api_json(url, "api.github.com", "fake-test-token", true).is_none()
            );
        }
    }

    #[test]
    fn source_key_prefers_first_nonempty_environment_then_config() {
        let mut cfg = crate::config::Config::default();
        *cfg.raw_mut() = serde_json::json!({"github_token":" config-token "});
        assert_eq!(
            super::resolve_source_key(
                &["FIRST", "SECOND"],
                "github_token",
                &cfg,
                |name| match name {
                    "FIRST" => Some("  ".into()),
                    "SECOND" => Some(" env-token ".into()),
                    _ => None,
                }
            ),
            "env-token"
        );
        assert_eq!(
            super::resolve_source_key(&[], "github_token", &cfg, |_| None),
            "config-token"
        );
    }
}

#[cfg(test)]
mod reddit_lemmy_tests {
    use super::*;
    use serde_json::json;

    // -- fixture helpers -------------------------------------------------

    fn t1(id: &str, author: &str, body: &str, parent: &str, created: f64, replies: Value) -> Value {
        json!({
            "kind": "t1",
            "data": {
                "id": id,
                "name": format!("t1_{id}"),
                "author": author,
                "body": body,
                "parent_id": parent,
                "created_utc": created,
                "replies": replies,
                "depth": 0
            }
        })
    }

    fn listing(children: Vec<Value>) -> Value {
        json!({"kind": "Listing", "data": {"children": children}})
    }

    fn more(id: &str, parent: &str, children: &[&str]) -> Value {
        json!({
            "kind": "more",
            "data": {
                "id": id,
                "parent_id": parent,
                "children": children,
                "count": children.len()
            }
        })
    }

    fn post_listing() -> Value {
        listing(vec![json!({
            "kind": "t3",
            "data": {
                "id": "art1",
                "name": "t3_art1",
                "title": "Launch thread",
                "author": "op",
                "created_utc": 1767225600.0,
                "selftext": "Opening body",
                "url": "https://www.reddit.com/r/rust/comments/art1/launch_thread/",
                "permalink": "/r/rust/comments/art1/launch_thread/"
            }
        })])
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    // -- URL parsing -----------------------------------------------------

    #[test]
    fn reddit_parts_accepts_permalinks_and_rejects_lookalikes() {
        assert_eq!(
            reddit_parts("https://www.reddit.com/r/rust/comments/art1/slug-here/?utm=share#frag"),
            Some(("rust".to_string(), "art1".to_string()))
        );
        assert_eq!(
            reddit_parts("https://old.reddit.com/r/AskReddit/comments/xyz"),
            Some(("AskReddit".to_string(), "xyz".to_string()))
        );
        assert_eq!(
            reddit_parts("https://www.reddit.com/r/rust/comments/art1/"),
            Some(("rust".to_string(), "art1".to_string()))
        );
        // Subreddit under the 2-char floor, non-thread paths,
        // foreign hosts and extra path segments all fail.
        assert_eq!(reddit_parts("https://www.reddit.com/r/r/comments/a1"), None);
        assert_eq!(
            reddit_parts("https://www.reddit.com/user/bob/comments/a1"),
            None
        );
        assert_eq!(
            reddit_parts("https://example.com/r/rust/comments/art1"),
            None
        );
        assert_eq!(
            reddit_parts("https://www.reddit.com/r/rust/comments/art1/slug/extra"),
            None
        );
    }

    #[test]
    fn lemmy_parts_ignores_query_and_fragment_suffixes() {
        // Audit 8 R03: the old parser folded "?…" into the id
        // and rejected the URL outright.
        assert_eq!(
            lemmy_parts("https://lemmy.example/post/123?sort=Hot#comment-5"),
            Some(("https://lemmy.example".to_string(), 123))
        );
        assert_eq!(
            lemmy_parts("https://lemmy.example:8443/post/7/"),
            Some(("https://lemmy.example:8443".to_string(), 7))
        );
        assert_eq!(lemmy_parts("https://lemmy.example/post/12a"), None);
        assert_eq!(lemmy_parts("https://lemmy.example/post/"), None);
        assert_eq!(lemmy_parts("https://lemmy.example/c/post/123"), None);
        assert_eq!(lemmy_parts("ftp://lemmy.example/post/3"), None);
    }

    // -- Reddit expansion (R02) ------------------------------------------

    #[test]
    fn reddit_expands_morechildren_and_continuations_in_parent_order() {
        let deep_more = more("_", "t1_d1", &["_"]);
        let d1 = t1(
            "d1",
            "dan",
            "Deep comment",
            "t1_c1",
            1767226200.0,
            listing(vec![deep_more]),
        );
        let c1 = t1(
            "c1",
            "cy",
            "Child comment",
            "t1_b1",
            1767226100.0,
            listing(vec![d1]),
        );
        let payload = json!([
            post_listing(),
            listing(vec![
                t1(
                    "a1",
                    "amy",
                    "First comment",
                    "t3_art1",
                    1767225900.0,
                    json!("")
                ),
                t1(
                    "b1",
                    "bob",
                    "Parent comment",
                    "t3_art1",
                    1767226000.0,
                    listing(vec![c1])
                ),
                more("m1", "t3_art1", &["e1", "f1"]),
            ])
        ]);
        let mut requested: Vec<String> = Vec::new();
        let (title, doc) = reddit_thread_html_from_payload(
            &payload,
            "https://www.reddit.com/r/rust/comments/art1/launch_thread/",
            &mut |url| {
                requested.push(url.to_string());
                if url.contains("/api/morechildren.json") && url.contains("children=e1%2Cf1") {
                    Some(json!({"json": {"data": {"things": [
                        t1("e1", "eve", "Hidden one", "t3_art1", 1767226300.0, json!("")),
                        t1("f1", "fred", "Hidden two", "t3_art1", 1767226400.0,
                           listing(vec![more("m2", "t1_f1", &["g1"])])),
                        // A duplicate of a1 must not be emitted twice.
                        t1("a1", "amy", "First comment", "t3_art1", 1767225900.0, json!("")),
                    ]}}}))
                } else if url.contains("/api/morechildren.json") && url.contains("children=g1") {
                    Some(json!({"json": {"data": {"things": [
                        t1("g1", "gwen", "Nested hidden", "t1_f1", 1767226500.0, json!("")),
                    ]}}}))
                } else if url.contains("/comments/art1/_/d1.json") {
                    Some(json!([
                        json!({}),
                        listing(vec![t1(
                            "h1",
                            "hank",
                            "Continuation child",
                            "t1_d1",
                            1767226600.0,
                            json!("")
                        )])
                    ]))
                } else {
                    None
                }
            },
        )
        .unwrap();
        assert_eq!(title, "Launch thread");
        assert!(doc.contains("<title>Launch thread</title>"));
        assert!(doc.contains("#1 Posted by u/op — 2026-01-01 00:00 UTC"));
        assert!(doc.contains("<p>Opening body</p>"));
        assert_eq!(count_occurrences(&doc, "<section"), 9);
        // Dedupe: a1 arrived twice, emitted once.
        assert_eq!(count_occurrences(&doc, "First comment"), 1);
        // Parent/child order is rebuilt after the flat expansion.
        let order = [
            "First comment",
            "Parent comment",
            "Child comment",
            "Deep comment",
            "Continuation child",
            "Hidden one",
            "Hidden two",
            "Nested hidden",
        ];
        let positions: Vec<usize> = order.iter().map(|b| doc.find(b).unwrap()).collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "order {positions:?} in {doc}"
        );
        // Dates and computed reply levels ride the headings.
        assert!(doc.contains("#2 Comment by u/amy — 2026-01-01 00:05 UTC — Reply level 1"));
        assert!(doc.contains("#6 Comment by u/hank"));
        assert!(doc.contains("Reply level 4"));
        // Exactly the expansion requests Python makes.
        assert_eq!(requested.len(), 3, "{requested:?}");
        assert!(requested.iter().any(|u| u.contains("morechildren")));
        assert!(requested.iter().any(|u| u.contains("_/d1.json")));
    }

    #[test]
    fn reddit_repeated_more_ids_terminate() {
        let payload = json!([
            post_listing(),
            listing(vec![more("m1", "t3_art1", &["x1"])])
        ]);
        let mut calls = 0usize;
        let (_title, doc) = reddit_thread_html_from_payload(
            &payload,
            "https://www.reddit.com/r/rust/comments/art1/",
            &mut |_url| {
                calls += 1;
                // The response keeps re-offering the same child.
                Some(json!({"json": {"data": {"things": [more("m2", "t3_art1", &["x1"])]}}}))
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(count_occurrences(&doc, "<section"), 1);
    }

    #[test]
    fn reddit_comment_cap_and_request_bound_hold() {
        let first_children: Vec<String> = (0..100).map(|i| format!("c{i}")).collect();
        let payload = json!([
            post_listing(),
            listing(vec![json!({
                "kind": "more",
                "data": {
                    "id": "m0",
                    "parent_id": "t3_art1",
                    "children": first_children,
                    "count": 100
                }
            })])
        ]);
        let mut calls = 0usize;
        let mut next = 0usize;
        let (_title, doc) = reddit_thread_html_from_payload(
            &payload,
            "https://www.reddit.com/r/rust/comments/art1/",
            &mut |_url| {
                calls += 1;
                let start = next;
                next += 100;
                let mut things: Vec<Value> = (start..next)
                    .map(|i| {
                        t1(
                            &format!("c{i}"),
                            "bot",
                            &format!("Body {i}"),
                            "t3_art1",
                            1767225900.0,
                            json!(""),
                        )
                    })
                    .collect();
                let more_children: Vec<String> =
                    (next..next + 100).map(|i| format!("c{i}")).collect();
                things.push(json!({
                    "kind": "more",
                    "data": {
                        "id": format!("m{start}"),
                        "parent_id": "t3_art1",
                        "children": more_children,
                        "count": 100
                    }
                }));
                Some(json!({"json": {"data": {"things": things}}}))
            },
        )
        .unwrap();
        assert_eq!(calls, REDDIT_MAX_MORE_REQUESTS);
        assert_eq!(count_occurrences(&doc, "<section"), REDDIT_MAX_COMMENTS + 1);
    }

    #[test]
    fn reddit_body_html_variants_match_python() {
        // Entity-escaped selftext_html is unescaped.
        let mut post = json!({
            "id": "p1", "name": "t3_p1", "title": "T", "author": "op",
            "created_utc": 1767225600.0,
            "selftext_html": "&lt;p&gt;One &lt;em&gt;two&lt;/em&gt;&lt;/p&gt;",
            "permalink": "/r/rust/comments/p1/t/"
        });
        let payload = json!([
            listing(vec![json!({"kind": "t3", "data": post.clone()})]),
            listing(vec![])
        ]);
        let (_t, doc) = reddit_thread_html_from_payload(
            &payload,
            "https://www.reddit.com/r/rust/comments/p1/t/",
            &mut |_| None,
        )
        .unwrap();
        assert!(doc.contains("<p>One <em>two</em></p>"), "{doc}");

        // Rendered body_html wins as-is; plain body splits into
        // paragraphs; a missing body is the placeholder.
        post["selftext"] = json!("Intro text");
        post["selftext_html"] = json!("");
        let payload = json!([
            listing(vec![json!({"kind": "t3", "data": post})]),
            listing(vec![
                json!({"kind": "t1", "data": {
                    "id": "k1", "name": "t1_k1", "author": "amy", "parent_id": "t3_p1",
                    "created_utc": 1767225900.0,
                    "body_html": "<p>Rendered <strong>body</strong></p>", "replies": ""
                }}),
                json!({"kind": "t1", "data": {
                    "id": "k2", "name": "t1_k2", "author": "bob", "parent_id": "t3_p1",
                    "created_utc": 1767225900.0,
                    "body": "Line one\n\nLine two", "replies": ""
                }}),
                json!({"kind": "t1", "data": {
                    "id": "k3", "name": "t1_k3", "author": "cy", "parent_id": "t3_p1",
                    "created_utc": 1767225900.0, "replies": ""
                }}),
            ])
        ]);
        let (_t, doc) = reddit_thread_html_from_payload(
            &payload,
            "https://www.reddit.com/r/rust/comments/p1/t/",
            &mut |_| None,
        )
        .unwrap();
        assert!(doc.contains("<p>Intro text</p>"), "{doc}");
        assert!(
            doc.contains("<p>Rendered <strong>body</strong></p>"),
            "{doc}"
        );
        assert!(doc.contains("<p>Line one</p><p>Line two</p>"), "{doc}");
        assert!(doc.contains("<p>[no text available]</p>"), "{doc}");
    }

    #[test]
    fn reddit_link_post_gets_linked_content_unless_it_is_the_permalink() {
        let mut post = json!({
            "id": "p2", "name": "t3_p2", "title": "Link", "author": "op",
            "created_utc": 1767225600.0, "selftext": "",
            "url": "https://example.com/story",
            "url_overridden_by_dest": "https://example.com/story",
            "permalink": "/r/rust/comments/p2/link/"
        });
        let build = |post: Value| {
            reddit_thread_html_from_payload(
                &json!([
                    listing(vec![json!({"kind": "t3", "data": post})]),
                    listing(vec![])
                ]),
                "https://www.reddit.com/r/rust/comments/p2/link/",
                &mut |_| None,
            )
            .unwrap()
            .1
        };
        let doc = build(post.clone());
        assert!(
            doc.contains("Linked content: <a href=\"https://example.com/story\">https://example.com/story</a>"),
            "{doc}"
        );
        post["url"] = json!("https://www.reddit.com/r/rust/comments/p2/link/");
        post["url_overridden_by_dest"] = json!("https://www.reddit.com/r/rust/comments/p2/link/");
        let doc = build(post);
        assert!(!doc.contains("Linked content"), "{doc}");
        assert!(doc.contains("<p>[no text available]</p>"), "{doc}");
    }

    #[test]
    fn reddit_rejects_malformed_payloads() {
        let url = "https://www.reddit.com/r/rust/comments/art1/";
        assert!(reddit_thread_html_from_payload(&json!({"nope": 1}), url, &mut |_| None).is_none());
        assert!(
            reddit_thread_html_from_payload(&json!([post_listing()]), url, &mut |_| None).is_none()
        );
        // No t3 in the first listing.
        let payload = json!([
            listing(vec![t1("a1", "amy", "x", "t3_art1", 1.0, json!(""))]),
            listing(vec![])
        ]);
        assert!(reddit_thread_html_from_payload(&payload, url, &mut |_| None).is_none());
        // Foreign thread URL.
        let payload = json!([post_listing(), listing(vec![])]);
        assert!(
            reddit_thread_html_from_payload(
                &payload,
                "https://example.com/r/rust/comments/art1/",
                &mut |_| None
            )
            .is_none()
        );
    }

    #[test]
    fn reddit_rss_fallback_document_dedupes_and_skips_empty() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<title>rust comments</title>
<entry><id>t3_art1</id><author><name>op</name></author><published>2026-01-01T00:00:00+00:00</published><content type="html">&lt;p&gt;Post body&lt;/p&gt;</content></entry>
<entry><id>t1_c1</id><author><name>amy</name></author><published>2026-01-01T01:00:00+00:00</published><content type="html">&lt;p&gt;Comment body&lt;/p&gt;</content></entry>
<entry><id>t1_c1</id><author><name>amy</name></author><published>2026-01-01T01:00:00+00:00</published><content type="html">&lt;p&gt;Comment body&lt;/p&gt;</content></entry>
<entry><id>t1_c2</id><author><name>bob</name></author><summary>Summary only</summary></entry>
<entry><id>t1_c3</id><author><name>cy</name></author><summary></summary></entry>
</feed>"#;
        let (title, doc) = reddit_rss_thread_document(xml).unwrap();
        assert_eq!(title, "rust comments");
        assert_eq!(count_occurrences(&doc, "<section"), 3);
        assert!(doc.contains("#1 Posted by u/op — 2026-01-01 00:00 UTC"));
        assert!(doc.contains("#2 Comment by u/amy — 2026-01-01 01:00 UTC"));
        assert!(doc.contains("Post body") && doc.contains("Comment body"));
        assert!(doc.contains("Summary only"));
        assert!(reddit_rss_thread_document(b"not a feed").is_none());
    }

    // -- Lemmy pagination (R03) ------------------------------------------

    fn lemmy_post_view() -> Value {
        json!({
            "post": {
                "id": 42,
                "name": "Big thread",
                "body": "Opening **post**",
                "published": "2026-02-01T10:00:00",
                "ap_id": "https://lemmy.example/post/42"
            },
            "creator": {"name": "op", "display_name": "The OP"}
        })
    }

    fn lemmy_comment(id: i64, path: &str, content: &str) -> Value {
        json!({
            "comment": {
                "id": id,
                "content": content,
                "path": path,
                "published": "2026-02-01T11:00:00",
                "ap_id": format!("https://lemmy.example/comment/{id}")
            },
            "creator": {"name": format!("u{id}")}
        })
    }

    #[test]
    fn lemmy_v3_pages_at_full_depth_in_api_order() {
        let mut requested: Vec<String> = Vec::new();
        let (title, doc) = lemmy_thread_html_from("https://lemmy.example", 42, &mut |url| {
            requested.push(url.to_string());
            if url.contains("/api/v3/post?") {
                return Some(json!({"post_view": lemmy_post_view()}));
            }
            if url.contains("/api/v3/comment/list") && url.contains("page=1") {
                let comments: Vec<Value> = (1..=50)
                    .map(|i| lemmy_comment(i, &format!("0.{i}"), &format!("Body {i} xyz")))
                    .collect();
                return Some(json!({"comments": comments}));
            }
            if url.contains("/api/v3/comment/list") && url.contains("page=2") {
                let mut comments: Vec<Value> = (51..=100)
                    .map(|i| lemmy_comment(i, &format!("0.{i}"), &format!("Body {i} xyz")))
                    .collect();
                comments[0] = lemmy_comment(51, "0.7.51", "Body 51 xyz");
                comments[1] = lemmy_comment(52, "0.7.8.52", "Body 52 xyz");
                comments[2] = lemmy_comment(53, "0.1.2.3.4.5.6.7.8.9.10.11.12.53", "Body 53 xyz");
                return Some(json!({"comments": comments}));
            }
            if url.contains("/api/v3/comment/list") && url.contains("page=3") {
                let mut comments: Vec<Value> = (101..=106)
                    .map(|i| lemmy_comment(i, &format!("0.{i}"), &format!("Body {i} xyz")))
                    .collect();
                // A repeat of an earlier id is deduplicated.
                comments.push(lemmy_comment(3, "0.3", "Body 3 xyz"));
                return Some(json!({"comments": comments}));
            }
            None
        })
        .unwrap();
        assert_eq!(title, "Big thread");
        assert_eq!(count_occurrences(&doc, "<section"), 107);
        assert!(doc.contains("#1 Posted by @The OP — 2026-02-01 10:00 UTC"));
        assert!(doc.contains("<strong>post</strong>"));
        // API order, not lexical path order: "0.9" precedes
        // "0.10" in the document although it sorts after it.
        assert!(doc.find("Body 9 xyz").unwrap() < doc.find("Body 10 xyz").unwrap());
        // Depths come from the path segments, beyond the old
        // max_depth=8 request.
        assert!(doc.contains("#52 Comment by @u51"));
        assert!(doc.contains("Reply level 2"));
        assert!(doc.contains("Reply level 3"));
        assert!(doc.contains("Reply level 13"));
        assert!(
            requested
                .iter()
                .any(|u| u.contains("max_depth=999") && u.contains("sort=Old")),
            "{requested:?}"
        );
        assert_eq!(
            requested
                .iter()
                .filter(|u| u.contains("comment/list"))
                .count(),
            3
        );
    }

    #[test]
    fn lemmy_v4_cursor_pagination_terminates_on_repeated_cursor() {
        let mut requested: Vec<String> = Vec::new();
        let (title, doc) = lemmy_thread_html_from(
            "https://lemmy.example",
            9,
            &mut |url| {
                requested.push(url.to_string());
                if url.contains("/api/v3/post?") {
                    return Some(json!({"error": "unknown version"}));
                }
                if url.contains("/api/v4/post?") {
                    return Some(json!({"post_view": {
                        "post": {"id": 9, "name": "V4 thread", "body": "", "published": "2026-03-01T09:00:00"},
                        "creator": {"name": "op"}
                    }}));
                }
                if url.contains("/api/v4/comment/list") && !url.contains("page_cursor=") {
                    return Some(json!({
                        "comments": [lemmy_comment(1, "0.1", "One"), lemmy_comment(2, "0.2", "Two")],
                        "next_page": "cursorA"
                    }));
                }
                if url.contains("/api/v4/comment/list") && url.contains("page_cursor=cursorA") {
                    return Some(json!({
                        "comments": [lemmy_comment(3, "0.3", "Three")],
                        "next_page": "cursorA"
                    }));
                }
                None
            },
        )
        .unwrap();
        assert_eq!(title, "V4 thread");
        assert_eq!(count_occurrences(&doc, "<section"), 4);
        assert_eq!(
            requested
                .iter()
                .filter(|u| u.contains("/api/v4/comment/list"))
                .count(),
            2,
            "{requested:?}"
        );
        assert!(!requested.iter().any(|u| u.contains("/api/v3/comment/list")));
    }

    #[test]
    fn lemmy_first_page_failure_fails_but_later_failure_keeps_comments() {
        // v4 page 1 without a comments list: the version fails
        // and, v3 having failed too, nothing is built.
        let result = lemmy_thread_html_from("https://lemmy.example", 9, &mut |url| {
            if url.contains("/api/v3/") {
                return Some(json!({"error": "nope"}));
            }
            if url.contains("/api/v4/post?") {
                return Some(json!({"post_view": {
                    "post": {"id": 9, "name": "V4", "published": "2026-03-01T09:00:00"},
                    "creator": {"name": "op"}
                }}));
            }
            Some(json!({"comments": {"not": "a list"}}))
        });
        assert!(result.is_none());

        // v3 page 2 breaks: the document keeps page 1.
        let (title, doc) = lemmy_thread_html_from("https://lemmy.example", 42, &mut |url| {
            if url.contains("/api/v3/post?") {
                return Some(json!({"post_view": lemmy_post_view()}));
            }
            if url.contains("page=1") {
                let comments: Vec<Value> = (1..=50)
                    .map(|i| lemmy_comment(i, &format!("0.{i}"), &format!("Body {i} xyz")))
                    .collect();
                return Some(json!({"comments": comments}));
            }
            Some(json!({"error": "boom"}))
        })
        .unwrap();
        assert_eq!(title, "Big thread");
        assert_eq!(count_occurrences(&doc, "<section"), 51);
    }

    // -- Lemmy document details ------------------------------------------

    #[test]
    fn lemmy_document_details_match_python() {
        let mut post_view = lemmy_post_view();
        post_view["creator"] = json!({"name": "handle", "display_name": ""});
        post_view["post"]["url"] = json!("https://example.com/story");
        let comments = vec![
            lemmy_comment(
                1,
                "0.1",
                "See [docs](https://example.com/docs) and **bold**\n\n- one\n- two",
            ),
            {
                let mut c = lemmy_comment(2, "0.2", "gone");
                c["comment"]["removed"] = json!(true);
                c
            },
            {
                let mut c = lemmy_comment(3, "0.3", "<script>alert(1)</script>");
                c["creator"] = json!({});
                c
            },
        ];
        let (title, doc) = lemmy_thread_document(&post_view, &comments).unwrap();
        assert_eq!(title, "Big thread");
        // Display name empty -> handle.
        assert!(doc.contains("#1 Posted by @handle"), "{doc}");
        // Post link differs from ap_id -> linked content line.
        assert!(
            doc.contains("Linked content: <a href=\"https://example.com/story\">"),
            "{doc}"
        );
        // Markdown renders; raw HTML stays escaped text.
        assert!(
            doc.contains("<a href=\"https://example.com/docs\">docs</a>"),
            "{doc}"
        );
        assert!(doc.contains("<strong>bold</strong>"), "{doc}");
        assert!(doc.contains("<li>one</li>"), "{doc}");
        assert!(
            doc.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{doc}"
        );
        assert!(!doc.contains("<script>alert"), "{doc}");
        // Removed marker wins over content; missing creator is [deleted].
        assert!(doc.contains("<p>[removed]</p>"), "{doc}");
        assert!(doc.contains("Comment by @[deleted]"), "{doc}");
        // Time labels: naive is UTC, offsets convert, junk passes through.
        assert_eq!(
            lemmy_time_label(Some(&json!("2026-03-04T05:06:07"))),
            "2026-03-04 05:06 UTC"
        );
        assert_eq!(
            lemmy_time_label(Some(&json!("2026-03-04T07:06:07+02:00"))),
            "2026-03-04 05:06 UTC"
        );
        assert_eq!(
            lemmy_time_label(Some(&json!("2026-03-04T05:06:07.999Z"))),
            "2026-03-04 05:06 UTC"
        );
        assert_eq!(lemmy_time_label(Some(&json!("not-a-date"))), "not-a-date");
        assert_eq!(lemmy_time_label(None), "");
        assert_eq!(
            reddit_time_label(Some(&json!(1767225900.0))),
            "2026-01-01 00:05 UTC"
        );
        assert_eq!(reddit_time_label(Some(&json!(0))), "");
    }
}
