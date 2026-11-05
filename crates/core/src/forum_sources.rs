//! Synthetic Google Groups and Groups.io archive-search subscriptions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::extract::{attr_value, find_elements, has_class, leaf_text};
use crate::feeds::{ParsedEntry, ParsedFeed};
use url::Url;

static SETTINGS: Mutex<(Option<PathBuf>, usize, usize)> = Mutex::new((None, 30, 100));

pub fn configure(cfg: &crate::config::Config, config_path: &Path) {
    *SETTINGS.lock().unwrap_or_else(|p| p.into_inner()) = (
        Some(config_path.to_owned()),
        cfg.get_i64("google_groups_max_items", 30).clamp(1, 100) as usize,
        cfg.get_i64("groups_io_search_max_items", 100)
            .clamp(1, 1000) as usize,
    );
}

fn web_url(value: &str) -> Option<Url> {
    let url = Url::parse(value.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

fn group_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

fn groups_io(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "groups.io" || host.ends_with(".groups.io"))
}

fn parts(url: &Url) -> Vec<&str> {
    url.path().trim_matches('/').split('/').collect()
}

/// Recognize synthetic subscriptions before ordinary RSS discovery.
pub fn subscription_url(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['<', '>', '`']);
    let email = value.strip_prefix("mailto:").unwrap_or(value);
    if let Some((name, domain)) = email.split_once('@')
        && domain.eq_ignore_ascii_case("googlegroups.com")
        && group_name(name)
    {
        return Some(format!("https://groups.google.com/g/{name}"));
    }
    let mut url = web_url(value)?;
    if url.host_str() == Some("groups.google.com") {
        if let Some(name) = url.fragment().and_then(|f| f.strip_prefix("!forum/"))
            && group_name(name)
        {
            return Some(format!("https://groups.google.com/g/{name}"));
        }
        let segments = parts(&url);
        if segments.len() == 2 && segments[0] == "g" && group_name(segments[1]) {
            return Some(format!("https://groups.google.com/g/{}", segments[1]));
        }
    }
    if groups_io(&url) {
        let segments = parts(&url);
        if segments.len() == 3 && segments[0] == "g" && segments[2] == "search" {
            url.set_fragment(None);
            return Some(url.to_string());
        }
    }
    None
}

/// Native feeds exposed by Groups.io and Google Developer Forums.
pub fn native_feed_url(value: &str) -> Option<String> {
    let mut url = web_url(value)?;
    let segments = parts(&url);
    if groups_io(&url)
        && segments.len() >= 2
        && segments[0] == "g"
        && (segments.len() == 2
            || (segments.len() == 3
                && matches!(segments[2], "topics" | "messages" | "rss" | "search")))
    {
        let path = format!("/g/{}/rss", segments[1]);
        url.set_scheme("https").ok()?;
        url.set_path(&path);
        url.set_query(None);
        url.set_fragment(None);
        return Some(url.to_string());
    }
    if url.host_str() == Some("discuss.google.dev") {
        let path = url.path().trim_end_matches('/');
        let path = if matches!(path, "" | "/latest" | "/categories") {
            "/latest.rss".to_string()
        } else if (segments.len() == 3 && segments[0] == "c" && segments[2].parse::<u64>().is_ok())
            || (segments.len() == 2 && segments[0] == "tag")
        {
            format!("{path}.rss")
        } else if segments.first() == Some(&"t") {
            let index = if segments.get(1).is_some_and(|s| s.parse::<u64>().is_ok()) {
                1
            } else {
                2
            };
            let id = segments.get(index)?.parse::<u64>().ok()?;
            format!("/t/-/{id}.rss")
        } else {
            return None;
        };
        url.set_path(&path);
        url.set_query(if path == "/latest.rss" {
            Some("order=created")
        } else {
            None
        });
        url.set_fragment(None);
        return Some(url.to_string());
    }
    None
}

fn class_text(html: &str, class: &str) -> String {
    find_elements(html, None, &|tag| has_class(tag, class))
        .first()
        .map(|span| leaf_text(&html[span.inner_start..span.inner_end]))
        .unwrap_or_default()
}

fn entry(
    url: String,
    title: String,
    author: String,
    summary: String,
    date: Option<String>,
) -> ParsedEntry {
    ParsedEntry {
        id: url.clone(),
        url,
        title,
        author: (!author.is_empty()).then_some(author),
        content_text: summary.clone(),
        summary,
        date,
        media_url: None,
        media_type: None,
        tags: Vec::new(),
        duration: None,
    }
}

/// Parse server-rendered Google Groups conversation rows, without JavaScript.
pub fn parse_google_group(html: &str, source: &str, limit: usize) -> Result<ParsedFeed, String> {
    let source = subscription_url(source).ok_or("Not a Google Groups group address")?;
    if !source.starts_with("https://groups.google.com/g/") {
        return Err("Not a Google Groups group address".into());
    }
    let text = leaf_text(html).to_lowercase();
    if text.contains("you don't have permission to access this content")
        || text.contains("you don’t have permission to access this content")
    {
        return Err("This Google Group requires sign-in. Import its browser cookies with Tools > Import Site Cookies, then refresh the feed.".into());
    }
    let title = find_elements(html, Some("title"), &|_| true)
        .first()
        .map(|span| leaf_text(&html[span.inner_start..span.inner_end]))
        .unwrap_or_default()
        .trim_end_matches(" - Google Groups")
        .trim()
        .to_string();
    let mut feed = ParsedFeed {
        title: if title.is_empty() {
            source.rsplit('/').next().unwrap_or("Google Groups").into()
        } else {
            title
        },
        entries: Vec::new(),
    };
    let root = Url::parse("https://groups.google.com/").unwrap();
    let mut seen = HashSet::new();
    for span in find_elements(html, None, &|tag| attr_value(tag, "data-rowid").is_some()) {
        let row = &html[span.start..span.end];
        let mut subject = class_text(row, "o1DPKc");
        if subject.is_empty() {
            subject = class_text(row, "t17a0d");
        }
        if subject.is_empty() {
            subject = attr_value(&html[span.start..span.inner_start], "aria-label")
                .unwrap_or_default()
                .trim_start_matches("Select ")
                .to_string();
        }
        let href = find_elements(row, Some("a"), &|tag| {
            attr_value(tag, "href").is_some_and(|href| href.contains("/c/"))
        })
        .first()
        .and_then(|span| attr_value(&row[span.start..span.inner_start], "href"));
        let Some(url) = href.and_then(|href| root.join(&href).ok()) else {
            continue;
        };
        let segments = parts(&url);
        if url.host_str() != root.host_str()
            || segments.len() < 4
            || segments[0] != "g"
            || segments[2] != "c"
            || !group_name(segments[1])
            || !group_name(segments[3])
            || subject.is_empty()
        {
            continue;
        }
        let canonical = format!(
            "https://groups.google.com/g/{}/c/{}",
            segments[1], segments[3]
        );
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let mut names = Vec::new();
        for span in find_elements(row, None, &|tag| has_class(tag, "z0zUgf")) {
            let name = leaf_text(&row[span.inner_start..span.inner_end]);
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        let published = class_text(row, "tRlaM");
        feed.entries.push(entry(
            canonical,
            subject,
            names.join(", "),
            class_text(row, "WzoK"),
            (!published.is_empty()).then_some(published),
        ));
        if feed.entries.len() >= limit.clamp(1, 100) {
            break;
        }
    }
    Ok(feed)
}

fn canonical_topic(url: &Url) -> Option<String> {
    let segments = parts(url);
    if !groups_io(url) || segments.len() < 4 || segments[0] != "g" || segments[2] != "topic" {
        return None;
    }
    let id = segments.last()?.parse::<u64>().ok()?;
    Some(format!(
        "https://{}/g/{}/topic/{id}",
        url.host_str()?,
        segments[1]
    ))
}

fn timestamp(seconds: i64) -> String {
    // Gregorian civil date from Unix days; avoids locale-dependent shell parsing.
    let days = seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    let tod = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    )
}

