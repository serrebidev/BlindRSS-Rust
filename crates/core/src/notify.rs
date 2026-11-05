//! New-article notification payload policy (Python
//! `gui/mainframe.py`'s notification assembly:
//! `_queue_new_article_notifications_from_state`,
//! `_notification_preview_text`, `_build_notification_body`,
//! `_build_notification_activation_payload`,
//! `_load_recent_notification_items`,
//! `_resolve_notification_article`).
//!
//! Core owns the policy — which articles notify, the title /
//! body text of each notification, the shared per-refresh cap,
//! dedupe, and the capped summary's suppressed count — so it
//! is testable without a window. The app supplies one
//! [`FeedBatch`] per refreshed feed (its articles already
//! rule-filtered: `StoreOutcome::new_articles` holds exactly
//! the notification-eligible ones, Python's D13 semantics) and
//! sends the planned notifications through the platform
//! surface, routing clicks back through
//! [`resolve_payload_article`] into normal article activation.

use crate::config::Config;
use crate::db::{Article, Db};

/// One new article as a notification candidate (Python's
/// per-article summary dict from
/// `providers/local.py::_record_new_article`). `preview_source`
/// is the raw text the preview is derived from (article content,
/// falling back to the description); the planner runs
/// [`preview_text`] over it, as Python's queue applies
/// `_notification_preview_text` to the recorded preview.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewArticleNote {
    pub id: String,
    pub title: String,
    pub preview_source: String,
    pub url: String,
    pub media_url: String,
    pub media_type: String,
}

impl From<&Article> for NewArticleNote {
    fn from(article: &Article) -> Self {
        NewArticleNote {
            id: article.id.clone(),
            title: article.title.clone(),
            preview_source: article
                .content
                .clone()
                .filter(|c| !c.trim().is_empty())
                .or_else(|| article.description.clone())
                .unwrap_or_default(),
            url: article.url.clone(),
            media_url: article.media_url.clone().unwrap_or_default(),
            media_type: article.media_type.clone().unwrap_or_default(),
        }
    }
}

/// One feed's new items in a refresh, in refresh order.
/// `articles` is the normal path; when a producer can only
/// report a count, `generic_count` + `resolved` drive Python's
/// generic branch: `resolved` holds the recent unread items
/// the app loaded for the feed (Python
/// `_load_recent_notification_items`, newest view order,
/// already skipping ids in `notified`), and any count beyond
/// what resolved covers falls back to generic bodies.
#[derive(Debug, Clone, Default)]
pub struct FeedBatch {
    pub feed_id: String,
    pub feed_title: String,
    pub articles: Vec<NewArticleNote>,
    pub generic_count: usize,
    pub resolved: Vec<NewArticleNote>,
}

/// The click payload a notification carries (Python
/// `_build_notification_activation_payload`): enough to
/// resolve and open the article through the normal activation
/// path when the user activates the notification.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActivationPayload {
    pub article_id: String,
    pub title: String,
    pub url: String,
    pub feed_id: String,
    pub media_url: String,
    pub media_type: String,
}

/// One notification to show, in show order.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedNotification {
    pub title: String,
    pub body: String,
    pub payload: Option<ActivationPayload>,
}

/// The planner's result for one refresh: the notifications to
/// show, how many article notifications the cap suppressed,
/// and the summary notification to append at the end of the
/// refresh when the cap suppressed any and the summary setting
/// is on (Python's `windows_notifications_show_summary_when_capped`
/// tail in the refresh drivers).
#[derive(Debug, Clone, Default)]
pub struct NotifyPlan {
    pub notifications: Vec<PlannedNotification>,
    pub suppressed: usize,
    pub summary: Option<PlannedNotification>,
}

/// Python `_notification_preview_text` /
/// `providers/local.py::_plain_text_preview`: strip markup to
/// plain text (tags become spaces, entities decoded),
/// collapse whitespace to single spaces, and truncate to
/// `max_len` characters with an ellipsis.
pub fn preview_text(raw: &str) -> String {
    preview_text_limited(raw, 180)
}

fn preview_text_limited(raw: &str, max_len: usize) -> String {
    let text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    // The queue's `_notification_preview_text` always runs
    // the markup strip (BeautifulSoup decodes entities even
    // in otherwise plain text), so this does too.
    let stripped = strip_tags(text);
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max_len {
        let keep: String = collapsed.chars().take(max_len.saturating_sub(3)).collect();
        format!("{}...", keep.trim_end())
    } else {
        collapsed
    }
}

