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

fn login_of(v: &Value) -> String {
    v.get("user")
        .map(|u| str_field(u, "login"))
        .unwrap_or_default()
}

pub fn github_page_text(url: &str) -> Option<(String, String)> {
    match github_page_parts(url)? {
        GithubPage::Commit(owner, repo, sha) => github_commit_text(&owner, &repo, &sha),
        GithubPage::Release(owner, repo, tag) => github_release_text(&owner, &repo, &tag),
        GithubPage::Discussion(owner, repo, num) => github_discussion_text(&owner, &repo, &num)
            // Python `_discussion_document`: anything the REST API
            // cannot serve falls through to the rendered page.
            .or_else(|| github_org_discussion_text(&owner, &num, url)),
        GithubPage::OrgDiscussion(owner, num) => github_org_discussion_text(&owner, &num, url),
    }
}

fn github_commit_text(owner: &str, repo: &str, sha: &str) -> Option<(String, String)> {
    let base = format!("https://api.github.com/repos/{owner}/{repo}");
    let commit = get_json(&format!("{base}/commits/{sha}"))?;
    let full_sha = str_field(&commit, "sha");
    if full_sha.is_empty() {
        return None;
    }
    let info = commit.get("commit").cloned().unwrap_or(Value::Null);
    let author_info = info.get("author").cloned().unwrap_or(Value::Null);
    let mut author = login_of(&commit);
    if author.is_empty() || author == "ghost" {
        author = str_field(&author_info, "name");
        if author.is_empty() {
            author = "unknown".to_string();
        }
    }
    let when = str_field(&author_info, "date");
    let message = str_field(&info, "message");
    let subject = message
        .lines()
        .next()
        .unwrap_or("(no commit message)")
        .trim()
        .to_string();
    let slug = format!("{owner}/{repo}");
    let mut out: Vec<String> = Vec::new();
    let mut heading = format!("Commit by {author}");
    if !when.is_empty() {
        heading += &format!(" - {when}");
    }
    out.push(heading);
    let mut bullets = vec![format!("Commit: {}", &full_sha[..full_sha.len().min(12)])];
    if let Some(stats) = commit.get("stats") {
        let add = stats.get("additions").and_then(|v| v.as_i64()).unwrap_or(0);
        let del = stats.get("deletions").and_then(|v| v.as_i64()).unwrap_or(0);
        bullets.push(format!("{add} additions, {del} deletions"));
    }
    if let Some(parents) = commit.get("parents").and_then(|v| v.as_array())
        && !parents.is_empty()
    {
        let short: Vec<String> = parents
            .iter()
            .map(|p| {
                let s = str_field(p, "sha");
                s[..s.len().min(7)].to_string()
            })
            .collect();
        bullets.push(format!("Parents: {}", short.join(", ")));
    }
    bullets.push(format!("Repository: {slug}"));
    out.push(bullets.join("\n"));
    if !message.trim().is_empty() {
        out.push(message.trim().to_string());
    }
    // Commit comments.
    if let Some(comments) = get_json(&format!("{base}/commits/{sha}/comments?per_page=100"))
        && let Some(items) = comments.as_array()
    {
        for c in items {
            let body = str_field(c, "body");
            if body.trim().is_empty() {
                continue;
            }
            out.push(format!(
                "Comment by {} - {}",
                login_of(c),
                str_field(c, "created_at")
            ));
            out.push(body.trim().to_string());
        }
    }
    // Changed files with patches (Python caps: 60 files with patch,
    // 400 diff lines per file, 4000 total).
    if let Some(files) = commit.get("files").and_then(|v| v.as_array())
        && !files.is_empty()
    {
        out.push(format!("Changed files ({})", files.len()));
        let mut diff_lines_total = 0usize;
        for (i, f) in files.iter().enumerate() {
            let name = str_field(f, "filename");
            let status = str_field(f, "status");
            let add = f.get("additions").and_then(|v| v.as_i64()).unwrap_or(0);
            let del = f.get("deletions").and_then(|v| v.as_i64()).unwrap_or(0);
            out.push(format!("{name} ({status}, +{add} -{del})"));
            if i < 60 && diff_lines_total < 4000 {
                let patch = str_field(f, "patch");
                if !patch.is_empty() {
                    let lines: Vec<&str> = patch.lines().take(400).collect();
                    diff_lines_total += lines.len();
                    out.push(lines.join("\n"));
                }
            }
        }
    }
    Some((format!("{subject} ({slug})"), out.join("\n\n")))
}

