//! Complete GitHub pages, matching Python core/github_source.py.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Target {
    pub kind: String,
    pub owner: String,
    pub repo: String,
    pub reference: String,
}

pub(crate) fn parse_target(value: &str) -> Option<Target> {
    use std::sync::LazyLock;
    static SHAPES: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
        r"^/([A-Za-z0-9](?:[A-Za-z0-9._-]{0,98}[A-Za-z0-9])?)/([A-Za-z0-9._-]{1,100})/(pull/[0-9]+(?:/(?:files|commits|checks|conversation)(?:/[^/]*)?)?|issues/[0-9]+|commit/[0-9a-fA-F]{7,40}(?:\.patch|\.diff)?|releases/tag/[^/]+|discussions/[0-9]+)/?$"
    ).unwrap()
    });
    let url = url::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(url.host_str(), Some("github.com" | "www.github.com"))
    {
        return None;
    }
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if parts.len() == 4
        && parts[0] == "orgs"
        && parts[2] == "discussions"
        && parts[3].bytes().all(|b| b.is_ascii_digit())
    {
        return Some(Target {
            kind: "org_discussion".into(),
            owner: parts[1].into(),
            repo: String::new(),
            reference: parts[3].into(),
        });
    }
    let captures = SHAPES.captures(url.path())?;
    let owner = &captures[1];
    if [
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
    ]
    .contains(&owner.to_lowercase().as_str())
    {
        return None;
    }
    let kind = match parts[2] {
        "issues" => "issue",
        "releases" => "release",
        "discussions" => "discussion",
        other => other,
    };
    let reference = if kind == "release" {
        parts[4].to_string()
    } else if kind == "commit" {
        parts[3]
            .trim_end_matches(".patch")
            .trim_end_matches(".diff")
            .to_string()
    } else {
        parts[3].to_string()
    };
    let reference =
        url::form_urlencoded::parse(format!("v={}", reference.replace('+', "%2B")).as_bytes())
            .next()?
            .1
            .into_owned();
    Some(Target {
        kind: kind.into(),
        owner: owner.into(),
        repo: captures[2].into(),
        reference,
    })
}

