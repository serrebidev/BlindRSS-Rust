//! Watch-next chapters (Python `youtube_fulltext.
//! _chapters_from_watch_next`): fixture InnerTube responses
//! drive the injected fetch — no network in tests.

use blindrss_core::ytfull::chapters_from_watch_next;
use serde_json::{Value, json};

fn fetch_from(next: Value, player: Option<Value>) -> impl Fn(&str, &str) -> Option<Value> {
    move |endpoint, _id| match endpoint {
        "next" => Some(next.clone()),
        "player" => player.clone(),
        _ => None,
    }
}

#[test]
fn player_bar_markers_win_and_runs_titles_join() {
    let next = json!({
        "contents": {},
        "playerOverlays": {"decoratedPlayerBarRenderer": {"chapteredPlayerBarRenderer": {
            "chapters": [
                {"chapterRenderer": {"timeRangeStartMillis": 0, "title": {"simpleText": "Intro"}}},
                {"chapterRenderer": {"timeRangeStartMillis": 61000, "title": {"runs": [{"text": "Part "}, {"text": "2"}]}}}
            ]
        }}}
    });
    let found = chapters_from_watch_next("abc123", &fetch_from(next, None)).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].title, "Intro");
    assert_eq!(found[0].start, 0.0);
    assert_eq!(found[1].title, "Part 2");
    assert_eq!(found[1].start, 61.0);
    assert_eq!(
        found[1].href,
        "https://www.youtube.com/watch?v=abc123&t=61s"
    );
}

#[test]
fn markers_map_description_chapters_are_a_source() {
    let next = json!({
        "contents": {},
        "playerOverlays": {"x": {"markersMap": [
            {"key": "AUTO_CHAPTERS", "value": {"chapters": [
                {"chapterRenderer": {"timeRangeStartMillis": 5000, "title": {"simpleText": "Auto"}}}
            ]}},
            {"key": "DESCRIPTION_CHAPTERS", "value": {"chapters": [
                {"chapterRenderer": {"timeRangeStartMillis": 0, "title": {"simpleText": "Start"}}},
                {"chapterRenderer": {"timeRangeStartMillis": 30000, "title": {"simpleText": "Next"}}}
            ]}}
        ]}}
    });
    let found = chapters_from_watch_next("abc123", &fetch_from(next, None)).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].title, "Start");
    assert_eq!(found[1].title, "Next");
}

#[test]
fn key_moments_panel_and_late_first_chapter_gets_untitled_opener() {
    let next = json!({
        "contents": {},
        "engagementPanels": [{"x": {"macroMarkersListRenderer": {"contents": [
            {"macroMarkersListItemRenderer": {"title": {"simpleText": "Big moment"}, "timeDescription": {"simpleText": "1:30"}}}
        ]}}}]
    });
    let found = chapters_from_watch_next("abc123", &fetch_from(next, None)).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].start, 0.0);
    assert_eq!(found[0].title, "<Untitled Chapter 1>");
    assert_eq!(found[1].start, 90.0);
    assert_eq!(found[1].title, "Big moment");
}

#[test]
fn description_timestamps_validate_against_player_length() {
    let next = json!({
        "contents": {},
        "videoDetails": {},
        "attributedDescription": {"content": "0:00 Cold open\n2:00 Middle\n99:00 Beyond the end\n"}
    });
    // attributedDescription is found anywhere in the tree.
    let next = json!({"contents": {}, "deep": next});
    let player = json!({"videoDetails": {"lengthSeconds": "600", "shortDescription": "0:00 Cold open\n2:00 Middle\n99:00 Beyond the end\n"}});
    let found = chapters_from_watch_next("abc123", &fetch_from(next, Some(player))).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[1].start, 120.0);
}

#[test]
fn settled_empty_and_unsettled_none_are_distinct() {
    // No timestamps anywhere -> Some(empty): no fallback.
    let next = json!({"contents": {}, "attributedDescription": {"content": "no times here"}});
    let found = chapters_from_watch_next("abc123", &fetch_from(next, None)).unwrap();
    assert!(found.is_empty());
    // No response at all -> None: the caller falls back.
    let none = chapters_from_watch_next("abc123", &|_, _| None);
    assert!(none.is_none());
    // A response without "contents" cannot settle it either.
    let bare = json!({"playerOverlays": {}});
    assert!(chapters_from_watch_next("abc123", &fetch_from(bare, None)).is_none());
}
