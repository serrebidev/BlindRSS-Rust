//! Batch 7: decoding chain, fallback parsers, site cleaners,
//! thread URL detection, enrichment storage.

use blindrss_core::captcha::provider_endpoints;
use blindrss_core::db::Db;
use blindrss_core::extract::{
    apply_site_cleaners, bloomberg_video_text, decode_bytes, markdown_to_text, parse_smry_stream,
    wayback_raw_url,
};

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn decode_chain() {
    // windows-1252 bytes with a header charset.
    let bytes = vec![0x93, b'H', b'i', 0x94];
    let text = decode_bytes(&bytes, "text/html; charset=windows-1252", "");
    assert!(text.contains("Hi"), "{text}");
    // Meta charset wins when the header has none.
    let mut html = b"<meta charset=\"iso-8859-1\">caf".to_vec();
    html.push(0xE9);
    let text = decode_bytes(&html, "text/html", "");
    assert!(text.contains("café"), "{text}");
    // The per-feed override beats everything.
    let text = decode_bytes(&html, "text/html; charset=utf-8", "iso-8859-1");
    assert!(text.contains("café"), "{text}");
    // Plain UTF-8 still works.
    assert_eq!(decode_bytes("héllo".as_bytes(), "", ""), "héllo");
}

#[test]
fn jina_markdown_to_text() {
    let md = "# Title\n\n**Why it matters:** read [the docs](https://x.test) now.\n\n* item one\n![img](https://x.test/i.png)";
    let text = markdown_to_text(md);
    assert!(text.contains("the docs"), "{text}");
    assert!(!text.contains("**"), "{text}");
    assert!(!text.contains("]("), "{text}");
    assert!(!text.contains("img"), "{text}");
}

#[test]
fn smry_stream_parse() {
    let body = "x".repeat(400);
    let sse = format!(
        "event: article\ndata: {{\"article\": {{\"title\": \"T\", \"textContent\": \"{body}\"}}}}\n"
    );
    let (title, text) = parse_smry_stream(&sse).unwrap();
    assert_eq!(title, "T");
    assert_eq!(text.len(), 400);
    // Too-short extractions are rejected (teaser pages).
    let sse = "data: {\"article\": {\"title\": \"T\", \"textContent\": \"short\"}}";
    assert!(parse_smry_stream(sse).is_none());
}

#[test]
fn wayback_raw_rewrite() {
    assert_eq!(
        wayback_raw_url("https://web.archive.org/web/20240101000000/https://example.com/a"),
        "https://web.archive.org/web/20240101000000id_/https://example.com/a"
    );
    assert_eq!(
        wayback_raw_url("https://web.archive.org/web/20240101000000id_/https://example.com/a"),
        "https://web.archive.org/web/20240101000000id_/https://example.com/a"
    );
}

#[test]
fn site_cleaners() {
    let bbc = apply_site_cleaners(
        "- Published\nHello ShareSave world",
        "https://www.bbc.com/news/1",
    );
    assert!(!bbc.contains("ShareSave"), "{bbc}");
    assert!(!bbc.starts_with("- Published"), "{bbc}");
    let wsj = apply_site_cleaners(
        "Body text.\n\nCopyright ©2026 Dow Jones & Company, Inc. All Rights Reserved. 87990cbe87990cbe",
        "https://www.wsj.com/articles/1",
    );
    assert!(!wsj.contains("Dow Jones"), "{wsj}");
    let tr = apply_site_cleaners(
        "Real body.\n\nJane Doe is an author.\n\nYou must confirm your public display name before commenting.\n\nPlease logout and then login again.",
        "https://www.techradar.com/news/1",
    );
    assert!(!tr.contains("confirm your public display name"), "{tr}");
    assert!(tr.contains("Real body."), "{tr}");
}

#[test]
fn bloomberg_video_jsonld() {
    let desc = "Markets rallied today as investors weighed the latest inflation data and central bank signals from three continents in a busy session.";
    let html = format!(
        r#"<script type="application/ld+json">{{"@type":"VideoObject","description":"{desc}"}}</script>"#
    );
    let text =
        bloomberg_video_text(&html, "https://www.bloomberg.com/news/videos/2026-01-01/x").unwrap();
    assert!(text.contains("Markets rallied"));
    assert!(bloomberg_video_text(&html, "https://www.bloomberg.com/news/articles/x").is_none());
}

#[test]
fn captcha_endpoints() {
    assert!(provider_endpoints("2captcha").is_some());
    assert!(provider_endpoints("solvecaptcha").is_some());
    assert!(provider_endpoints("bogus").is_none());
}

#[test]
fn enrichment_merges_into_the_row() {
    let path = scratch("blindrss_batch7_enrich.db");
    let db = Db::open_or_create(&path).unwrap();
    let feed = db.add_feed("https://example.com/f", "Feed", None).unwrap();
    let art = blindrss_core::db::Article {
        id: "e1".into(),
        feed_id: feed.id.clone(),
        title: "T".into(),
        url: "https://example.com/a".into(),
        content: None,
        description: None,
        date: None,
        author: None,
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    };
    db.insert_article_if_new(&art).unwrap();
    let meta = blindrss_core::enrich::EnrichMeta {
        author: "Jane Doe".into(),
        tags: vec!["AI".into(), "Chips".into()],
        section: String::new(),
    };
    db.apply_enrichment("e1", &meta).unwrap();
    assert_eq!(db.article_tags("e1").unwrap(), vec!["AI", "Chips"]);
    let stored = db.articles(None, 1).unwrap();
    assert_eq!(stored[0].author.as_deref(), Some("Jane Doe"));
    // A second enrich does not overwrite the author or duplicate tags.
    let meta2 = blindrss_core::enrich::EnrichMeta {
        author: "Someone Else".into(),
        tags: vec!["ai".into(), "Rust".into()],
        section: String::new(),
    };
    db.apply_enrichment("e1", &meta2).unwrap();
    let stored = db.articles(None, 1).unwrap();
    assert_eq!(stored[0].author.as_deref(), Some("Jane Doe"));
    assert_eq!(db.article_tags("e1").unwrap(), vec!["AI", "Chips", "Rust"]);
    // feed_settings round trip (encoding override storage).
    db.set_feed_settings(&feed.id, &serde_json::json!({"encoding": "shift_jis"}))
        .unwrap();
    let settings = db.feed_settings(&feed.id).unwrap();
    assert_eq!(settings["encoding"], "shift_jis");
    let _ = std::fs::remove_file(&path);
}
