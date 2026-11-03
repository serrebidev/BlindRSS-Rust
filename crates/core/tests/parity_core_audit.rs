use blindrss_core::{
    Db,
    feeds::parse_feed,
    refresh::{apply_rules_to_existing, store_parsed},
    rules::{RuleArticle, rule_matches},
    takeout::parse_takeout_zip,
};
use serde_json::json;
use std::io::Write;

fn database() -> (Db, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss-parity-{}.db", uuid::Uuid::new_v4()));
    (Db::open_or_create(&path).unwrap(), path)
}
fn parsed() -> blindrss_core::feeds::ParsedFeed {
    parse_feed(br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>News</title><id>feed</id><updated>2026-10-30T00:00:00Z</updated><entry><id>item</id><title>Title</title><updated>2026-10-30T00:00:00Z</updated><link rel="self" href="https://example.com/api/item"/><link rel="enclosure" href="https://example.com/podcast.mp3" type="audio/mpeg"/><link rel="alternate" href="https://example.com/article"/><category term="Technology"/><category term="technology"/></entry></feed>"#).unwrap()
}
#[test]
fn feed_links_and_tags_reach_filter_rules() {
    let (db, path) = database();
    let feed = db
        .add_feed("https://example.com/feed", "News", None)
        .unwrap();
    db.create_filter_rule(
        "tag",
        &json!({"match":"all","conditions":[{"field":"tag","value":"technology"}]}).to_string(),
        r#"{"mark_favorite":true}"#,
        true,
        false,
    )
    .unwrap();
    let parsed = parsed();
    assert_eq!(parsed.entries[0].url, "https://example.com/article");
    store_parsed(&db, &feed, &parsed, "deleted").unwrap();
    assert_eq!(db.article_tags("item").unwrap(), vec!["Technology"]);
    assert!(db.article_by_id("item").unwrap().unwrap().is_favorite);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn python_tags_survive_enrichment_and_existing_filters_use_real_facts() {
    let (db, path) = database();
    let feed = db
        .add_feed("https://example.com/feed", "News", None)
        .unwrap();
    store_parsed(&db, &feed, &parsed(), "deleted").unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE articles SET tags = 'Python\nRust' WHERE id = 'item'",
        [],
    )
    .unwrap();
    assert_eq!(db.article_tags("item").unwrap(), vec!["Python", "Rust"]);
    assert_eq!(
        db.all_article_tags().unwrap()["item"],
        vec!["Python", "Rust"]
    );
    db.apply_enrichment(
        "item",
        &blindrss_core::enrich::EnrichMeta {
            tags: vec!["rust".into(), "Feeds".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let raw: String = conn
        .query_row("SELECT tags FROM articles WHERE id = 'item'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(raw, "Python\nRust\nFeeds");
    db.mark_opened("item").unwrap();
    db.record_article_version("item", "Title", Some("one"))
        .unwrap();
    db.record_article_version("item", "Title", Some("two"))
        .unwrap();
    db.create_filter_rule("facts", &json!({"match":"all","conditions":[{"field":"tag","value":"feeds"},{"field":"opened","value":true},{"field":"updated","value":true}]}).to_string(), r#"{"mark_read":true}"#, true, false).unwrap();
    assert_eq!(apply_rules_to_existing(&db, "deleted").unwrap(), 1);
    assert!(db.article_by_id("item").unwrap().unwrap().is_read);
    drop(conn);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn unknown_rule_fields_do_not_match_any_group() {
    let (db, path) = database();
    let feed = db
        .add_feed("https://example.com/feed", "News", None)
        .unwrap();
    store_parsed(&db, &feed, &parsed(), "deleted").unwrap();
    let a = db.article_by_id("item").unwrap().unwrap();
    let facts = RuleArticle {
        article: &a,
        feed_title: "News",
        opened: false,
        updated: false,
        tags: &[],
    };
    assert!(!rule_matches(
        &json!({"match":"any","conditions":[{"field":"unknown"},{"field":"title","value":"missing"}]}),
        &facts
    ));
    drop(db);
    std::fs::remove_file(path).unwrap();
}
fn archive(name: &str, body: &str) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file(name, zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(body.as_bytes()).unwrap();
    writer.finish().unwrap().into_inner()
}
#[test]
fn takeout_owner_channels_and_bom_headers() {
    let bytes = archive(
        "Takeout/YouTube and YouTube Music/channels/channel.csv",
        "\u{feff}Channel ID,Channel title (Original)\nUCabcdefghijk,My channel\n",
    );
    let feeds = parse_takeout_zip(&bytes).unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].title, "My channel");
    assert_eq!(feeds[0].source, "owner");
}
#[test]
fn takeout_rejects_oversized_members_and_empty_exports() {
    let bytes = archive(
        "Takeout/YouTube and YouTube Music/subscriptions/subscriptions.csv",
        &"x".repeat(64 * 1024 * 1024 + 1),
    );
    assert!(parse_takeout_zip(&bytes).unwrap_err().contains("too large"));
    let bytes = archive("Takeout/YouTube and YouTube Music/empty.txt", "");
    assert!(parse_takeout_zip(&bytes).is_err());
}

#[test]
fn takeout_history_reads_unicode_titles_and_rejects_other_hosts() {
    let bytes = archive(
        "Takeout/YouTube and YouTube Music/history/watch-history.html",
        "ééééééééééééééééééééé<a href='https://www.youtube.com/channel/UCabcdefghijk'>Café &amp; News</a><a href='https://other.example/channel/UCnotyoutube1'>Wrong</a>",
    );
    let feeds = parse_takeout_zip(&bytes).unwrap();
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].title, "Café & News");
}