fn github_release_text(owner: &str, repo: &str, tag: &str) -> Option<(String, String)> {
    let base = format!("https://api.github.com/repos/{owner}/{repo}");
    let release = get_json(&format!("{base}/releases/tags/{tag}"))?;
    let tag_name = str_field(&release, "tag_name");
    if tag_name.is_empty() {
        return None;
    }
    let slug = format!("{owner}/{repo}");
    let author = login_of(&release);
    let mut title = str_field(&release, "name");
    if title.is_empty() {
        title = tag_name.clone();
    }
    let published = {
        let p = str_field(&release, "published_at");
        if p.is_empty() {
            str_field(&release, "created_at")
        } else {
            p
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut heading = format!("Release published by {author}");
    if !published.is_empty() {
        heading += &format!(" - {published}");
    }
    out.push(heading);
    let mut bullets = vec![format!("Tag: {tag_name}")];
    if release.get("prerelease").and_then(|v| v.as_bool()) == Some(true) {
        bullets.push("Pre-release".to_string());
    }
    if release.get("draft").and_then(|v| v.as_bool()) == Some(true) {
        bullets.push("Draft".to_string());
    }
    bullets.push(format!("Repository: {slug}"));
    out.push(bullets.join("\n"));
    let body = str_field(&release, "body");
    if !body.trim().is_empty() {
        out.push(body.trim().to_string());
    }
    if let Some(assets) = release.get("assets").and_then(|v| v.as_array())
        && !assets.is_empty()
    {
        out.push(format!("Assets ({})", assets.len()));
        let rows: Vec<String> = assets
            .iter()
            .map(|a| {
                let name = str_field(a, "name");
                let name = if name.is_empty() { "asset" } else { &name };
                match a.get("download_count").and_then(|v| v.as_i64()) {
                    Some(d) => format!("{name} ({d} downloads)"),
                    None => name.to_string(),
                }
            })
            .collect();
        out.push(rows.join("\n"));
    }
    Some((format!("{title} ({slug})"), out.join("\n\n")))
}

fn github_discussion_text(owner: &str, repo: &str, num: &str) -> Option<(String, String)> {
    let base = format!("https://api.github.com/repos/{owner}/{repo}");
    let disc = get_json(&format!("{base}/discussions/{num}"))?;
    let title = str_field(&disc, "title");
    if title.is_empty() {
        return None;
    }
    let slug = format!("{owner}/{repo}");
    let mut out: Vec<String> = Vec::new();
    out.push(format!(
        "Discussion opened by {} - {}",
        login_of(&disc),
        str_field(&disc, "created_at")
    ));
    let body = str_field(&disc, "body");
    if !body.trim().is_empty() {
        out.push(body.trim().to_string());
    }
    if let Some(comments) = get_json(&format!("{base}/discussions/{num}/comments?per_page=100"))
        && let Some(items) = comments.as_array()
    {
        let mut sorted: Vec<&Value> = items.iter().collect();
        sorted.sort_by_key(|c| str_field(c, "created_at"));
        for c in sorted {
            let body = str_field(c, "body");
            if body.trim().is_empty() {
                continue;
            }
            let is_reply = c.get("parent_id").map(|v| !v.is_null()).unwrap_or(false);
            let label = if is_reply { "Reply" } else { "Comment" };
            out.push(format!(
                "{label} by {} - {}",
                login_of(c),
                str_field(c, "created_at")
            ));
            out.push(body.trim().to_string());
        }
    }
    Some((format!("{title} ({slug}#{num})"), out.join("\n\n")))
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