/// Tag-stripping in the shape of BeautifulSoup's
/// `get_text(" ", strip=True)` (Python's parser path) / its
/// regex fallback (`<[^>]+>` → " "): every tag becomes a
/// space, text between tags is kept, entities are decoded.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                // Consume through the closing '>'; a '<' with
                // no '>' ahead is literal text, so what was
                // scanned is put back.
                let mut scanned = String::new();
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == '>' {
                        closed = true;
                        break;
                    }
                    scanned.push(inner);
                }
                if closed {
                    out.push(' ');
                } else {
                    out.push('<');
                    out.push_str(&scanned);
                }
            }
            '&' => {
                let mut entity = String::new();
                let mut terminated = false;
                for inner in chars.by_ref() {
                    if inner == ';' {
                        terminated = true;
                        break;
                    }
                    if !inner.is_alphanumeric() && inner != '#' {
                        // Not an entity: emit what we saw.
                        out.push('&');
                        out.push_str(&entity);
                        out.push(inner);
                        entity.clear();
                        break;
                    }
                    entity.push(inner);
                    if entity.len() > 10 {
                        out.push('&');
                        out.push_str(&entity);
                        entity.clear();
                        break;
                    }
                }
                if terminated {
                    match decode_entity(&entity) {
                        Some(decoded) => out.push(decoded),
                        None => {
                            out.push('&');
                            out.push_str(&entity);
                            out.push(';');
                        }
                    }
                } else if !entity.is_empty() {
                    out.push('&');
                    out.push_str(&entity);
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => {
            let num = entity.strip_prefix('#')?;
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            char::from_u32(code)
        }
    }
}

/// Python `_build_notification_body`.
pub fn notification_body(preview: &str, feed_title: &str, include_feed: bool) -> String {
    let preview = preview.trim();
    let feed = feed_title.trim();
    if include_feed && !feed.is_empty() && !preview.is_empty() {
        return format!("{feed} - {preview}");
    }
    if include_feed && !feed.is_empty() {
        return feed.to_string();
    }
    if !preview.is_empty() {
        return preview.to_string();
    }
    "New article available.".to_string()
}

fn display_title(raw: &str) -> String {
    let title = raw.trim();
    if title.is_empty() {
        "New article".to_string()
    } else {
        title.to_string()
    }
}

/// Python `_build_notification_activation_payload`: None when
/// the item carries nothing activatable (no id, url or media
/// url).
pub fn activation_payload(item: &NewArticleNote, feed_id: &str) -> Option<ActivationPayload> {
    let article_id = item.id.trim().to_string();
    let url = item.url.trim().to_string();
    let media_url = item.media_url.trim().to_string();
    if article_id.is_empty() && url.is_empty() && media_url.is_empty() {
        return None;
    }
    Some(ActivationPayload {
        article_id,
        title: display_title(&item.title),
        url,
        feed_id: feed_id.trim().to_string(),
        media_url,
        media_type: item.media_type.trim().to_string(),
    })
}