pub(crate) fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn pointer<'a>(value: &'a Value, path: &str) -> &'a str {
    value.pointer(path).and_then(Value::as_str).unwrap_or("")
}
fn login(value: &Value) -> &str {
    [
        pointer(value, "/user/login"),
        pointer(value, "/author/login"),
        string(value, "login"),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or("ghost")
}
fn number(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}
fn when(value: &str) -> String {
    if value.len() >= 16 && value.is_char_boundary(16) && value.as_bytes().get(10) == Some(&b'T') {
        format!("{} {} UTC", &value[..10], &value[11..16])
    } else {
        value.to_string()
    }
}
fn plain(value: &str) -> String {
    value
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p>{}</p>", escape(p).replace('\n', "<br>")))
        .collect()
}
fn body(value: &Value) -> String {
    let html = string(value, "body_html");
    if html.trim().is_empty() {
        plain(string(value, "body"))
    } else {
        html.into()
    }
}
fn section(index: usize, heading: &str, body: &str) -> String {
    format!(
        "<section class=\"blindrss-github-post\"><h2>#{index} {}</h2><div class=\"blindrss-github-body\">{}</div></section>",
        escape(heading),
        if body.is_empty() {
            "<p>[no text available]</p>"
        } else {
            body
        }
    )
}
fn bullets(rows: impl IntoIterator<Item = String>) -> String {
    let rows: String = rows
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| format!("<li>{s}</li>"))
        .collect();
    if rows.is_empty() {
        rows
    } else {
        format!("<ul>{rows}</ul>")
    }
}
fn list(path: &str, limit: usize, get: &mut impl FnMut(&str) -> Option<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for page in 1..=20 {
        let Some(Value::Array(items)) = get(&format!("{path}?per_page=100&page={page}")) else {
            break;
        };
        let size = items.len();
        out.extend(items.into_iter().filter(Value::is_object));
        if size < 100 || out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}
fn names(value: &Value, field: &str, key: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| {
            if v.is_string() {
                v.as_str().unwrap_or("")
            } else {
                v.get(key).and_then(Value::as_str).unwrap_or("")
            }
        })
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}
fn summary(value: &Value, target: &Target) -> String {
    let mut rows = Vec::new();
    if target.kind == "pull" {
        let state = if value.get("merged").and_then(Value::as_bool) == Some(true)
            || value.get("merged_at").is_some_and(|v| !v.is_null())
        {
            "Merged"
        } else if string(value, "state") == "closed" {
            "Closed"
        } else if value.get("draft").and_then(Value::as_bool) == Some(true) {
            "Draft"
        } else {
            "Open"
        };
        rows.push(format!("State: {state}"));
        let head = [pointer(value, "/head/label"), pointer(value, "/head/ref")]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("?");
        rows.push(escape(&format!(
            "Merges {head} into {}",
            pointer(value, "/base/ref")
        )));
        rows.push(escape(&format!(
            "{} commits, {} files changed, {} additions, {} deletions",
            number(value, "commits"),
            number(value, "changed_files"),
            number(value, "additions"),
            number(value, "deletions")
        )));
        if !string(value, "merged_at").is_empty() {
            rows.push(escape(&format!(
                "Merged: {} by {}",
                when(string(value, "merged_at")),
                pointer(value, "/merged_by/login")
            )));
        }
    }
    for (field, key, label) in [
        ("labels", "name", "Labels"),
        ("assignees", "login", "Assignees"),
    ] {
        let names = names(value, field, key);
        if !names.is_empty() {
            rows.push(escape(&format!("{label}: {names}")));
        }
    }
    for (path, label) in [
        ("/milestone/title", "Milestone"),
        ("/category/name", "Category"),
    ] {
        let value = pointer(value, path);
        if !value.is_empty() {
            rows.push(escape(&format!("{label}: {value}")));
        }
    }
    for (key, label) in [
        ("closed_at", "Closed"),
        ("answer_chosen_at", "Marked answered"),
    ] {
        let value = string(value, key);
        if !value.is_empty() {
            rows.push(escape(&format!("{label}: {}", when(value))));
        }
    }
    rows.push(format!(
        "Repository: <a href=\"https://github.com/{}/{}\">{}/{}</a>",
        escape(&target.owner),
        escape(&target.repo),
        escape(&target.owner),
        escape(&target.repo)
    ));
    bullets(rows)
}

fn files_sections(files: &[Value], sections: &mut Vec<String>) {
    if files.is_empty() {
        return;
    }
    sections.push(section(
        sections.len() + 1,
        &format!("Changed files ({})", files.len()),
        &bullets(files.iter().map(|file| {
            let old = string(file, "previous_filename");
            escape(&format!(
                "{} — {}, {} added, {} removed{}",
                string(file, "filename"),
                string(file, "status"),
                number(file, "additions"),
                number(file, "deletions"),
                if old.is_empty() {
                    String::new()
                } else {
                    format!(" (was {old})")
                }
            ))
        })),
    ));
    let patches: Vec<_> = files
        .iter()
        .filter(|v| !string(v, "patch").trim().is_empty())
        .collect();
    let mut used = 0;
    let mut rendered = 0;
    for file in patches.iter().take(60) {
        let cap = 400.min(4000 - usize::min(used, 4000));
        if cap == 0 {
            break;
        }
        let lines: Vec<_> = string(file, "patch").lines().collect();
        let shown: String = lines
            .iter()
            .take(cap)
            .map(|line| escape(line))
            .collect::<Vec<_>>()
            .join("<br>");
        used += lines.len().min(cap);
        rendered += 1;
        let truncated = if lines.len() > cap {
            format!(
                "<p>[Diff truncated: {} more lines. Open the page on GitHub for the rest.]</p>",
                lines.len() - cap
            )
        } else {
            String::new()
        };
        sections.push(section(
            sections.len() + 1,
            &format!("Diff: {}", string(file, "filename")),
            &format!("<pre>{shown}</pre>{truncated}"),
        ));
    }
    if patches.len() > rendered {
        sections.push(section(sections.len()+1, "Remaining diffs not shown", &format!("<p>{} more changed files are not included here (the diff is too large to read in full). Open the page on GitHub to see them.</p>", patches.len()-rendered)));
    }
}

