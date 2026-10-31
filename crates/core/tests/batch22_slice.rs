//! Batch 22 (final sweep): media_url, article_lang,
//! translation_updates, cookies_import.

use blindrss_core::article_lang::{
    ContentLangSources, lang_from_page_html, normalize_lang, resolve_content_language,
};
use blindrss_core::config::Config;
use blindrss_core::cookies_import as ci;
use blindrss_core::media_url::{
    is_media_url, looks_like_direct_media_file, normalize_media_url, title_from_url,
};
use blindrss_core::translation_updates as tu;
use std::collections::HashMap;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("blindrss-b22-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn media_url_normalize_matches_python() {
    assert_eq!(
        normalize_media_url("  <www.youtube.com/watch?v=abc> "),
        "https://www.youtube.com/watch?v=abc"
    );
    assert_eq!(
        normalize_media_url("//example.com/ep.mp3"),
        "https://example.com/ep.mp3"
    );
    assert_eq!(
        normalize_media_url("https://example.com:8080/x"),
        "https://example.com:8080/x"
    );
    assert_eq!(normalize_media_url("how to bake bread"), "");
    assert_eq!(normalize_media_url("mailto:someone@example.com"), "");
    assert_eq!(normalize_media_url("magnet:?xt=urn:btih:abc"), "");
    assert_eq!(normalize_media_url("example"), "");
    // Scheme-less localhost is rejected at the scheme-guess stage
    // (no dot); with a scheme it is accepted (Python's exact split).
    assert_eq!(normalize_media_url("localhost:8080/x"), "");
    assert_eq!(
        normalize_media_url("https://localhost:8080/x"),
        "https://localhost:8080/x"
    );
    assert_eq!(normalize_media_url("C:\\videos\\ep.mp4"), "");
    assert!(is_media_url("example.com/file.mp4"));
    assert!(!is_media_url("just some words"));
}

#[test]
fn media_url_titles_and_direct_files() {
    assert!(looks_like_direct_media_file(
        "https://example.com/media/My%20Episode.mp3?dl=1"
    ));
    assert!(looks_like_direct_media_file(
        "https://example.com/live.m3u8"
    ));
    assert!(!looks_like_direct_media_file(
        "https://youtube.com/watch?v=abc"
    ));
    assert_eq!(
        title_from_url("https://example.com/files/the_big_episode+part_2.mp3"),
        "the big episode part 2"
    );
    assert_eq!(title_from_url("https://www.bbc.co.uk/"), "bbc.co.uk");
    assert_eq!(title_from_url("not a url"), "media");
}

#[test]
fn article_lang_normalize_and_priority() {
    assert_eq!(normalize_lang(Some("pt_BR")).as_deref(), Some("pt-BR"));
    assert_eq!(
        normalize_lang(Some("ru_RU.UTF-8")).as_deref(),
        Some("ru-RU")
    );
    assert_eq!(normalize_lang(Some("en-us")).as_deref(), Some("en-US"));
    assert_eq!(normalize_lang(Some("zh-Hans")).as_deref(), Some("zh-Hans"));
    assert_eq!(normalize_lang(Some("unknown")), None);
    assert_eq!(normalize_lang(Some("{{lang}}")), None);
    assert_eq!(normalize_lang(None), None);

    assert_eq!(
        lang_from_page_html("<html lang=\"fr-FR\"><body>x</body>").as_deref(),
        Some("fr-FR")
    );
    assert_eq!(
        lang_from_page_html("<html xml:lang='de'>").as_deref(),
        Some("de")
    );
    assert_eq!(lang_from_page_html("<html>"), None);

    let mut sources = ContentLangSources {
        feed_lang: Some("es".into()),
        ..Default::default()
    };
    assert_eq!(resolve_content_language(&sources, "en"), "es");
    sources.page_html = Some("<html lang=\"it\">".into());
    assert_eq!(resolve_content_language(&sources, "en"), "it");
    sources.translation_target = Some("nl".into());
    assert_eq!(resolve_content_language(&sources, "en"), "nl");
    let empty = ContentLangSources::default();
    assert_eq!(resolve_content_language(&empty, "fr"), "fr");
    assert_eq!(resolve_content_language(&empty, "junk value"), "en");
}

struct FakeFetch {
    responses: HashMap<String, tu::FetchOutcome>,
}

