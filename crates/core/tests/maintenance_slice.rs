//! Backup/restore and retention tests.

use blindrss_core::maintenance::{
    apply_pending_restore, create_backup, restore_backup, run_auto_backup, stage_restore,
};

fn scratch(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("blindrss_mnt_{}_{}", std::process::id(), name));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn backup_restore_roundtrip() {
    let db_path = scratch("src.db");
    let cfg_path = scratch("src.json");
    let zip_path = scratch("backup.zip");
    std::fs::write(&cfg_path, br#"{"locale": "fr"}"#).unwrap();
    {
        // A real DB through the Python-schema fixture helper path:
        // open via blindrss Db on a schema-less file, then add the
        // articles table the fixture builder normally provides.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE articles (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO articles VALUES ('a1', 'Hello');",
        )
        .unwrap();
    }
    create_backup(&db_path, &cfg_path, db_path.parent().unwrap(), &zip_path).unwrap();

    let db2 = scratch("dst.db");
    let cfg2 = scratch("dst.json");
    restore_backup(&zip_path, &db2, &cfg2).unwrap();
    let conn = rusqlite::Connection::open(&db2).unwrap();
    let title: String = conn
        .query_row("SELECT title FROM articles WHERE id='a1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Hello");
    assert_eq!(
        std::fs::read_to_string(&cfg2).unwrap(),
        r#"{"locale": "fr"}"#
    );
}

#[test]
fn prune_removes_old_unread_keeps_favorites_and_tombstones() {
    let path = scratch("prune.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE articles (id TEXT PRIMARY KEY, feed_id TEXT, title TEXT, \
         url TEXT, content TEXT, description TEXT, date TEXT, author TEXT, \
         is_read INTEGER DEFAULT 0, is_favorite INTEGER DEFAULT 0, \
         media_url TEXT, media_type TEXT, chapter_url TEXT, opened_at REAL);
         CREATE TABLE deleted_articles (feed_id TEXT, article_id TEXT, url TEXT, \
         deleted_at REAL, title TEXT, content TEXT, description TEXT, date TEXT, \
         author TEXT, media_url TEXT, media_type TEXT, chapter_url TEXT);
         INSERT INTO articles (id, feed_id, date, is_read, is_favorite) VALUES
           ('old-read', 'f1', '2000-01-01', 1, 0),
           ('old-unread', 'f1', '2000-01-01', 0, 0),
           ('old-fav', 'f1', '2000-01-01', 1, 1),
           ('new-read', 'f1', '2999-01-01', 1, 0);
         INSERT INTO deleted_articles (article_id, deleted_at) VALUES ('ancient', 1.0);",
    )
    .unwrap();
    drop(conn);
    let db = blindrss_core::Db::open(&path).unwrap();
    // Python cleanup_old_articles (Audit 8 D04/D05): old read AND
    // unread non-favorites are removed; favorites survive; and
    // tombstones are never aged out by prune (only restore or a
    // confirmed purge removes them), so 'ancient' stays as the
    // resurrection guard for its article.
    let removed = db.prune_old_articles(30).unwrap();
    assert_eq!(removed, 2); // old-read + old-unread articles
    let left = db.articles(None, 10).unwrap();
    assert_eq!(left.len(), 2); // old-fav + new-read
    assert!(db.deleted_article_ids().unwrap().contains("ancient"));
}

// ---- Audit 8 Task 5: safe backup/restore lifecycle (O01,O02,O09,O10,O12) ----

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

static DIR_CTR: AtomicU64 = AtomicU64::new(0);

fn temp_root(tag: &str) -> std::path::PathBuf {
    let n = DIR_CTR.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!(
        "blindrss_a8t5_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn make_db(path: &std::path::Path, url: &str) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE feeds (url TEXT); INSERT INTO feeds VALUES ('{url}');"
    ))
    .unwrap();
}