pub(crate) fn document(
    target: &Target,
    get: &mut impl FnMut(&str) -> Option<Value>,
) -> Option<(String, String)> {
    let base = format!("/repos/{}/{}", encode(&target.owner), encode(&target.repo));
    let endpoint = match target.kind.as_str() {
        "pull" => "pulls",
        "issue" => "issues",
        "commit" => "commits",
        "release" => "releases/tags",
        "discussion" => "discussions",
        _ => return None,
    };
    let object = get(&format!("{base}/{endpoint}/{}", encode(&target.reference)))?;
    if !object.is_object()
        || !(object.get("number").is_some()
            || object.get("sha").is_some()
            || object.get("tag_name").is_some())
    {
        return None;
    }
    let mut title = string(&object, "title").to_string();
    let mut author = login(&object).to_string();
    let mut lead = summary(&object, target);
    let mut heading = match target.kind.as_str() {
        "pull" => "Pull request opened by",
        "issue" => "Issue opened by",
        "commit" => "Commit by",
        "release" => "Release published by",
        _ => "Discussion opened by",
    }
    .to_string();
    let mut date = string(&object, "created_at");
    if target.kind == "commit" {
        let message = pointer(&object, "/commit/message");
        title = message.lines().next().unwrap_or("Commit").to_string();
        if author == "ghost" {
            author = pointer(&object, "/commit/author/name").into();
        }
        date = pointer(&object, "/commit/author/date");
        lead.push_str(&bullets([
            escape(&format!("Commit: {}", string(&object, "sha"))),
            escape(&format!(
                "{} additions, {} deletions",
                object
                    .pointer("/stats/additions")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                object
                    .pointer("/stats/deletions")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            )),
            escape(&format!("Parents: {}", names(&object, "parents", "sha"))),
        ]));
        lead.push_str(&plain(message));
    } else {
        if target.kind == "release" {
            title = [string(&object, "name"), string(&object, "tag_name")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("Release")
                .into();
            date = [string(&object, "published_at"), date]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or("");
            lead.push_str(&bullets([
                escape(&format!("Tag: {}", string(&object, "tag_name"))),
                if object.get("prerelease").and_then(Value::as_bool) == Some(true) {
                    "Pre-release".into()
                } else {
                    String::new()
                },
                if object.get("draft").and_then(Value::as_bool) == Some(true) {
                    "Draft".into()
                } else {
                    String::new()
                },
            ]));
        }
        lead.push_str(&body(&object));
    }
    heading.push_str(&format!(" {author}"));
    if !date.is_empty() {
        heading.push_str(&format!(" — {}", when(date)));
    }
    let mut sections = vec![section(1, &heading, &lead)];
    if target.kind == "pull" {
        let commits = list(
            &format!("{base}/pulls/{}/commits", target.reference),
            250,
            get,
        );
        if !commits.is_empty() {
            let rows = commits.iter().map(|commit| {
                let sha = string(commit, "sha");
                let who = if login(commit) == "ghost" {
                    pointer(commit, "/commit/author/name")
                } else {
                    login(commit)
                };
                escape(&format!(
                    "{} {} — {who}",
                    sha.chars().take(7).collect::<String>(),
                    pointer(commit, "/commit/message")
                        .lines()
                        .next()
                        .unwrap_or("(no commit message)")
                ))
            });
            sections.push(section(
                sections.len() + 1,
                &format!("Commits ({})", commits.len()),
                &bullets(rows),
            ));
        }
    }
    let mut events: Vec<(String, usize, String, String)> = Vec::new();
    let comments_path = match target.kind.as_str() {
        "pull" | "issue" => Some(format!("{base}/issues/{}/comments", target.reference)),
        "commit" => Some(format!("{base}/commits/{}/comments", target.reference)),
        "discussion" => Some(format!("{base}/discussions/{}/comments", target.reference)),
        _ => None,
    };
    if let Some(path) = comments_path {
        let comments = list(&path, 2000, get);
        let mut numbers = std::collections::HashMap::new();
        for comment in comments {
            let text = body(&comment);
            if text.trim().is_empty() {
                continue;
            }
            let mut label = if !string(&object, "answer_html_url").is_empty()
                && string(&comment, "html_url") == string(&object, "answer_html_url")
            {
                "Answer"
            } else {
                "Comment"
            }
            .to_string();
            label.push_str(&format!(" by {}", login(&comment)));
            if let Some(parent) = numbers.get(&number(&comment, "parent_id")) {
                label.push_str(&format!(" — reply to #{parent}"));
            }
            numbers.insert(number(&comment, "id"), sections.len() + events.len() + 1);
            let created = string(&comment, "created_at");
            if !created.is_empty() {
                label.push_str(&format!(" — {}", when(created)));
            }
            events.push((created.into(), events.len(), label, text));
        }
    }
    if target.kind == "pull" {
        for review in list(
            &format!("{base}/pulls/{}/reviews", target.reference),
            2000,
            get,
        ) {
            let state = string(&review, "state");
            let text = body(&review);
            if state == "PENDING"
                || (text.trim().is_empty() && matches!(state, "COMMENTED" | "DISMISSED"))
            {
                continue;
            }
            let verdict = match state {
                "APPROVED" => "approved".into(),
                "CHANGES_REQUESTED" => "requested changes".into(),
                "COMMENTED" => "commented".into(),
                "DISMISSED" => "review dismissed".into(),
                other => other.replace('_', " ").to_lowercase(),
            };
            let date = string(&review, "submitted_at");
            let heading = format!(
                "Review by {} — {verdict}{}",
                login(&review),
                if date.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", when(date))
                }
            );
            events.push((
                date.into(),
                events.len(),
                heading,
                if text.is_empty() {
                    plain(&format!("{} {verdict}.", login(&review)))
                } else {
                    text
                },
            ));
        }
        for comment in list(
            &format!("{base}/pulls/{}/comments", target.reference),
            2000,
            get,
        ) {
            let text = body(&comment);
            if text.is_empty() {
                continue;
            }
            let location = ["line", "original_line", "position"]
                .into_iter()
                .find_map(|key| comment.get(key).and_then(Value::as_i64));
            let path = string(&comment, "path");
            let location = if let Some(line) = location {
                format!("{path} line {line}")
            } else {
                path.into()
            };
            let date = string(&comment, "created_at");
            let heading = format!(
                "Review comment by {}{}{}{}",
                login(&comment),
                if location.is_empty() {
                    String::new()
                } else {
                    format!(" on {location}")
                },
                if number(&comment, "in_reply_to_id") != 0 {
                    " — reply"
                } else {
                    ""
                },
                if date.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", when(date))
                }
            );
            let lines: Vec<_> = string(&comment, "diff_hunk").lines().collect();
            let context = if lines.is_empty() {
                String::new()
            } else {
                format!(
                    "<p>Code the comment is attached to:</p><pre>{}</pre>",
                    lines
                        .iter()
                        .skip(lines.len().saturating_sub(6))
                        .map(|line| escape(line))
                        .collect::<Vec<_>>()
                        .join("<br>")
                )
            };
            events.push((date.into(), events.len(), heading, context + &text));
        }
    }
    if target.kind != "discussion" {
        events.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    }
    for (_, _, heading, body) in events {
        sections.push(section(sections.len() + 1, &heading, &body));
    }
    if target.kind == "pull" {
        files_sections(
            &list(
                &format!("{base}/pulls/{}/files", target.reference),
                300,
                get,
            ),
            &mut sections,
        );
    }
    if target.kind == "commit" {
        files_sections(
            object
                .get("files")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new()),
            &mut sections,
        );
    }
    if target.kind == "release"
        && let Some(assets) = object.get("assets").and_then(Value::as_array)
        && !assets.is_empty()
    {
        let rows = assets.iter().map(|asset| {
            format!(
                "<a href=\"{}\">{} ({} downloads)</a>",
                escape(string(asset, "browser_download_url")),
                escape(string(asset, "name")),
                number(asset, "download_count")
            )
        });
        sections.push(section(
            sections.len() + 1,
            &format!("Assets ({})", assets.len()),
            &bullets(rows),
        ));
    }
    let canonical = canonical(target);
    let full_title = if matches!(target.kind.as_str(), "pull" | "issue" | "discussion") {
        format!(
            "{title} ({}/{}#{})",
            target.owner, target.repo, target.reference
        )
    } else {
        format!("{title} ({}/{})", target.owner, target.repo)
    };
    Some((
        full_title.clone(),
        format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title><meta name=\"author\" content=\"{}\"></head><body><h1>{}</h1><p><a href=\"{}\">Open this page on GitHub</a></p>{}</body></html>",
            escape(&full_title),
            escape(&author),
            escape(&full_title),
            escape(&canonical),
            sections.join("")
        ),
    ))
}

