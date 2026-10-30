//! Downloads / Takeout / archive tests.

use blindrss_core::download::{compose_youtube_text, sanitize_filename, vtt_to_text};
use std::io::Write;

#[test]
fn vtt_rolling_dedupe() {
    let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:02.000\nHello\n\n00:00:02.000 --> 00:00:04.000\nHello world\n\n00:00:04.000 --> 00:00:06.000\nHello world again\n\n00:00:06.000 --> 00:00:08.000\nNew line\n";
    let text = vtt_to_text(vtt);
    assert_eq!(text, "Hello world again New line");
}

#[test]
fn sanitize_and_compose() {
    assert_eq!(sanitize_filename("a/b:c*d"), "a_b_c_d");
    assert_eq!(sanitize_filename("  "), "download");
    let chapters = vec![blindrss_core::Chapter {
        start: 65.0,
        title: Some("Middle".into()),
        href: None,
    }];
    let text = compose_youtube_text("Desc here", &chapters, "Transcript body");
    assert!(text.contains("Desc here"));
    assert!(text.contains("1:05 Middle"));
    assert!(text.contains("Transcript:\nTranscript body"));
}

#[test]
fn takeout_subscriptions_csv() {
    // Build a Takeout-shaped ZIP in memory.
    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file(
            "Takeout/YouTube and YouTube Music/subscriptions/subscriptions.csv",
            opts,
        )
        .unwrap();
        zw.write_all(
            b"Channel ID,Channel URL,Channel Title\nUCabcdefghij123456,https://www.youtube.com/channel/UCabcdefghij123456,\"Some, Channel\"\nUCzzzzzzzzzz999999,https://www.youtube.com/channel/UCzzzzzzzzzz999999,Other\n",
        )
        .unwrap();
        zw.finish().unwrap();
    }
    let feeds = blindrss_core::takeout::parse_takeout_zip(&buf).unwrap();
    assert_eq!(feeds.len(), 2);
    assert_eq!(
        feeds[0].url,
        "https://www.youtube.com/feeds/videos.xml?channel_id=UCabcdefghij123456"
    );
    assert_eq!(feeds[0].title, "Some, Channel");
    assert!(blindrss_core::takeout::parse_takeout_zip(b"not a zip").is_err());
}

#[test]
fn cdx_parse() {
    let json = r#"[["urlkey","timestamp","original"],["com,example)/feed","20240101000000","http://example.com/feed"],["com,example)/feed","20250101000000","http://example.com/feed"]]"#;
    let stamps = blindrss_core::archive::parse_cdx_timestamps(json);
    assert_eq!(stamps, vec!["20240101000000", "20250101000000"]);
}
