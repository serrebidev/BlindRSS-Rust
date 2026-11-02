//! Accessible Browser support (Python `gui/accessibility.py`'s
//! GUI-free helpers).
//!
//! The pure presentation logic behind the VoiceOver-first
//! Accessible Browser window: chapter normalization/formatting
//! for the reader, the full-text arbitration the browser's
//! reader uses (web vs feed vs provider server-side fetch,
//! longest wins, with Python's cacheability rules), the
//! flattened view-entry model (special views, categories as
//! expandable rows, feeds with unread counts and breadcrumb
//! labels), and the macOS VoiceOver probe that auto-opens the
//! window. The frame itself lives in the app.

use crate::extract::{self, ExtractOutcome};

/// A body shorter than this is probably a snippet/paywall stub,
/// so the provider's server-side fetcher is worth asking
/// (Python `_PROVIDER_FETCH_MIN_LEN`).
pub const PROVIDER_FETCH_MIN_LEN: usize = 1500;

/// One chapter as the reader presents it (Python's normalized
/// chapter dicts: start seconds, title, optional link).
#[derive(Debug, Clone, PartialEq)]
pub struct AccessibleChapter {
    pub start: f64,
    pub title: String,
    pub href: String,
}

/// Python `normalize_accessible_chapters`: keep mapping-shaped
/// chapters, clamp bad/negative/non-finite starts to 0, default
/// the title to "Untitled chapter", take href from href/url/
/// link, and sort by start.
pub fn normalize_accessible_chapters(chapters: &[(f64, &str, &str)]) -> Vec<AccessibleChapter> {
    let mut out: Vec<AccessibleChapter> = chapters
        .iter()
        .map(|(start, title, href)| {
            let start = if start.is_finite() && *start >= 0.0 {
                *start
            } else {
                0.0
            };
            let title = title.trim();
            AccessibleChapter {
                start,
                title: if title.is_empty() {
                    "Untitled chapter".to_string()
                } else {
                    title.to_string()
                },
                href: href.trim().to_string(),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Python `format_accessible_chapter_timestamp`: h:mm:ss when
/// there are hours, else mm:ss.
pub fn format_accessible_chapter_timestamp(start: f64) -> String {
    let seconds = if start.is_finite() && start >= 0.0 {
        start
    } else {
        0.0
    };
    let total = seconds as i64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

/// Python `format_accessible_chapters`: a count line, then one
/// "Chapter N: <timestamp>, <title>." line per chapter, with a
/// " Link: <url>" suffix when the chapter carries one.
pub fn format_accessible_chapters(chapters: &[(f64, &str, &str)]) -> String {
    let list = normalize_accessible_chapters(chapters);
    let mut lines = vec![format!("Chapters available: {}.", list.len())];
    for (i, ch) in list.iter().enumerate() {
        let mut line = format!(
            "Chapter {}: {}, {}.",
            i + 1,
            format_accessible_chapter_timestamp(ch.start),
            ch.title
        );
        if !ch.href.is_empty() {
            line.push_str(&format!(" Link: {}", ch.href));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Python `is_extractable_fulltext_url`: a URL can yield webpage
/// text (or bounded ID3 text) unless it points straight at
/// non-ID3 media.
pub fn is_extractable_fulltext_url(url: &str) -> bool {
    let value = url.trim();
    !value.is_empty()
        && (!extract::looks_like_media_url(value) || extract::looks_like_id3_media_url(value))
}

/// A provider's server-side full-text fetch:
/// (article_id, url) -> raw HTML when the provider has more.
pub type ProviderFetch = dyn Fn(&str, &str) -> Option<String>;

/// The article fields `extract_article_body` arbitrates over.
pub struct ArticleBodyInput<'a> {
    pub id: &'a str,
    pub url: &'a str,
    pub content_html: &'a str,
    pub title: &'a str,
    pub author: &'a str,
}

/// Python `extract_article_body`: try web extraction, the feed
/// body (cleaned and raw views), and — when the best so far is
/// short — the provider's server-side fetch; keep the longest.
/// Returns (body, cacheable) with Python's cacheability rules:
/// web/provider wins are cacheable; a feed win is cacheable
/// when there is no web target or web extraction worked; a
/// feed fallback after total web failure is NOT cached so the
/// next visit retries. Blocking (network I/O); call off the UI
/// thread.
pub fn extract_article_body(
    input: &ArticleBodyInput,
    provider_fetch: Option<&ProviderFetch>,
    encoding: &str,
    keep_alt: bool,
) -> (Option<String>, bool) {
    let url = input.url.trim();
    let has_web_target = is_extractable_fulltext_url(url)
        && !(extract::looks_like_id3_media_url(url) && !input.content_html.trim().is_empty());

    let mut web_text: Option<String> = None;
    if has_web_target {
        let (outcome, _err) = extract::fetch_full_text_opts(url, "", None, encoding, keep_alt);
        if let ExtractOutcome::Article { text, .. } = outcome {
            let t = text.trim();
            if !t.is_empty() {
                web_text = Some(t.to_string());
            }
        }
    }

    // Feed candidates: the extractor's cleaned view and the
    // plain html-to-text view; boilerplate-stripped either way;
    // the longer is the feed candidate.
    let mut feed_candidates: Vec<String> = Vec::new();
    if let ExtractOutcome::Article { text, .. } =
        extract::extract_article(input.content_html, url, None)
    {
        let t = text.trim();
        if !t.is_empty() {
            feed_candidates.push(t.to_string());
        }
    }
    let raw = extract::html_to_text_opts(input.content_html, keep_alt);
    let raw = raw.trim();
    if !raw.is_empty() {
        feed_candidates.push(raw.to_string());
    }
    let feed_candidates: Vec<String> = feed_candidates
        .iter()
        .map(|c| crate::postprocess::strip_leading_boilerplate(c))
        .collect();
    let feed_text = feed_candidates.iter().max_by_key(|c| c.len()).cloned();

    let (mut best, mut source): (Option<String>, &str) = match (&web_text, &feed_text) {
        (Some(w), Some(f)) => {
            if w.len() > f.len() {
                (web_text.clone(), "web")
            } else {
                (feed_text.clone(), "feed")
            }
        }
        (Some(_), None) => (web_text.clone(), "web"),
        (None, Some(_)) => (feed_text.clone(), "feed"),
        (None, None) => (None, ""),
    };

    let need_more = best
        .as_ref()
        .is_none_or(|b| b.len() < PROVIDER_FETCH_MIN_LEN);
    if let (Some(fetch), true) = (provider_fetch, has_web_target && need_more)
        && !input.id.trim().is_empty()
        && let Some(provider_html) = fetch(input.id.trim(), url)
        && let ExtractOutcome::Article { text, .. } =
            extract::extract_article(&provider_html, url, None)
    {
        let provider_text = text.trim();
        if !provider_text.is_empty() && best.as_ref().is_none_or(|b| provider_text.len() > b.len())
        {
            best = Some(provider_text.to_string());
            source = "provider";
        }
    }

    match best {
        None => (None, false),
        Some(b) => {
            let cacheable = match source {
                "web" | "provider" => true,
                _ => !has_web_target || web_text.is_some(),
            };
            (Some(b), cacheable)
        }
    }
}

/// Python `voiceover_is_running`: macOS only; pgrep -x
/// VoiceOver. False on every other platform.
pub fn voiceover_is_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("pgrep")
            .args(["-x", "VoiceOver"])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Python's UNCATEGORIZED sentinel.
pub const UNCATEGORIZED: &str = "Uncategorized";

/// The last path segment of a category path (Python
/// `category_display_leaf`).
pub fn category_display_leaf(path: &str) -> String {
    path.rsplit('/').next().unwrap_or("").trim().to_string()
}

/// One flattened Accessible Browser view row.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessibleViewEntry {
    pub label: String,
    pub view_id: String,
    /// "special" | "category" | "feed"
    pub kind: String,
    pub parent_cats: Vec<String>,
    pub cat_name: String,
}

/// A feed as the entry builder needs it.
pub struct AccessibleFeed<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub category: &'a str,
    pub unread_count: i64,
}

/// Python `build_accessible_view_entries`: the three special
/// views (plus Favorites when asked), then categories in
/// case-insensitive order walked depth-first — each category
/// row, its feeds (title order, unread suffix, breadcrumb
/// suffix), then its child categories. `hierarchy` maps a
/// category path to its parent path.
pub fn build_accessible_view_entries(
    feeds: &[AccessibleFeed],
    categories: &[&str],
    hierarchy: &[(&str, &str)],
    include_favorites: bool,
) -> Vec<AccessibleViewEntry> {
    let special = |label: &str, view_id: &str| AccessibleViewEntry {
        label: label.to_string(),
        view_id: view_id.to_string(),
        kind: "special".to_string(),
        parent_cats: Vec::new(),
        cat_name: String::new(),
    };
    let mut entries = vec![
        special("All Articles", "all"),
        special("Unread Articles", "unread:all"),
        special("Read Articles", "read:all"),
    ];
    if include_favorites {
        entries.push(special("Favorites", "favorites:all"));
    }

    fn add_cat(cat_names: &mut Vec<String>, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !cat_names.iter().any(|c| c == name) {
            cat_names.push(name.to_string());
        }
    }
    let mut cat_names: Vec<String> = Vec::new();
    for c in categories {
        add_cat(&mut cat_names, c);
    }
    for f in feeds {
        let cat = if f.category.trim().is_empty() {
            UNCATEGORIZED
        } else {
            f.category.trim()
        };
        add_cat(&mut cat_names, cat);
    }
    if cat_names.is_empty() && !feeds.is_empty() {
        add_cat(&mut cat_names, UNCATEGORIZED);
    }

    let parent_of: std::collections::HashMap<&str, &str> = hierarchy.iter().copied().collect();
    let mut children_of: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut top_level: Vec<String> = Vec::new();
    let mut sorted_cats = cat_names.clone();
    sorted_cats.sort_by_key(|a| a.to_lowercase());
    for cat in &sorted_cats {
        let parent = parent_of.get(cat.as_str()).copied().unwrap_or("").trim();
        if !parent.is_empty() && cat_names.iter().any(|c| c == parent) {
            children_of
                .entry(parent.to_string())
                .or_default()
                .push(cat.clone());
        } else {
            top_level.push(cat.clone());
        }
    }
    for kids in children_of.values_mut() {
        kids.sort_by_key(|a| a.to_lowercase());
    }

    fn walk(
        cat: &str,
        path: &[String],
        feeds: &[AccessibleFeed],
        children_of: &std::collections::HashMap<String, Vec<String>>,
        entries: &mut Vec<AccessibleViewEntry>,
    ) {
        let mut category_path = path.to_vec();
        category_path.push(cat.to_string());
        let path_label = category_path
            .iter()
            .map(|c| {
                if c == UNCATEGORIZED {
                    c.clone()
                } else {
                    category_display_leaf(c)
                }
            })
            .collect::<Vec<_>>()
            .join(" > ");
        entries.push(AccessibleViewEntry {
            label: format!("Category: {path_label}"),
            view_id: format!("category:{cat}"),
            kind: "category".to_string(),
            parent_cats: path.to_vec(),
            cat_name: cat.to_string(),
        });
        let mut cat_feeds: Vec<&AccessibleFeed> = feeds
            .iter()
            .filter(|f| {
                let fc = if f.category.trim().is_empty() {
                    UNCATEGORIZED
                } else {
                    f.category.trim()
                };
                fc == cat
            })
            .collect();
        cat_feeds.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.id.cmp(b.id))
        });
        for f in cat_feeds {
            let title = if f.title.trim().is_empty() {
                f.id
            } else {
                f.title.trim()
            };
            let mut label = format!("Feed: {title}");
            if f.unread_count > 0 {
                label.push_str(&format!(", unread: {}", f.unread_count));
            }
            label.push_str(&format!(" ({path_label})"));
            entries.push(AccessibleViewEntry {
                label,
                view_id: f.id.to_string(),
                kind: "feed".to_string(),
                parent_cats: category_path.clone(),
                cat_name: String::new(),
            });
        }
        if let Some(kids) = children_of.get(cat) {
            for child in kids.clone() {
                walk(&child, &category_path, feeds, children_of, entries);
            }
        }
    }
    for cat in &top_level {
        walk(cat, &[], feeds, &children_of, &mut entries);
    }
    entries
}

/// Python `visible_accessible_view_entries`: category/feed rows
/// show only when every ancestor category is expanded.
pub fn visible_accessible_view_entries(
    entries: &[AccessibleViewEntry],
    expanded_categories: &[&str],
) -> Vec<AccessibleViewEntry> {
    let expanded: Vec<&str> = expanded_categories
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    entries
        .iter()
        .filter(|e| {
            if (e.kind == "category" || e.kind == "feed")
                && e.parent_cats.iter().any(|p| !expanded.contains(&p.trim()))
            {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

/// Python `format_accessible_view_label`: rows indent by their
/// ancestor depth; category rows gain ", expanded"/",
/// collapsed".
pub fn format_accessible_view_label(
    entry: &AccessibleViewEntry,
    expanded_categories: &[&str],
) -> String {
    let expanded: Vec<&str> = expanded_categories
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    let parents: Vec<&str> = entry
        .parent_cats
        .iter()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    let indent = "  ".repeat(parents.len());
    if entry.kind != "category" {
        return format!("{indent}{}", entry.label);
    }
    let state = if !entry.cat_name.is_empty() && expanded.contains(&entry.cat_name.trim()) {
        "expanded"
    } else {
        "collapsed"
    };
    format!("{indent}{}, {state}", entry.label)
}