/// Parse one archive-search page and its safe, same-group next-page URL.
pub fn parse_groups_search(
    html: &str,
    source: &str,
    limit: usize,
) -> Result<(ParsedFeed, Option<String>), String> {
    let url = web_url(source)
        .filter(groups_io)
        .ok_or("Not a Groups.io search URL")?;
    let segments = parts(&url);
    if segments.len() != 3 || segments[0] != "g" || segments[2] != "search" {
        return Err("Not a Groups.io search URL".into());
    }
    let query = url
        .query_pairs()
        .find(|(key, _)| key == "q")
        .map(|(_, value)| value.to_string())
        .unwrap_or_default();
    let title = if query.is_empty() {
        format!("{} search", segments[1])
    } else {
        format!("{} search: {query}", segments[1])
    };
    let mut feed = ParsedFeed {
        title,
        entries: Vec::new(),
    };
    let mut seen = HashSet::new();
    static TIME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"DisplayShortTime\s*\(\s*([0-9]{10,})").unwrap()
    });
    for span in find_elements(html, Some("td"), &|tag| attr_value(tag, "id").is_some()) {
        let cell = &html[span.inner_start..span.inner_end];
        let link = find_elements(cell, Some("a"), &|tag| {
            attr_value(tag, "href").is_some_and(|href| href.contains("/topic/"))
        })
        .into_iter()
        .next();
        let Some(link) = link else { continue };
        let href = attr_value(&cell[link.start..link.inner_start], "href").unwrap_or_default();
        let Some(topic) = url
            .join(&href)
            .ok()
            .filter(|u| u.host_str() == url.host_str())
            .and_then(|u| canonical_topic(&u))
        else {
            continue;
        };
        if !seen.insert(topic.clone()) {
            continue;
        }
        let title = leaf_text(&cell[link.inner_start..link.inner_end]);
        let ns = TIME.captures(cell).and_then(|c| c[1].parse::<i64>().ok());
        let date = ns.map(|ns| timestamp(ns / 1_000_000_000));
        feed.entries.push(entry(
            topic,
            if title.is_empty() {
                "Groups.io topic".into()
            } else {
                title
            },
            class_text(cell, "user-chip-name"),
            class_text(cell, "truncate-two-lines"),
            date,
        ));
        if feed.entries.len() >= limit.clamp(1, 1000) {
            break;
        }
    }
    let next = find_elements(html, Some("a"), &|tag| attr_value(tag, "href").is_some())
        .into_iter()
        .find_map(|span| {
            let tag = &html[span.start..span.inner_start];
            let label = leaf_text(&html[span.inner_start..span.inner_end]).to_lowercase();
            if !matches!(label.as_str(), "next" | "next page" | "older")
                && !attr_value(tag, "rel")
                    .is_some_and(|v| v.split_whitespace().any(|r| r == "next"))
            {
                return None;
            }
            let next = url.join(&attr_value(tag, "href")?).ok()?;
            (next.origin() == url.origin() && next.path() == url.path()).then(|| next.to_string())
        });
    Ok((feed, next))
}

fn fetch(url: &str, config_path: Option<&Path>) -> Result<String, String> {
    let jar = config_path
        .map(crate::cookies::load_jar_beside)
        .unwrap_or_default();
    let header = crate::cookies::cookie_header_for(&jar, url);
    crate::extract::fetch_html_with_cookies(url, &header)
}

/// None means ordinary feed retrieval should handle this URL.
pub fn fetch_feed(value: &str) -> Option<Result<ParsedFeed, String>> {
    let source = subscription_url(value)?;
    let (config_path, google_limit, search_limit) =
        SETTINGS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    Some((|| {
        if source.starts_with("https://groups.google.com/g/") {
            let html = fetch(&format!("{source}?hl=en"), config_path.as_deref())?;
            return parse_google_group(&html, &source, google_limit);
        }
        let mut next = Some(source);
        let mut visited = HashSet::new();
        let mut seen = HashSet::new();
        let mut result = ParsedFeed {
            title: String::new(),
            entries: Vec::new(),
        };
        while let Some(url) = next.take() {
            if !visited.insert(url.clone())
                || visited.len() > 500
                || result.entries.len() >= search_limit
            {
                break;
            }
            let html = fetch(&url, config_path.as_deref())?;
            let (page, more) =
                parse_groups_search(&html, &url, search_limit - result.entries.len())?;
            if result.title.is_empty() {
                result.title = page.title
            }
            result.entries.extend(
                page.entries
                    .into_iter()
                    .filter(|e| seen.insert(e.url.clone())),
            );
            next = more;
        }
        Ok(result)
    })())
}
/// Authenticated Groups.io topic body; absent credentials or API failure use normal extraction.
pub fn topic_html(value: &str) -> Option<(String, String)> {
    let url = web_url(value)?;
    let canonical = canonical_topic(&url)?;
    let id = canonical.rsplit('/').next()?.parse().ok()?;
    let key = crate::threads::source_key(&["BLINDRSS_GROUPS_IO_API_KEY"], "groups_io_api_key");
    if key.is_empty() {
        return None;
    }
    api_topic_document(id, &canonical, &mut |path| {
        crate::threads::source_api_json(
            &format!("https://groups.io{path}"),
            "groups.io",
            &key,
            false,
        )
    })
}

