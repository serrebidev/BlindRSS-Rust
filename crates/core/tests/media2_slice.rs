//! Downloads / Takeout / archive tests.

use blindrss_core::download::sanitize_filename;
use std::io::Write;

// Audit 8 Task 16: the vtt_to_text / compose_youtube_text
// tests that used to live here pinned download.rs's second,
// divergent YouTube document builder (rolling-caption dedupe
// and a differently shaped document). That implementation
// was removed in favor of the one shared ytfull pipeline;
// its transcript/comment behavior is covered by the Task 16
// tests in batch35b_slice.rs and download_parity.rs.

#[test]
fn sanitize_filename_shapes() {
    assert_eq!(sanitize_filename("a/b:c*d"), "a_b_c_d");
    assert_eq!(sanitize_filename("  "), "download");
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
            b"Channel ID,Channel URL,Channel title\nUCabcdefghij123456,https://www.youtube.com/channel/UCabcdefghij123456,\"Some, Channel\"\nUCzzzzzzzzzz999999,https://www.youtube.com/channel/UCzzzzzzzzzz999999,Other\n",
        )
        .unwrap();
        zw.finish().unwrap();
    }
    let feeds = blindrss_core::takeout::parse_takeout_zip(&buf)
        .unwrap()
        .feeds;
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
