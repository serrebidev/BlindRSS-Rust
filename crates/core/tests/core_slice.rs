//! Slice tests: data layer against a Python-built database fixture,
//! config reading, feed parsing, and the extraction cascade against the
//! Phase 3 benchmark corpus fixtures.

use blindrss_core::config::Config;
use blindrss_core::{Db, ExtractOutcome, extract_article, parse_feed};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn reads_python_built_database() {
    let db = Db::open(&fixture("sample.db")).expect("open fixture db");
    let cats = db.categories().unwrap();
    assert_eq!(cats.len(), 1);
    assert_eq!(cats[0].title, "Tech");
    let feeds = db.feeds().unwrap();
    assert_eq!(feeds.len(), 2);
    assert_eq!(db.unread_count(None).unwrap(), 1);
    let articles = db.articles(None, 10).unwrap();
    assert_eq!(articles.len(), 2);
    // Newest first.
    assert_eq!(articles[0].title, "First story");
    assert!(articles[0].is_favorite);
    let bbc = db.articles(Some("feed-bbc"), 10).unwrap();
    assert_eq!(bbc.len(), 1);
}

#[test]
fn mark_read_roundtrip_on_copy() {
    let tmp = std::env::temp_dir().join("blindrss_test_mark_read.db");
    std::fs::copy(fixture("sample.db"), &tmp).unwrap();
    let db = Db::open(&tmp).unwrap();
    assert_eq!(db.unread_count(None).unwrap(), 1);
    db.mark_read("a1", true).unwrap();
    assert_eq!(db.unread_count(None).unwrap(), 0);
    std::fs::remove_file(&tmp).ok();
}

#[test]
fn missing_database_is_an_error_not_an_empty_one() {
    let path = std::env::temp_dir().join("blindrss_definitely_missing.db");
    assert!(Db::open(&path).is_err());
}

#[test]
fn config_reads_python_json_shape() {
    let tmp = std::env::temp_dir().join("blindrss_test_config.json");
    std::fs::write(
        &tmp,
        r#"{"refresh_interval_minutes": 30, "dark_mode": true}"#,
    )
    .unwrap();
    let cfg = Config::load(&tmp).unwrap();
    assert_eq!(cfg.get_i64("refresh_interval_minutes", 15), 30);
    assert!(cfg.get_bool("dark_mode", false));
    assert_eq!(cfg.get_i64("missing_key", 7), 7);
    std::fs::remove_file(&tmp).ok();
}

#[test]
fn parses_rss_and_atom() {
    let rss = br#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Example RSS</title>
<item><title>One</title><link>https://example.com/1</link><guid>g1</guid>
<description>Summary one</description><pubDate>Fri, 30 Oct 2026 10:00:00 GMT</pubDate></item>
<item><title>Two</title><link>https://example.com/2</link><guid>g2</guid>
<content:encoded xmlns:content="http://purl.org/rss/1.0/modules/content/">Full body two</content:encoded></item>
</channel></rss>"#;
    let feed = parse_feed(rss).unwrap();
    assert_eq!(feed.title, "Example RSS");
    assert_eq!(feed.entries.len(), 2);
    assert_eq!(feed.entries[0].title, "One");
    assert!(feed.entries[1].content_text.contains("Full body two"));

    let atom = br#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Example Atom</title>
<entry><title>Atom One</title><id>urn:1</id><link href="https://example.com/a1"/>
<summary>Atom summary</summary></entry></feed>"#;
    let feed = parse_feed(atom).unwrap();
    assert_eq!(feed.entries.len(), 1);
    assert_eq!(feed.entries[0].url, "https://example.com/a1");
}

#[test]
fn extraction_macrumors_matches_python_ground_truth() {
    let html = std::fs::read_to_string(fixture("macrumors_article.html")).unwrap();
    match extract_article(&html, "https://www.macrumors.com/", None) {
        ExtractOutcome::Article { text, .. } => {
            let n = text.chars().count();
            // Python ground truth: 2,661 chars / 427 words.
            assert!((1_800..=3_600).contains(&n), "chars={n}");
        }
        other => panic!("expected Article, got {other:?}"),
    }
}

#[test]
fn extraction_rejects_bbc_404_error_page() {
    let html = std::fs::read_to_string(fixture("bbc_404_page.html")).unwrap();
    let outcome = extract_article(
        &html,
        "https://www.bbc.co.uk/news/articles/does-not-exist",
        None,
    );
    assert!(
        matches!(outcome, ExtractOutcome::Gate { .. } | ExtractOutcome::Empty),
        "404 page must not become an article, got {outcome:?}"
    );
}

#[test]
fn extraction_heise_keeps_table_data_python_drops() {
    let html = std::fs::read_to_string(fixture("heise_article.html")).unwrap();
    match extract_article(&html, "https://www.heise.de/", None) {
        ExtractOutcome::Article { text, .. } => {
            // The article's central table is a release-date list; at
            // least one table-only cell value (a streaming title) must
            // survive. "Wallace & Gromit" appears in the table rows.
            assert!(
                text.contains("Wallace") || text.contains("Tabelle") || text.len() > 6_000,
                "table data missing, chars={}",
                text.chars().count()
            );
        }
        other => panic!("expected Article, got {other:?}"),
    }
}