fn api_topic_document(
    id: u64,
    canonical: &str,
    get: &mut impl FnMut(&str) -> Option<serde_json::Value>,
) -> Option<(String, String)> {
    use serde_json::Value;
    let field = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let mut token = String::new();
    let mut title = String::new();
    let mut messages = Vec::new();
    let mut tokens = HashSet::new();
    for _ in 0..200 {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("topic_id", &id.to_string())
            .append_pair("limit", "100");
        if !token.is_empty() {
            query.append_pair("page_token", &token);
        }
        let response = get(&format!("/api/v1/gettopic?{}", query.finish()))?;
        let page = if let Some(items) = response.as_array() {
            items.iter().rev().find(|item| {
                item.is_object()
                    && (item.get("object").and_then(Value::as_str) == Some("list")
                        || ["data", "messages", "next_page_token", "total_count"]
                            .iter()
                            .any(|key| item.get(key).is_some()))
            })?
        } else {
            &response
        };
        if title.is_empty() {
            title = field(page, "subject");
            if title.is_empty() {
                title = field(page, "title");
            }
        }
        if let Some(items) = page
            .get("data")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .or_else(|| page.get("messages").and_then(Value::as_array))
        {
            messages.extend(items.iter().filter(|v| v.is_object()).cloned());
        }
        let next = field(page, "next_page_token");
        if messages.len() >= 10_000 || next.is_empty() || !tokens.insert(next.clone()) {
            break;
        }
        token = next;
    }
    if messages.is_empty() {
        return None;
    }
    if title.is_empty() {
        title = field(&messages[0], "subject");
    }
    if title.is_empty() {
        title = "Groups.io topic".into();
    }
    let mut sections = String::new();
    let mut seen = HashSet::new();
    for (index, message) in messages.iter().take(10_000).enumerate() {
        let number = message
            .get("msg_num")
            .or_else(|| message.get("number"))
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_else(|| (index + 1).to_string());
        if !seen.insert(number.clone()) {
            continue;
        }
        let mut author = field(message, "name");
        if author.is_empty() {
            author = field(message, "author");
        }
        if author.is_empty() {
            author = "Unknown author".into();
        }
        let created = message
            .get("created")
            .or_else(|| message.get("published"))
            .map(|v| {
                if let Some(number) = v.as_f64() {
                    let seconds = if number > 10_000_000_000_000.0 {
                        number / 1_000_000_000.0
                    } else if number > 10_000_000_000.0 {
                        number / 1000.0
                    } else {
                        number
                    };
                    if (-62_135_596_800.0..=253_402_300_799.0).contains(&seconds) {
                        timestamp(seconds as i64)
                    } else {
                        String::new()
                    }
                } else {
                    v.as_str().unwrap_or("").trim().to_string()
                }
            })
            .unwrap_or_default();
        let heading = format!(
            "#{} Message by {author}{} — Message #{number}",
            index + 1,
            if created.is_empty() {
                String::new()
            } else {
                format!(" — {created}")
            }
        );
        let raw = field(message, "body");
        let mut body = if message.get("is_plain_text").and_then(Value::as_bool) == Some(true) {
            format!(
                "<p>{}</p>",
                crate::threads::escape_source_html(&raw).replace('\n', "<br>")
            )
        } else {
            ammonia::clean(&raw)
        };
        if let Some(attachments) = message.get("attachments").and_then(Value::as_array) {
            for attachment in attachments {
                let href = [field(attachment, "download_url"), field(attachment, "url")]
                    .into_iter()
                    .find(|v| !v.is_empty())
                    .unwrap_or_default();
                let label = [field(attachment, "name"), field(attachment, "filename")]
                    .into_iter()
                    .find(|v| !v.is_empty())
                    .unwrap_or_else(|| "Attachment".into());
                if !href.is_empty() {
                    body.push_str(&format!(
                        "<p><a href=\"{}\">{}</a></p>",
                        crate::threads::escape_source_html(&href),
                        crate::threads::escape_source_html(&label)
                    ));
                }
            }
        }
        sections.push_str(&format!("<section class=\"blindrss-groupsio-post\"><h2>{}</h2><div class=\"blindrss-groupsio-body\">{body}</div></section>", crate::threads::escape_source_html(&heading)));
    }
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><p><a href=\"{}\">Open topic on Groups.io</a></p>{sections}</body></html>",
        crate::threads::escape_source_html(&title),
        crate::threads::escape_source_html(&title),
        crate::threads::escape_source_html(canonical)
    );
    Some((title, html))
}

// ---------------------------------------------------------------------------
// HTML forum thread linearization (Python article_extractor._FORUM_LAYOUTS,
// _forum_blocks, _extract_forum_thread_text; article_html._forum_reconstruct).
//
// Discussion sites render a thread as a flat list of sibling post blocks
// with no single node holding the conversation, so generic extraction
// keeps one block and throws the rest away. Each layout names the same
// parts, so one linearizer serves every platform: every post becomes a
// "#<number> <byline> — <time>" header line followed by its body, with
// per-post junk (signatures, controls) stripped and ellipsized link text
// replaced by the real href. The Rust extractor has no CSS engine, so
// the layouts are expressed over the balanced tag scanner in
// crate::extract (tag / class / attribute steps, descendant chains).
// ---------------------------------------------------------------------------

use crate::extract::{ElemSpan, remove_elements};

#[derive(Clone, Copy, Default)]
struct Step {
    tag: Option<&'static str>,
    class: Option<&'static str>,
    attr: Option<&'static str>,
    attr_eq: Option<&'static str>,
}

const fn tag_step(tag: &'static str) -> Step {
    Step {
        tag: Some(tag),
        class: None,
        attr: None,
        attr_eq: None,
    }
}

