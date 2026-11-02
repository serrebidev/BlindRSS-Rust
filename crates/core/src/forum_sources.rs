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
