//! Audit 8 Task 25 — consented cookie identity/import correctness
//! (B03 Firefox imports, B04 yt-dlp jar merge, B05 clearance
//! identity + targeted recovery, B08 exported-file import, B12
//! per-operation temp dirs, B17 v20 honesty). B06/B07 jar rules
//! live in cookies_slice.rs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use blindrss_core::chromium::{JarRecord, read_cookie_db};
use blindrss_core::config::Config;
use blindrss_core::cookies;
use blindrss_core::cookies_import;
use blindrss_core::firefox::{self, FirefoxProfile};

static COUNTER: AtomicU64 = AtomicU64::new(0);
const NOW: i64 = 1_800_000_000;
const FUTURE: i64 = 1_900_000_000;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "blindrss-t25-{tag}-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(host: &str, name: &str, value: &str, expires: i64) -> JarRecord {
    JarRecord {
        host: host.into(),
        http_only: false,
        include_subdomains: host.starts_with('.'),
        path: "/".into(),
        secure: false,
        expires,
        name: name.into(),
        value: value.into(),
    }
}

/// Build a Firefox profile fixture: cookies.sqlite with the
/// given (host, name, value, expiry) rows, plus a
/// compatibility.ini when `last_version` is given.
fn firefox_profile(
    base: &Path,
    name: &str,
    rows: &[(&str, &str, &str, i64)],
    last_version: Option<&str>,
) -> FirefoxProfile {
    let profile_dir = base.join(name);
    std::fs::create_dir_all(&profile_dir).unwrap();
    let db_path = profile_dir.join("cookies.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (
                id INTEGER PRIMARY KEY, name TEXT, value TEXT, host TEXT,
                path TEXT, expiry INTEGER, lastAccessed INTEGER,
                creationTime INTEGER, isSecure INTEGER, isHttpOnly INTEGER,
                inBrowserElement INTEGER, sameSite INTEGER,
                rawSameSite INTEGER, schemeMap INTEGER
            );",
        )
        .unwrap();
        for (host, cname, value, expiry) in rows {
            conn.execute(
                "INSERT INTO moz_cookies (name, value, host, path, expiry, isSecure, isHttpOnly)
                 VALUES (?1, ?2, ?3, '/', ?4, 0, 0)",
                rusqlite::params![cname, value, host, expiry],
            )
            .unwrap();
        }
    }
    if let Some(v) = last_version {
        std::fs::write(
            profile_dir.join("compatibility.ini"),
            format!("[Compatibility]\nLastVersion={v}\n"),
        )
        .unwrap();
    }
    FirefoxProfile {
        browser: "Firefox".into(),
        profile: name.into(),
        profile_dir,
        cookie_db: db_path,
        mtime: 1000.0,
    }
}

// ---------------------------------------------------------------- B03

#[test]
fn firefox_profile_reads_rows_and_derives_user_agent() {
    let base = temp_dir("ffread");
    let profile = firefox_profile(
        &base,
        "abc.default",
        &[
            (".example.com", "cf_clearance", "tok", FUTURE),
            ("plain.example.com", "sess", "s1", 0),
            (".example.com", "old", "gone", 1000),
        ],
        Some("128.0_20240601000000"),
    );
    let records = firefox::read_profile_cookies(&profile).unwrap();
    assert_eq!(records.len(), 3);
    let clearance = records.iter().find(|r| r.name == "cf_clearance").unwrap();
    assert_eq!(clearance.host, ".example.com");
    assert!(clearance.include_subdomains);
    assert_eq!(clearance.expires, FUTURE);
    let session = records.iter().find(|r| r.name == "sess").unwrap();
    assert_eq!(session.expires, 0);
    let ua = firefox::profile_user_agent(&profile.profile_dir);
    assert!(ua.contains("rv:128.0"), "{ua}");
    assert!(ua.ends_with("Firefox/128.0"), "{ua}");

    let bad = firefox_profile(&base, "bad.default", &[], Some("not-a-version"));
    assert_eq!(firefox::profile_user_agent(&bad.profile_dir), "");
    let missing = firefox_profile(&base, "none.default", &[], None);
    assert_eq!(firefox::profile_user_agent(&missing.profile_dir), "");
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn firefox_profiles_discovered_in_both_layouts_and_deduped() {
    let base = temp_dir("ffdisc");
    let root = base.join("firefox-root");
    // Windows-style layout: <root>/Profiles/<name>/cookies.sqlite
    let p1 = root.join("Profiles").join("one.default");
    std::fs::create_dir_all(&p1).unwrap();
    std::fs::write(p1.join("cookies.sqlite"), b"x").unwrap();
    // Unix-style layout: <root>/<name>/cookies.sqlite
    let p2 = root.join("two.default-release");
    std::fs::create_dir_all(&p2).unwrap();
    std::fs::write(p2.join("cookies.sqlite"), b"x").unwrap();
    let roots = vec![
        ("Firefox".to_string(), root.clone()),
        ("Firefox".to_string(), root.clone()),
    ];
    let found = firefox::list_profiles_in(&roots);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().any(|p| p.profile_dir == p1));
    assert!(found.iter().any(|p| p.profile_dir == p2));
    std::fs::remove_dir_all(base).unwrap();
}

