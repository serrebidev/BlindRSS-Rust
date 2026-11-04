//! Audit 8 Task 11 regression tests: preview/paging/search and
//! explicit Mark All Read scope (G03, G04, G05 core halves).
//!
//! Python's model (gui/mainframe.py): every article view — feed,
//! all, category — pages at `article_page_size` behind the "Load
//! more items (Enter)" placeholder, whatever read/media filter
//! or sort is active; the display order is the sort's primary key
//! with recency (date, id) as the stable tiebreak. Search runs
//! over the CURRENT view with whitespace-split AND terms across
//! title/content/description/author/url/media_url/feed title
//! (title only in title_only mode), preserving scope, filters
//! and sort. Mark All Read is one operation behind distinct
//! wrappers: the main command is global, tree context commands
//! are scoped to their node.

use blindrss_core::Db;
use blindrss_core::db::{ArticleQuery, ViewScope};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> (Db, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "blindrss_audit8_t11_{tag}_{}_{nanos}_{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let db = Db::open_or_create(&path).unwrap();
    (db, path)
}

fn seed(path: &PathBuf, sql: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
}

fn feed_sql(id: &str, title: &str, category: &str) -> String {
    format!(
        "INSERT INTO feeds (id, url, title, title_is_custom, category, icon_url) \
         VALUES ('{id}','https://example.com/{id}','{title}',0,'{category}',NULL);"
    )
}

#[allow(clippy::too_many_arguments)]
fn article_sql(
    id: &str,
    feed: &str,
    title: &str,
    date: &str,
    read: bool,
    fav: bool,
    media: Option<&str>,
    content: &str,
    description: &str,
    author: &str,
    url: &str,
) -> String {
    let media_sql = match media {
        Some(m) => format!("'{m}'"),
        None => "NULL".to_string(),
    };
    format!(
        "INSERT INTO articles (id, feed_id, title, url, content, description, date, \
         author, is_read, is_favorite, media_url, media_type) \
         VALUES ('{id}','{feed}','{title}','{url}','{content}','{description}','{date}',\
         '{author}',{},{},{} ,NULL);",
        read as i64, fav as i64, media_sql
    )
}

fn query(scope: ViewScope) -> ArticleQuery {
    ArticleQuery {
        scope,
        read_filter: 0,
        media_filter: 0,
        sort_by: "date".to_string(),
        ascending: false,
        search_terms: Vec::new(),
        search_title_only: false,
    }
}

/// Page through a query at `page` rows and return every id in
/// display order; asserts pages never overlap.
fn tile(db: &Db, q: &ArticleQuery, page: usize) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let rows = db.articles_view_page(q, page, offset).unwrap();
        if rows.is_empty() {
            break;
        }
        for a in &rows {
            assert!(!seen.contains(&a.id), "row {} paged twice", a.id);
            seen.push(a.id.clone());
        }
        offset += rows.len();
    }
    seen
}

#[test]
fn filtered_unread_feed_pages_reach_every_match_in_order() {
    // G03: an unread-only feed view used to fall off the paged
    // path entirely and cap at 300 with no way to reach older
    // matches. 25 unread + 10 read interleaved by date.
    let (db, path) = scratch("unread");
    let mut sql = feed_sql("f1", "Alpha Feed", "News");
    for i in 0..35 {
        let date = format!("2026-10-{:02}", 31 - (i % 31));
        sql.push_str(&article_sql(
            &format!("e{i:02}"),
            "f1",
            &format!("Ep {i:02}"),
            &date,
            i % 7 == 3, // read rows sprinkled in
            false,
            None,
            "body",
            "d",
            "A",
            &format!("https://example.com/e{i:02}"),
        ));
    }
    seed(&path, &sql);

    let mut q = query(ViewScope::Feed("f1".into()));
    q.read_filter = 1;
    assert_eq!(db.articles_view_count(&q).unwrap(), 30);
    let tiled = tile(&db, &q, 10);
    assert_eq!(tiled.len(), 30);
    let whole = db.articles_view_page(&q, 500, 0).unwrap();
    let whole_ids: Vec<String> = whole.iter().map(|a| a.id.clone()).collect();
    assert_eq!(tiled, whole_ids, "pages must tile the display order");
    // All returned rows are unread, newest first.
    assert!(whole.iter().all(|a| !a.is_read));
    assert!(whole.windows(2).all(|w| w[0].date >= w[1].date));
    std::fs::remove_file(&path).ok();
}

