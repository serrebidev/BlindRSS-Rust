//! Batch 35b regression tests: multi-page following, text merging,
//! the metered-preview notice, ID3 show notes, YouTube structured
//! text shapes, and GitHub page classification. All offline: the
//! helpers under test are pure, and the ID3 cases read synthetic
//! local files.

use blindrss_core::extract::{
    find_next_page, id3_metadata, looks_like_id3_media_url, looks_like_media_url,
    looks_like_metered_preview, merge_texts, metered_preview_notice,
};
use blindrss_core::threads::{GithubPage, github_page_parts};
use blindrss_core::ytfull::{chapters_from_info, format_comments, video_id_from_url};
use serde_json::json;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_mp3(tag: &[u8]) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("blindrss-b35b-{}-{n}.mp3", std::process::id()));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(tag).unwrap();
    // Fake audio bytes after the tag.
    f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
    path.to_string_lossy().to_string()
}

fn synchsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn text_frame(id: &str, text: &str, v24: bool) -> Vec<u8> {
    let mut payload = vec![3u8]; // UTF-8
    payload.extend_from_slice(text.as_bytes());
    let mut frame = Vec::new();
    frame.extend_from_slice(id.as_bytes());
    if v24 {
        frame.extend_from_slice(&synchsafe(payload.len()));
    } else {
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    }
    frame.extend_from_slice(&[0, 0]); // flags
    frame.extend_from_slice(&payload);
    frame
}

fn uslt_frame(text: &str, v24: bool) -> Vec<u8> {
    let mut payload = vec![3u8]; // UTF-8
    payload.extend_from_slice(b"eng");
    payload.push(0); // empty descriptor
    payload.extend_from_slice(text.as_bytes());
    let mut frame = Vec::new();
    frame.extend_from_slice(b"USLT");
    if v24 {
        frame.extend_from_slice(&synchsafe(payload.len()));
    } else {
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    }
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&payload);
    frame
}

fn id3_tag(frames: &[u8], v24: bool) -> Vec<u8> {
    let mut tag = Vec::new();
    tag.extend_from_slice(b"ID3");
    tag.push(if v24 { 4 } else { 3 });
    tag.push(0);
    tag.push(0); // flags
    tag.extend_from_slice(&synchsafe(frames.len()));
    tag.extend_from_slice(frames);
    tag
}

#[test]
fn next_page_link_and_anchor_forms() {
    let base = "https://example.com/story";
    let html = r#"<html><head><link rel="next" href="/story/2"></head><body>x</body></html>"#;
    assert_eq!(
        find_next_page(html, base).as_deref(),
        Some("https://example.com/story/2")
    );
    let html = r#"<a rel="next" href="page2.html">whatever</a>"#;
    assert_eq!(
        find_next_page(html, base).as_deref(),
        Some("https://example.com/page2.html")
    );
    let html = r#"<a href="/p/2">Next page</a>"#;
    assert_eq!(
        find_next_page(html, base).as_deref(),
        Some("https://example.com/p/2")
    );
    let html = r#"<a href="/p/2">›</a>"#;
    assert_eq!(
        find_next_page(html, base).as_deref(),
        Some("https://example.com/p/2")
    );
    let html = r#"<a href="/p/2" class="pagination-next">2</a>"#;
    assert_eq!(
        find_next_page(html, base).as_deref(),
        Some("https://example.com/p/2")
    );
}

#[test]
fn next_page_guards() {
    // Next-story teasers are a different article, not page 2.
    let html = r#"<a href="/other" class="next-story-link">Next story</a>"#;
    assert_eq!(find_next_page(html, "https://example.com/story"), None);
    // Hosts whose "next" points at a different story never follow.
    let html = r#"<link rel="next" href="/story/2">"#;
    assert_eq!(find_next_page(html, "https://www.wired.com/story/x"), None);
    assert_eq!(
        find_next_page(html, "https://www.bloomberg.com/news/x"),
        None
    );
    // Social share targets are not pagination.
    let html = r#"<a href="https://facebook.com/sharer">Next</a>"#;
    assert_eq!(find_next_page(html, "https://example.com/story"), None);
    // No candidate at all.
    assert_eq!(
        find_next_page("<p>hello</p>", "https://example.com/story"),
        None
    );
}