/// The config's notification-excluded feed ids (Python
/// `_get_notification_excluded_feed_ids`).
fn excluded_feed_ids(cfg: &Config) -> std::collections::HashSet<String> {
    cfg.raw()
        .get("windows_notifications_excluded_feeds")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Plan one refresh's notifications (Python
/// `_queue_new_article_notifications_from_state` applied to
/// each feed state in order, plus the refresh drivers'
/// capped-summary tail). Pure: no I/O beyond reading `cfg`.
pub fn plan_notifications(cfg: &Config, batches: &[FeedBatch]) -> NotifyPlan {
    let mut plan = NotifyPlan::default();
    if !cfg.get_bool("windows_notifications_enabled", false) {
        return plan;
    }
    let excluded = excluded_feed_ids(cfg);
    let include_feed = cfg.get_bool("windows_notifications_include_feed_name", true);
    let max = cfg.get_i64("windows_notifications_max_per_refresh", 0);
    // Python: remaining None means unlimited (cap <= 0).
    let mut remaining: Option<usize> = if max <= 0 { None } else { Some(max as usize) };
    let mut notified: std::collections::HashSet<String> = std::collections::HashSet::new();

    for batch in batches {
        let feed_id = batch.feed_id.trim();
        if !feed_id.is_empty() && excluded.contains(feed_id) {
            continue;
        }
        let feed_title = batch.feed_title.trim();
        // Python returns from the queue for this state when
        // the budget is already spent: its items are neither
        // shown nor counted as suppressed.
        if remaining == Some(0) {
            continue;
        }
        if batch.articles.is_empty() {
            plan_generic(
                batch,
                feed_id,
                feed_title,
                include_feed,
                &mut remaining,
                &mut notified,
                &mut plan,
            );
        } else {
            for item in &batch.articles {
                if remaining == Some(0) {
                    plan.suppressed += 1;
                    continue;
                }
                let id = item.id.trim();
                if !id.is_empty() {
                    if notified.contains(id) {
                        continue;
                    }
                    notified.insert(id.to_string());
                }
                let body = notification_body(
                    &preview_text(&item.preview_source),
                    feed_title,
                    include_feed,
                );
                plan.notifications.push(PlannedNotification {
                    title: display_title(&item.title),
                    body,
                    payload: activation_payload(item, feed_id),
                });
                if let Some(rem) = remaining {
                    remaining = Some(rem - 1);
                }
            }
        }
    }

    if plan.suppressed > 0 && cfg.get_bool("windows_notifications_show_summary_when_capped", true) {
        let hidden = plan.suppressed;
        let body = if hidden == 1 {
            format!("{hidden} new article notification was suppressed by your cap.")
        } else {
            format!("{hidden} new article notifications were suppressed by your cap.")
        };
        plan.summary = Some(PlannedNotification {
            title: "BlindRSS".to_string(),
            body,
            payload: None,
        });
    }
    plan
}

/// Python's generic branch: the state carried only a count.
/// Resolved items notify per-article first; the unresolved
/// remainder falls back to generic bodies.
#[allow(clippy::too_many_arguments)]
fn plan_generic(
    batch: &FeedBatch,
    feed_id: &str,
    feed_title: &str,
    include_feed: bool,
    remaining: &mut Option<usize>,
    notified: &mut std::collections::HashSet<String>,
    plan: &mut NotifyPlan,
) {
    let mut generic = batch.generic_count;
    if generic == 0 {
        return;
    }
    if let Some(rem) = *remaining
        && generic > rem
    {
        plan.suppressed += generic - rem;
        generic = rem;
    }
    let mut sent = 0usize;
    // Python's loader returns at most `generic` items.
    for item in batch.resolved.iter().take(generic) {
        let id = item.id.trim();
        if !id.is_empty() {
            if notified.contains(id) {
                continue;
            }
            notified.insert(id.to_string());
        }
        let body = notification_body(
            &preview_text(&item.preview_source),
            feed_title,
            include_feed,
        );
        plan.notifications.push(PlannedNotification {
            title: display_title(&item.title),
            body,
            payload: activation_payload(item, feed_id),
        });
        sent += 1;
        if let Some(rem) = *remaining {
            *remaining = Some(rem - 1);
        }
    }
    for _ in 0..generic.saturating_sub(sent) {
        let body = if include_feed && !feed_title.is_empty() {
            feed_title.to_string()
        } else {
            "New article available.".to_string()
        };
        plan.notifications.push(PlannedNotification {
            title: "New article".to_string(),
            body,
            payload: None,
        });
        if let Some(rem) = *remaining {
            *remaining = Some(rem - 1);
        }
    }
}

/// Resolve a clicked notification's payload to an article
/// (Python `_resolve_notification_article`): the currently
/// displayed articles first (by id, then by url), then the
/// store by id, then an article constructed from the payload
/// itself when it carries a url or media url to open.
pub fn resolve_payload_article(
    payload: &ActivationPayload,
    current: &[Article],
    db: Option<&Db>,
) -> Option<Article> {
    let article_id = payload.article_id.trim();
    let article_url = payload.url.trim();
    for article in current {
        if !article_id.is_empty() && article.id.trim() == article_id {
            return Some(article.clone());
        }
        if !article_url.is_empty() && article.url.trim() == article_url {
            return Some(article.clone());
        }
    }
    if !article_id.is_empty()
        && let Some(db) = db
        && let Ok(Some(article)) = db.article_by_id(article_id)
    {
        return Some(article);
    }
    let media_url = payload.media_url.trim();
    if article_url.is_empty() && media_url.is_empty() {
        return None;
    }
    let id = if !article_id.is_empty() {
        article_id.to_string()
    } else if !article_url.is_empty() {
        article_url.to_string()
    } else {
        media_url.to_string()
    };
    Some(Article {
        id,
        feed_id: payload.feed_id.trim().to_string(),
        title: display_title(&payload.title),
        url: article_url.to_string(),
        content: Some(String::new()),
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: if media_url.is_empty() {
            None
        } else {
            Some(media_url.to_string())
        },
        media_type: if payload.media_type.trim().is_empty() {
            None
        } else {
            Some(payload.media_type.trim().to_string())
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_strips_tags_and_collapses_whitespace() {
        assert_eq!(
            preview_text("<p>Hello <b>wide</b>   world</p>\n<p>again</p>"),
            "Hello wide world again"
        );
        assert_eq!(preview_text("plain   text\nhere"), "plain text here");
        assert_eq!(preview_text(""), "");
        assert_eq!(preview_text("a &amp; b &lt;ok&gt;"), "a & b <ok>");
    }

    #[test]
    fn preview_truncates_at_180_with_ellipsis() {
        let raw = "x".repeat(200);
        let out = preview_text(&raw);
        assert_eq!(out.chars().count(), 180);
        assert!(out.ends_with("..."));
        let exact = "y".repeat(180);
        assert_eq!(preview_text(&exact), exact);
    }

    #[test]
    fn body_combines_feed_and_preview() {
        assert_eq!(notification_body("prev", "Feed", true), "Feed - prev");
        assert_eq!(notification_body("", "Feed", true), "Feed");
        assert_eq!(notification_body("prev", "Feed", false), "prev");
        assert_eq!(notification_body("", "", true), "New article available.");
    }
}
