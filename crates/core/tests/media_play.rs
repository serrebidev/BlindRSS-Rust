//! Playback-resolution regression tests (Brandon's Double Tap
//! report, 2026-10-30): a podcast article whose feed entry has no
//! enclosure must not be fed straight to yt-dlp, enclosures must
//! keep their type, and page detection must find embedded audio.

use blindrss_core::feeds::parse_feed;
use blindrss_core::media::{
    detect_media_in_page, is_media_file_url, is_playable_media, is_ytdlp_supported,
    resolve_for_play,
};

const RSS_TWO_ENCLOSURES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
<title>Pod</title><link>https://example.com</link>
<item>
  <title>Episode one</title>
  <link>https://example.com/ep1</link>
  <guid>ep1</guid>
  <enclosure url="https://example.com/cover.jpg" length="10" type="image/jpeg" />
  <enclosure url="https://example.com/ep1.mp3" length="20" type="audio/mpeg" />
</item>
<item>
  <title>Episode two</title>
  <link>https://example.com/ep2</link>
  <guid>ep2</guid>
  <enclosure url="https://example.com/ep2.m4a" length="20" type="audio/mp4" />
</item>
</channel></rss>"#;

#[test]
fn enclosure_type_carried_and_audio_preferred() {
    let feed = parse_feed(RSS_TWO_ENCLOSURES.as_bytes()).unwrap();
    assert_eq!(feed.entries.len(), 2);
    let one = &feed.entries[0];
    assert_eq!(
        one.media_url.as_deref(),
        Some("https://example.com/ep1.mp3")
    );
    assert_eq!(one.media_type.as_deref(), Some("audio/mpeg"));
    let two = &feed.entries[1];
    assert_eq!(
        two.media_url.as_deref(),
        Some("https://example.com/ep2.m4a")
    );
    assert_eq!(two.media_type.as_deref(), Some("audio/mp4"));
}

#[test]
fn ytdlp_support_gate_matches_python_allowlist() {
    assert!(is_ytdlp_supported("https://www.youtube.com/watch?v=x"));
    assert!(is_ytdlp_supported("https://youtu.be/x"));
    assert!(is_ytdlp_supported("https://soundcloud.com/artist/track"));
    assert!(is_ytdlp_supported("https://m.twitch.tv/videos/1"));
    assert!(!is_ytdlp_supported("https://doubletaponair.com/episode/"));
    assert!(!is_ytdlp_supported("https://example.com/file.mp3"));
    assert!(!is_ytdlp_supported("file:///tmp/x.mp3"));
    assert!(!is_ytdlp_supported(""));
}

#[test]
fn playable_media_test_matches_python() {
    assert!(is_playable_media("https://x.test/a.mp3", None));
    assert!(is_playable_media(
        "https://x.test/watch",
        Some("audio/mpeg")
    ));
    assert!(is_playable_media("https://x.test/watch", Some("video/mp4")));
    assert!(!is_playable_media(
        "https://x.test/cover.jpg",
        Some("image/jpeg")
    ));
    assert!(!is_playable_media(
        "https://x.test/episode/",
        Some("text/html")
    ));
    assert!(!is_playable_media("https://x.test/episode/", None));
    assert!(is_media_file_url("https://x.test/a.MP3?dl=1"));
}

#[test]
fn detect_media_finds_embedded_audio() {
    let base = "https://doubletaponair.com/episode-one/";
    let html = r#"<html><head>
      <meta property="og:audio" content="https://cdn.test/ep1.mp3" />
      </head><body><a href="/about">About</a></body></html>"#;
    let (url, ty) = detect_media_in_page(html, base).unwrap();
    assert_eq!(url, "https://cdn.test/ep1.mp3");
    assert_eq!(ty.as_deref(), Some("audio/mpeg"));

    let html2 = r#"<html><body>
      <video src="https://cdn.test/clip.mp4"></video>
      <audio controls src="ep2.mp3"></audio>
      </body></html>"#;
    let (url2, ty2) = detect_media_in_page(html2, base).unwrap();
    assert_eq!(url2, "https://doubletaponair.com/episode-one/ep2.mp3");
    assert_eq!(ty2.as_deref(), Some("audio/mpeg"));

    let html3 = "<html><body><img src=\"cover.jpg\"><p>No media</p></body></html>";
    assert!(detect_media_in_page(html3, base).is_none());
}

#[test]
fn resolve_prefers_enclosure_without_network() {
    let r = resolve_for_play(
        Some("https://example.com/ep.mp3"),
        Some("audio/mpeg"),
        "https://example.com/ep",
        None,
    )
    .unwrap();
    assert_eq!(r.url, "https://example.com/ep.mp3");
    assert!(!r.detected && !r.via_ytdlp);
}

#[test]
fn resolve_detects_from_stored_content() {
    let html = "<audio src=\"https://cdn.test/show.mp3\"></audio>";
    let r = resolve_for_play(None, None, "https://blog.test/post", Some(html)).unwrap();
    assert_eq!(r.url, "https://cdn.test/show.mp3");
    assert!(r.detected);
}

#[test]
fn resolve_no_media_anywhere_is_a_clean_error() {
    let r = resolve_for_play(None, None, "", None);
    assert_eq!(r.unwrap_err(), "Article has no playable media");
}