#[test]
fn merge_texts_dedupes_and_keeps_short_lines() {
    let long = "This is a long paragraph that appears on both pages of the article.";
    let page1 = format!("{long}\nPros\nFirst page unique content lives here now.");
    let page2 = format!("{long}\nPros\nSecond page unique content lives here now.");
    let merged = merge_texts(&[page1, page2]);
    assert_eq!(
        merged.matches(long).count(),
        1,
        "long dup kept once: {merged}"
    );
    assert_eq!(
        merged.matches("Pros").count(),
        2,
        "short repeats are content: {merged}"
    );
    assert!(merged.contains("First page unique"));
    assert!(merged.contains("Second page unique"));
    // Pages join on a blank line; lines within a page on one newline.
    assert!(
        merged.contains("First page unique content lives here now.\n\nPros"),
        "page break: {merged:?}"
    );
}

#[test]
fn merge_texts_table_blocks_move_as_units() {
    let table =
        "Table with 2 rows and 2 columns:\nRow 1: Name. Value.\nRow 2: A. B.\nEnd of table.";
    let page1 = format!("Lead paragraph that is long enough to dedupe.\n{table}");
    let page2 = format!("{table}\nTrailing paragraph that is long enough too.");
    let merged = merge_texts(&[page1, page2]);
    assert_eq!(merged.matches("Table with 2 rows").count(), 1, "{merged}");
    assert_eq!(merged.matches("End of table.").count(), 1, "{merged}");
    assert!(merged.contains("Row 2: A. B."));
    // A "Table with" line that is not a real marker is a normal line.
    let merged =
        merge_texts(&["Table with chairs around it seats six people at dinner.".to_string()]);
    assert!(merged.contains("Table with chairs"));
    // Structure markers repeat per quote/heading and are kept.
    let merged = merge_texts(&[
        "Quote:\nSome quoted text\nEnd of quote.".to_string(),
        "Quote:\nOther quoted text\nEnd of quote.".to_string(),
    ]);
    assert_eq!(merged.matches("Quote:").count(), 2, "{merged}");
    assert_eq!(merged.matches("End of quote.").count(), 2, "{merged}");
}

#[test]
fn metered_preview_marker() {
    let html = "<html><body><p>Subscribe to read as many articles as you like.</p></body></html>";
    assert!(looks_like_metered_preview(html));
    assert!(!looks_like_metered_preview(
        "<p>An ordinary article body.</p>"
    ));
    assert!(metered_preview_notice().starts_with("Only the free preview"));
}

#[test]
fn media_url_shapes() {
    assert!(looks_like_media_url("https://x.test/ep.mp3?dl=1"));
    assert!(looks_like_media_url("https://x.test/pic.PNG"));
    assert!(looks_like_media_url("https://x.test/doc.pdf"));
    assert!(!looks_like_media_url("https://x.test/story.html"));
    assert!(looks_like_id3_media_url("https://x.test/ep.mp3"));
    assert!(!looks_like_id3_media_url("https://x.test/ep.m4a"));
}