#[test]
fn alphabetical_pages_keep_recency_ties_in_both_directions() {
    // G03: alphabetical views capped at the first 300/500 rows.
    // Duplicate titles must keep Python's stable tiebreak —
    // (date DESC, id DESC) — ascending AND descending (Python's
    // stable sort with reverse=True does not flip equal keys).
    let (db, path) = scratch("alpha");
    let mut sql = feed_sql("f1", "Alpha Feed", "News");
    // Titles repeat in a cycle so ties abound; dates descend
    // with insertion so recency order is e00, e01, ...
    for i in 0..24 {
        let title = ["Beta", "alpha", "Beta", "Gamma"][(i % 4) as usize];
        let date = format!("2026-10-{:02}", 30 - i);
        sql.push_str(&article_sql(
            &format!("e{i:02}"),
            "f1",
            title,
            &date,
            false,
            false,
            None,
            "body",
            "d",
            "A",
            &format!("https://example.com/e{i:02}"),
        ));
    }
    seed(&path, &sql);

    let mut q = query(ViewScope::Feed("f1".into()));
    q.sort_by = "name".into();
    q.ascending = true;
    let asc = tile(&db, &q, 7);
    assert_eq!(asc.len(), 24);
    // 'alpha' (case-insensitive) first, then the Beta ties in
    // recency order, then Gamma ties in recency order.
    assert_eq!(asc[0], "e01");
    let beta_asc: Vec<&String> = asc
        .iter()
        .filter(|id| {
            let n: usize = id[1..].parse().unwrap();
            n.is_multiple_of(2)
        })
        .collect();
    let mut beta_sorted = beta_asc.clone();
    beta_sorted.sort();
    // Recency order among ties = ascending insertion index here
    // (dates descend with i), i.e. e00, e02, e04, ...
    assert_eq!(beta_asc, beta_sorted);

    q.ascending = false;
    let desc = tile(&db, &q, 7);
    assert_eq!(desc.len(), 24);
    // Descending flips the KEY order (Gamma, Beta, alpha) but
    // ties still run recency-first, NOT reversed.
    assert_eq!(desc[0], "e03", "Gamma group, most recent first");
    let beta_desc: Vec<&String> = desc
        .iter()
        .filter(|id| {
            let n: usize = id[1..].parse().unwrap();
            n.is_multiple_of(2)
        })
        .collect();
    assert_eq!(beta_desc, beta_sorted);
    std::fs::remove_file(&path).ok();
}

#[test]
fn date_ascending_and_media_filtered_category_pages_tile() {
    // G03: ascending date order and media-filtered category
    // views were both truncated at the cap.
    let (db, path) = scratch("ascmedia");
    let mut sql = feed_sql("f1", "Alpha Feed", "News");
    sql.push_str(&feed_sql("f2", "Beta Feed", "News / Tech"));
    for i in 0..12 {
        let date = format!("2026-10-{:02}", 10 + i);
        let media = (i % 2 == 0).then_some("https://cdn.example.com/ep.mp3");
        sql.push_str(&article_sql(
            &format!("a{i:02}"),
            "f1",
            &format!("A{i:02}"),
            &date,
            false,
            false,
            media,
            "body",
            "d",
            "A",
            &format!("https://example.com/a{i:02}"),
        ));
        sql.push_str(&article_sql(
            &format!("b{i:02}"),
            "f2",
            &format!("B{i:02}"),
            &date,
            false,
            false,
            media,
            "body",
            "d",
            "A",
            &format!("https://example.com/b{i:02}"),
        ));
    }
    seed(&path, &sql);

    // Category "News" covers the subtree (f1 + f2): media-only
    // rows are the even ones, 12 of 24.
    let mut q = query(ViewScope::Category("News".into()));
    q.media_filter = 1;
    assert_eq!(db.articles_view_count(&q).unwrap(), 12);
    let tiled = tile(&db, &q, 5);
    assert_eq!(tiled.len(), 12);

    // Ascending date over the whole subtree: oldest first.
    q.media_filter = 0;
    q.ascending = true;
    let rows = db.articles_view_page(&q, 500, 0).unwrap();
    assert_eq!(rows.len(), 24);
    assert!(rows.windows(2).all(|w| w[0].date <= w[1].date));
    assert_eq!(rows[0].date.as_deref(), Some("2026-10-10"));
    std::fs::remove_file(&path).ok();
}