impl tu::CatalogFetch for FakeFetch {
    fn fetch(&self, url: &str, _etag: &str) -> tu::FetchOutcome {
        for lang in self.responses.keys() {
            if url.contains(&format!("/{lang}/")) {
                // Can't move out of a borrow; rebuild by kind.
                return match self.responses.get(lang).unwrap() {
                    tu::FetchOutcome::Updated { text, etag } => tu::FetchOutcome::Updated {
                        text: text.clone(),
                        etag: etag.clone(),
                    },
                    tu::FetchOutcome::Unchanged => tu::FetchOutcome::Unchanged,
                    tu::FetchOutcome::Missing => tu::FetchOutcome::Missing,
                    tu::FetchOutcome::Failed(e) => tu::FetchOutcome::Failed(e.clone()),
                };
            }
        }
        tu::FetchOutcome::Missing
    }
}

fn big_po(msgid: &str, msgstr: &str) -> String {
    let mut out = String::new();
    for i in 0..60 {
        out.push_str(&format!(
            "msgid \"{msgid} {i}\"\nmsgstr \"{msgstr} {i}\"\n\n"
        ));
    }
    out
}

#[test]
fn translation_updates_round_trip() {
    let dir = scratch("tu");
    let bundled = vec![dir.join("bundled")];
    let mut responses = HashMap::new();
    responses.insert(
        "fr".to_string(),
        tu::FetchOutcome::Updated {
            text: big_po("Hello", "Bonjour"),
            etag: "W/\"abc\"".to_string(),
        },
    );
    responses.insert("de".to_string(), tu::FetchOutcome::Unchanged);
    let fetch = FakeFetch { responses };
    let langs = vec!["fr".to_string(), "de".to_string(), "en".to_string()];
    let result = tu::check_and_update(&dir, &bundled, &langs, &fetch, 1_700_000_000);
    assert_eq!(result.updated, vec!["fr".to_string()]);
    assert_eq!(result.checked, 2);
    assert_eq!(tu::installed_languages(&dir), vec!["fr".to_string()]);
    assert_eq!(tu::last_check_time(&dir), 1_700_000_000);
    assert!(tu::is_due(&dir, "weekly", 1_700_000_000 + 8 * 86_400));
    assert!(!tu::is_due(&dir, "weekly", 1_700_000_000 + 86_400));

    // A too-small catalog is refused as damaged.
    let mut responses = HashMap::new();
    responses.insert(
        "fr".to_string(),
        tu::FetchOutcome::Updated {
            text: "msgid \"a\"\nmsgstr \"b\"\n".to_string(),
            etag: "W/\"x\"".to_string(),
        },
    );
    let fetch = FakeFetch { responses };
    let result = tu::check_and_update(&dir, &bundled, &langs[..1], &fetch, 1_700_000_100);
    assert!(result.updated.is_empty());

    // Version transition wipes overrides once.
    assert!(tu::prepare_overrides_for_app_version(
        &dir, &bundled, "0.1.22"
    ));
    assert!(tu::installed_languages(&dir).is_empty());
    assert!(!tu::prepare_overrides_for_app_version(
        &dir, &bundled, "0.1.22"
    ));

    // Stand-down when the override root IS a bundled root.
    let dir2 = scratch("tu2");
    let same = vec![tu::override_root(&dir2)];
    let result = tu::check_and_update(&dir2, &same, &langs, &fetch, 1);
    assert_eq!(result.checked, 0);
}

