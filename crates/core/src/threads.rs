//! Thread reconstructions (Python's article-extractor thread
//! paths + core/github_source.py, core/forum_sources.py).
//!
//! When an article URL is a thread on a platform with a real
//! API, Python rebuilds the thread as a synthetic document —
//! the opening post followed by every comment in order —
//! instead of scraping the HTML shell. This module does the
//! same for the API-driven platforms and returns plain text
//! in the reader's spoken shape:
//!
//!   Post 1 - <author> - <date>
//!   <body>
//!
//!   Post 2 - <author> - <date> (reply level 1)
//!   <body>
//!
//! Detection fails closed: anything that does not confirm
//! returns None and normal extraction runs. The HTML-selector
//! forum configs (Invision, vBulletin, XenForo...) are not
//! ported — the Rust extractor has no CSS-selector engine;
//! see PARITY §7.

use serde_json::Value;
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
// Lemmy: /post/<id> on any instance -> api/v3 post + comments.
// ---------------------------------------------------------------------------

fn lemmy_parts(url: &str) -> Option<(String, i64)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (host, path) = rest.split_once('/')?;
    let mut segments = path.split('/');
    if segments.next()? != "post" {
        return None;
    }
    let id: i64 = segments.next()?.parse().ok()?;
    Some((format!("{scheme}://{host}"), id))
}

pub fn lemmy_thread(url: &str) -> Option<(String, String)> {
    let (origin, post_id) = lemmy_parts(url)?;
    let post = get_json(&format!("{origin}/api/v3/post?id={post_id}"))?;
    let post_view = post.get("post_view")?;
    let post_obj = post_view.get("post")?;
    let title = str_field(post_obj, "name");
    if title.is_empty() {
        return None; // not actually a Lemmy instance: fail closed
    }
    let author = post_view
        .get("creator")
        .map(|c| str_field(c, "name"))
        .unwrap_or_default();
    let mut out = vec![format!(
        "Post 1 - {} - {}",
        author,
        str_field(post_obj, "published")
    )];
    let body = str_field(post_obj, "body");
    if !body.trim().is_empty() {
        out.push(body);
    }
    if let Some(link) = post_obj.get("url").and_then(|v| v.as_str())
        && !link.is_empty()
    {
        out.push(format!("Link: {link}"));
    }
    let comments = get_json(&format!(
        "{origin}/api/v3/comment/list?post_id={post_id}&max_depth=8&sort=New&limit=200"
    ))?;
    let mut views: Vec<&Value> = comments
        .get("comments")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // Lemmy paths look like "0.123.456": sorting by path keeps
    // parents before their replies, in thread order.
    views.sort_by_key(|v| {
        v.get("comment")
            .map(|c| str_field(c, "path"))
            .unwrap_or_default()
    });
    for (i, view) in views.iter().enumerate() {
        let comment = view.get("comment")?;
        let content = str_field(comment, "content");
        if content.trim().is_empty() {
            continue;
        }
        let level = str_field(comment, "path").matches('.').count();
        let author = view
            .get("creator")
            .map(|c| str_field(c, "name"))
            .unwrap_or_default();
        out.push(format!(
            "Post {} - {} - {} (reply level {})",
            i + 2,
            author,
            str_field(comment, "published"),
            level
        ));
        out.push(content);
    }
    Some((title, out.join("\n\n")))
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
// Reddit: thread page -> <path>.json (post + nested comments).
// ---------------------------------------------------------------------------

pub fn reddit_thread(url: &str) -> Option<(String, String)> {
    let host = url.split('/').nth(2).unwrap_or("").to_lowercase();
    if !(host == "reddit.com" || host.ends_with(".reddit.com")) {
        return None;
    }
    if !url.contains("/comments/") {
        return None;
    }
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    let json_url = format!("{}.json?raw_json=1", clean.trim_end_matches('/'));
    let data = get_json(&json_url)?;
    let listing = data.as_array()?;
    let post = listing
        .first()?
        .get("data")?
        .get("children")?
        .as_array()?
        .first()?
        .get("data")?
        .clone();
    let title = str_field(&post, "title");
    if title.is_empty() {
        return None;
    }
    let mut out = vec![format!("Post 1 - {} - {}", str_field(&post, "author"), "")];
    let selftext = str_field(&post, "selftext");
    if !selftext.trim().is_empty() {
        out.push(selftext);
    }
    if let Some(link) = post.get("url").and_then(|v| v.as_str())
        && !link.is_empty()
        && !link.contains("/comments/")
    {
        out.push(format!("Link: {link}"));
    }
    let mut counter = 1usize;
    if let Some(comments) = listing
        .get(1)
        .and_then(|l| l.get("data"))
        .and_then(|d| d.get("children"))
        .and_then(|c| c.as_array())
    {
        for child in comments {
            walk_reddit(child, 0, &mut out, &mut counter);
        }
    }
    Some((title, out.join("\n\n")))
}

fn walk_reddit(node: &Value, depth: usize, out: &mut Vec<String>, counter: &mut usize) {
    let data = match node.get("data") {
        Some(d) => d,
        None => return,
    };
    if node.get("kind").and_then(|k| k.as_str()) == Some("t1") {
        let body = str_field(data, "body");
        if !body.trim().is_empty() {
            *counter += 1;
            out.push(format!(
                "Post {} - {} (reply level {})",
                counter,
                str_field(data, "author"),
                depth
            ));
            out.push(body);
        }
    }
    if let Some(replies) = data.get("replies")
        && let Some(children) = replies
            .get("data")
            .and_then(|d| d.get("children"))
            .and_then(|c| c.as_array())
    {
        for child in children {
            walk_reddit(child, depth + 1, out, counter);
        }
    }
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