const fn class_step(class: &'static str) -> Step {
    Step {
        tag: None,
        class: Some(class),
        attr: None,
        attr_eq: None,
    }
}

const fn tag_class_step(tag: &'static str, class: &'static str) -> Step {
    Step {
        tag: Some(tag),
        class: Some(class),
        attr: None,
        attr_eq: None,
    }
}

fn step_matches(step: &Step, open: &str) -> bool {
    if let Some(class) = step.class
        && !has_class(open, class)
    {
        return false;
    }
    match (step.attr, step.attr_eq) {
        (Some(name), Some(value)) => attr_value(open, name).as_deref() == Some(value),
        (Some(name), None) => attr_value(open, name).is_some(),
        _ => true,
    }
}

/// All elements matching a descendant selector chain, in document order.
fn select(html: &str, steps: &[Step]) -> Vec<ElemSpan> {
    let mut spans = vec![ElemSpan {
        start: 0,
        end: html.len(),
        inner_start: 0,
        inner_end: html.len(),
    }];
    for step in steps {
        let mut next = Vec::new();
        for span in &spans {
            let frag = &html[span.inner_start..span.inner_end];
            for found in find_elements(frag, step.tag, &|open: &str| step_matches(step, open)) {
                next.push(ElemSpan {
                    start: span.inner_start + found.start,
                    end: span.inner_start + found.end,
                    inner_start: span.inner_start + found.inner_start,
                    inner_end: span.inner_start + found.inner_end,
                });
            }
        }
        spans = next;
        if spans.is_empty() {
            break;
        }
    }
    spans
}

fn select_first(html: &str, steps: &[Step]) -> Option<ElemSpan> {
    select(html, steps).into_iter().next()
}

fn span_text(html: &str, span: &ElemSpan) -> String {
    leaf_text(&html[span.inner_start..span.inner_end])
}

/// Python `_forum_node_text`: the first match per selector, whitespace
/// collapsed, joined with " — ".
fn node_text(html: &str, selectors: &[&[Step]]) -> String {
    let mut bits = Vec::new();
    for selector in selectors {
        if let Some(span) = select_first(html, selector) {
            let text = span_text(html, &span);
            let text = fix_header_punct(&text);
            if !text.is_empty() {
                bits.push(text);
            }
        }
    }
    bits.join(" — ")
}

fn fix_header_punct(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ' ' && matches!(chars.peek(), Some(',' | '.' | ';' | ':')) {
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

struct ForumLayout {
    name: &'static str,
    lead: Option<&'static [Step]>,
    lead_body: &'static [Step],
    lead_header: &'static [&'static [Step]],
    post: &'static [Step],
    post_body: &'static [Step],
    post_number: Option<&'static [Step]>,
    post_header: &'static [&'static [Step]],
    post_time: Option<&'static [Step]>,
    junk: &'static [&'static [Step]],
}

// Semantic reconstructions (this module's and threads.rs' documents):
// the generated <h2> already carries number, author and timestamp.
const SEMANTIC_NUMBER: &[Step] = &[tag_step("h2")];

const GOOGLE_GROUPS_LAYOUT: ForumLayout = ForumLayout {
    name: "googlegroups",
    lead: None,
    lead_body: &[],
    lead_header: &[],
    post: &[tag_class_step("section", "blindrss-googlegroups-post")],
    post_body: &[class_step("blindrss-googlegroups-body")],
    post_number: Some(SEMANTIC_NUMBER),
    post_header: &[],
    post_time: None,
    junk: &[],
};

const GOOGLE_SUPPORT_LAYOUT: ForumLayout = ForumLayout {
    name: "googlesupport",
    lead: None,
    lead_body: &[],
    lead_header: &[],
    post: &[tag_class_step("section", "blindrss-googlesupport-post")],
    post_body: &[class_step("blindrss-googlesupport-body")],
    post_number: Some(SEMANTIC_NUMBER),
    post_header: &[],
    post_time: None,
    junk: &[],
};

// Invision Community 4 — nsaneforums.com. The topic opener and every
// reply use the same post element; recommended-post cards elsewhere on
// the page do not match, so they are not duplicated in the reader.
const INVISION_POST: &[Step] = &[tag_class_step("article", "ipsComment")];
const INVISION_BODY: &[Step] = &[Step {
    tag: None,
    class: None,
    attr: Some("data-role"),
    attr_eq: Some("commentContent"),
}];
const INVISION_HEADER: &[&[Step]] = &[&[tag_class_step("h3", "cAuthorPane_author")]];
const TIME_STEP: &[Step] = &[tag_step("time")];
const INVISION_LAYOUT: ForumLayout = ForumLayout {
    name: "invision",
    lead: None,
    lead_body: &[],
    lead_header: &[],
    post: INVISION_POST,
    post_body: INVISION_BODY,
    post_number: None,
    post_header: INVISION_HEADER,
    post_time: Some(TIME_STEP),
    junk: &[],
};

// vBulletin 4 — filesharingtalk.com. The empty template postbit at the
// bottom of vBulletin pages has no id; requiring one prevents it from
// becoming a blank phantom reply.
const VBULLETIN_POST: &[Step] = &[Step {
    tag: Some("li"),
    class: Some("postbitlegacy"),
    attr: Some("id"),
    attr_eq: None,
}];
const VBULLETIN_BODY: &[Step] = &[tag_class_step("blockquote", "postcontent")];
const VBULLETIN_NUMBER: &[Step] = &[class_step("postcounter")];
const VBULLETIN_HEADER: &[&[Step]] = &[&[class_step("username")]];
const VBULLETIN_TIME: &[Step] = &[class_step("postdate")];
const VBULLETIN_JUNK: &[&[Step]] = &[&[class_step("signature")]];
const VBULLETIN_LAYOUT: ForumLayout = ForumLayout {
    name: "vbulletin4",
    lead: None,
    lead_body: &[],
    lead_header: &[],
    post: VBULLETIN_POST,
    post_body: VBULLETIN_BODY,
    post_number: Some(VBULLETIN_NUMBER),
    post_header: VBULLETIN_HEADER,
    post_time: Some(VBULLETIN_TIME),
    junk: VBULLETIN_JUNK,
};

