//! Playback-resolution regression tests (Brandon's Double Tap
//! report, 2026-10-30): a podcast article whose feed entry has no
//! enclosure must not be fed straight to yt-dlp, enclosures must
//! keep their type, and page detection must find embedded audio.

use blindrss_core::feeds::parse_feed;
#[cfg(unix)]
use blindrss_core::media;
use blindrss_core::media::{
    detect_media_in_page, is_media_file_url, is_playable_media, is_ytdlp_supported,
    range_cache_context, resolve_for_play, should_resolve_direct_media_url,
};
use blindrss_core::ytdlp_recovery::{
    CookieAttempt, YOUTUBE_PLAYER_CLIENTS, YOUTUBE_PLAYER_CLIENTS_FALLBACK, cookie_attempts,
    is_chromium_cookie_source, is_dpapi_cookie_error, is_uncookieable_error,
    normalize_ytdlp_single_item_url, recovery_phases, vlc_audio_format, youtube_player_client_arg,
    youtube_single_item_recovery_urls,
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

/// Audit 8 Task 20 (G11): the row-annotation composition the
/// GUI renders — one playback-state scan plus grouped chapter
/// counts over a real database, resolved through the shared
/// lookup order and composed exactly as Python's row cells.
#[test]
fn row_annotations_compose_from_scanned_state() {
    use blindrss_core::Db;
    use blindrss_core::db::{Chapter, PlaybackState};
    use blindrss_core::media::article_has_playable_media;
    use blindrss_core::playback::{article_media_label, display_title, state_for_article};

    let path = std::env::temp_dir().join(format!(
        "blindrss_t20_ann_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);
    let db = Db::open_or_create(&path).unwrap();
    db.add_feed("https://example.com/feed", "Feed", Some("Uncategorized"))
        .unwrap();
    let feeds = db.feeds().unwrap();
    let feed_id = feeds[0].id.clone();
    let episode = blindrss_core::Article {
        id: "ep1".into(),
        feed_id,
        title: "Episode One".into(),
        url: "https://example.com/ep1".into(),
        content: Some("body".into()),
        description: Some("desc".into()),
        date: Some("2026-01-01".into()),
        author: Some("Author".into()),
        is_read: false,
        is_favorite: false,
        media_url: Some("https://cdn.example.com/ep1.mp3".into()),
        media_type: Some("audio/mpeg".into()),
    };
    db.insert_article_if_new(&episode).unwrap();
    db.upsert_playback_state(
        "article:ep1",
        &PlaybackState {
            position_ms: 65_000,
            duration_ms: Some(3_600_000),
            completed: false,
            seek_supported: None,
            title: None,
        },
    )
    .unwrap();
    db.replace_chapters(
        "ep1",
        &[Chapter {
            start: 0.0,
            title: Some("Intro".into()),
            href: None,
        }],
    )
    .unwrap();

    // The GUI's precompute: one scan, one grouped count query.
    let states: std::collections::HashMap<String, PlaybackState> =
        db.all_playback_states().unwrap().into_iter().collect();
    let counts = db.chapter_counts(&["ep1".to_string()]).unwrap();
    let state = state_for_article(
        &states,
        &episode.id,
        episode.media_url.as_deref(),
        &episode.url,
    );
    assert!(article_has_playable_media(&episode));
    assert_eq!(
        display_title(
            &episode.title,
            state,
            counts.get("ep1").copied().unwrap_or(0) > 0
        ),
        "Episode One, 1:05 of 1:00:00, Chapters available"
    );
    assert_eq!(
        article_media_label(true, state),
        "Contains audio, 1:00:00, played 1:05"
    );
    std::fs::remove_file(&path).ok();
}

// ---------------------------------------------------------------------
// Audit 8 Task 22 (M16): Python's shared yt-dlp recovery policy.

#[test]
fn ytdlp_recovery_policy_helpers_match_python() {
    // Client pools (core/discovery.py).
    assert_eq!(YOUTUBE_PLAYER_CLIENTS, ["default", "android_vr"]);
    assert_eq!(YOUTUBE_PLAYER_CLIENTS_FALLBACK.len(), 11);
    assert_eq!(
        youtube_player_client_arg(&YOUTUBE_PLAYER_CLIENTS, None),
        "youtube:player_client=default,android_vr"
    );
    assert_eq!(
        youtube_player_client_arg(&YOUTUBE_PLAYER_CLIENTS, Some("Cgxyz")),
        "youtube:player_client=default,android_vr;visitor_data=Cgxyz"
    );

    // Radio-mix links collapse to the single item (player_client
    // context stripped); ordinary watch links pass through.
    assert_eq!(
        normalize_ytdlp_single_item_url(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDdQw4w9WgXcQ&start_radio=1"
        ),
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    );
    assert_eq!(
        normalize_ytdlp_single_item_url("https://youtu.be/dQw4w9WgXcQ?list=RDx&start_radio=true"),
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    );
    assert_eq!(
        normalize_ytdlp_single_item_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLabc"),
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLabc"
    );

    // Recovery URLs for a canonical watch URL.
    assert_eq!(
        youtube_single_item_recovery_urls("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
        vec![
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ".to_string(),
        ]
    );
    assert!(youtube_single_item_recovery_urls("https://example.com/watch?v=abc").is_empty());

    // VLC-friendly audio selectors: YouTube page URLs take the
    // m4a-first selector; SoundCloud takes the progressive-MP3 one.
    assert_eq!(
        vlc_audio_format("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
        "bestaudio[ext=m4a]/bestaudio[ext=mp4]/bestaudio/best[height<=480]/worst"
    );
    assert_eq!(
        vlc_audio_format("https://soundcloud.com/artist/track"),
        "bestaudio[protocol^=http][ext=mp3]/bestaudio[ext=mp3]/bestaudio/best"
    );
    assert_eq!(
        vlc_audio_format("https://vimeo.com/12345"),
        "bestaudio[ext=m4a]/bestaudio[ext=mp4]/bestaudio/best[height<=480]/worst"
    );

    // Uncookieable errors: cookie/login wording or a YouTube page
    // URL means cookies may still help; "unsupported url" /
    // "please report this issue" mean they cannot.
    let watch = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    assert!(!is_uncookieable_error("ERROR: Sign in to confirm", watch));
    assert!(!is_uncookieable_error("ERROR: Unsupported URL", watch));
    assert!(is_uncookieable_error(
        "ERROR: Unsupported URL",
        "https://example.com/video/1"
    ));
    assert!(is_uncookieable_error(
        "something broke; please report this issue on GitHub",
        "https://example.com/video/1"
    ));

    // DPAPI detection and the Chromium source family.
    assert!(is_dpapi_cookie_error("Failed to decrypt with DPAPI"));
    assert!(!is_dpapi_cookie_error("some other failure"));
    assert!(is_chromium_cookie_source("chrome"));
    assert!(is_chromium_cookie_source("brave:C:\\Users\\x\\Brave-Beta"));
    assert!(!is_chromium_cookie_source("firefox"));
    assert!(!is_chromium_cookie_source(
        "firefox:C:\\librewolf\\Profiles"
    ));

    // Attempt order: anonymous base, cookie file, browser
    // sources; Bloomberg hosts get the cookie file FIRST.
    // (Python only attempts a cookie file that exists.)
    let cookie_path = std::env::temp_dir().join(format!(
        "blindrss-policy-cookies-{}.txt",
        std::process::id()
    ));
    std::fs::write(&cookie_path, "# Netscape HTTP Cookie File\n").unwrap();
    let cookie_arg = cookie_path.to_string_lossy().into_owned();
    let attempts = cookie_attempts(Some(&cookie_arg));
    assert!(matches!(attempts[0], CookieAttempt::Base));
    assert!(matches!(attempts[1], CookieAttempt::CookieFile(_)));
    // A configured-but-missing cookie file is not attempted.
    let missing = cookie_attempts(Some("/nonexistent/cookies.txt"));
    assert!(matches!(missing[0], CookieAttempt::Base));
    assert!(!matches!(missing[1], CookieAttempt::CookieFile(_)));
    let bloomberg = blindrss_core::ytdlp_recovery::cookie_attempts_for_url(
        Some(&cookie_arg),
        "https://www.bloomberg.com/news/videos/1",
    );
    let _ = std::fs::remove_file(&cookie_path);
    assert!(matches!(bloomberg[0], CookieAttempt::CookieFile(_)));
    assert!(matches!(bloomberg[1], CookieAttempt::Base));

    // Phase shape for a YouTube target: primary pool with the
    // full attempt list, fallback pool with base+cookiefile,
    // then one impersonated phase per recovery URL.
    let phases = recovery_phases("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    assert_eq!(phases.len(), 4);
    assert!(!phases[0].fallback_attempts && !phases[0].impersonate);
    assert!(phases[1].fallback_attempts && !phases[1].impersonate);
    assert!(phases[2].impersonate && phases[3].impersonate);
    assert!(phases[2].target_url.contains("music.youtube.com"));
    assert!(phases[3].target_url.contains("youtube-nocookie.com"));
    // Non-YouTube targets get the single primary phase.
    assert_eq!(recovery_phases("https://vimeo.com/12345").len(), 1);
}

#[cfg(unix)]
fn fake_ytdlp(
    dir: &std::path::Path,
    win_substr: &str,
    browser_err: &str,
    err: &str,
) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("win"), win_substr).unwrap();
    let script = format!(
        "#!/bin/sh\nDIR='{}'\necho \"$@\" >> \"$DIR/log\"\nWIN=$(cat \"$DIR/win\")\ncase \"$*\" in\n  *\"$WIN\"*) echo \"https://cdn.example.com/stream.mp4\"; exit 0;;\nesac\ncase \"$*\" in\n  *--cookies-from-browser*) echo '{}' >&2; exit 1;;\nesac\necho '{}' >&2\nexit 1\n",
        dir.display(),
        browser_err,
        err
    );
    let exe = dir.join("yt-dlp");
    std::fs::write(&exe, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

#[cfg(unix)]
fn fake_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "blindrss-fakeytdlp-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[cfg(unix)]
#[test]
fn stream_url_recovery_reaches_fallback_client_pool() {
    // M16: the anonymous primary-pool attempt fails; the wider
    // fallback client pool (phase 2) succeeds.
    let dir = fake_dir("clients");
    let exe = fake_ytdlp(
        &dir,
        "web_safari",
        "cookie read failed",
        "ERROR: Sign in to confirm",
    );
    let url = media::stream_url_with(&exe, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", None)
        .expect("fallback pool resolves");
    assert_eq!(url, "https://cdn.example.com/stream.mp4");
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert!(
        log.contains("player_client=default,android_vr"),
        "log: {log}"
    );
    assert!(log.contains("web_safari"), "log: {log}");
    // The VLC audio selector rides every attempt.
    assert!(log.contains("bestaudio[ext=m4a]"), "log: {log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn stream_url_recovery_uses_cookie_file_attempt() {
    // M18/M16: a cookie-required video resolves on the
    // cookie-file attempt, right after the anonymous base attempt.
    let dir = fake_dir("cookie");
    std::fs::create_dir_all(&dir).unwrap();
    let cookie_file = dir.join("cookies.txt");
    std::fs::write(&cookie_file, "# Netscape HTTP Cookie File\n").unwrap();
    let exe = fake_ytdlp(
        &dir,
        &cookie_file.to_string_lossy(),
        "cookie read failed",
        "ERROR: Sign in to confirm you're not a bot",
    );
    let url = media::stream_url_with(
        &exe,
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        Some(&cookie_file.to_string_lossy()),
    )
    .expect("cookie file attempt resolves");
    assert_eq!(url, "https://cdn.example.com/stream.mp4");
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "base then cookie-file: {log}");
    assert!(lines[1].contains("--cookies"), "log: {log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn stream_url_uncookieable_error_skips_browser_attempts() {
    // M16: an uncookieable failure must not burn time on browser
    // cookie attempts — only base (+cookie file) attempts remain,
    // in every phase.
    let dir = fake_dir("uncookieable");
    let exe = fake_ytdlp(
        &dir,
        "zzz-never-matches",
        "cookie read failed",
        "ERROR: Unsupported URL; please report this issue",
    );
    let result = media::stream_url_with(&exe, "https://example.com/video/123", None);
    assert!(result.is_err());
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert!(
        !log.contains("--cookies-from-browser"),
        "no browser attempts: {log}"
    );
    // One phase (non-YouTube), one base attempt.
    assert_eq!(log.lines().count(), 1, "log: {log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn stream_url_dpapi_failure_skips_chromium_but_tries_firefox() {
    // M16: once a Chromium cookie read fails with DPAPI, the
    // rest of the Chromium family is skipped for the session;
    // Firefox is a different store and is still tried (and here
    // it succeeds).
    let dir = fake_dir("dpapi");
    let exe = fake_ytdlp(
        &dir,
        "firefox",
        "Failed to decrypt with DPAPI",
        "ERROR: Sign in to confirm",
    );
    let url = media::stream_url_with(&exe, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", None)
        .expect("firefox attempt resolves");
    assert_eq!(url, "https://cdn.example.com/stream.mp4");
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    let browser_lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("--cookies-from-browser"))
        .collect();
    assert_eq!(browser_lines.len(), 2, "first chromium + firefox: {log}");
    assert!(browser_lines[1].contains("firefox"), "log: {log}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn stream_url_recovery_reaches_recovery_urls_with_impersonation() {
    // M16: after both client pools fail, the music.youtube.com
    // recovery URL is tried with --impersonate chrome.
    let dir = fake_dir("recovery");
    let exe = fake_ytdlp(
        &dir,
        "music.youtube.com",
        "cookie read failed",
        "ERROR: Sign in to confirm",
    );
    let url = media::stream_url_with(&exe, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", None)
        .expect("recovery URL resolves");
    assert_eq!(url, "https://cdn.example.com/stream.mp4");
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    let win_line = log
        .lines()
        .find(|l| l.contains("music.youtube.com"))
        .expect("recovery attempt logged");
    assert!(win_line.contains("--impersonate chrome"), "log: {log}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// Audit 8 Task 22 (M20): original/resolved URL cache context.

#[test]
fn range_cache_context_and_resolve_gate_match_python() {
    // Python resolves redirects unless the URL already points at
    // a seekable media file on a non-tracker host.
    assert!(!should_resolve_direct_media_url(
        "https://cdn.example.com/ep.mp3",
        ""
    ));
    assert!(should_resolve_direct_media_url(
        "https://feeds.example.com/redirect/ep",
        ""
    ));
    assert!(should_resolve_direct_media_url(
        "https://chtbl.com/track/ep.mp3",
        ""
    ));

    // Resolved + different + not via yt-dlp: the ORIGINAL is the
    // cache context, so an expired signed redirect refreshes
    // through it and reuses the cached segments.
    assert_eq!(
        range_cache_context(
            "https://pod.example.com/show/episode",
            "https://signed.example.com/ep.mp3?token=abc",
            false,
            true,
        ),
        (
            true,
            Some("https://pod.example.com/show/episode".to_string())
        )
    );
    // yt-dlp resolutions never carry the context; unresolved or
    // unchanged URLs have nothing to carry.
    assert_eq!(
        range_cache_context(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://googlevideo.example.com/v",
            true,
            true,
        ),
        (false, None)
    );
    assert_eq!(
        range_cache_context(
            "https://cdn.example.com/ep.mp3",
            "https://cdn.example.com/ep.mp3",
            false,
            true,
        ),
        (false, None)
    );
    assert_eq!(
        range_cache_context(
            "https://pod.example.com/show/episode",
            "https://signed.example.com/ep.mp3?token=abc",
            false,
            false,
        ),
        (false, None)
    );
}