fn search_seed(db: &Db, path: &PathBuf) {
    let mut sql = feed_sql("f1", "Alpha Feed", "News");
    sql.push_str(&feed_sql("f2", "Beta Feed", "News"));
    // Terms split across title + content.
    sql.push_str(&article_sql(
        "s1",
        "f1",
        "Rust weekly",
        "2026-10-30",
        false,
        false,
        None,
        "the release notes",
        "d",
        "Ann",
        "https://example.com/1",
    ));
    // Terms split across author + content.
    sql.push_str(&article_sql(
        "s2",
        "f1",
        "Plain title",
        "2026-10-29",
        false,
        false,
        None,
        "all about rust",
        "d",
        "Release Team",
        "https://example.com/2",
    ));
    // Terms split across url + description.
    sql.push_str(&article_sql(
        "s3",
        "f1",
        "Another",
        "2026-10-28",
        false,
        false,
        None,
        "nothing",
        "release day",
        "Ann",
        "https://example.com/rust",
    ));
    // Second term only in media_url.
    sql.push_str(&article_sql(
        "s4",
        "f1",
        "Rust audio",
        "2026-10-27",
        false,
        false,
        Some("https://cdn.example.com/release.mp3"),
        "nothing",
        "d",
        "Ann",
        "https://example.com/4",
    ));
    // Only one term: never a match for the two-term query.
    sql.push_str(&article_sql(
        "s5",
        "f1",
        "Release only",
        "2026-10-26",
        false,
        false,
        None,
        "nothing",
        "d",
        "Ann",
        "https://example.com/5",
    ));
    // Both terms but in the other feed (scope exclusion).
    sql.push_str(&article_sql(
        "s6",
        "f2",
        "Rust release",
        "2026-10-31",
        false,
        false,
        None,
        "rust release",
        "d",
        "Ann",
        "https://example.com/6",
    ));
    // Term only in the body, for title_only contrast.
    sql.push_str(&article_sql(
        "s7",
        "f1",
        "Untitled",
        "2026-10-25",
        false,
        false,
        None,
        "zebra crossing",
        "d",
        "Ann",
        "https://example.com/7",
    ));
    seed(path, &sql);
    let _ = db;
}

#[test]
fn search_is_and_terms_across_all_fields_within_view_scope() {
    // G04: the old search needed one contiguous phrase in
    // title/content/description, searched globally and capped
    // at 300. Python: whitespace AND terms over title, content,
    // description, author, url, media_url and the feed title,
    // inside the current view.
    let (db, path) = scratch("search");
    search_seed(&db, &path);

    let mut q = query(ViewScope::Feed("f1".into()));
    q.search_terms = vec!["rust".into(), "release".into()];
    let hits = db.articles_view_page(&q, 100, 0).unwrap();
    let mut ids: Vec<&str> = hits.iter().map(|a| a.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["s1", "s2", "s3", "s4"]);
    assert_eq!(db.articles_view_count(&q).unwrap(), 4);

    // The out-of-scope twin joins when the scope is All.
    q.scope = ViewScope::All;
    assert_eq!(db.articles_view_count(&q).unwrap(), 5);

    // Feed title is part of the searched text: "alpha" hits
    // every f1 article and no f2 article.
    q.scope = ViewScope::All;
    q.search_terms = vec!["alpha".into()];
    let hits = db.articles_view_page(&q, 100, 0).unwrap();
    assert_eq!(hits.len(), 6);
    assert!(hits.iter().all(|a| a.feed_id == "f1"));

    // title_only: only the title field counts.
    q.search_terms = vec!["zebra".into()];
    assert_eq!(db.articles_view_count(&q).unwrap(), 1);
    q.search_title_only = true;
    assert_eq!(db.articles_view_count(&q).unwrap(), 0);
    q.search_terms = vec!["rust".into()];
    let hits = db.articles_view_page(&q, 100, 0).unwrap();
    let mut ids: Vec<&str> = hits.iter().map(|a| a.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["s1", "s4", "s6"]);
    std::fs::remove_file(&path).ok();
}