// FluxBB / PunBB — audiogames.net. `.post-num` is the number the thread
// itself uses, which posters quote at each other.
const FLUXBB_POST: &[Step] = &[tag_class_step("div", "post")];
const FLUXBB_BODY: &[Step] = &[class_step("post-entry"), class_step("entry-content")];
const FLUXBB_NUMBER: &[Step] = &[class_step("post-num")];
const FLUXBB_HEADER: &[&[Step]] = &[&[class_step("post-byline")]];
const FLUXBB_TIME: &[Step] = &[class_step("post-link")];
const FLUXBB_JUNK: &[&[Step]] = &[
    &[class_step("sig-content")],
    &[class_step("post-options")],
    &[class_step("postfoot")],
];
const FLUXBB_LAYOUT: ForumLayout = ForumLayout {
    name: "fluxbb",
    lead: None,
    lead_body: &[],
    lead_header: &[],
    post: FLUXBB_POST,
    post_body: FLUXBB_BODY,
    post_number: Some(FLUXBB_NUMBER),
    post_header: FLUXBB_HEADER,
    post_time: Some(FLUXBB_TIME),
    junk: FLUXBB_JUNK,
};

// Drupal comment threads — applevis.com. The opening post is a different
// element from the replies.
const DRUPAL_LEAD: &[Step] = &[tag_class_step("article", "node")];
const DRUPAL_LEAD_BODY: &[Step] = &[class_step("node__content"), class_step("field--name-body")];
const DRUPAL_LEAD_HEADER: &[&[Step]] = &[&[class_step("node__meta"), tag_step("p")]];
const DRUPAL_POST: &[Step] = &[tag_class_step("article", "comment")];
const DRUPAL_BODY: &[Step] = &[class_step("comment__text-content")];
const DRUPAL_HEADER: &[&[Step]] = &[
    &[class_step("comment__title"), tag_step("h3"), tag_step("a")],
    &[class_step("comment__author")],
];
const DRUPAL_JUNK: &[&[Step]] = &[
    &[class_step("comment__links")],
    &[tag_class_step("ul", "links")],
];
const DRUPAL_LAYOUT: ForumLayout = ForumLayout {
    name: "drupal",
    lead: Some(DRUPAL_LEAD),
    lead_body: DRUPAL_LEAD_BODY,
    lead_header: DRUPAL_LEAD_HEADER,
    post: DRUPAL_POST,
    post_body: DRUPAL_BODY,
    post_number: None,
    post_header: DRUPAL_HEADER,
    post_time: None,
    junk: DRUPAL_JUNK,
};

// XenForo 2 — no Python layout exists (the audit's gap 3 names XenForo,
// but the Python tree ships none); this follows the same _FORUM_LAYOUTS
// shape against XenForo 2's standard thread markup: one
// article.message--post per post, body in .bbWrapper, author in
// .message-name, post number in the attribution-opposite link.
const XENFORO_POST: &[Step] = &[tag_class_step("article", "message--post")];
const XENFORO_BODY: &[Step] = &[class_step("bbWrapper")];
const XENFORO_NUMBER: &[Step] = &[class_step("message-attribution-opposite"), tag_step("a")];
const XENFORO_HEADER: &[&[Step]] = &[&[class_step("message-name")]];
const XENFORO_JUNK: &[&[Step]] = &[&[class_step("message-signature")]];
const XENFORO_LAYOUT: ForumLayout = ForumLayout {
    name: "xenforo",
    lead: None,
    lead_body: &[],
    lead_header: &[],
    post: XENFORO_POST,
    post_body: XENFORO_BODY,
    post_number: Some(XENFORO_NUMBER),
    post_header: XENFORO_HEADER,
    post_time: Some(TIME_STEP),
    junk: XENFORO_JUNK,
};

const FORUM_LAYOUTS: &[&ForumLayout] = &[
    &GOOGLE_GROUPS_LAYOUT,
    &GOOGLE_SUPPORT_LAYOUT,
    &INVISION_LAYOUT,
    &VBULLETIN_LAYOUT,
    &FLUXBB_LAYOUT,
    &DRUPAL_LAYOUT,
    &XENFORO_LAYOUT,
];

/// The layout whose reply markup this page uses, keyed on the reply
/// blocks, never on the opening post alone (Python `_forum_layout_of`).
fn forum_layout_of(html: &str) -> Option<&'static ForumLayout> {
    FORUM_LAYOUTS
        .iter()
        .copied()
        .find(|layout| !select(html, layout.post).is_empty())
}

/// Hosts whose pages Python routes through the forum linearizer
/// (`_FORUM_THREAD_HOSTS`); the API/web reconstructors have their own
/// paths. XenForo markup is distinctive enough to key on markup alone.
pub fn is_forum_thread_host(url: &str) -> bool {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default();
    [
        "audiogames.net",
        "applevis.com",
        "filesharingtalk.com",
        "nsaneforums.com",
    ]
    .iter()
    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn step_of(selector: &[Step]) -> Option<&Step> {
    selector.first()
}

/// Strip per-post junk, expand truncated links, and drop the body
/// wrapper's own class/id (Python `_clean_forum_body`).
fn clean_forum_body(body_html: &str, layout: &ForumLayout) -> String {
    let junk_steps: Vec<Step> = layout
        .junk
        .iter()
        .filter_map(|sel| step_of(sel).copied())
        .collect();
    let (mut body, _) = remove_elements(body_html, &|name: &str, open: &str| {
        junk_steps
            .iter()
            .any(|step| (step.tag.is_none() || step.tag == Some(name)) && step_matches(step, open))
    });
    body = expand_truncated_link_text(&body);
    strip_root_class_id(&body)
}

fn strip_root_class_id(html: &str) -> String {
    let Some(gt) = html.find('>') else {
        return html.to_string();
    };
    let open = &html[..=gt];
    static ATTR_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\s+(class|id)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap()
    });
    format!("{}{}", ATTR_RE.replace_all(open, ""), &html[gt + 1..])
}

/// FluxBB shortens a long link's visible text to "https://start … end"
/// while the href stays intact; spoken aloud that form is useless, so
/// anchors whose text is a truncated URL are shown as their href
/// (Python `_expand_truncated_link_text`).
fn expand_truncated_link_text(body: &str) -> String {
    static TRUNCATED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)^https?://\S*\s*(…|\.\.\.)\s*\S*$").unwrap()
    });
    let mut out = body.to_string();
    for span in select(body, &[tag_step("a")]).into_iter().rev() {
        let open = &body[span.start..span.inner_start];
        let Some(href) = attr_value(open, "href") else {
            continue;
        };
        let href = href.trim().to_string();
        if href.is_empty() {
            continue;
        }
        let text = leaf_text(&body[span.inner_start..span.inner_end]);
        if !text.is_empty() && TRUNCATED.is_match(&text) {
            out.replace_range(span.inner_start..span.inner_end, &href);
        }
    }
    out
}

