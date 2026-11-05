use blindrss_core::{Article, config::Config, download, media};
use serde_json::json;
use sha2::{Digest, Sha256};

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn article() -> Article {
    Article {
        id: "episode-1".into(),
        feed_id: "feed-1".into(),
        title: "Episode".into(),
        url: "https://www.youtube.com/watch?v=offline".into(),
        content: None,
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: Some("https://example.test/episode.mp3".into()),
        media_type: Some("audio/mpeg".into()),
    }
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("blindrss-download-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn python_download_index_plays_offline_before_ytdlp() {
    let dir = scratch();
    let path = dir.join("episode.mp3");
    std::fs::write(&path, b"audio").unwrap();
    let article = article();
    let key = format!("url:{}", digest(&article.url));
    let mut cfg = Config::default();
    cfg.raw_mut()["downloaded_media"] = json!({key: {"path": path}});
    assert_eq!(
        download::downloaded_media_path(&cfg, &article),
        Some(path.clone())
    );
    let resolved = media::resolve_article_for_play(&cfg, &article).unwrap();
    assert_eq!(
        url::Url::parse(&resolved.url)
            .unwrap()
            .to_file_path()
            .unwrap(),
        path
    );
    assert!(!resolved.via_ytdlp);
    assert!(!resolved.detected);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recording_download_uses_python_keys_and_preserves_other_entries() {
    let dir = scratch();
    let path = dir.join("episode.mp3");
    std::fs::write(&path, b"audio").unwrap();
    let mut cfg = Config::default();
    cfg.raw_mut()["active_provider"] = json!("miniflux");
    cfg.raw_mut()["downloaded_media"] = json!({"other": "keep"});
    let article = article();
    download::record_article_download(&mut cfg, &article, &path).unwrap();
    let index = &cfg.raw()["downloaded_media"];
    assert_eq!(index["other"], "keep");
    for (kind, value) in [
        ("article", "miniflux|episode-1"),
        ("article-id", "miniflux|feed-1|episode-1"),
        ("media", "https://example.test/episode.mp3"),
        ("url", "https://www.youtube.com/watch?v=offline"),
    ] {
        let key = format!("{kind}:{}", digest(value));
        assert_eq!(index[&key]["article_id"], "episode-1");
        assert_eq!(index[&key]["path"], json!(path));
    }
    std::fs::remove_file(&path).unwrap();
    assert!(download::downloaded_media_path(&cfg, &article).is_none());
    assert!(download::record_article_download(&mut cfg, &article, &path).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn local_file_url_enclosure_is_playable_without_network() {
    let dir = scratch();
    let path = dir.join("episode space.mp3");
    std::fs::write(&path, b"audio").unwrap();
    let uri = url::Url::from_file_path(&path).unwrap().to_string();
    let resolved = media::resolve_for_play(Some(&uri), None, "", None).unwrap();
    assert_eq!(resolved.url, uri);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_direct_download_preserves_previous_file_and_cleans_partial() {
    use std::io::{Read, Write};
    let dir = scratch();
    let previous = dir.join("Episode.mp3");
    std::fs::write(&previous, b"complete previous download").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 2048];
        let _ = stream.read(&mut request);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\npartial",
            )
            .unwrap();
    });
    assert!(
        download::download_direct(&format!("http://{addr}/episode.mp3"), &dir, "Episode").is_err()
    );
    server.join().unwrap();
    assert_eq!(
        std::fs::read(previous).unwrap(),
        b"complete previous download"
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------
// Audit 8 Task 22 (M19): download layout, MIME extension, legacy
// lookup and retention follow Python's mainframe rules.

#[test]
fn download_layout_matches_python() {
    // Feed folder under the configured root; a pasted-link
    // destination override replaces the feed folder.
    let root = std::path::PathBuf::from("/downloads");
    assert_eq!(
        download::download_dir_for(&root, Some("My Feed: Daily?"), None),
        root.join("My Feed_ Daily_")
    );
    assert_eq!(
        download::download_dir_for(&root, Some("My Feed"), Some("Custom Folder")),
        root.join("Custom Folder")
    );
    assert_eq!(
        download::download_dir_for(&root, None, None),
        root.join("Feed")
    );
    // Python _safe_name: runs of bad characters become ONE
    // underscore, trailing dots are stripped, empty -> untitled.
    assert_eq!(
        download::download_dir_for(&root, Some("a//b::c"), None),
        root.join("a_b_c")
    );
    assert_eq!(
        download::download_dir_for(&root, Some("..."), None),
        root.join("untitled")
    );
}

#[test]
fn download_extension_comes_from_url_then_response_mime() {
    // URL extension (<= 5 chars incl. dot) wins.
    assert_eq!(
        download::extension_for_download("https://x.test/ep.m4a", Some("audio/mpeg")),
        ".m4a"
    );
    // Extensionless/shortener URLs fall back to the response
    // media type — and the last resort is .mp3, never .bin.
    assert_eq!(
        download::extension_for_download("https://x.test/12345", Some("audio/mpeg")),
        ".mp3"
    );
    assert_eq!(
        download::extension_for_download("https://x.test/12345", Some("Audio/MP4; charset=binary")),
        ".m4a"
    );
    assert_eq!(
        download::extension_for_download("https://x.test/12345", Some("application/octet-stream")),
        ".mp3"
    );
    assert_eq!(
        download::extension_for_download("https://x.test/12345", None),
        ".mp3"
    );
    // An over-long "extension" is not an extension.
    assert_eq!(
        download::extension_for_download("https://x.test/ep.toolongext", Some("audio/ogg")),
        ".ogg"
    );
}

#[test]
fn direct_download_of_extensionless_url_uses_response_mime() {
    use std::io::{Read, Write};
    let dir = scratch();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 2048];
        let _ = stream.read(&mut request);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 4\r\nConnection: close\r\n\r\nDATA",
            )
            .unwrap();
    });
    let path = download::download_direct(&format!("http://{addr}/episode/42"), &dir, "My Episode")
        .expect("download succeeds");
    server.join().unwrap();
    assert_eq!(path.file_name().unwrap(), "My Episode.mp3");
    assert_eq!(std::fs::read(&path).unwrap(), b"DATA");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_download_lookup_finds_feed_folder_files() {
    // M19: a file Python (or an older Rust build) left in the
    // article's feed folder is found by title prefix, newest
    // first, partial sidecars excluded.
    let dir = scratch();
    let feed_dir = dir.join("My Feed");
    std::fs::create_dir_all(&feed_dir).unwrap();
    std::fs::write(feed_dir.join("Episode One.mp3"), b"old").unwrap();
    std::fs::write(feed_dir.join("Episode One-1.mp3"), b"newer").unwrap();
    std::fs::write(feed_dir.join("Episode One.mp3.part"), b"partial").unwrap();
    std::fs::write(feed_dir.join("Other.mp3"), b"other").unwrap();
    let found =
        download::find_downloaded_file(&feed_dir, "Episode One").expect("legacy file found");
    assert!(
        found.ends_with("Episode One-1.mp3") || found.ends_with("Episode One.mp3"),
        "got {}",
        found.display()
    );
    assert!(download::find_downloaded_file(&feed_dir, "Missing").is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn download_retention_is_scoped_and_recursive() {
    // M19: per-folder retention after a download touches only
    // that folder; the startup sweep covers the root AND its
    // feed subfolders, never anything outside the root.
    let dir = scratch();
    let feed_dir = dir.join("Feed A");
    std::fs::create_dir_all(&feed_dir).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86400);
    for p in [dir.join("root-old.mp3"), feed_dir.join("feed-old.mp3")] {
        std::fs::write(&p, b"x").unwrap();
        set_mtime(&p, old);
    }
    std::fs::write(feed_dir.join("feed-new.mp3"), b"x").unwrap();
    let seconds = Some(7 * 86400);
    // Scoped pass: only the feed folder is swept.
    download::apply_download_retention(&feed_dir, seconds);
    assert!(dir.join("root-old.mp3").exists());
    assert!(!feed_dir.join("feed-old.mp3").exists());
    assert!(feed_dir.join("feed-new.mp3").exists());
    // Sweep pass: root level follows.
    download::sweep_download_retention(&dir, seconds);
    assert!(!dir.join("root-old.mp3").exists());
    // Unlimited retention is a no-op.
    std::fs::write(feed_dir.join("feed-old2.mp3"), b"x").unwrap();
    set_mtime(&feed_dir.join("feed-old2.mp3"), old);
    download::sweep_download_retention(&dir, None);
    assert!(feed_dir.join("feed-old2.mp3").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

fn set_mtime(path: &std::path::Path, t: std::time::SystemTime) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(t))
        .unwrap();
}

// ---------------------------------------------------------------------
// Audit 8 Task 22 (M17): the playback-download cache is Python's
// ytplay_cache, keyed by media ID, sidecars excluded.

#[test]
fn play_cache_layout_and_accounting_match_python() {
    use blindrss_core::play_cache;
    let dir = scratch();
    let cache = dir.join("ytplay_cache");
    std::fs::create_dir_all(&cache).unwrap();
    // A copied Python cache: completed downloads plus partial
    // sidecars from an interrupted yt-dlp run.
    std::fs::write(cache.join("dQw4w9WgXcQ.m4a"), vec![0u8; 100]).unwrap();
    std::fs::write(cache.join("abc12345.mp4.part"), vec![0u8; 900]).unwrap();
    std::fs::write(cache.join("abc12345.ytdl"), vec![0u8; 50]).unwrap();

    // Default layout: <data dir>/ytplay_cache (Python
    // play_cache.default_cache_dir), configured dir wins.
    assert_eq!(play_cache::cache_dir(None, &dir), dir.join("ytplay_cache"));
    let custom = dir.join("elsewhere");
    assert_eq!(play_cache::cache_dir(Some(&custom), &dir), custom);

    // Accounting sees only completed files.
    assert_eq!(play_cache::file_count(&cache), 1);
    assert_eq!(play_cache::size_bytes(&cache), 100);
    assert_eq!(play_cache::human_size(100), "100 B");
    assert_eq!(play_cache::human_size(2048), "2.0 KB");
    assert_eq!(play_cache::human_size(3 * 1024 * 1024), "3.0 MB");

    // Offline reuse: every URL form for the same video finds the
    // Python-cached file without any download.
    for url in [
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtu.be/dQw4w9WgXcQ",
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=RDx",
        "https://www.youtube.com/shorts/dQw4w9WgXcQ",
    ] {
        let hit = play_cache::fetch_cached_audio(&cache, url).expect(url);
        assert_eq!(hit, cache.join("dQw4w9WgXcQ.m4a"), "url {url}");
    }
    // A video with only a partial sidecar is NOT a hit.
    assert!(
        play_cache::fetch_cached_audio(&cache, "https://www.youtube.com/watch?v=abc12345")
            .is_none()
    );

    // Pruning ignores sidecars: a 1 KB cap does not evict
    // anything (the completed file is 100 B); the partials do
    // not count toward the total.
    play_cache::prune_once(&cache, 1024);
    assert!(cache.join("dQw4w9WgXcQ.m4a").exists());

    // Clearing removes EVERYTHING (partials included) and
    // reports what Python reports.
    let (removed, freed) = play_cache::clear_cache(&cache);
    assert_eq!(removed, 3);
    assert_eq!(freed, 1050);
    assert_eq!(play_cache::file_count(&cache), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn play_cache_prune_evicts_oldest_completed_only() {
    use blindrss_core::play_cache;
    let dir = scratch();
    let older = dir.join("old111.m4a");
    let newer = dir.join("new222.m4a");
    std::fs::write(&older, vec![0u8; 600]).unwrap();
    std::fs::write(&newer, vec![0u8; 600]).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    set_mtime(&older, old);
    // 700-byte cap: total 1200 -> evict the oldest completed.
    play_cache::prune_once(&dir, 700);
    assert!(!older.exists());
    assert!(newer.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------
// Audit 8 Task 22 (M18): yt-dlp download recovery — MP4 mux failure
// retried as MKV, cookie access honored, no audio merge retry.

#[cfg(unix)]
fn fake_download_ytdlp(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join("yt-dlp");
    std::fs::write(&exe, body.replace("__DIR__", &dir.to_string_lossy())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

#[cfg(unix)]
#[test]
fn ytdlp_download_retries_mp4_mux_failure_as_mkv() {
    let dir = scratch();
    let out_dir = dir.join("out");
    let script = r#"#!/bin/sh
echo "$@" >> "__DIR__/log"
case "$*" in
  *"--merge-output-format mp4"*)
    echo "ERROR: Conversion failed!" >&2; exit 1;;
  *"--merge-output-format mkv"*)
    OUT=""
    PREV=""
    for a in "$@"; do if [ "$PREV" = "-o" ]; then OUT="$a"; fi; PREV="$a"; done
    OUT=$(echo "$OUT" | sed 's/%(ext)s/mkv/')
    echo data > "$OUT"
    echo "$OUT"
    exit 0;;
esac
echo "unexpected args" >&2; exit 1
"#;
    let exe = fake_download_ytdlp(&dir, script);
    let path = download::download_with_ytdlp_using(
        &exe,
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        &out_dir,
        "best",
        "Fake Title",
        None,
    )
    .expect("mkv retry succeeds");
    assert!(path.ends_with("Fake Title.mkv"), "got {}", path.display());
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert_eq!(log.lines().count(), 2, "mp4 attempt + mkv retry: {log}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn ytdlp_download_audio_preset_never_retries_merge() {
    let dir = scratch();
    let out_dir = dir.join("out");
    let script = r#"#!/bin/sh
echo "$@" >> "__DIR__/log"
echo "ERROR: Conversion failed!" >&2
exit 1
"#;
    let exe = fake_download_ytdlp(&dir, script);
    let result = download::download_with_ytdlp_using(
        &exe,
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        &out_dir,
        "audio_best",
        "Fake Title",
        None,
    );
    assert!(result.is_err());
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert!(
        !log.contains("--merge-output-format"),
        "audio presets carry no merge format: {log}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn ytdlp_download_honors_cookie_file() {
    // M18: a cookie-required download succeeds on the
    // cookie-file attempt of the primary phase.
    let dir = scratch();
    let out_dir = dir.join("out");
    let cookie_file = dir.join("cookies.txt");
    std::fs::write(&cookie_file, "# Netscape HTTP Cookie File\n").unwrap();
    let script = r#"#!/bin/sh
echo "$@" >> "__DIR__/log"
case "$*" in
  *"--cookies __COOKIE__"*)
    OUT=""
    PREV=""
    for a in "$@"; do if [ "$PREV" = "-o" ]; then OUT="$a"; fi; PREV="$a"; done
    OUT=$(echo "$OUT" | sed 's/%(ext)s/mp4/')
    echo data > "$OUT"
    echo "$OUT"
    exit 0;;
esac
echo "ERROR: Sign in to confirm you're not a bot" >&2
exit 1
"#
    .replace("__COOKIE__", &cookie_file.to_string_lossy());
    let exe = fake_download_ytdlp(&dir, &script);
    let path = download::download_with_ytdlp_using(
        &exe,
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        &out_dir,
        "best",
        "Fake Title",
        Some(&cookie_file.to_string_lossy()),
    )
    .expect("cookie attempt downloads");
    assert!(path.ends_with("Fake Title.mp4"), "got {}", path.display());
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert_eq!(log.lines().count(), 2, "base then cookie-file: {log}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn play_cache_fetch_downloads_by_media_id_via_run_tool() {
    // M17: a cache miss downloads through the shared recovery
    // phases (run_tool) into the media-ID template, and the
    // produced file is found from the printed filepath.
    use blindrss_core::play_cache;
    let dir = scratch();
    let cache = dir.join("ytplay_cache");
    let script = r#"#!/bin/sh
echo "$@" >> "__DIR__/log"
OUT=""
PREV=""
for a in "$@"; do if [ "$PREV" = "-o" ]; then OUT="$a"; fi; PREV="$a"; done
OUT=$(echo "$OUT" | sed 's/%(id)s/dQw4w9WgXcQ/; s/%(ext)s/m4a/')
echo data > "$OUT"
echo "$OUT"
exit 0
"#;
    let exe = fake_download_ytdlp(&dir, script);
    let url = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    let path = play_cache::fetch_youtube_audio_using(&exe, url, &cache, 0, None)
        .expect("download lands in cache");
    assert_eq!(path, cache.join("dQw4w9WgXcQ.m4a"));
    // Second fetch is a pure cache hit: no new yt-dlp run.
    let again =
        play_cache::fetch_youtube_audio_using(&exe, url, &cache, 0, None).expect("cache hit");
    assert_eq!(again, path);
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert_eq!(log.lines().count(), 1, "one download only: {log}");
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------
// Audit 8 Task 16 (R10): the main-window YouTube full-text path
// (download::youtube_full_text_*) runs on the ONE shared
// pipeline — same extraction, same comment retry/union, same
// document as the reader paths.
// ---------------------------------------------------------------------

#[cfg(unix)]
fn fake_info_ytdlp(dir: &std::path::Path) -> std::path::PathBuf {
    let script = r#"#!/bin/sh
DIR='__DIR__'
echo "$@" >> "$DIR/log"
N=0
[ -f "$DIR/n" ] && N=$(cat "$DIR/n")
N=$((N+1))
echo "$N" > "$DIR/n"
[ -f "$DIR/err$N" ] && cat "$DIR/err$N" >&2
[ -f "$DIR/exit$N" ] && exit $(cat "$DIR/exit$N")
cat "$DIR/payload$N.json"
exit 0
"#;
    fake_download_ytdlp(dir, script)
}

#[cfg(unix)]
fn write_info_run(
    dir: &std::path::Path,
    n: usize,
    payload: Option<&serde_json::Value>,
    stderr: Option<&str>,
) {
    if let Some(payload) = payload {
        std::fs::write(
            dir.join(format!("payload{n}.json")),
            serde_json::to_string(payload).unwrap(),
        )
        .unwrap();
    }
    if let Some(stderr) = stderr {
        std::fs::write(dir.join(format!("err{n}")), stderr).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn youtube_full_text_uses_shared_pipeline_with_comment_retry() {
    let dir = scratch();
    let exe = fake_info_ytdlp(&dir);
    let first = json!({
        "id": "abc123XYZ09", "title": "Vid", "channel": "Chan",
        "description": "The description",
        "comments": [{"id": "c1", "author": "Ann", "text": "First comment", "parent": "root"}],
    });
    let second = json!({
        "id": "abc123XYZ09", "title": "Vid", "channel": "Chan",
        "description": "The description",
        "comments": [{"id": "c2", "author": "Bob", "text": "Second comment", "parent": "root"}],
    });
    write_info_run(
        &dir,
        1,
        Some(&first),
        Some("WARNING: Incomplete data received\n"),
    );
    write_info_run(&dir, 2, Some(&second), None);
    let text =
        download::youtube_full_text_with(&exe, "https://www.youtube.com/watch?v=abc123XYZ09", "en")
            .expect("full text");
    // Same document shape as the reader pipeline: sections in
    // Python's order, unioned comments, no truncation notice
    // once the retry completed.
    assert!(text.contains("Description"), "{text}");
    assert!(
        text.contains("First comment") && text.contains("Second comment"),
        "{text}"
    );
    assert!(!text.contains("incomplete comment page"), "{text}");
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert_eq!(log.lines().count(), 2, "one retry: {log}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn youtube_full_text_rejects_non_video_and_failed_extraction() {
    let dir = scratch();
    let exe = fake_info_ytdlp(&dir);
    // A non-video URL never reaches yt-dlp (Python
    // extract_video_info returns {}), and the caller sees the
    // same failure the plain reader reports.
    let err = download::youtube_full_text_with(&exe, "https://example.com/story", "en")
        .expect_err("non-video rejected");
    assert!(
        err.contains("Could not retrieve YouTube video details"),
        "{err}"
    );
    assert!(
        !dir.join("log").exists(),
        "yt-dlp must not run for non-videos"
    );
    // A failed extraction is an error, not an empty document.
    write_info_run(&dir, 1, None, Some("ERROR: unavailable\n"));
    std::fs::write(dir.join("exit1"), "1").unwrap();
    let err =
        download::youtube_full_text_with(&exe, "https://www.youtube.com/watch?v=abc123XYZ09", "en")
            .expect_err("failed extraction");
    assert!(!err.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
