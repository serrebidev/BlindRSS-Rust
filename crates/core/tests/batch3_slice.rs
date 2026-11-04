//! Batch 3 core: SponsorBlock parsing + YouTube ids, silence
//! range parsing/merging.

use blindrss_core::silence::{finalize_ranges, parse_silencedetect};
use blindrss_core::sponsorblock::{parse_segments, youtube_video_id};

#[test]
fn video_id_forms() {
    assert_eq!(
        youtube_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=4s").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        youtube_video_id("https://youtu.be/dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        youtube_video_id("https://www.youtube.com/shorts/dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        youtube_video_id("https://music.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        youtube_video_id("https://example.com/watch?v=dQw4w9WgXcQ"),
        None
    );
    assert_eq!(
        youtube_video_id("https://www.youtube.com/feed/subscriptions"),
        None
    );
}

#[test]
fn segments_parse_and_filter() {
    // The hash-prefix response shape: a list of videos that
    // share the prefix, each with its own segments. Only the
    // requested video's real skip segments survive (the full
    // semantics live in sponsorblock_parity.rs).
    let json = r#"[
      {"videoID": "dQw4w9WgXcQ", "segments": [
        {"segment": [12.5, 30.0], "category": "sponsor", "actionType": "skip", "UUID": "u1"},
        {"segment": [60.0, 75.25], "category": "intro", "actionType": "skip", "UUID": "u2"},
        {"segment": [90.0, 90.0], "category": "sponsor", "actionType": "skip", "UUID": "u3"}
      ]},
      {"videoID": "collision99", "segments": [
        {"segment": [1.0, 9.0], "category": "sponsor", "actionType": "skip", "UUID": "zz"}
      ]}
    ]"#;
    let segs = parse_segments(json, "dQw4w9WgXcQ");
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].start, 12.5);
    assert_eq!(segs[0].end, 30.0);
    assert_eq!(segs[0].uuid, "u1");
    assert!(parse_segments(json, "nothere").is_empty());
    assert!(parse_segments("not json", "dQw4w9WgXcQ").is_empty());
}

#[test]
fn silencedetect_parsing_and_merge() {
    let stderr = "\
[silencedetect @ 0x1] silence_start: 10.0
[silencedetect @ 0x1] silence_end: 12.0 | silence_duration: 2.0
[silencedetect @ 0x1] silence_start: 12.2
[silencedetect @ 0x1] silence_end: 14.0 | silence_duration: 1.8
[silencedetect @ 0x1] silence_start: 99.0
";
    let raw = parse_silencedetect(stderr);
    assert_eq!(raw, vec![(10.0, 12.0), (12.2, 14.0)]);
    // The 0.2s gap merges (<= 0.26), then 60ms padding per side.
    let merged = finalize_ranges(raw, 0.26, 0.06);
    assert_eq!(merged.len(), 1);
    assert!((merged[0].0 - 10.06).abs() < 1e-9);
    assert!((merged[0].1 - 13.94).abs() < 1e-9);
    // A 0.7s sliver disappears entirely under padding.
    assert!(finalize_ranges(vec![(5.0, 5.1)], 0.26, 0.06).is_empty());
}