/// One linearized post: its "#3 Reply by alice — …" header line and its
/// cleaned body HTML.
#[derive(Debug, Clone, PartialEq)]
pub struct ForumBlock {
    pub header: String,
    pub body_html: String,
}

fn post_header(html: &str, layout: &ForumLayout, index: usize) -> String {
    let mut number = String::new();
    if let Some(selector) = layout.post_number
        && let Some(span) = select_first(html, selector)
    {
        number = span_text(html, &span);
    }
    if number.is_empty() {
        number = format!("#{index}");
    }
    let mut selectors: Vec<&[Step]> = layout.post_header.to_vec();
    if let Some(time) = layout.post_time {
        selectors.push(time);
    }
    let rest = node_text(html, &selectors);
    if rest.is_empty() {
        number
    } else {
        format!("{number} {rest}")
    }
}

/// (header, body) for the opening post and every reply, in order
/// (Python `_forum_blocks`). Posts whose body text is empty are dropped
/// after numbering, as in Python's two consumers.
pub fn forum_blocks(html: &str) -> Vec<ForumBlock> {
    let Some(layout) = forum_layout_of(html) else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    let mut index = 1usize;
    if let Some(lead_sel) = layout.lead
        && let Some(lead) = select_first(html, lead_sel)
    {
        let lead_html = &html[lead.start..lead.end];
        if let Some(body) = select_first(lead_html, layout.lead_body) {
            let header = {
                let text = node_text(lead_html, layout.lead_header);
                if text.is_empty() {
                    format!("#{index}")
                } else {
                    format!("#{index} {text}")
                }
            };
            blocks.push((header, body, lead_html));
            index += 1;
        }
    }
    for post in select(html, layout.post) {
        let post_html = &html[post.start..post.end];
        let Some(body) = select_first(post_html, layout.post_body) else {
            continue;
        };
        let header = post_header(post_html, layout, index);
        blocks.push((header, body, post_html));
        index += 1;
    }
    blocks
        .into_iter()
        .filter_map(|(header, body, post_html)| {
            let body_html = &post_html[body.start..body.end];
            let cleaned = clean_forum_body(body_html, layout);
            if crate::extract::leaf_text(&cleaned).is_empty() {
                return None;
            }
            Some(ForumBlock {
                header,
                body_html: cleaned,
            })
        })
        .collect()
}

