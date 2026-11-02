//! Batch 37 (Accessible Browser helpers): chapter
//! normalization/formatting, the view-entry model, and the
//! article-body arbitration's offline paths.

use blindrss_core::accessible::*;

#[test]
fn chapters_normalize_sort_and_format() {
    let chapters = [
        (65.0, " Middle ", ""),
        (-5.0, "", "https://example.com/a"),
        (3600.0, "End", ""),
        (f64::NAN, "Start", ""),
    ];
    let normalized = normalize_accessible_chapters(&chapters);
    assert_eq!(normalized.len(), 4);
    // Sorted by start; bad starts clamp to 0 (stable order kept).
    assert_eq!(normalized[0].title, "Untitled chapter");
    assert_eq!(normalized[0].href, "https://example.com/a");
    assert_eq!(normalized[2].title, "Middle");
    assert_eq!(normalized[3].start, 3600.0);

    assert_eq!(format_accessible_chapter_timestamp(0.0), "00:00");
    assert_eq!(format_accessible_chapter_timestamp(65.9), "01:05");
    assert_eq!(format_accessible_chapter_timestamp(3661.0), "1:01:01");
    assert_eq!(format_accessible_chapter_timestamp(-3.0), "00:00");

    let text = format_accessible_chapters(&chapters);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Chapters available: 4.");
    assert!(lines[1].starts_with("Chapter 1: 00:00, Untitled chapter."));
    assert!(lines[1].ends_with("Link: https://example.com/a"));
    assert!(lines[3].starts_with("Chapter 3: 01:05, Middle."));
}

#[test]
fn view_entries_walk_categories_depth_first() {
    let feeds = [
        AccessibleFeed {
            id: "f2",
            title: "Beta",
            category: "Tech",
            unread_count: 0,
        },
        AccessibleFeed {
            id: "f1",
            title: "Alpha",
            category: "Tech",
            unread_count: 3,
        },
        AccessibleFeed {
            id: "f3",
            title: "Kid",
            category: "Tech/Pods",
            unread_count: 1,
        },
        AccessibleFeed {
            id: "f4",
            title: "Loose",
            category: "",
            unread_count: 0,
        },
    ];
    let hierarchy = [("Tech/Pods", "Tech")];
    let entries = build_accessible_view_entries(&feeds, &[], &hierarchy, true);
    let ids: Vec<&str> = entries.iter().map(|e| e.view_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "all",
            "unread:all",
            "read:all",
            "favorites:all",
            "category:Tech",
            "f1",
            "f2",
            "category:Tech/Pods",
            "f3",
            "category:Uncategorized",
            "f4",
        ]
    );
    let alpha = entries.iter().find(|e| e.view_id == "f1").unwrap();
    assert_eq!(alpha.label, "Feed: Alpha, unread: 3 (Tech)");
    let kid = entries.iter().find(|e| e.view_id == "f3").unwrap();
    assert_eq!(kid.label, "Feed: Kid, unread: 1 (Tech > Pods)");
    let pods = entries
        .iter()
        .find(|e| e.view_id == "category:Tech/Pods")
        .unwrap();
    assert_eq!(pods.label, "Category: Tech > Pods");

    // Nothing expanded: only the specials and the top-level
    // category rows show (the frame starts with every category
    // expanded; the pure function hides strictly by the set).
    let visible = visible_accessible_view_entries(&entries, &[]);
    let visible_ids: Vec<&str> = visible.iter().map(|e| e.view_id.as_str()).collect();
    assert_eq!(
        visible_ids,
        vec![
            "all",
            "unread:all",
            "read:all",
            "favorites:all",
            "category:Tech",
            "category:Uncategorized"
        ]
    );
    // Top level expanded: loose + Tech feeds show, the
    // grandchild feed stays hidden until Tech/Pods expands.
    let visible = visible_accessible_view_entries(&entries, &["Tech", "Uncategorized"]);
    assert!(visible.iter().any(|e| e.view_id == "f1"));
    assert!(visible.iter().any(|e| e.view_id == "f4"));
    assert!(!visible.iter().any(|e| e.view_id == "f3"));

    // Labels: indent by depth, category state suffix.
    let tech = entries
        .iter()
        .find(|e| e.view_id == "category:Tech")
        .unwrap();
    assert_eq!(
        format_accessible_view_label(tech, &[]),
        "Category: Tech, collapsed"
    );
    assert_eq!(
        format_accessible_view_label(tech, &["Tech"]),
        "Category: Tech, expanded"
    );
    assert_eq!(
        format_accessible_view_label(kid, &["Tech", "Tech/Pods"]),
        "    Feed: Kid, unread: 1 (Tech > Pods)"
    );
}

#[test]
fn body_arbitration_feed_only_is_cacheable() {
    let input = ArticleBodyInput {
        id: "a1",
        url: "",
        content_html: "<p>Feed body text that is complete.</p>",
        title: "T",
        author: "",
    };
    let (body, cacheable) = extract_article_body(&input, None, "", false);
    let body = body.expect("feed body");
    assert!(body.contains("Feed body text"));
    assert!(cacheable, "no web target: feed is all there is");
}

#[test]
fn body_arbitration_provider_wins_when_longer() {
    let long_provider = format!("<p>{}</p>", "Provider paragraph. ".repeat(120));
    let input = ArticleBodyInput {
        id: "a9",
        // A closed loopback port: web extraction fails fast, the
        // short feed stub stays below the provider-fetch gate.
        url: "http://127.0.0.1:9/story",
        content_html: "<p>Short stub.</p>",
        title: "T",
        author: "",
    };
    let fetch = move |_id: &str, _url: &str| Some(long_provider.clone());
    let (body, cacheable) = extract_article_body(&input, Some(&fetch), "", false);
    let body = body.expect("provider body");
    assert!(body.len() > 1500);
    assert!(cacheable, "provider win is authoritative");
}

#[test]
fn body_arbitration_feed_fallback_after_web_failure_not_cached() {
    let input = ArticleBodyInput {
        id: "a10",
        url: "http://127.0.0.1:9/story",
        content_html: "<p>Feed fallback body, long enough to read.</p>",
        title: "T",
        author: "",
    };
    let (body, cacheable) = extract_article_body(&input, None, "", false);
    assert!(body.is_some());
    assert!(
        !cacheable,
        "total web failure for a web article retries next visit"
    );
}

#[test]
fn extractable_url_shapes() {
    assert!(is_extractable_fulltext_url("https://example.com/story"));
    assert!(is_extractable_fulltext_url("https://example.com/ep.mp3"));
    assert!(!is_extractable_fulltext_url("https://example.com/ep.mp4"));
    assert!(!is_extractable_fulltext_url(""));
    assert!(!voiceover_is_running() || cfg!(target_os = "macos"));
}