fn feed_urls(db: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn.prepare("SELECT url FROM feeds").unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn zip_with(path: &std::path::Path, members: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in members {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap();
}

fn zip_names(path: &std::path::Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let mut ar = zip::ZipArchive::new(file).unwrap();
    (0..ar.len())
        .map(|i| ar.by_index(i).unwrap().name().to_string())
        .collect()
}

const EXTRAS: [&str; 4] = [
    "site_cookies.txt",
    "site_cookies_ua.txt",
    "site_cookies_ua_hosts.json",
    "youtube_cookies.txt",
];

#[test]
fn a8_restore_removes_stale_wal_shm() {
    // O01: an old WAL/SHM beside the target must not survive a restore.
    let root = temp_root("wal");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/new");
    std::fs::write(data.join("config.json"), br#"{"v": 2}"#).unwrap();
    let zip = root.join("b.zip");
    create_backup(&data.join("rss.db"), &data.join("config.json"), &data, &zip).unwrap();
    // Damage the live copy and leave stale sidecars behind.
    std::fs::remove_file(data.join("rss.db")).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/old");
    std::fs::write(data.join("rss.db-wal"), b"stale-wal").unwrap();
    std::fs::write(data.join("rss.db-shm"), b"stale-shm").unwrap();
    stage_restore(&zip, &data).unwrap();
    assert!(apply_pending_restore(&data, &data.join("rss.db")));
    assert!(!data.join("rss.db-wal").exists(), "stale WAL survived");
    assert!(!data.join("rss.db-shm").exists(), "stale SHM survived");
    assert_eq!(feed_urls(&data.join("rss.db")), ["https://example.com/new"]);
}

#[test]
fn a8_stage_rejects_header_valid_corrupt_db() {
    // O02: header-only validation is not enough; a DB whose pages are
    // corrupt must fail integrity_check and never be staged.
    let root = temp_root("corruptdb");
    let good = root.join("good.db");
    make_db(&good, "https://example.com/x");
    // Pad to several pages, then corrupt a middle page while keeping
    // the 100-byte header intact.
    {
        let conn = rusqlite::Connection::open(&good).unwrap();
        conn.execute_batch(
            "CREATE TABLE filler (x TEXT); \
             INSERT INTO filler SELECT printf('%.*c', 900, 'x') FROM \
             (WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n<40) SELECT n FROM c);",
        )
        .unwrap();
    }
    let mut bytes = std::fs::read(&good).unwrap();
    assert!(bytes.len() > 8192);
    for b in &mut bytes[4096..6144] {
        *b = 0xAB;
    }
    let zip = root.join("bad.zip");
    zip_with(&zip, &[("rss.db", &bytes)]);
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let err = stage_restore(&zip, &data).unwrap_err();
    assert!(!err.is_empty());
    assert!(
        !data.join("restore_pending").exists(),
        "corrupt DB was staged"
    );
}

#[test]
fn a8_stage_rejects_bad_config() {
    // O02: non-object or unparseable config must be rejected.
    let root = temp_root("badcfg");
    for (i, cfg) in [b"{broken".as_slice(), b"[]", b"null", b"\"text\""]
        .iter()
        .enumerate()
    {
        let zip = root.join(format!("bad{i}.zip"));
        zip_with(&zip, &[("config.json", cfg)]);
        let data = root.join(format!("data{i}"));
        std::fs::create_dir_all(&data).unwrap();
        assert!(
            stage_restore(&zip, &data).is_err(),
            "config {cfg:?} was accepted"
        );
        assert!(!data.join("restore_pending").exists());
    }
}

#[test]
fn a8_config_only_backup_stages_and_applies() {
    // O02: Python allows a config-only archive (no rss.db member).
    let root = temp_root("cfgonly");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/keep");
    std::fs::write(data.join("config.json"), br#"{"old": true}"#).unwrap();
    let zip = root.join("cfg.zip");
    zip_with(&zip, &[("config.json", br#"{"fresh": 1}"#.as_slice())]);
    stage_restore(&zip, &data).unwrap();
    assert!(apply_pending_restore(&data, &data.join("rss.db")));
    assert_eq!(
        std::fs::read_to_string(data.join("config.json")).unwrap(),
        r#"{"fresh": 1}"#
    );
    assert_eq!(
        feed_urls(&data.join("rss.db")),
        ["https://example.com/keep"],
        "config-only restore touched the database"
    );
}

#[test]
fn a8_failed_second_stage_preserves_first_stage() {
    // O02: a failed re-stage must leave the earlier valid stage intact.
    let root = temp_root("secondstage");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/first");
    std::fs::write(data.join("config.json"), br#"{"stage": 1}"#).unwrap();
    let good_zip = root.join("good.zip");
    create_backup(
        &data.join("rss.db"),
        &data.join("config.json"),
        &data,
        &good_zip,
    )
    .unwrap();
    stage_restore(&good_zip, &data).unwrap();
    // Second stage attempt: valid header DB + invalid config.
    let mut db_bytes = std::fs::read(data.join("rss.db")).unwrap();
    db_bytes.extend_from_slice(b"trailing");
    let bad_zip = root.join("bad.zip");
    zip_with(
        &bad_zip,
        &[
            ("rss.db", &db_bytes),
            ("config.json", b"{broken".as_slice()),
        ],
    );
    assert!(stage_restore(&bad_zip, &data).is_err());
    // The first stage must still apply exactly.
    std::fs::write(data.join("config.json"), br#"{"stage": 0}"#).unwrap();
    assert!(apply_pending_restore(&data, &data.join("rss.db")));
    assert_eq!(
        std::fs::read_to_string(data.join("config.json")).unwrap(),
        r#"{"stage": 1}"#
    );
    assert_eq!(
        feed_urls(&data.join("rss.db")),
        ["https://example.com/first"]
    );
}

#[test]
fn a8_foreign_zip_rejected() {
    let root = temp_root("foreign");
    let zip = root.join("x.zip");
    zip_with(&zip, &[("../evil.txt", b"x".as_slice())]);
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let err = stage_restore(&zip, &data).unwrap_err();
    assert!(
        err.contains("not a BlindRSS backup"),
        "unexpected error: {err}"
    );
    assert!(!data.join("restore_pending").exists());
    assert!(!root.join("evil.txt").exists());
}

#[test]
fn a8_optional_files_round_trip_and_omission_preserves() {
    // O09 + O02: all four cookie/UA files ride along when present;
    // an archive that omits them leaves the live copies alone.
    let root = temp_root("extras");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/e");
    std::fs::write(data.join("config.json"), br#"{}"#).unwrap();
    for name in EXTRAS {
        std::fs::write(data.join(name), format!("content-of-{name}")).unwrap();
    }
    let zip = root.join("full.zip");
    create_backup(&data.join("rss.db"), &data.join("config.json"), &data, &zip).unwrap();
    let mut names = zip_names(&zip);
    names.sort();
    let mut want: Vec<String> = EXTRAS.iter().map(|s| s.to_string()).collect();
    want.push("config.json".into());
    want.push("rss.db".into());
    want.sort();
    assert_eq!(names, want);
    // Wipe the live extras, restore, contents come back.
    for name in EXTRAS {
        std::fs::remove_file(data.join(name)).unwrap();
    }
    stage_restore(&zip, &data).unwrap();
    assert!(apply_pending_restore(&data, &data.join("rss.db")));
    for name in EXTRAS {
        assert_eq!(
            std::fs::read_to_string(data.join(name)).unwrap(),
            format!("content-of-{name}")
        );
    }
    // Archive without extras: live extras survive the restore.
    let zip2 = root.join("bare.zip");
    zip_with(
        &zip2,
        &[
            ("rss.db", &std::fs::read(data.join("rss.db")).unwrap()),
            ("config.json", br#"{"bare": 1}"#.as_slice()),
        ],
    );
    stage_restore(&zip2, &data).unwrap();
    assert!(apply_pending_restore(&data, &data.join("rss.db")));
    for name in EXTRAS {
        assert_eq!(
            std::fs::read_to_string(data.join(name)).unwrap(),
            format!("content-of-{name}"),
            "omitted optional file {name} was clobbered"
        );
    }
}

#[test]
fn a8_failed_backup_preserves_existing_archive() {
    // O10: a backup that fails mid-way must not truncate the old one.
    let root = temp_root("atomic");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.json"), br#"{}"#).unwrap();
    let dest = root.join("keep.zip");
    std::fs::write(&dest, b"PREVIOUS GOOD ARCHIVE").unwrap();
    // A directory as the DB path makes the snapshot fail.
    let fake_db = root.join("dbdir");
    std::fs::create_dir_all(&fake_db).unwrap();
    assert!(create_backup(&fake_db, &data.join("config.json"), &data, &dest).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"PREVIOUS GOOD ARCHIVE");
    let partials: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("partial"))
        .collect();
    assert!(partials.is_empty(), "stray partial left: {partials:?}");
}

#[test]
fn a8_concurrent_backups_do_not_collide() {
    // O10: PID-derived temp names collide across concurrent backups.
    let root = temp_root("concurrent");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/c");
    std::fs::write(data.join("config.json"), br#"{}"#).unwrap();
    let mut handles = Vec::new();
    for i in 0..4 {
        let db = data.join("rss.db");
        let cfg = data.join("config.json");
        let dir = data.clone();
        let out = root.join(format!("out{i}.zip"));
        handles.push(std::thread::spawn(move || {
            create_backup(&db, &cfg, &dir, &out)
        }));
    }
    for h in handles {
        h.join().unwrap().unwrap();
    }
    for i in 0..4 {
        let names = zip_names(&root.join(format!("out{i}.zip")));
        assert!(names.contains(&"rss.db".to_string()), "out{i}: {names:?}");
    }
}

#[test]
fn a8_auto_backup_rotation_mixed_names() {
    // O12: rotation counts only Python-format auto backups, newest
    // kept; unrelated and manual files are never deleted.
    let root = temp_root("rotation");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    make_db(&data.join("rss.db"), "https://example.com/r");
    std::fs::write(data.join("config.json"), br#"{}"#).unwrap();
    let folder = root.join("auto");
    std::fs::create_dir_all(&folder).unwrap();
    for day in 1..=4 {
        std::fs::write(
            folder.join(format!("BlindRSS-auto-backup-2026-01-0{day}-000000.zip")),
            b"",
        )
        .unwrap();
    }
    // An old epoch-style Rust name sorts before every dated name.
    std::fs::write(folder.join("BlindRSS-auto-backup-1700000000.zip"), b"").unwrap();
    std::fs::write(folder.join("keep-me.zip"), b"").unwrap();
    std::fs::write(folder.join("BlindRSS-backup-2026-01-05.zip"), b"").unwrap();
    std::fs::write(folder.join("BlindRSS-auto-backup-notes.txt"), b"").unwrap();
    let made = run_auto_backup(
        &folder,
        &data,
        &data.join("config.json"),
        &data.join("rss.db"),
        2,
    )
    .unwrap();
    assert!(made.exists());
    let made_name = made.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        made_name.starts_with("BlindRSS-auto-backup-") && made_name.ends_with(".zip"),
        "unexpected auto name: {made_name}"
    );
    // Dated Python format: YYYY-MM-DD-HHMMSS between prefix and .zip.
    let stamp = made_name
        .strip_prefix("BlindRSS-auto-backup-")
        .unwrap()
        .strip_suffix(".zip")
        .unwrap();
    assert_eq!(stamp.len(), 17, "stamp not Python-format: {stamp}");
    assert_eq!(stamp.as_bytes()[4], b'-');
    let mut autos: Vec<String> = std::fs::read_dir(&folder)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.starts_with("BlindRSS-auto-backup-") && n.ends_with(".zip"))
        .collect();
    autos.sort();
    assert_eq!(autos.len(), 2, "rotation kept {autos:?}");
    assert_eq!(
        autos[1], made_name,
        "newest kept must include the new backup"
    );
    assert!(folder.join("keep-me.zip").exists());
    assert!(folder.join("BlindRSS-backup-2026-01-05.zip").exists());
    assert!(folder.join("BlindRSS-auto-backup-notes.txt").exists());
}

#[test]
fn a8_restore_backup_direct_validates_before_writing() {
    // O02 for the direct helper: bad config + good DB in one archive
    // must fail without writing either target.
    let root = temp_root("direct");
    let src = root.join("src.db");
    make_db(&src, "https://example.com/d");
    let zip = root.join("mixed.zip");
    zip_with(
        &zip,
        &[
            ("rss.db", &std::fs::read(&src).unwrap()),
            ("config.json", b"[1,2]".as_slice()),
        ],
    );
    let db2 = root.join("dst.db");
    let cfg2 = root.join("dst.json");
    assert!(restore_backup(&zip, &db2, &cfg2).is_err());
    assert!(!db2.exists(), "DB written despite invalid config");
    assert!(!cfg2.exists());
}