// ------------------------------------------------------- B03/B04 watcher

#[test]
fn watcher_clearance_mode_takes_only_firefox_clearance_and_pins_ua() {
    let base = temp_dir("watch-clear");
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let jar_path = data.join("site_cookies.txt");
    let profile = firefox_profile(
        &base,
        "p.default",
        &[
            (".example.com", "SID", "session-secret", FUTURE),
            (".example.com", "cf_clearance", "clear-tok", FUTURE),
            (".youtube.com", "SID", "yt-session", FUTURE),
        ],
        Some("128.0_20240601000000"),
    );
    let mut config = Config::default();
    let stored = cookies_import::auto_import_browser_profiles_with(
        &mut config,
        &jar_path,
        NOW,
        &[],
        &[profile],
    );
    assert_eq!(stored, 1);
    let jar = std::fs::read_to_string(&jar_path).unwrap();
    assert!(jar.contains("cf_clearance"), "{jar}");
    assert!(
        !jar.contains("session-secret"),
        "session cookie leaked: {jar}"
    );
    assert!(!jar.contains("yt-session"), "{jar}");
    // No consented full import ran, so no yt-dlp jar may appear.
    assert!(!data.join("youtube_cookies.txt").exists());
    // The clearance's issuing UA is pinned to its host.
    let uas = cookies::load_host_user_agents(&jar_path);
    let pinned = cookies::host_user_agent_for(&uas, "https://www.example.com/");
    assert!(pinned.contains("Firefox/128.0"), "{pinned}");
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn watcher_full_mode_merges_sessions_and_youtube_jar() {
    let base = temp_dir("watch-full");
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let jar_path = data.join("site_cookies.txt");
    // Pre-existing yt-dlp jar: one YouTube record to keep, one
    // unrelated record Python drops as pollution on rewrite.
    std::fs::write(
        data.join("youtube_cookies.txt"),
        "# Netscape HTTP Cookie File\n\
         .youtube.com\tTRUE\t/\tTRUE\t1900000000\tOLD\tkeepme\n\
         .example.com\tTRUE\t/\tFALSE\t1900000000\tPOLLUTE\tdropme\n",
    )
    .unwrap();
    let profile = firefox_profile(
        &base,
        "p.default",
        &[
            (".example.com", "SID", "session-secret", FUTURE),
            (".example.com", "cf_clearance", "clear-tok", FUTURE),
            (".youtube.com", "SID", "yt-session", FUTURE),
        ],
        Some("128.0_20240601000000"),
    );
    let mut config = Config::default();
    config.set_path(
        "installed_browser_cookie_import_enabled",
        serde_json::json!(true),
    );
    // A cookie file the user configured themselves is never
    // repointed at the managed jar.
    config.set_path(
        "ytdlp_cookies_file",
        serde_json::json!("/user/chosen/cookies.txt"),
    );
    let stored = cookies_import::auto_import_browser_profiles_with(
        &mut config,
        &jar_path,
        NOW,
        &[],
        std::slice::from_ref(&profile),
    );
    assert_eq!(stored, 3);
    let jar = std::fs::read_to_string(&jar_path).unwrap();
    assert!(jar.contains("session-secret"), "{jar}");
    assert!(jar.contains("clear-tok"), "{jar}");
    let yt = std::fs::read_to_string(data.join("youtube_cookies.txt")).unwrap();
    assert!(yt.contains("yt-session"), "{yt}");
    assert!(yt.contains("keepme"), "existing YouTube record lost: {yt}");
    assert!(!yt.contains("dropme"), "unrelated record kept: {yt}");
    assert!(cookies_import::is_blindrss_managed_jar(&yt));
    assert_eq!(
        config.get_str("ytdlp_cookies_file", ""),
        "/user/chosen/cookies.txt"
    );

    // With no user-configured file, the pointer IS set.
    let data2 = base.join("data2");
    std::fs::create_dir_all(&data2).unwrap();
    let jar2 = data2.join("site_cookies.txt");
    let mut config2 = Config::default();
    config2.set_path(
        "installed_browser_cookie_import_enabled",
        serde_json::json!(true),
    );
    cookies_import::auto_import_browser_profiles_with(&mut config2, &jar2, NOW, &[], &[profile]);
    assert_eq!(
        config2.get_str("ytdlp_cookies_file", ""),
        data2
            .join("youtube_cookies.txt")
            .to_string_lossy()
            .to_string()
    );
    std::fs::remove_dir_all(base).unwrap();
}

// ---------------------------------------------------------------- B05

#[test]
fn identity_store_host_precedence_and_global_rules() {
    let base = temp_dir("identity");
    let config_path = base.join("config.json");
    let jar = cookies::parse_netscape_jar(
        ".example.com\tTRUE\t/\tFALSE\t1900000000\tcf_clearance\ttok\n\
         .example.com\tTRUE\t/\tFALSE\t1900000000\tsession\ts1\n\
         plain.org\tFALSE\t/\tFALSE\t1900000000\tvisit\tv1\n",
    );
    cookies::set_host_user_agent(&config_path, "example.com", "UA-parent");
    cookies::set_host_user_agent(&config_path, "news.example.com", "UA-news");
    let entries = cookies::load_host_user_agents(&config_path);
    // Longest stored suffix wins.
    assert_eq!(
        cookies::host_user_agent_for(&entries, "https://news.example.com/a"),
        "UA-news"
    );
    assert_eq!(
        cookies::host_user_agent_for(&entries, "https://www.example.com/a"),
        "UA-parent"
    );
    // Host UA applies because the jar sends cookies for the URL.
    assert_eq!(
        cookies::user_agent_for(&config_path, &jar, "https://news.example.com/a", NOW),
        "UA-news"
    );
    // Global UA: only beside clearance.
    cookies::set_global_user_agent(&config_path, "UA-global");
    assert_eq!(cookies::get_global_user_agent(&config_path), "UA-global");
    assert_eq!(
        cookies::user_agent_for(&config_path, &jar, "https://plain.org/a", NOW),
        "",
        "no clearance, no host pin: default identity must stay"
    );
    // A host UA with NO cookies for the URL does not apply.
    cookies::set_host_user_agent(&config_path, "nocookies.net", "UA-orphan");
    assert_eq!(
        cookies::user_agent_for(&config_path, &jar, "https://nocookies.net/a", NOW),
        ""
    );
    // Clearing the global UA removes the sidecar file.
    cookies::set_global_user_agent(&config_path, "");
    assert_eq!(cookies::get_global_user_agent(&config_path), "");
    assert!(!base.join("site_cookies_ua.txt").exists());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn forget_clearance_removes_only_clearance_and_matching_uas() {
    let base = temp_dir("forget");
    let config_path = base.join("config.json");
    std::fs::write(
        base.join("site_cookies.txt"),
        "# Netscape HTTP Cookie File\n\
         .example.com\tTRUE\t/\tFALSE\t1900000000\tcf_clearance\ttok\n\
         .example.com\tTRUE\t/\tFALSE\t1900000000\tsession\ts1\n\
         .other.com\tTRUE\t/\tFALSE\t1900000000\tcf_clearance\tother\n",
    )
    .unwrap();
    cookies::set_host_user_agent(&config_path, "example.com", "UA-parent");
    cookies::set_host_user_agent(&config_path, "news.example.com", "UA-news");
    cookies::set_host_user_agent(&config_path, "other.com", "UA-other");
    assert!(cookies::forget_clearance_for(
        &config_path,
        "https://example.com/story"
    ));
    let jar = std::fs::read_to_string(base.join("site_cookies.txt")).unwrap();
    assert!(
        !jar.contains("example.com\tTRUE\t/\tFALSE\t1900000000\tcf_clearance"),
        "{jar}"
    );
    assert!(jar.contains("session\ts1"), "session cookie touched: {jar}");
    assert!(jar.contains("other.com"), "other host touched: {jar}");
    let entries = cookies::load_host_user_agents(&config_path);
    assert_eq!(
        cookies::host_user_agent_for(&entries, "https://example.com/"),
        "",
        "the forgotten host's pin must go"
    );
    assert_eq!(
        cookies::host_user_agent_for(&entries, "https://other.com/"),
        "UA-other"
    );
    // Nothing left to forget for that host now.
    assert!(!cookies::forget_clearance_for(
        &config_path,
        "https://example.com/story"
    ));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn record_browser_session_stores_only_clearance_for_the_host() {
    let base = temp_dir("record-session");
    let config_path = base.join("config.json");
    let records = vec![
        record(".example.com", "cf_clearance", "tok", FUTURE),
        record(".example.com", "SID", "session-secret", FUTURE),
        record(".other.com", "cf_clearance", "foreign", FUTURE),
        record("example.com", "__cf_bm", "bm", 0),
    ];
    let stored = cookies::record_browser_session(
        &config_path,
        "https://news.example.com/story",
        &records,
        "BrowserUA/1.0",
        NOW,
    );
    assert_eq!(stored, 2);
    let jar = std::fs::read_to_string(base.join("site_cookies.txt")).unwrap();
    assert!(jar.contains("cf_clearance\ttok"), "{jar}");
    assert!(!jar.contains("session-secret"), "{jar}");
    assert!(!jar.contains("foreign"), "third-party cookie stored: {jar}");
    // The session-shaped clearance got Python's one-hour expiry.
    let bm_line = jar.lines().find(|l| l.contains("__cf_bm")).unwrap();
    assert!(
        bm_line.contains(&format!("\t{}\t", NOW + 3600)),
        "{bm_line}"
    );
    // The browser's UA is pinned to the request host.
    let entries = cookies::load_host_user_agents(&config_path);
    assert_eq!(
        cookies::host_user_agent_for(&entries, "https://news.example.com/"),
        "BrowserUA/1.0"
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn groups_io_recovery_picks_the_auth_profile_only() {
    let base = temp_dir("groupsio");
    let config_path = base.join("config.json");
    let weak = firefox_profile(
        &base,
        "weak.default",
        &[(".groups.io", "prefs", "p1", FUTURE)],
        Some("128.0_20240601000000"),
    );
    let strong = firefox_profile(
        &base,
        "strong.default",
        &[
            (".groups.io", "groupsio_session", "login-tok", FUTURE),
            (".groups.io", "prefs", "p2", FUTURE),
            (".example.com", "cf_clearance", "unrelated", FUTURE),
        ],
        Some("129.0_20240601000000"),
    );
    // A non-Groups.io URL is refused before anything is read.
    assert!(!cookies::refresh_groups_io_cookies_with_profiles(
        &config_path,
        "https://example.com/",
        &[weak.clone(), strong.clone()],
    ));
    assert!(!base.join("site_cookies.txt").exists());
    assert!(cookies::refresh_groups_io_cookies_with_profiles(
        &config_path,
        "https://groups.io/g/test",
        &[weak, strong],
    ));
    let jar = std::fs::read_to_string(base.join("site_cookies.txt")).unwrap();
    assert!(jar.contains("login-tok"), "{jar}");
    assert!(
        jar.contains("prefs\tp2"),
        "winner's cookies merged whole: {jar}"
    );
    assert!(!jar.contains("p1"), "losing profile blended in: {jar}");
    assert!(!jar.contains("unrelated"), "other sites merged: {jar}");
    let entries = cookies::load_host_user_agents(&config_path);
    let ua = cookies::host_user_agent_for(&entries, "https://groups.io/");
    assert!(ua.contains("Firefox/129.0"), "{ua}");
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn reddit_recovery_and_clearance_refresh_report_changes() {
    let base = temp_dir("reddit");
    let config_path = base.join("config.json");
    let profile = firefox_profile(
        &base,
        "p.default",
        &[
            (".reddit.com", "reddit_session", "r-login", FUTURE),
            (".example.com", "cf_clearance", "fresh-tok", FUTURE),
        ],
        Some("128.0_20240601000000"),
    );
    assert!(cookies::refresh_reddit_cookies_with_profiles(
        &config_path,
        "https://www.reddit.com/r/test",
        std::slice::from_ref(&profile),
    ));
    let jar = std::fs::read_to_string(base.join("site_cookies.txt")).unwrap();
    assert!(jar.contains("r-login"), "{jar}");
    assert!(
        !jar.contains("fresh-tok"),
        "reddit recovery took other sites: {jar}"
    );

    // Clearance refresh: the jar's stale token is replaced, and a
    // second identical refresh reports no change.
    assert!(cookies::refresh_clearance_with_profiles(
        &config_path,
        "https://example.com/story",
        std::slice::from_ref(&profile),
    ));
    let jar = std::fs::read_to_string(base.join("site_cookies.txt")).unwrap();
    assert!(jar.contains("fresh-tok"), "{jar}");
    assert!(!cookies::refresh_clearance_with_profiles(
        &config_path,
        "https://example.com/story",
        &[profile],
    ));
    std::fs::remove_dir_all(base).unwrap();
}

// ---------------------------------------------------------------- B08

#[test]
fn site_jar_file_validation_and_merge_protect_existing_data() {
    let base = temp_dir("jarfile");
    let jar_path = base.join("site_cookies.txt");
    std::fs::write(
        &jar_path,
        "# Netscape HTTP Cookie File\n.keep.me\tTRUE\t/\tFALSE\t1900000000\tstay\tyes\n",
    )
    .unwrap();
    let before = std::fs::read(&jar_path).unwrap();

    let (ok, msg) = cookies_import::validate_site_jar_file(&base.join("missing.txt"));
    assert!(!ok);
    assert_eq!(msg, "File not found.");

    let bad = base.join("bad.txt");
    std::fs::write(&bad, "this is not a cookie export\n").unwrap();
    let (ok, _) = cookies_import::validate_site_jar_file(&bad);
    assert!(!ok);
    assert!(cookies_import::merge_jar_file(&jar_path, &bad).is_err());
    assert_eq!(
        std::fs::read(&jar_path).unwrap(),
        before,
        "an invalid import must leave the jar byte-identical"
    );

    let good = base.join("good.txt");
    std::fs::write(
        &good,
        "# Netscape HTTP Cookie File\n.new.site\tTRUE\t/\tFALSE\t1900000000\tcf_clearance\ttok\n",
    )
    .unwrap();
    let (ok, _) = cookies_import::validate_site_jar_file(&good);
    assert!(ok);
    assert_eq!(cookies_import::merge_jar_file(&jar_path, &good).unwrap(), 1);
    let jar = std::fs::read_to_string(&jar_path).unwrap();
    assert!(jar.contains("stay\tyes"), "{jar}");
    assert!(jar.contains("cf_clearance\ttok"), "{jar}");
    std::fs::remove_dir_all(base).unwrap();
}

// ----------------------------------------------------------- B12 + B17

fn chromium_fixture_db(base: &Path, name: &str, v20: bool) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("Cookies");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
         INSERT INTO meta VALUES ('version', '30');
         CREATE TABLE cookies (
            host_key TEXT, path TEXT, is_secure INTEGER, is_httponly INTEGER,
            expires_utc INTEGER, name TEXT, value TEXT, encrypted_value BLOB
         );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO cookies VALUES ('.example.com', '/', 0, 0, 0, 'plain', 'value1', x'')",
        [],
    )
    .unwrap();
    if v20 {
        conn.execute(
            "INSERT INTO cookies VALUES ('.example.com', '/', 0, 0, 0, 'bound', '', x'763230616263')",
            [],
        )
        .unwrap();
    }
    db
}

fn leftover_temp_dirs(prefix: &str) -> usize {
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .count()
}

#[test]
fn chromium_reads_use_unique_temp_dirs_and_count_v20_separately() {
    let base = temp_dir("chromium");
    let db_a = chromium_fixture_db(&base, "a", true);
    let db_b = chromium_fixture_db(&base, "b", false);
    let before = leftover_temp_dirs("blindrss-chromium-cookies-");
    // Two same-process reads at once: under the old PID-only temp
    // directory, each reader's cleanup deleted the other's copy.
    let (ra, rb) = std::thread::scope(|scope| {
        let ta = scope.spawn(|| read_cookie_db(&db_a, None));
        let tb = scope.spawn(|| read_cookie_db(&db_b, None));
        (ta.join().unwrap(), tb.join().unwrap())
    });
    let out_a = ra.unwrap();
    assert_eq!(out_a.records.len(), 1);
    assert_eq!(out_a.records[0].name, "plain");
    // B17: the v20 row is skipped and counted as v20, honestly
    // distinct from generic decrypt failures.
    assert_eq!(out_a.skipped_v20, 1);
    assert_eq!(out_a.skipped, 0);
    let out_b = rb.unwrap();
    assert_eq!(out_b.records.len(), 1);
    assert_eq!(out_b.skipped_v20, 0);
    assert_eq!(leftover_temp_dirs("blindrss-chromium-cookies-"), before);
    std::fs::remove_dir_all(base).unwrap();
}
