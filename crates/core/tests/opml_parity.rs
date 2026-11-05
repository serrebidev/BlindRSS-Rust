//! Audit 8 Task 27 (F13): OPML import carries Python's full
//! compatibility surface (providers/local.py::import_opml and
//! core/utils.py::parse_opml): encoding fallback, tolerant
//! parsing, case-insensitive attributes, title fallbacks,
//! children of feed outlines, offline URL resolution,
//! custom-title preservation, category rows and paths.

use blindrss_core::Db;
use blindrss_core::opml::{import_opml, import_opml_under, parse_opml};

fn temp_db() -> (Db, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("blindrss-opml-{}.db", uuid::Uuid::new_v4()));
    (Db::open_or_create(&path).unwrap(), path)
}

#[test]
fn nested_folders_build_category_paths_and_rows() {
    let (db, path) = temp_db();
    let xml = br#"<?xml version="1.0"?>
<opml version="2.0"><head><title>Subs</title></head><body>
<outline text="News">
  <outline text="Tech">
    <outline text="Rust Blog" xmlUrl="https://blog.rust-lang.org/feed.xml"/>
  </outline>
  <outline text="Daily" xmlUrl="https://daily.example/rss"/>
</outline>
<outline text="Loose" xmlUrl="https://loose.example/rss"/>
</body></opml>"#;
    let added = import_opml(&db, xml).unwrap();
    assert_eq!(added, 3);
    let feeds = db.feeds().unwrap();
    let rust = feeds.iter().find(|f| f.title == "Rust Blog").unwrap();
    assert_eq!(rust.category.as_deref(), Some("News / Tech"));
    assert!(rust.title_is_custom);
    let daily = feeds.iter().find(|f| f.title == "Daily").unwrap();
    assert_eq!(daily.category.as_deref(), Some("News"));
    // Top-level feeds land in the Uncategorized sentinel,
    // stored literally as Python stores it.
    let loose = feeds.iter().find(|f| f.title == "Loose").unwrap();
    assert_eq!(loose.category.as_deref(), Some("Uncategorized"));
    // Category rows exist for the real paths (not the sentinel).
    let cats: Vec<String> = db
        .categories()
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    assert!(cats.contains(&"News".to_string()), "{cats:?}");
    assert!(cats.contains(&"News / Tech".to_string()), "{cats:?}");
    assert!(!cats.contains(&"Uncategorized".to_string()), "{cats:?}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn feed_outline_children_stay_in_the_same_category() {
    let (db, path) = temp_db();
    // Python: an outline WITH xmlUrl processes its children
    // under the SAME category — the parent feed is not a folder.
    let xml = br#"<opml><body>
<outline text="Parent Feed" xmlUrl="https://parent.example/rss">
  <outline text="Child Feed" xmlUrl="https://child.example/rss"/>
</outline>
</body></opml>"#;
    import_opml(&db, xml).unwrap();
    let feeds = db.feeds().unwrap();
    assert_eq!(feeds.len(), 2);
    for feed in &feeds {
        assert_eq!(feed.category.as_deref(), Some("Uncategorized"));
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn attributes_are_case_insensitive_and_title_falls_back() {
    let (db, path) = temp_db();
    let xml = br#"<opml><body>
<outline TEXT="Shouty" XMLURL="https://shouty.example/rss"/>
<outline title="Title Attr Only" xmlUrl="https://title.example/rss"/>
<outline xmlUrl="https://noname.example/rss"/>
</body></opml>"#;
    import_opml(&db, xml).unwrap();
    let feeds = db.feeds().unwrap();
    let shouty = feeds.iter().find(|f| f.url.contains("shouty")).unwrap();
    assert_eq!(shouty.title, "Shouty");
    assert!(shouty.title_is_custom);
    let titled = feeds.iter().find(|f| f.url.contains("title.")).unwrap();
    assert_eq!(titled.title, "Title Attr Only");
    assert!(titled.title_is_custom);
    // No text/title at all: Python stores "Unknown Feed" and
    // leaves the title non-custom so refresh may replace it.
    let noname = feeds.iter().find(|f| f.url.contains("noname")).unwrap();
    assert_eq!(noname.title, "Unknown Feed");
    assert!(!noname.title_is_custom);
    let _ = std::fs::remove_file(path);
}

#[test]
fn malformed_documents_recover_and_missing_body_fails() {
    let (db, path) = temp_db();
    // Unclosed outlines and a missing </body> tail: the
    // tolerant parse still recovers both feeds (BeautifulSoup's
    // parsers recover the same way in Python).
    let broken = br#"<opml><body>
<outline text="Folder">
<outline text="One" xmlUrl="https://one.example/rss">
<outline text="Two" xmlUrl="https://two.example/rss">
"#;
    let added = import_opml(&db, broken).unwrap();
    assert_eq!(added, 2);
    let feeds = db.feeds().unwrap();
    assert!(
        feeds
            .iter()
            .all(|f| f.category.as_deref() == Some("Folder"))
    );
    // No body at all is a hard failure, like Python.
    assert!(import_opml(&db, b"<opml><head><title>x</title></head></opml>").is_err());
    assert!(import_opml(&db, b"not xml at all").is_err());
    assert!(import_opml(&db, b"").is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn encoding_fallback_decodes_bom_and_cp1252_files() {
    let (db, path) = temp_db();
    // UTF-8 with BOM.
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(
        "<opml><body><outline text=\"Bom Feed\" xmlUrl=\"https://bom.example/rss\"/></body></opml>"
            .as_bytes(),
    );
    import_opml(&db, &bom).unwrap();
    // A cp1252 file: 0xE9 is é in both latin-1 and cp1252;
    // 0x93/0x94 are cp1252 smart quotes. Python's decode chain
    // (utf-8, utf-8-sig, latin-1, cp1252) stops at latin-1 —
    // it never fails — so the smart quotes land as the latin-1
    // control characters U+0093/U+0094 exactly as Python
    // stores them. The port mirrors the chain, quirks included.
    let cp = b"<opml><body><outline text=\"Caf\xe9 \x93News\x94\" xmlUrl=\"https://cafe.example/rss\"/></body></opml>";
    import_opml(&db, cp).unwrap();
    let feeds = db.feeds().unwrap();
    let cafe = feeds.iter().find(|f| f.url.contains("cafe")).unwrap();
    assert_eq!(cafe.title, "Café \u{93}News\u{94}");
    assert!(feeds.iter().any(|f| f.title == "Bom Feed"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn offline_resolution_and_duplicate_detection_use_resolved_urls() {
    let (db, path) = temp_db();
    let xml = br#"<opml><body>
<outline text="Group" xmlUrl="my-group@googlegroups.com"/>
<outline text="Rumble Chan" xmlUrl="https://rumble.com/c/MyChan"/>
<outline text="YT Search" xmlUrl="https://www.youtube.com/results?search_query=rust"/>
</body></opml>"#;
    import_opml(&db, xml).unwrap();
    let feeds = db.feeds().unwrap();
    // Google Groups forum links become the synthetic
    // subscription URL; Rumble channel pages normalize to the
    // /videos listing; YouTube searches stay verbatim.
    assert!(
        feeds
            .iter()
            .any(|f| f.url == "https://groups.google.com/g/my-group"),
        "{feeds:?}"
    );
    assert!(
        feeds
            .iter()
            .any(|f| f.url == "https://rumble.com/c/MyChan/videos"),
        "{feeds:?}"
    );
    assert!(
        feeds
            .iter()
            .any(|f| f.url == "https://www.youtube.com/results?search_query=rust"),
        "{feeds:?}"
    );
    // Re-importing the same document adds nothing: the
    // duplicate check covers original AND resolved URLs.
    let again = import_opml(&db, xml).unwrap();
    assert_eq!(again, 0);
    let _ = std::fs::remove_file(path);
}

#[test]
fn import_under_target_prefixes_and_uncategorized_target_is_plain() {
    let (db, path) = temp_db();
    let xml = br#"<opml><body>
<outline text="Folder"><outline text="Inner" xmlUrl="https://inner.example/rss"/></outline>
<outline text="Top" xmlUrl="https://top.example/rss"/>
</body></opml>"#;
    import_opml_under(&db, xml, Some("Imports")).unwrap();
    let feeds = db.feeds().unwrap();
    let inner = feeds.iter().find(|f| f.title == "Inner").unwrap();
    assert_eq!(inner.category.as_deref(), Some("Imports / Folder"));
    let top = feeds.iter().find(|f| f.title == "Top").unwrap();
    assert_eq!(top.category.as_deref(), Some("Imports"));
    // Target "Uncategorized" is the sentinel: Python treats it
    // as no target, it does not become a path prefix.
    let (db2, path2) = temp_db();
    import_opml_under(&db2, xml, Some("Uncategorized")).unwrap();
    let feeds2 = db2.feeds().unwrap();
    let inner2 = feeds2.iter().find(|f| f.title == "Inner").unwrap();
    assert_eq!(inner2.category.as_deref(), Some("Folder"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path2);
}

#[test]
fn folder_titles_split_on_the_path_separator() {
    let (db, path) = temp_db();
    let xml = br#"<opml><body>
<outline text="A / B"><outline text="Deep" xmlUrl="https://deep.example/rss"/></outline>
</body></opml>"#;
    import_opml(&db, xml).unwrap();
    let feeds = db.feeds().unwrap();
    assert_eq!(feeds[0].category.as_deref(), Some("A / B"));
    let cats: Vec<String> = db
        .categories()
        .unwrap()
        .into_iter()
        .map(|c| c.title)
        .collect();
    assert!(cats.contains(&"A".to_string()) && cats.contains(&"A / B".to_string()));
    let _ = std::fs::remove_file(path);
}

#[test]
fn parse_opml_triples_match_utils_shape() {
    // The hosted base import consumes (title, url, category)
    // triples: category defaults to the Uncategorized sentinel
    // and titles come from text/title without the "Unknown
    // Feed" substitution (utils.parse_opml yields them raw).
    let xml = br#"<opml><body>
<outline text="Folder"><outline text="Inner" title="Ignored" xmlUrl="https://inner.example/rss"/></outline>
<outline xmlUrl="https://bare.example/rss"/>
</body></opml>"#;
    let triples = parse_opml(xml).unwrap();
    assert_eq!(triples.len(), 2);
    assert_eq!(
        triples[0],
        (
            "Inner".to_string(),
            "https://inner.example/rss".to_string(),
            "Folder".to_string()
        )
    );
    assert_eq!(triples[1].1, "https://bare.example/rss");
    assert_eq!(triples[1].2, "Uncategorized");
    assert!(parse_opml(b"<opml></opml>").is_err());
}

// ---------------------------------------------------------------------------
// F03 (Miniflux preparation): bulk_import_document is strict
// where the import parse is tolerant, and wraps the body's
// children in the target category's folder outline.
// ---------------------------------------------------------------------------

#[test]
fn bulk_document_wraps_children_in_target_folder() {
    let doc = "<opml version=\"2.0\"><body><outline text=\"A\" xmlUrl=\"https://a.example/rss\" /></body></opml>";
    let out = blindrss_core::opml::bulk_import_document(doc, Some("Tech")).unwrap();
    assert!(
        out.contains("<body><outline text=\"Tech\" title=\"Tech\"><outline"),
        "{out}"
    );
    assert!(out.contains("</outline></body>"), "{out}");
    // No target: the document passes through unchanged.
    let same = blindrss_core::opml::bulk_import_document(doc, None).unwrap();
    assert_eq!(same, doc);
    // Empty body folds into the folder element itself.
    let empty =
        blindrss_core::opml::bulk_import_document("<opml><body/></opml>", Some("Tech")).unwrap();
    assert!(
        empty.contains("<body><outline text=\"Tech\" title=\"Tech\" /></body>"),
        "{empty}"
    );
    // Category text is attribute-escaped.
    let esc = blindrss_core::opml::bulk_import_document(doc, Some("A & \"B\"")).unwrap();
    assert!(esc.contains("text=\"A &amp; &quot;B&quot;\""), "{esc}");
}

#[test]
fn bulk_document_rejects_malformed_xml_and_missing_body() {
    // The tolerant import parse recovers this; ElementTree
    // (and therefore this preparation) must refuse it.
    assert!(
        blindrss_core::opml::bulk_import_document("<opml><body><outline></opml>", None).is_err()
    );
    assert!(blindrss_core::opml::bulk_import_document("<opml><head/></opml>", None).is_err());
    assert!(blindrss_core::opml::bulk_import_document("<opml><body>", None).is_err());
}