#[test]
fn id3_v23_show_notes() {
    let mut frames = Vec::new();
    frames.extend(text_frame("TIT2", "Episode One", false));
    frames.extend(text_frame("TPE1", "The Host", false));
    frames.extend(uslt_frame("Show notes for episode one, with links.", false));
    let path = temp_mp3(&id3_tag(&frames, false));
    let (title, author, content) = id3_metadata(&path).expect("id3 metadata");
    assert_eq!(title, "Episode One");
    assert_eq!(author, "The Host");
    assert!(content.contains("Show notes for episode one"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn id3_v24_synchsafe_frames() {
    let mut frames = Vec::new();
    frames.extend(text_frame("TIT2", "Second Episode", true));
    frames.extend(text_frame("TPE2", "Band Host", true));
    frames.extend(uslt_frame("Version four notes.", true));
    let path = temp_mp3(&id3_tag(&frames, true));
    let (title, author, content) = id3_metadata(&path).expect("id3 v2.4 metadata");
    assert_eq!(title, "Second Episode");
    assert_eq!(author, "Band Host");
    assert!(content.contains("Version four notes."));
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------
// Audit 8 Task 14 investigation: ID3 extended headers and
// unsynchronisation. Ground truth established against mutagen (the
// parser Python uses) on these exact byte shapes: extended-header
// tags and unsynchronised tags both parse fully there.
// ---------------------------------------------------------------------

fn id3_tag_flagged(body: &[u8], version: u8, flags: u8) -> Vec<u8> {
    let mut tag = Vec::new();
    tag.extend_from_slice(b"ID3");
    tag.push(version);
    tag.push(0);
    tag.push(flags);
    tag.extend_from_slice(&synchsafe(body.len()));
    tag.extend_from_slice(body);
    tag
}

fn unsynchronise(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for b in data {
        out.push(*b);
        if *b == 0xFF {
            out.push(0);
        }
    }
    out
}

#[test]
fn id3_v23_extended_header_is_skipped() {
    // Extended header: 4-byte size (6, excluding itself), 2 flag
    // bytes, 4 padding-size bytes; frames follow it.
    let mut body = Vec::new();
    body.extend_from_slice(&6u32.to_be_bytes());
    body.extend_from_slice(&[0, 0]);
    body.extend_from_slice(&[0, 0, 0, 0]);
    body.extend(text_frame("TIT2", "Ext Episode", false));
    body.extend(uslt_frame("Notes behind an extended header.", false));
    let path = temp_mp3(&id3_tag_flagged(&body, 3, 0x40));
    let (title, _author, content) = id3_metadata(&path).expect("extended-header metadata");
    assert_eq!(title, "Ext Episode");
    assert!(
        content.contains("Notes behind an extended header."),
        "{content}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn id3_v24_extended_header_is_skipped() {
    // v2.4 extended header size is synchsafe and includes itself:
    // 4 size bytes + 1 flag-count byte + 1 flag byte = 6.
    let mut body = Vec::new();
    body.extend_from_slice(&synchsafe(6));
    body.push(1);
    body.push(0);
    body.extend(text_frame("TIT2", "Ext Four", true));
    body.extend(uslt_frame("Notes in a v24 extended header tag.", true));
    let path = temp_mp3(&id3_tag_flagged(&body, 4, 0x40));
    let (title, _author, content) = id3_metadata(&path).expect("v24 extended-header metadata");
    assert_eq!(title, "Ext Four");
    assert!(
        content.contains("Notes in a v24 extended header tag."),
        "{content}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn id3_v23_unsynchronised_tag_is_decoded() {
    // TIT2 in Latin-1: "AÿB" — the 0xFF is stored stuffed as
    // 0xFF 0x00 under the tag unsynchronisation flag, and the
    // frame size describes the de-unsynchronised payload.
    let mut tit2 = Vec::new();
    tit2.extend_from_slice(b"TIT2");
    tit2.extend_from_slice(&4u32.to_be_bytes());
    tit2.extend_from_slice(&[0, 0]);
    tit2.extend_from_slice(&[0x00, b'A', 0xFF, b'B']);
    let mut frames = tit2;
    frames.extend(uslt_frame("Unsync notes here.", false));
    let stored = unsynchronise(&frames);
    let path = temp_mp3(&id3_tag_flagged(&stored, 3, 0x80));
    let (title, _author, content) = id3_metadata(&path).expect("unsync metadata");
    assert_eq!(title, "A\u{FF}B");
    assert!(content.contains("Unsync notes here."), "{content}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn id3_v24_frame_unsync_and_datalen_flags() {
    // Per-frame v2.4 flags: second flag byte 0x02 = this frame's
    // payload is unsynchronised (size counts the stored bytes);
    // 0x01 = a 4-byte data-length indicator precedes the payload.
    let payload = vec![0x00, b'A', 0xFF, b'B'];
    let stored_payload = unsynchronise(&payload);
    let mut tit2 = Vec::new();
    tit2.extend_from_slice(b"TIT2");
    tit2.extend_from_slice(&synchsafe(stored_payload.len()));
    tit2.extend_from_slice(&[0, 0x02]);
    tit2.extend_from_slice(&stored_payload);
    let mut uslt_payload = Vec::new();
    uslt_payload.extend_from_slice(&[3u8]);
    uslt_payload.extend_from_slice(b"eng");
    uslt_payload.push(0);
    uslt_payload.extend_from_slice(b"Flagged frame notes.");
    let mut uslt = Vec::new();
    uslt.extend_from_slice(b"USLT");
    uslt.extend_from_slice(&synchsafe(uslt_payload.len() + 4));
    uslt.extend_from_slice(&[0, 0x01]);
    uslt.extend_from_slice(&(uslt_payload.len() as u32).to_be_bytes());
    uslt.extend_from_slice(&uslt_payload);
    let mut frames = tit2;
    frames.extend(uslt);
    let path = temp_mp3(&id3_tag_flagged(&frames, 4, 0));
    let (title, _author, content) = id3_metadata(&path).expect("flagged-frame metadata");
    assert_eq!(title, "A\u{FF}B");
    assert!(content.contains("Flagged frame notes."), "{content}");
    let _ = std::fs::remove_file(&path);
}

/// zlib stream of the USLT payload [enc=3]["eng"][0]"Compressed
/// show notes for the episode." — generated once with Python's
/// zlib (the same codec mutagen uses) and pinned as bytes.
const COMPRESSED_USLT_ZLIB: &[u8] = &[
    120, 156, 99, 78, 205, 75, 103, 112, 206, 207, 45, 40, 74, 45, 46, 78, 77, 81, 40, 206, 200,
    47, 87, 200, 203, 47, 73, 45, 86, 72, 203, 47, 82, 40, 201, 72, 85, 72, 45, 200, 44, 206, 79,
    73, 213, 3, 0, 78, 44, 15, 124,
];

#[test]
fn id3_v24_compressed_frame_is_inflated() {
    // v2.4 COMPRESS (0x08): a 4-byte data-length indicator, then
    // the zlib stream; the frame size counts the stored bytes.
    let mut frame_data = Vec::new();
    frame_data.extend_from_slice(&synchsafe(43));
    frame_data.extend_from_slice(COMPRESSED_USLT_ZLIB);
    let mut uslt = Vec::new();
    uslt.extend_from_slice(b"USLT");
    uslt.extend_from_slice(&synchsafe(frame_data.len()));
    uslt.extend_from_slice(&[0, 0x08]);
    uslt.extend_from_slice(&frame_data);
    let path = temp_mp3(&id3_tag_flagged(&uslt, 4, 0));
    let (_title, _author, content) = id3_metadata(&path).expect("compressed v24 metadata");
    assert!(
        content.contains("Compressed show notes for the episode."),
        "{content}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn id3_v23_compressed_frame_is_inflated() {
    // v2.3 COMPRESS (second flag byte 0x80): a 4-byte big-endian
    // decompressed-size prefix, then the zlib stream.
    let mut frame_data = Vec::new();
    frame_data.extend_from_slice(&43u32.to_be_bytes());
    frame_data.extend_from_slice(COMPRESSED_USLT_ZLIB);
    let mut uslt = Vec::new();
    uslt.extend_from_slice(b"USLT");
    uslt.extend_from_slice(&(frame_data.len() as u32).to_be_bytes());
    uslt.extend_from_slice(&[0, 0x80]);
    uslt.extend_from_slice(&frame_data);
    let path = temp_mp3(&id3_tag_flagged(&uslt, 3, 0));
    let (_title, _author, content) = id3_metadata(&path).expect("compressed v23 metadata");
    assert!(
        content.contains("Compressed show notes for the episode."),
        "{content}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn id3_no_notes_is_none() {
    let mut frames = Vec::new();
    frames.extend(text_frame("TIT2", "Silent Episode", false));
    let path = temp_mp3(&id3_tag(&frames, false));
    assert!(id3_metadata(&path).is_none());
    let _ = std::fs::remove_file(&path);
    assert!(id3_metadata("/nonexistent/file.mp3").is_none());
}

#[test]
fn youtube_video_id_shapes() {
    assert_eq!(
        video_id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=3").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        video_id_from_url("https://youtu.be/dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        video_id_from_url("https://www.youtube.com/shorts/dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        video_id_from_url("https://music.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        video_id_from_url("https://www.youtube.com/feed/subscriptions"),
        None
    );
    assert_eq!(video_id_from_url("https://vimeo.com/12345"), None);
    assert_eq!(video_id_from_url("https://www.youtube.com/watch"), None);
}

#[test]
fn youtube_chapters_from_description_grammar() {
    let info = json!({
        "id": "abc123XYZ09",
        "description": "Intro line\n0:00 Start here\n2:30 The middle part\n1:05:00 Final hour",
    });
    let chapters = chapters_from_info(&info, "https://www.youtube.com/watch?v=abc123XYZ09");
    assert_eq!(chapters.len(), 3);
    assert_eq!(chapters[0].title, "Start here");
    assert_eq!(chapters[1].start, 150.0);
    assert_eq!(chapters[2].start, 3900.0);
    // Fewer than three timestamped lines is not a chapter list.
    let info = json!({"id": "abc123XYZ09", "description": "0:00 Only\n1:00 Two"});
    assert!(chapters_from_info(&info, "https://www.youtube.com/watch?v=abc123XYZ09").is_empty());
    // Info-dict chapters win over the description grammar.
    let info = json!({
        "id": "abc123XYZ09",
        "chapters": [{"start_time": 12.0, "title": "Real chapter"}],
        "description": "0:00 A\n1:00 B\n2:00 C",
    });
    let chapters = chapters_from_info(&info, "https://www.youtube.com/watch?v=abc123XYZ09");
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].title, "Real chapter");
}

#[test]
fn youtube_comment_tree_shape() {
    let comments = json!([
        {"id": "c1", "author": "Ann", "text": "Top level thought", "parent": "root"},
        {"id": "c2", "author": "Bob", "text": "A reply to Ann", "parent": "c1"},
        {"id": "c3", "author": "Cid", "text": "Nested reply", "parent": "c2"},
        {"id": "c4", "author": "Dee", "text": "", "parent": "root"},
        {"id": "c5", "author": "Eli", "text": "Orphan with missing parent", "parent": "gone"},
    ]);
    let lines = format_comments(&comments);
    let joined = lines.join("\n");
    assert!(joined.contains("Comment by Ann:"), "{joined}");
    assert!(joined.contains("Reply level 1 by Bob:"), "{joined}");
    assert!(joined.contains("Reply level 2 by Cid:"), "{joined}");
    // The orphan whose parent is unknown becomes a root comment.
    assert!(joined.contains("Comment by Eli:"), "{joined}");
    // Empty-text comments are dropped.
    assert!(!joined.contains("Dee"), "{joined}");
    // Parent prints before its replies.
    assert!(
        joined.find("Top level thought").unwrap() < joined.find("A reply to Ann").unwrap(),
        "{joined}"
    );
}

#[test]
fn github_page_classification() {
    match github_page_parts("https://github.com/serrebidev/BlindRSS/discussions/42") {
        Some(GithubPage::Discussion(o, r, n)) => {
            assert_eq!(
                (o.as_str(), r.as_str(), n.as_str()),
                ("serrebidev", "BlindRSS", "42")
            );
        }
        other => panic!("expected discussion, got {other:?}"),
    }
    match github_page_parts("https://github.com/orgs/community/discussions/1234") {
        Some(GithubPage::OrgDiscussion(o, n)) => {
            assert_eq!((o.as_str(), n.as_str()), ("community", "1234"));
        }
        other => panic!("expected org discussion, got {other:?}"),
    }
    match github_page_parts(
        "https://github.com/serrebidev/BlindRSS/commit/0123456789abcdef0123456789abcdef01234567",
    ) {
        Some(GithubPage::Commit(o, r, s)) => {
            assert_eq!(o, "serrebidev");
            assert_eq!(r, "BlindRSS");
            assert!(s.starts_with("0123456"));
        }
        other => panic!("expected commit, got {other:?}"),
    }
    match github_page_parts("https://github.com/serrebidev/BlindRSS/releases/tag/v1.2.3") {
        Some(GithubPage::Release(o, r, t)) => {
            assert_eq!(
                (o.as_str(), r.as_str(), t.as_str()),
                ("serrebidev", "BlindRSS", "v1.2.3")
            );
        }
        other => panic!("expected release, got {other:?}"),
    }
    // Reserved owners and non-GitHub pages are not GitHub pages.
    assert!(github_page_parts("https://github.com/settings/profile").is_none());
    assert!(github_page_parts("https://example.com/serrebidev/BlindRSS/discussions/42").is_none());
    assert!(github_page_parts("https://github.com/serrebidev/BlindRSS").is_none());
}