/// Linearize a discussion thread into one attributed post per block
/// (Python `_extract_forum_thread_text`). Empty when the page has no
/// recognizable posts, so generic extraction still runs.
pub fn forum_thread_text(html: &str) -> String {
    forum_blocks(html)
        .iter()
        .map(|block| {
            let text = crate::extract::html_to_text(&block.body_html);
            let text = text.trim();
            if block.header.is_empty() {
                text.to_string()
            } else {
                format!("{}\n{}", block.header, text)
            }
        })
        .filter(|block| !block.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Rebuild a thread as one node — an <h2> heading + body per post —
/// for the rich reader (Python article_html `_forum_reconstruct`).
/// None when the page has no recognizable posts.
pub fn forum_thread_html(html: &str) -> Option<String> {
    let blocks = forum_blocks(html);
    if blocks.is_empty() {
        return None;
    }
    let mut out = String::from("<article>");
    for block in &blocks {
        if !block.header.is_empty() {
            out.push_str(&format!(
                "<h2>{}</h2>",
                crate::threads::escape_source_html(&block.header)
            ));
        }
        out.push_str(&block.body_html);
    }
    out.push_str("</article>");
    Some(out)
}

// ---------------------------------------------------------------------------
// Complete-thread documents (Python forum_sources.py renderers).
// ---------------------------------------------------------------------------

struct ThreadPost {
    id: String,
    number: String,
    author: String,
    created: String,
    reply_to: String,
    body: String,
}

/// Python `_render_thread_document`: one <section> per post under a
/// "#<number> Posted/Message by <author>" heading.
fn render_thread_document(
    title: &str,
    posts: &[ThreadPost],
    canonical_url: &str,
    source_label: &str,
    css_prefix: &str,
) -> String {
    let esc = crate::threads::escape_source_html;
    let mut sections = String::new();
    let mut seen = HashSet::new();
    for (i, post) in posts.iter().enumerate() {
        let fallback = i + 1;
        let identity = if post.id.is_empty() {
            fallback.to_string()
        } else {
            post.id.clone()
        };
        if !seen.insert(identity) {
            continue;
        }
        let number = if post.number.is_empty() {
            fallback.to_string()
        } else {
            post.number.clone()
        };
        let role = if fallback == 1 { "Posted" } else { "Message" };
        let author = {
            let author = post.author.trim();
            if author.is_empty() {
                "Unknown author"
            } else {
                author
            }
        };
        let mut heading = format!("#{number} {role} by {author}");
        if !post.created.trim().is_empty() {
            heading.push_str(&format!(" — {}", post.created.trim()));
        }
        if !post.reply_to.trim().is_empty() {
            heading.push_str(&format!(" — Reply to #{}", post.reply_to.trim()));
        }
        let body = if post.body.trim().is_empty() {
            "<p>[no text available]</p>"
        } else {
            post.body.trim()
        };
        sections.push_str(&format!(
            "<section class=\"blindrss-{css_prefix}-post\"><h2>{}</h2><div class=\"blindrss-{css_prefix}-body\">{body}</div></section>",
            esc(&heading)
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    let title = {
        let title = title.trim();
        if title.is_empty() {
            format!("{source_label} thread")
        } else {
            title.to_string()
        }
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><p><a href=\"{}\">Open thread on {}</a></p>{sections}</body></html>",
        esc(&title),
        esc(&title),
        esc(canonical_url),
        esc(source_label)
    )
}

/// Python `_clean_post_html`: drop script/style/embed/form machinery
/// (and, for quoted replies, the quote blocks) from a post body.
fn clean_post_html(raw: &str, drop_quotes: bool) -> String {
    const JUNK_TAGS: &[&str] = &[
        "script", "style", "iframe", "object", "embed", "form", "input", "button",
    ];
    let (cleaned, _) = remove_elements(raw, &|name: &str, open: &str| {
        JUNK_TAGS.contains(&name)
            || (drop_quotes && (has_class(open, "gmail_quote") || has_class(open, "wqmMgb")))
    });
    cleaned.trim().to_string()
}

fn page_title(html: &str) -> String {
    find_elements(html, Some("title"), &|_| true)
        .first()
        .map(|span| leaf_text(&html[span.inner_start..span.inner_end]))
        .unwrap_or_default()
}

fn strip_groups_suffix(title: &str) -> String {
    let title = title.trim();
    if let Some(rest) = title.rsplit_once(" - Google Groups")
        && rest.1.trim().is_empty()
    {
        return rest.0.trim().to_string();
    }
    title.to_string()
}

/// Canonical Google Groups conversation URL (Python
/// `normalize_google_groups_url` restricted to thread paths, including
/// the legacy `#!topic/<group>/<conversation>` fragment form).
pub fn google_groups_thread_url(value: &str) -> Option<String> {
    let url = web_url(value)?;
    if url.host_str() != Some("groups.google.com") {
        return None;
    }
    if let Some(fragment) = url.fragment()
        && let Some((group, conversation)) = fragment
            .strip_prefix("!topic/")
            .and_then(|rest| rest.split_once('/'))
        && group_name(group)
        && group_name(conversation)
    {
        return Some(format!(
            "https://groups.google.com/g/{group}/c/{conversation}"
        ));
    }
    let segments = parts(&url);
    if segments.len() >= 4
        && segments[0] == "g"
        && segments[2] == "c"
        && group_name(segments[1])
        && group_name(segments[3])
    {
        let mut canonical = format!(
            "https://groups.google.com/g/{}/c/{}",
            segments[1], segments[3]
        );
        if segments.len() >= 6 && segments[4] == "m" && group_name(segments[5]) {
            canonical.push_str(&format!("/m/{}", segments[5]));
        }
        return Some(canonical);
    }
    None
}

pub fn is_google_groups_thread_url(value: &str) -> bool {
    google_groups_thread_url(value).is_some()
}

/// Rebuild a Google Groups conversation from its server-rendered page
/// (Python `download_google_groups_thread_html`'s parse half). Returns
/// the semantic thread document, or "" when the page carries no
/// readable messages (or is the permission-denied page).
pub fn google_groups_thread_document(html: &str, url: &str) -> String {
    let Some(canonical) = google_groups_thread_url(url) else {
        return String::new();
    };
    let text = leaf_text(html).to_lowercase();
    if text.contains("you don't have permission to access this content")
        || text.contains("you don’t have permission to access this content")
    {
        return String::new();
    }
    let title = strip_groups_suffix(&page_title(html));
    let title = if title.is_empty() {
        "Google Groups conversation".to_string()
    } else {
        title
    };
    let mut posts = Vec::new();
    for block in find_elements(html, None, &|tag| has_class(tag, "eH2Xlc")) {
        let frag = &html[block.start..block.end];
        let body_span = select_first(
            frag,
            &[Step {
                tag: None,
                class: None,
                attr: Some("role"),
                attr_eq: Some("region"),
            }],
        )
        .filter(|span| attr_value(&frag[span.start..span.inner_start], "aria-labelledby").is_some())
        .or_else(|| select_first(frag, &[class_step("ptW7te")]));
        let Some(body_span) = body_span else {
            continue;
        };
        let body = clean_post_html(&frag[body_span.start..body_span.end], true);
        if body.is_empty() {
            continue;
        }
        let author = select_first(frag, &[tag_step("h3")])
            .map(|span| span_text(frag, &span))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "Unknown author".to_string());
        let created = select_first(frag, &[class_step("zX2W9c")])
            .or_else(|| select_first(frag, &[tag_step("time")]))
            .map(|span| span_text(frag, &span))
            .unwrap_or_default();
        let id = find_elements(frag, None, &|tag| {
            attr_value(tag, "data-message-id").is_some()
        })
        .first()
        .and_then(|span| attr_value(&frag[span.start..span.inner_start], "data-message-id"))
        .unwrap_or_default();
        posts.push(ThreadPost {
            id,
            number: (posts.len() + 1).to_string(),
            author,
            created,
            reply_to: String::new(),
            body,
        });
    }
    render_thread_document(&title, &posts, &canonical, "Google Groups", "googlegroups")
}

/// Google Help Community thread URL (support.google.com/<product>/
/// thread/<id>).
pub fn is_google_support_thread_url(value: &str) -> bool {
    let Some(url) = web_url(value) else {
        return false;
    };
    if url.host_str() != Some("support.google.com") {
        return false;
    }
    let segments = parts(&url);
    segments.len() >= 3
        && segments[1] == "thread"
        && !segments[2].is_empty()
        && segments[2].chars().all(|c| c.is_ascii_digit())
        && group_name(segments[0])
}

/// Decode the JS string literal of `var thread_view='...';` the way
/// Python's ast.literal_eval does (Python forum_sources
/// `_decode_thread_view`'s first half).
fn decode_js_single_quoted(raw: &str) -> Option<String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    let hex = |chars: &[char], start: usize, len: usize| -> Option<u32> {
        let slice: String = chars.iter().skip(start).take(len).collect();
        if slice.len() == len && slice.chars().all(|c| c.is_ascii_hexdigit()) {
            u32::from_str_radix(&slice, 16).ok()
        } else {
            None
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied()?;
        match next {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '\n' => {}
            'x' => {
                let value = hex(&chars, i + 2, 2)?;
                out.push(char::from_u32(value)?);
                i += 2;
            }
            'u' => {
                let value = hex(&chars, i + 2, 4)?;
                out.push(char::from_u32(value)?);
                i += 4;
            }
            'U' => {
                let value = hex(&chars, i + 2, 8)?;
                out.push(char::from_u32(value)?);
                i += 8;
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
        i += 2;
    }
    Some(out)
}

fn nested<'a>(value: &'a serde_json::Value, indexes: &[usize]) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for index in indexes {
        current = current.get(*index)?;
    }
    Some(current)
}

fn nested_str(value: &serde_json::Value, indexes: &[usize]) -> String {
    nested(value, indexes)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Python `_timestamp_label`: epoch seconds/ms/µs → "YYYY-MM-DD HH:MM UTC".
fn support_timestamp_label(value: Option<&serde_json::Value>) -> String {
    let mut number = value.and_then(|v| v.as_f64()).unwrap_or(0.0);
    if number > 10_000_000_000_000.0 {
        number /= 1_000_000.0;
    } else if number > 10_000_000_000.0 {
        number /= 1000.0;
    }
    if number <= 0.0 {
        return String::new();
    }
    let iso = timestamp(number as i64);
    if iso.len() >= 16 {
        format!("{} {} UTC", &iso[..10], &iso[11..16])
    } else {
        String::new()
    }
}

/// Decode a Google Help Community's embedded complete conversation
/// (Python `google_support_thread_document`).
pub fn google_support_thread_document(html: &str, url: &str) -> String {
    static THREAD_VIEW: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)\bvar\s+thread_view='((?:\\.|[^'])*)';").unwrap()
    });
    let Some(captures) = THREAD_VIEW.captures(html) else {
        return String::new();
    };
    let Some(decoded) = decode_js_single_quoted(&captures[1]) else {
        return String::new();
    };
    let Ok(data) = serde_json::from_str::<serde_json::Value>(&decoded) else {
        return String::new();
    };
    if !data.is_array() {
        return String::new();
    }
    let Some(topic) = nested(&data, &[1]) else {
        return String::new();
    };
    if !topic.is_array() {
        return String::new();
    }
    let title = {
        let title = nested_str(topic, &[8]);
        if title.trim().is_empty() {
            "Google Support thread".to_string()
        } else {
            title.trim().to_string()
        }
    };
    let mut posts = vec![ThreadPost {
        id: {
            let id = nested_str(topic, &[0, 0]);
            if id.is_empty() { "1".to_string() } else { id }
        },
        number: "1".to_string(),
        author: {
            let author = nested_str(&data, &[3, 0, 0]);
            if author.trim().is_empty() {
                "Unknown author".to_string()
            } else {
                author.trim().to_string()
            }
        },
        created: support_timestamp_label(nested(topic, &[0, 1])),
        reply_to: String::new(),
        body: clean_post_html(&nested_str(topic, &[12]), false),
    }];
    if let Some(replies) = nested(&data, &[39, 0]).and_then(|v| v.as_array()) {
        for wrapper in replies {
            let Some(post) = nested(wrapper, &[0]) else {
                continue;
            };
            if !post.is_array() {
                continue;
            }
            let body = clean_post_html(&nested_str(post, &[3]), false);
            if body.is_empty() {
                continue;
            }
            posts.push(ThreadPost {
                id: {
                    let id = nested_str(post, &[0, 0]);
                    if id.is_empty() {
                        (posts.len() + 1).to_string()
                    } else {
                        id
                    }
                },
                number: (posts.len() + 1).to_string(),
                author: {
                    let author = nested_str(wrapper, &[2, 0, 0]);
                    if author.trim().is_empty() {
                        "Unknown author".to_string()
                    } else {
                        author.trim().to_string()
                    }
                },
                created: support_timestamp_label(nested(post, &[0, 1])),
                reply_to: String::new(),
                body,
            });
        }
    }
    render_thread_document(&title, &posts, url, "Google Support", "googlesupport")
}

