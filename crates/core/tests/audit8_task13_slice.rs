//! Audit 8 Task 13 regressions (R01, R13, R14): metadata
//! enrichment must be lossless. Python spec:
//! `core/metadata_enrich.py` (extraction priority JSON-LD ->
//! microdata -> OpenGraph -> trafilatura; caps apply to
//! additions only; stored tags are preserved whole and deduped
//! with Unicode case folding; "Unknown" authors are
//! placeholders) and `core/article_html.py` lines 1028/1056/1092
//! (rich-reader header precedence: ID3/YouTube resolved
//! metadata overrides the feed fallback, ordinary page meta
//! only fills gaps).

use blindrss_core::Db;
use blindrss_core::article_html::render_rich_article;
use blindrss_core::db::Article;
use blindrss_core::enrich::{EnrichMeta, extract_metadata};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_db() -> (Db, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "blindrss-a8t13-{}-{nanos}-{n}.db",
        std::process::id()
    ));
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

/// Insert one article and set its stored author/tags directly,
/// the way a Python-written row would already carry them.
fn seed_article(db: &Db, path: &PathBuf, id: &str, author: Option<&str>, tags: &[&str]) {
    let feed = db
        .add_feed("https://example.com/feed", "Feed", None)
        .unwrap();
    let art = Article {
        id: id.into(),
        feed_id: feed.id.clone(),
        title: "T".into(),
        url: "https://example.com/a".into(),
        content: None,
        description: None,
        date: None,
        author: author.map(|a| a.to_string()),
        is_read: false,
        is_favorite: false,
        media_url: None,
        media_type: None,
    };
    db.insert_article_if_new(&art).unwrap();
    if !tags.is_empty() {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute(
            "UPDATE articles SET tags = ?2 WHERE id = ?1",
            rusqlite::params![id, tags.join("\n")],
        )
        .unwrap();
    }
}

fn tag_names(prefix: &str, count: usize) -> Vec<String> {
    (0..count).map(|i| format!("{prefix}{i}")).collect()
}

// ---------------------------------------------------------------- R01