pub(crate) fn canonical(target: &Target) -> String {
    if target.kind == "org_discussion" {
        return format!(
            "https://github.com/orgs/{}/discussions/{}",
            target.owner, target.reference
        );
    }
    let route = match target.kind.as_str() {
        "issue" => "issues",
        "release" => "releases/tag",
        "discussion" => "discussions",
        other => other,
    };
    format!(
        "https://github.com/{}/{}/{route}/{}",
        target.owner,
        target.repo,
        encode(&target.reference)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn target_shapes_match_python_and_reject_lookalikes() {
        for (url, kind, reference) in [
            (
                "http://www.github.com/owner/repo/pull/12/files",
                "pull",
                "12",
            ),
            (
                "https://github.com/owner/repo/issues/3#comment",
                "issue",
                "3",
            ),
            (
                "https://github.com/owner/repo/commit/abcdef123.patch",
                "commit",
                "abcdef123",
            ),
            (
                "https://github.com/owner/repo/releases/tag/v1%2Ftest",
                "release",
                "v1/test",
            ),
            (
                "https://github.com/owner/repo/discussions/2",
                "discussion",
                "2",
            ),
            (
                "https://github.com/orgs/community/discussions/5",
                "org_discussion",
                "5",
            ),
        ] {
            let target = parse_target(url).unwrap();
            assert_eq!(target.kind, kind);
            assert_eq!(target.reference, reference);
        }
        for url in [
            "https://github.com.evil.test/a/b/issues/1",
            "https://github.com/orgs/foo/issues/1",
            "https://github.com/o/r/issues/1/other",
            "https://github.com/o/r/commit/no-sha",
        ] {
            assert!(parse_target(url).is_none(), "{url}");
        }
    }

    #[test]
    fn pull_keeps_metadata_commits_review_timeline_and_diffs() {
        let target = Target {
            kind: "pull".into(),
            owner: "o".into(),
            repo: "r".into(),
            reference: "7".into(),
        };
        let mut requests = Vec::new();
        let (_, html) = document(&target, &mut |path| {
            requests.push(path.to_string());
            Some(if path.ends_with("/pulls/7") { json!({"number":7,"title":"Fix <parser>","state":"closed","merged":true,"user":{"login":"Alice"},"body_html":"<p>Opening <a href='https://example.com'>evidence</a></p>","base":{"ref":"main"},"head":{"label":"o:fix"},"labels":[{"name":"bug"}],"commits":1,"changed_files":1,"additions":2,"deletions":1}) }
                else if path.contains("/commits?") { json!([{"sha":"abcdef123","commit":{"message":"Fix handling\n\nDetails","author":{"name":"Git author"}}}]) }
                else if path.contains("/issues/7/comments?") { json!([{"created_at":"2026-01-03T00:00:00Z","user":{"login":"Carol"},"body":"Discussion comment"}]) }
                else if path.contains("/reviews?") { json!([{"state":"APPROVED","submitted_at":"2026-01-01T00:00:00Z","user":{"login":"Bob"}},{"state":"PENDING","body":"hidden"}]) }
                else if path.contains("/pulls/7/comments?") { json!([{"created_at":"2026-01-02T00:00:00Z","user":{"login":"Dan"},"body":"Inline review","path":"src/lib.rs","line":12,"diff_hunk":"@@ -1 +1 @@\n-old\n+new"}]) }
                else if path.contains("/files?") { json!([{"filename":"src/lib.rs","status":"modified","additions":2,"deletions":1,"patch":"@@ -1 +1 @@\n-old\n+new"}]) }
                else { json!([]) })
        }).unwrap();
        for text in [
            "Merged",
            "o:fix",
            "bug",
            "abcdef1",
            "Git author",
            "approved",
            "Inline review",
            "src/lib.rs line 12",
            "Discussion comment",
            "Changed files",
            "Diff: src/lib.rs",
            "https://example.com",
        ] {
            assert!(html.contains(text), "missing {text}: {html}");
        }
        assert!(html.find("approved").unwrap() < html.find("Inline review").unwrap());
        assert!(html.find("Inline review").unwrap() < html.find("Discussion comment").unwrap());
        assert!(!html.contains("hidden"));
        assert!(html.contains("Fix &lt;parser&gt;"));
        assert_eq!(requests.len(), 6);
    }

    #[test]
    fn issue_pages_every_comment_and_release_preserves_assets() {
        let target = Target {
            kind: "issue".into(),
            owner: "o".into(),
            repo: "r".into(),
            reference: "8".into(),
        };
        let (_, html) = document(&target, &mut |path| Some(if path.ends_with("/issues/8") { json!({"number":8,"title":"Issue","user":{"login":"Alice"},"body":"Initial"}) }
            else if path.ends_with("page=1") { Value::Array((0..100).map(|i| json!({"body":format!("Comment {i}"),"created_at":format!("2026-01-01T00:00:{i:03}Z")})).collect()) }
            else { json!([{"body":"Last comment"}]) })).unwrap();
        assert!(html.contains("Last comment"));
        let release = Target {
            kind: "release".into(),
            reference: "v1/test".into(),
            ..target
        };
        let (title, html) = document(&release, &mut |path| {
            assert!(path.ends_with("/releases/tags/v1%2Ftest"));
            Some(json!({"tag_name":"v1/test","name":"Release","author":{"login":"Alice"},"body_html":"<p>Notes</p>","assets":[{"name":"file.zip","browser_download_url":"https://example.com/file.zip","download_count":42}]}))
        }).unwrap();
        assert!(title.contains("Release"));
        assert!(html.contains("42 downloads"));
        assert!(html.contains("https://example.com/file.zip"));
    }

    #[test]
    fn commit_and_discussion_include_comments_and_replies() {
        let target = Target {
            kind: "commit".into(),
            owner: "o".into(),
            repo: "r".into(),
            reference: "abcdef123".into(),
        };
        let (_, html) = document(&target, &mut |path| Some(if path.ends_with("/commits/abcdef123") { json!({"sha":"abcdef123","commit":{"message":"Commit subject\n\nLong explanation","author":{"name":"Git author","date":"2026-01-01T00:00:00Z"}},"files":[{"filename":"file","patch":"+added"}]}) } else { json!([{"body":"Commit comment"}]) })).unwrap();
        assert!(html.contains("Long explanation"));
        assert!(html.contains("Commit comment"));
        assert!(html.contains("Git author"));
        assert!(html.contains("+added"));
        let target = Target {
            kind: "discussion".into(),
            reference: "8".into(),
            ..target
        };
        let (_, html) = document(&target, &mut |path| Some(if path.ends_with("/discussions/8") { json!({"number":8,"title":"Question","body":"Question body","answer_html_url":"https://github.com/o/r/discussions/8#comment-1"}) }
            else { json!([{"id":1,"body":"Answer body","html_url":"https://github.com/o/r/discussions/8#comment-1"},{"id":2,"parent_id":1,"body":"Reply body"}]) })).unwrap();
        assert!(html.contains("Answer by"));
        assert!(html.contains("reply to #2"));
        assert!(html.contains("Reply body"));
    }
}
