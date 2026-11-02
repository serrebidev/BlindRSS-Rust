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