#[test]
fn enrichment_preserves_twenty_existing_tags_and_fills_to_forty() {
    let (db, path) = scratch_db();
    let existing: Vec<String> = tag_names("keep", 20);
    let refs: Vec<&str> = existing.iter().map(|s| s.as_str()).collect();
    seed_article(&db, &path, "a1", None, &refs);
    let fresh: Vec<String> = tag_names("new", 25);
    db.apply_enrichment(
        "a1",
        &EnrichMeta {
            author: String::new(),
            tags: fresh,
            section: String::new(),
        },
    )
    .unwrap();
    let stored = db.article_tags("a1").unwrap();
    assert_eq!(stored.len(), 40, "existing 20 + additions fill to 40");
    assert_eq!(&stored[..20], &existing[..], "existing tags keep order");
    assert_eq!(&stored[20..], &tag_names("new", 20)[..]);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn enrichment_preserves_fortyfive_existing_tags() {
    let (db, path) = scratch_db();
    let existing: Vec<String> = tag_names("mine", 45);
    let refs: Vec<&str> = existing.iter().map(|s| s.as_str()).collect();
    seed_article(&db, &path, "a2", None, &refs);
    db.apply_enrichment(
        "a2",
        &EnrichMeta {
            author: String::new(),
            tags: vec!["extra".into()],
            section: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        db.article_tags("a2").unwrap(),
        existing,
        "no stored tag is ever dropped by enrichment"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn enrichment_dedupes_with_unicode_case_folding() {
    let (db, path) = scratch_db();
    seed_article(&db, &path, "a3", None, &["Äpfel", "Résumé"]);
    db.apply_enrichment(
        "a3",
        &EnrichMeta {
            author: String::new(),
            tags: vec!["äPFEL".into(), "RÉSUMÉ".into(), "Neu".into()],
            section: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        db.article_tags("a3").unwrap(),
        vec!["Äpfel", "Résumé", "Neu"],
        "first spelling wins under Unicode lowercase dedupe"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn enrichment_appends_section_to_stored_tags() {
    let (db, path) = scratch_db();
    seed_article(&db, &path, "a4", None, &[]);
    db.apply_enrichment(
        "a4",
        &EnrichMeta {
            author: String::new(),
            tags: Vec::new(),
            section: "Technology".into(),
        },
    )
    .unwrap();
    assert_eq!(db.article_tags("a4").unwrap(), vec!["Technology"]);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------- R13

#[test]
fn enrichment_fills_placeholder_authors_only() {
    let (db, path) = scratch_db();
    for (id, stored_author) in [
        ("u1", Some("Unknown")),
        ("u2", Some("(Unknown)")),
        ("u3", Some("unknown")),
        ("u4", None),
    ] {
        seed_article(&db, &path, id, stored_author, &[]);
        db.apply_enrichment(
            id,
            &EnrichMeta {
                author: "Real Author".into(),
                tags: Vec::new(),
                section: String::new(),
            },
        )
        .unwrap();
        assert_eq!(
            db.article_by_id(id).unwrap().unwrap().author.as_deref(),
            Some("Real Author"),
            "{id}: placeholder {stored_author:?} must be filled"
        );
    }
    seed_article(&db, &path, "u5", Some("Kept Person"), &[]);
    db.apply_enrichment(
        "u5",
        &EnrichMeta {
            author: "Other Person".into(),
            tags: Vec::new(),
            section: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        db.article_by_id("u5").unwrap().unwrap().author.as_deref(),
        Some("Kept Person"),
        "a real stored author is never replaced"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn jsonld_article_beats_organization_and_generic_meta() {
    let html = r#"<html><head>
        <meta name="author" content="Meta Author">
        <script type="application/ld+json">
        [{"@type":"Organization","name":"Org","keywords":["OrgTag"]},
         {"@type":"NewsArticle","author":{"@type":"Person","name":"Ld Author"},
          "keywords":"Tech, Science","articleSection":"World"}]
        </script></head><body><p>Story.</p></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    assert_eq!(meta.author, "Ld Author");
    assert_eq!(meta.tags, vec!["Tech", "Science"]);
    assert_eq!(meta.section, "World");
}

#[test]
fn jsonld_author_arrays_and_keyword_objects() {
    let html = r#"<html><head>
        <script type="application/ld+json">
        {"@type":"Article",
         "author":[{"name":"First"},{"name":"Second"},{"name":"Third"},{"name":"Fourth"}],
         "keywords":[{"name":"ObjTag"},"Plain",{"name":"ObjTag"}]}
        </script></head><body></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    assert_eq!(meta.author, "First, Second, Third", "at most three names");
    assert_eq!(meta.tags, vec!["ObjTag", "Plain"]);
}

#[test]
fn jsonld_graph_container_and_list_article_type() {
    let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context":"https://schema.org","@graph":[
          {"@type":"WebPage"},
          {"@type":["BlogPosting","Article"],"author":"Graph Author","keywords":["G1"]}]}
        </script></head><body></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    assert_eq!(meta.author, "Graph Author");
    assert_eq!(meta.tags, vec!["G1"]);
}

#[test]
fn microdata_only_page_supplies_all_fields() {
    let html = r#"<html><head><title>Micro</title></head><body>
        <article itemscope itemtype="https://schema.org/NewsArticle">
          <span itemprop="author">Micro Author</span>
          <meta itemprop="keywords" content="Micro, Data">
          <span itemprop="articleSection">Local</span>
          <p>Body text.</p>
        </article></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    assert_eq!(meta.author, "Micro Author");
    assert_eq!(meta.tags, vec!["Micro", "Data"]);
    assert_eq!(meta.section, "Local");
}

#[test]
fn microdata_nested_person_author_arrives_via_fallback() {
    // Verified against Python with the spec libraries
    // (extruct 0.18.0, trafilatura 2.3.1; the Python app
    // tracks trafilatura from upstream master): the extruct
    // microdata stage yields NO author for this common shape
    // (a nested Person's name lives under its "properties"
    // and _person_name reads only a top-level "name"), but
    // the trafilatura fallback reads the itemprop="author"
    // element's text, so the pipeline result has the author.
    // The port reproduces both stages.
    let html = r#"<html><head><title>Micro</title></head><body>
        <article itemscope itemtype="https://schema.org/NewsArticle">
          <span itemprop="author" itemscope itemtype="https://schema.org/Person">
            <span itemprop="name">Nested Author</span>
          </span>
          <meta itemprop="keywords" content="Micro, Data">
          <span itemprop="articleSection">Local</span>
          <p>Body text.</p>
        </article></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    assert_eq!(meta.author, "Nested Author");
    assert_eq!(meta.tags, vec!["Micro", "Data"]);
    assert_eq!(meta.section, "Local");
}

#[test]
fn opengraph_repeated_tags_and_url_author_rejected() {
    let html = r#"<html><head>
        <meta property="og:title" content="Og Story">
        <meta property="article:author" content="https://example.com/profiles/jane">
        <meta property="article:tag" content="Alpha">
        <meta property="article:tag" content="Beta">
        <meta property="article:tag" content="Alpha">
        <meta property="article:section" content="Culture">
        </head><body><p>Story body.</p></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    assert_eq!(meta.author, "", "a profile URL is not a byline");
    // Python-verified: the trafilatura fallback (the metadata is
    // incomplete without an author) reads article:section as a
    // category, so the section also lands in the merged tags.
    assert_eq!(meta.tags, vec!["Alpha", "Beta", "Culture"]);
    assert_eq!(meta.section, "Culture");
}

#[test]
fn generic_meta_page_still_enriches_via_fallback() {
    let html = r#"<html><head><title>Plain Page</title>
        <meta name="author" content="Byline Person">
        <meta name="news_keywords" content="K1, K2">
        </head><body><article>
        <p>A plain page whose only metadata lives in generic meta
        tags, the shape the trafilatura fallback reads.</p>
        </article></body></html>"#;
    let meta = extract_metadata(html, "https://example.com/story");
    // Python-verified: the trafilatura fallback reads the author
    // meta; it does not surface news_keywords for this shape, so
    // only the author is pinned here.
    assert_eq!(meta.author, "Byline Person");
}

// ---------------------------------------------------------------- R14

fn serve_once(body: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/story")
}

fn prose_page() -> String {
    let prose = concat!(
        "<p>The city council voted on Tuesday to approve the new transit plan, ",
        "a decision that ends two years of debate over bus routes and funding ",
        "for the downtown corridor project and its connecting neighbourhoods.</p>",
        "<p>Officials said the first phase will add forty electric buses to the ",
        "fleet and rebuild three stations, with construction expected to begin ",
        "in the spring and finish before the end of next year, they confirmed.</p>",
        "<p>Residents who spoke at the meeting welcomed the plan but asked the ",
        "council to protect existing stops in the east end, where many seniors ",
        "rely on the current route for medical appointments and daily shopping.</p>",
        "<p>The mayor said the budget includes money for those stops and promised ",
        "a full review of ridership data before any route is changed, adding that ",
        "public consultations will be held in every affected ward this winter.</p>"
    );
    format!(
        "<html><head><title>Page Title</title>\
         <meta name=\"byl\" content=\"By Page Author\"></head>\
         <body><article>{prose}</article></body></html>"
    )
}

#[test]
fn rich_header_page_meta_fills_gaps_but_feed_values_win() {
    let url = serve_once(prose_page());
    // Empty fallback author: the page byline fills it; the feed
    // title is kept (page meta only fills gaps on this path).
    let out =
        render_rich_article(&url, "", "Feed Title", "", "", "", "", "", "en").expect("rendered");
    assert!(out.contains("<h1>Feed Title</h1>"), "{out}");
    assert!(out.contains("Page Author"), "{out}");
    assert!(!out.contains("By Page Author"), "{out}");
    // A real feed author is NOT replaced by the page byline.
    let out = render_rich_article(&url, "", "Feed Title", "Feed Author", "", "", "", "", "en")
        .expect("rendered");
    assert!(out.contains("Feed Author"), "{out}");
    assert!(!out.contains("Page Author"), "{out}");
    // An empty feed title is filled from the page <title>.
    let out = render_rich_article(&url, "", "", "", "", "", "", "", "en").expect("rendered");
    assert!(out.contains("<h1>Page Title</h1>"), "{out}");
}

fn synchsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn text_frame(id: &str, text: &str) -> Vec<u8> {
    let mut payload = vec![3u8]; // UTF-8
    payload.extend_from_slice(text.as_bytes());
    let mut frame = Vec::new();
    frame.extend_from_slice(id.as_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&payload);
    frame
}

fn uslt_frame(text: &str) -> Vec<u8> {
    let mut payload = vec![3u8]; // UTF-8
    payload.extend_from_slice(b"eng");
    payload.push(0);
    payload.extend_from_slice(text.as_bytes());
    let mut frame = Vec::new();
    frame.extend_from_slice(b"USLT");
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&payload);
    frame
}

fn temp_mp3(frames: &[u8]) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("blindrss-a8t13-{}-{n}.mp3", std::process::id()));
    let mut tag = Vec::new();
    tag.extend_from_slice(b"ID3");
    tag.push(3);
    tag.push(0);
    tag.push(0);
    tag.extend_from_slice(&synchsafe(frames.len()));
    tag.extend_from_slice(frames);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(&tag).unwrap();
    f.write_all(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn rich_header_id3_metadata_overrides_stale_fallback() {
    let mut frames = Vec::new();
    frames.extend(text_frame("TIT2", "Episode One"));
    frames.extend(text_frame("TPE1", "The Host"));
    frames.extend(uslt_frame("Show notes for episode one, with links."));
    let path = temp_mp3(&frames);
    let out = render_rich_article(
        &path,
        "",
        "Stale Feed Title",
        "Feed Credited Author",
        "",
        "",
        "",
        "",
        "en",
    )
    .expect("rendered");
    assert!(out.contains("<h1>Episode One</h1>"), "{out}");
    assert!(out.contains("The Host"), "{out}");
    assert!(!out.contains("Stale Feed Title"), "{out}");
    assert!(!out.contains("Feed Credited Author"), "{out}");
    let _ = std::fs::remove_file(&path);

    // No artist frame: the fallback author survives, the title
    // still comes from the tag.
    let mut frames = Vec::new();
    frames.extend(text_frame("TIT2", "Second Episode"));
    frames.extend(uslt_frame("Notes without an artist frame."));
    let path = temp_mp3(&frames);
    let out = render_rich_article(
        &path,
        "",
        "Stale Feed Title",
        "Feed Credited Author",
        "",
        "",
        "",
        "",
        "en",
    )
    .expect("rendered");
    assert!(out.contains("<h1>Second Episode</h1>"), "{out}");
    assert!(out.contains("Feed Credited Author"), "{out}");
    let _ = std::fs::remove_file(&path);
}