/// Python `_is_forum_thread_host` also covers the web-forum thread
/// URLs this module rebuilds (Google Groups conversations and
/// Google Help Community threads).
fn forum_gate(url: &str) -> bool {
    is_forum_thread_host(url)
        || is_google_groups_thread_url(url)
        || is_google_support_thread_url(url)
}

/// The forum linearizer as the extraction cascade calls it: the
/// Python layouts are host-gated (`_is_forum_thread_host`), while the
/// XenForo layout — which has no Python host list — keys on its
/// distinctive markup alone.
pub fn forum_thread_text_for(url: &str, html: &str) -> String {
    match forum_layout_of(html) {
        Some(layout) if layout.name == "xenforo" || forum_gate(url) => forum_thread_text(html),
        _ => String::new(),
    }
}

/// Rich-view counterpart of `forum_thread_text_for`.
pub fn forum_thread_html_for(url: &str, html: &str) -> Option<String> {
    match forum_layout_of(html) {
        Some(layout) if layout.name == "xenforo" || forum_gate(url) => forum_thread_html(html),
        _ => None,
    }
}

/// Fetch + rebuild a Google Groups conversation or a Google Help
/// Community thread as a semantic document. None means ordinary
/// extraction should handle this URL.
pub fn web_thread_html(value: &str) -> Option<(String, String)> {
    let (config_path, _, _) = SETTINGS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(canonical) = google_groups_thread_url(value) {
        let html = fetch(&format!("{canonical}?hl=en"), config_path.as_deref()).ok()?;
        let document = google_groups_thread_document(&html, &canonical);
        if document.is_empty() {
            return None;
        }
        let title = page_title(&document);
        return Some((title, document));
    }
    if is_google_support_thread_url(value) {
        let html = fetch(value, config_path.as_deref()).ok()?;
        let document = google_support_thread_document(&html, value);
        if document.is_empty() {
            return None;
        }
        let title = page_title(&document);
        return Some((title, document));
    }
    None
}

#[cfg(test)]
mod topic_api_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn api_topic_pages_extended_payload_and_preserves_message_bodies() {
        let mut paths = Vec::new();
        let (_, html) = api_topic_document(123, "https://groups.io/g/readers/topic/123", &mut |path| {
            paths.push(path.to_string());
            Some(if paths.len() == 1 { json!([{"object":"user"},{"object":"list","subject":"Topic <title>","data":[{"msg_num":1,"name":"Alice","created":1000000000000i64,"body":"<p>First body</p>"}],"next_page_token":"next page"}]) }
                else { json!({"messages":[{"msg_num":2,"author":"Bob","body":"<plain>","is_plain_text":true,"attachments":[{"name":"File","download_url":"https://example.com/file"}]}]}) })
        }).unwrap();
        assert!(paths[1].contains("page_token=next+page"));
        for text in [
            "Topic &lt;title&gt;",
            "2001-09-09T01:46:40Z",
            "First body",
            "Bob",
            "&lt;plain&gt;",
            "https://example.com/file",
        ] {
            assert!(html.contains(text), "missing {text}");
        }
    }
}