#[test]
fn search_preserves_filters_sort_and_paging() {
    // G04: search results used to bypass the active read/media
    // filters and the sort, and stopped at 300 rows.
    let (db, path) = scratch("searchview");
    let mut sql = feed_sql("f1", "Alpha Feed", "News");
    for i in 0..15 {
        let date = format!("2026-10-{:02}", 16 - i);
        sql.push_str(&article_sql(
            &format!("m{i:02}"),
            "f1",
            &format!("Match {i:02}"),
            &date,
            i % 3 == 0, // every third already read
            false,
            None,
            "needle",
            "d",
            "A",
            &format!("https://example.com/m{i:02}"),
        ));
    }
    seed(&path, &sql);

    let mut q = query(ViewScope::Feed("f1".into()));
    q.search_terms = vec!["needle".into()];
    q.read_filter = 1; // unread only: 10 of 15
    q.sort_by = "name".into();
    q.ascending = true;
    assert_eq!(db.articles_view_count(&q).unwrap(), 10);
    let tiled = tile(&db, &q, 4);
    assert_eq!(tiled.len(), 10);
    // Name order among the unread matches.
    assert_eq!(tiled[0], "m01");
    assert_eq!(tiled[1], "m02");
    std::fs::remove_file(&path).ok();
}

#[test]
fn mark_view_read_honours_each_scope() {
    // G05 (core half): one operation, explicit scopes. The old
    // local path passed the raw view string to a feed-id update,
    // so category/smart/favorites scopes silently marked
    // nothing (only the displayed rows' in-memory flags flipped).
    let (db, path) = scratch("markview");
    let mut sql = feed_sql("f1", "Alpha Feed", "News");
    sql.push_str(&feed_sql("f2", "Beta Feed", "News / Tech"));
    sql.push_str(&feed_sql("f3", "Gamma Feed", "Podcasts"));
    for (id, feed, fav) in [
        ("a1", "f1", false),
        ("a2", "f1", true),
        ("b1", "f2", false),
        ("c1", "f3", true),
        ("c2", "f3", false),
    ] {
        sql.push_str(&article_sql(
            id,
            feed,
            &format!("Title {id}"),
            "2026-10-30",
            false,
            fav,
            None,
            "body",
            "d",
            "A",
            &format!("https://example.com/{id}"),
        ));
    }
    // b1 is labeled into "News" from outside the subtree.
    sql.push_str("INSERT INTO article_labels (article_id, category) VALUES ('b1','News');");
    // A smart folder matching titles containing "c" only.
    seed(&path, &sql);
    let smart_id = db
        .create_smart_folder(
            "C titles",
            r#"{"match":"all","conditions":[{"field":"title","op":"contains","value":"Title c"}]}"#,
        )
        .unwrap();

    let unread = |db: &Db| db.unread_count(None).unwrap();
    assert_eq!(unread(&db), 5);

    // Feed scope: only f1's rows.
    assert_eq!(db.mark_view_read(Some("f1")).unwrap(), 2);
    assert_eq!(unread(&db), 3);

    // Category scope: membership clause — f2 sits in the
    // subtree ("News / Tech") and b1 is labeled in besides.
    assert_eq!(db.mark_view_read(Some("cat:News")).unwrap(), 1);
    assert_eq!(unread(&db), 2);

    // Favorites scope: only favorite rows (c1; a2 already read).
    assert_eq!(db.mark_view_read(Some("favorites:all")).unwrap(), 1);
    assert_eq!(unread(&db), 1);

    // Smart scope: the rule's matches only (c2 remains).
    assert_eq!(
        db.mark_view_read(Some(&format!("smart:{smart_id}")))
            .unwrap(),
        1
    );
    assert_eq!(unread(&db), 0);

    // Global scope on a fresh unread row marks it; the
    // recommendations pseudo-feed is not a local scope.
    seed(
        &path,
        &article_sql(
            "z1",
            "f3",
            "Title z1",
            "2026-10-30",
            false,
            false,
            None,
            "body",
            "d",
            "A",
            "https://example.com/z1",
        ),
    );
    assert_eq!(
        db.mark_view_read(Some(
            blindrss_core::youtube_account::RECOMMENDATIONS_FEED_ID
        ))
        .unwrap(),
        0
    );
    assert_eq!(unread(&db), 1);
    assert_eq!(db.mark_view_read(None).unwrap(), 1);
    assert_eq!(unread(&db), 0);
    std::fs::remove_file(&path).ok();
}
