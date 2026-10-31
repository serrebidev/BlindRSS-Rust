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

/// Try every thread reconstructor for this URL.
pub fn thread_text(url: &str) -> Option<(String, String)> {
    lemmy_thread(url)
        .or_else(|| github_thread(url))
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

fn github_parts(url: &str) -> Option<(String, String, String, String)> {
    // (owner, repo, kind, number-or-sha)
    let rest = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 4 {
        return None;
    }
    let (owner, repo, kind, num) = (parts[0], parts[1], parts[2], parts[3]);
    match kind {
        "issues" | "pull" if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() => Some((
            owner.to_string(),
            repo.to_string(),
            kind.to_string(),
            num.to_string(),
        )),
        _ => None,
    }
}

pub fn github_thread(url: &str) -> Option<(String, String)> {
    let (owner, repo, kind, num) = github_parts(url)?;
    let api = format!("https://api.github.com/repos/{owner}/{repo}/issues/{num}");
    let issue = get_json(&api)?;
    let title = str_field(&issue, "title");
    if title.is_empty() {
        return None;
    }
    let author = issue
        .get("user")
        .map(|u| str_field(u, "login"))
        .unwrap_or_default();
    let mut out = vec![format!(
        "Post 1 - {} - {}",
        author,
        str_field(&issue, "created_at")
    )];
    let body = str_field(&issue, "body");
    if !body.trim().is_empty() {
        out.push(body);
    }
    let comments_url = str_field(&issue, "comments_url");
    if !comments_url.is_empty()
        && let Some(comments) = get_json(&format!("{comments_url}?per_page=100"))
        && let Some(items) = comments.as_array()
    {
        for (i, c) in items.iter().enumerate() {
            let cbody = str_field(c, "body");
            if cbody.trim().is_empty() {
                continue;
            }
            let cauthor = c
                .get("user")
                .map(|u| str_field(u, "login"))
                .unwrap_or_default();
            out.push(format!(
                "Post {} - {} - {}",
                i + 2,
                cauthor,
                str_field(c, "created_at")
            ));
            out.push(cbody);
        }
    }
    let _ = kind;
    Some((title, out.join("\n\n")))
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

pub fn discourse_thread(url: &str) -> Option<(String, String)> {
    let (origin, id) = discourse_parts(url)?;
    let topic = get_json(&format!("{origin}/t/{id}.json"))?;
    let title = str_field(&topic, "title");
    if title.is_empty() {
        return None; // not a Discourse site: fail closed
    }
    let posts = topic.get("post_stream")?.get("posts")?.as_array()?.clone();
    let mut out = Vec::new();
    for (i, p) in posts.iter().enumerate() {
        let cooked = str_field(p, "cooked");
        if cooked.trim().is_empty() {
            continue;
        }
        out.push(format!(
            "Post {} - {} - {}",
            i + 1,
            str_field(p, "username"),
            str_field(p, "created_at")
        ));
        out.push(crate::extract::html_to_text(&cooked));
    }
    if out.is_empty() {
        return None;
    }
    Some((title, out.join("\n\n")))
}