#[test]
fn youtube_recommendations_parse_and_store() {
    // A minimal TV browse payload with two video tiles, one
    // duplicate, and one junk tile.
    let payload = serde_json::json!({
        "contents": {"shelves": [
            {"tileRenderer": {
                "contentType": "TILE_CONTENT_TYPE_VIDEO",
                "contentId": "dQw4w9WgXcQ",
                "metadata": {"tileMetadataRenderer": {
                    "title": {"simpleText": "First Video"},
                    "lines": [{"lineRenderer": {"items": [
                        {"lineItemRenderer": {"text": {"simpleText": "Channel One"}}}
                    ]}}]
                }}
            }},
            {"tileRenderer": {
                "contentType": "TILE_CONTENT_TYPE_VIDEO",
                "contentId": "dQw4w9WgXcQ",
                "metadata": {"tileMetadataRenderer": {
                    "title": {"simpleText": "Duplicate"},
                    "lines": []
                }}
            }},
            {"tileRenderer": {
                "contentType": "TILE_CONTENT_TYPE_VIDEO",
                "contentId": "abc123XYZ_-",
                "metadata": {"tileMetadataRenderer": {
                    "title": {"runs": [{"text": "Second"}, {"text": " Video"}]},
                    "lines": [{"lineRenderer": {"items": [
                        {"lineItemRenderer": {"text": {"runs": [{"text": "Chan"}, {"text": "nel Two"}]}}}
                    ]}}]
                }}
            }},
            {"tileRenderer": {
                "contentType": "TILE_CONTENT_TYPE_PLAYLIST",
                "contentId": "PLxxxxxxxxx",
                "metadata": {"tileMetadataRenderer": {"title": {"simpleText": "Skip me"}, "lines": []}}
            }}
        ]}
    });
    let videos = blindrss_core::youtube_account::parse_recommendations(&payload);
    assert_eq!(videos.len(), 2);
    assert_eq!(videos[0].1, "First Video");
    assert_eq!(videos[0].2, "Channel One");
    assert_eq!(videos[1].1, "Second Video");
    assert_eq!(videos[1].2, "Channel Two");

    // Store + read back through the Db with state retained.
    let dir = scratch("ytrec");
    let db_path = dir.join("rss.db");
    std::fs::write(&db_path, b"").unwrap();
    let db = blindrss_core::db::Db::open(&db_path).unwrap();
    let account = blindrss_core::youtube_account::account_key("refresh-token-xyz");
    assert_eq!(account.len(), 64);
    db.save_recommendations(&account, &videos).unwrap();
    let articles = db.recommendations(&account).unwrap();
    assert_eq!(articles.len(), 2);
    assert_eq!(
        articles[0].feed_id,
        blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID
    );
    assert_eq!(
        articles[0].media_url.as_deref(),
        Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
    );
    // State changes land on the recommendations row.
    assert!(
        db.set_recommendation_state(&articles[0].id, Some(true), Some(true))
            .unwrap()
    );
    let again = db.recommendations(&account).unwrap();
    assert!(again[0].is_read && again[0].is_favorite);
    // A second snapshot keeps state and drops stale rows from view.
    db.save_recommendations(&account, &videos[1..]).unwrap();
    let current = db.recommendations(&account).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].title, "Second Video");
}

#[test]
fn cookies_import_youtube_flow() {
    let dir = scratch("ck");
    let downloads = dir.join("Downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let jar_text = "# Netscape HTTP Cookie File\n\
        .youtube.com\tTRUE\t/\tTRUE\t1893456000\tSID\tabc123\n\
        example.com\tFALSE\t/\tFALSE\t1893456000\tother\tx\n";
    let export = downloads.join("cookies.txt");
    std::fs::write(&export, jar_text).unwrap();
    assert!(ci::is_netscape_cookie_jar(jar_text));
    assert!(ci::cookie_jar_has_youtube(jar_text));
    assert!(!ci::is_blindrss_managed_jar(jar_text));
    assert!(ci::is_blindrss_managed_jar(
        "# Imported by BlindRSS\nexample.com\tFALSE\t/\tFALSE\t1\ta\tb\n"
    ));

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let mut config = Config::default();
    let data_dir = dir.join("data");
    let dest =
        ci::auto_import_youtube_cookies(&mut config, &data_dir, Some(vec![downloads.clone()]), now)
            .expect("fresh export imports");
    assert_eq!(dest, data_dir.join("youtube_cookies.txt"));
    // Second pass: the persisted mtime blocks re-import.
    assert!(
        ci::auto_import_youtube_cookies(&mut config, &data_dir, Some(vec![downloads]), now,)
            .is_none()
    );
    let (ok, _) = ci::validate_cookie_file(&dest);
    assert!(ok);
}

#[test]
fn cookies_import_downloads_merge() {
    let dir = scratch("ck2");
    let downloads = dir.join("Downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    std::fs::write(
        downloads.join("example.com_cookies.txt"),
        "example.com\tFALSE\t/\tFALSE\t1893456000\tsession\txyz\n",
    )
    .unwrap();
    let jar = dir.join("site_cookies.txt");
    let mut config = Config::default();
    let imported =
        ci::auto_import_downloads(&mut config, &jar, Some(vec![downloads.clone()]), 100.0);
    assert!(imported.is_some());
    let text = std::fs::read_to_string(&jar).unwrap();
    assert!(text.contains("session\txyz"));
    // Marker persisted: nothing new on the second pass.
    assert!(ci::auto_import_downloads(&mut config, &jar, Some(vec![downloads]), 200.0).is_none());
}
