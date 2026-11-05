use blindrss_core::browser::{feed_text_from_page_source, is_challenge_page};

#[test]
fn browser_feed_response_requires_a_structural_feed() {
    let rss = "<rss version='2.0'><channel><title>News</title></channel></rss>";
    assert_eq!(feed_text_from_page_source(rss).as_deref(), Some(rss));
    let wrapped = "<html><body><pre>&lt;rss version='2.0'&gt;&lt;channel&gt;&lt;title&gt;News&lt;/title&gt;&lt;/channel&gt;&lt;/rss&gt;</pre></body></html>";
    assert!(
        feed_text_from_page_source(wrapped)
            .unwrap()
            .starts_with("<rss")
    );
    assert!(feed_text_from_page_source("<html><body>Not a feed</body></html>").is_none());
    assert!(
        feed_text_from_page_source(
            "{\"version\":\"https://jsonfeed.org/version/1.1\",\"items\":[]}"
        )
        .is_some()
    );
    assert!(
        feed_text_from_page_source(
            "{\"version\":\"https://jsonfeed.org/version/1.1\",\"items\":42}"
        )
        .is_none()
    );
}

#[test]
fn browser_challenges_cannot_be_accepted_as_article_content() {
    for marker in [
        "Just a moment...",
        "_cf_chl_opt",
        "Verify you are human",
        "Performing security verification",
    ] {
        assert!(is_challenge_page(&format!(
            "<html><body>{marker}</body></html>"
        )));
    }
    assert!(!is_challenge_page(
        "<article>A published story about the day's news.</article>"
    ));
}

#[test]
#[ignore = "requires installed Chromium; opens only an isolated managed headless profile"]
fn installed_browser_fetches_javascript_page_and_harvests_own_cookie() {
    use std::io::{Read, Write};
    use std::time::Duration;
    let directory =
        std::env::temp_dir().join(format!("blindrss-browser-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let mut config = blindrss_core::config::Config::default();
    config.set_path("browser_download_if_missing", serde_json::json!(false));
    blindrss_core::browser::configure(&config, &directory.join("config.json"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let body = format!(
                "<html><head><title>Browser fixture</title><link rel='alternate' type='application/rss+xml' href='/feed.xml'></head><body><article>{}</article><script>document.body.setAttribute('data-hydrated','yes');</script></body></html>",
                "Actual story. ".repeat(40)
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nSet-Cookie: cf_clearance=fixture; Path=/\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let response = blindrss_core::browser::fetch_page(
        &format!("http://{address}/story"),
        Duration::from_secs(20),
        None,
    )
    .unwrap();
    assert!(response.html.contains("data-hydrated=\"yes\""));
    assert!(
        std::fs::read_to_string(directory.join("site_cookies.txt"))
            .unwrap()
            .contains("cf_clearance\tfixture")
    );
    blindrss_core::browser::shutdown();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn browser_feed_validation_rejects_malformed_xml_and_html_feed_mentions() {
    for body in [
        "<rss><channel></rss>",
        "<rss><title>&unknown;</title></rss>",
        "<![CDATA[bad]]><rss/>",
        "<rss 42/>",
        "<rss/ ><rss/>",
        "<html><body><rss/></body></html>",
        "{\"version\":\"wrong\",\"items\":[]}",
    ] {
        assert!(feed_text_from_page_source(body).is_none(), "{body}");
    }
    for body in [
        "<feed xmlns='http://www.w3.org/2005/Atom'/>",
        "<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'/>",
        "<channel/>",
    ] {
        assert_eq!(feed_text_from_page_source(body).as_deref(), Some(body));
    }
}

#[test]
fn browser_rejects_invalid_targets_and_cancelled_requests_before_launch() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    assert!(
        blindrss_core::browser::fetch_page("file:///private", Duration::from_secs(1), None)
            .is_err()
    );
    assert!(blindrss_core::browser::fetch_page("http://127.0.0.1/", Duration::ZERO, None).is_err());
    let cancelled = AtomicBool::new(true);
    assert!(
        blindrss_core::browser::fetch_page(
            "http://127.0.0.1/",
            Duration::from_secs(1),
            Some(&cancelled)
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// Audit 8 Task 24: qualification deciders + fixture-driven
// browser lifecycle and production wiring tests. The fake
// browser (tests/fixtures/fake_browser.py) speaks just
// enough CDP; no live browser is involved.
// ---------------------------------------------------------------------------

#[test]
fn refresh_qualification_matches_python() {
    use blindrss_core::browser::{
        fetch_error_status, refresh_error_qualifies, refresh_response_qualifies,
    };
    // Python `_http_status_from_error` reads the status
    // off the failure; the port reads ureq's rendered
    // `http status: NNN` — and nothing else: the OS error
    // number in a reset message is not a status.
    assert_eq!(
        fetch_error_status("fetch failed: http status: 403"),
        Some(403)
    );
    assert_eq!(
        fetch_error_status("fetch failed: Connection reset by peer (os error 104)"),
        None
    );
    assert_eq!(fetch_error_status("no digits at all"), None);
    // Qualifying statuses and reset-shaped errors.
    for error in [
        "fetch failed: http status: 401",
        "fetch failed: http status: 403",
        "fetch failed: http status: 429",
        "fetch failed: http status: 503",
        "fetch failed: http status: 520",
        "fetch failed: Connection reset by peer (os error 104)",
        "fetch failed: unexpected EOF",
        "fetch failed: handshake failure",
    ] {
        assert!(refresh_error_qualifies(error), "{error}");
    }
    // Non-qualifying: other statuses, timeouts, DNS.
    for error in [
        "fetch failed: http status: 404",
        "fetch failed: http status: 500",
        "fetch failed: request timeout",
        "fetch failed: failed to lookup address information",
        "fetch failed: temporary failure in name resolution",
        "read failed: broken pipe",
    ] {
        assert!(!refresh_error_qualifies(error), "{error}");
    }
    // Response branch.
    assert!(refresh_response_qualifies(403, None, ""));
    assert!(refresh_response_qualifies(520, None, ""));
    assert!(refresh_response_qualifies(
        200,
        Some("challenge"),
        "<html></html>"
    ));
    assert!(refresh_response_qualifies(
        200,
        None,
        "<html><body>see challenges.cloudflare.com — Just a moment...</body></html>"
    ));
    assert!(!refresh_response_qualifies(
        200,
        None,
        "<rss version='2.0'><channel><title>Feed</title></channel></rss>"
    ));
    assert!(!refresh_response_qualifies(404, None, "missing"));
}

#[cfg(unix)]
static FIXTURE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
fn fixture_script() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_browser.py")
}

#[cfg(unix)]
struct FixtureEnv {
    dir: std::path::PathBuf,
    log: std::path::PathBuf,
}

#[cfg(unix)]
impl FixtureEnv {
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn count(&self, needle: &str) -> usize {
        self.log_text().matches(needle).count()
    }
}

/// Point the browser stage at the fake browser with the
/// given page map and configure it against a fresh data
/// directory. Caller holds FIXTURE_SERIAL.
#[cfg(unix)]
fn setup_fixture(
    tag: &str,
    cfg: &blindrss_core::config::Config,
    pages: serde_json::Value,
) -> FixtureEnv {
    setup_fixture_with_delay(tag, cfg, pages, 0.0)
}

/// As `setup_fixture`, but the fake browser waits
/// `start_delay_seconds` before its DevTools port starts
/// answering — a loaded machine's cold browser start.
#[cfg(unix)]
fn setup_fixture_with_delay(
    tag: &str,
    cfg: &blindrss_core::config::Config,
    pages: serde_json::Value,
    start_delay_seconds: f64,
) -> FixtureEnv {
    let dir = std::env::temp_dir().join(format!("blindrss-t24-{}-{}", tag, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("browser.log");
    let behavior = serde_json::json!({
        "log": log.to_string_lossy(),
        "user_agent": "FakeBrowserUA/1.0",
        "pages": pages,
        "start_delay_seconds": start_delay_seconds,
    });
    let behavior_path = dir.join("behavior.json");
    std::fs::write(&behavior_path, behavior.to_string()).unwrap();
    // SAFETY: the fixture tests are serialized on
    // FIXTURE_SERIAL, and these variables are read only by
    // the browser stage they drive.
    unsafe {
        std::env::set_var("BLINDRSS_BROWSER", fixture_script());
        std::env::set_var("BLINDRSS_FAKE_BROWSER_BEHAVIOR", &behavior_path);
    }
    blindrss_core::browser::shutdown();
    blindrss_core::browser::configure(cfg, &dir.join("config.json"));
    FixtureEnv { dir, log }
}

#[cfg(unix)]
fn teardown_fixture(env: &FixtureEnv) {
    blindrss_core::browser::shutdown();
    // SAFETY: serialized on FIXTURE_SERIAL, as above.
    unsafe {
        std::env::remove_var("BLINDRSS_BROWSER");
        std::env::remove_var("BLINDRSS_FAKE_BROWSER_BEHAVIOR");
    }
    let _ = std::fs::remove_dir_all(&env.dir);
}

#[cfg(unix)]
fn article_html(marker: &str) -> String {
    format!(
        "<html><head><title>{marker}</title></head><body><article><h1>{marker}</h1>{}</article></body></html>",
        format!("<p>{marker} — the full story follows here with enough text to be a usable document for the reader.</p>").repeat(24)
    )
}

#[cfg(unix)]
const CHALLENGE_HTML: &str = "<html><head><title>Just a moment...</title></head><body><h1>Just a moment...</h1><p>Performing security verification — challenges.cloudflare.com</p><div class='cf-turnstile'></div></body></html>";

#[test]
#[cfg(unix)]
fn fixture_warm_session_is_reused_and_shutdown_kills_it() {
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let url = "http://127.0.0.1:9/warm/story";
    let pages = serde_json::json!({
        url: {
            "html": article_html("Warm story"),
            "cookies": [{
                "name": "cf_clearance", "value": "fixture-token",
                "domain": "127.0.0.1", "path": "/",
                "secure": false, "httpOnly": false, "expires": 0
            }]
        }
    });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("warm", &cfg, pages);
    let first =
        blindrss_core::browser::fetch_page_with_options(url, 30.0, false, "", None).unwrap();
    assert!(first.html.contains("Warm story"));
    let second =
        blindrss_core::browser::fetch_page_with_options(url, 30.0, false, "", None).unwrap();
    assert!(second.html.contains("Warm story"));
    // ONE launch across both fetches: the session is warm.
    assert_eq!(env.count("LAUNCH "), 1);
    assert_eq!(env.count("NAVIGATE "), 2);
    // The harvest stored only the clearance cookie.
    let jar = std::fs::read_to_string(env.dir.join("site_cookies.txt")).unwrap();
    assert!(jar.contains("cf_clearance\tfixture-token"), "{jar}");
    // Shutdown kills the browser process: its DevTools
    // port stops answering.
    let launch = env.log_text();
    let port: u16 = launch
        .split("--remote-debugging-port=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|p| p.parse().ok())
        .unwrap();
    blindrss_core::browser::shutdown();
    assert!(
        std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err(),
        "browser port still answering after shutdown"
    );
    teardown_fixture(&env);
}

/// Regression (macOS CI, run 41978897138): browser
/// startup is not charged against the caller's fetch
/// timeout — Python's `timeout_s` budgets the settle,
/// not the driver start. On the loaded macOS runner the
/// fixture needed ~30 s to answer discovery; every
/// 15/30 s fetch died in the launch phase ("Browser
/// retrieval timed out or cancelled") before navigating,
/// while the 90 s production paths passed slowly. Here
/// the browser needs 20 s to become ready and the fetch
/// timeout is 15 s: the fetch must still succeed.
#[test]
#[cfg(unix)]
fn fixture_slow_browser_start_still_fetches() {
    use std::time::Instant;
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let url = "http://127.0.0.1:9/slowstart/story";
    let pages = serde_json::json!({ url: { "html": article_html("Slow start story") } });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture_with_delay("slowstart", &cfg, pages, 20.0);
    let began = Instant::now();
    let response =
        blindrss_core::browser::fetch_page_with_options(url, 15.0, false, "", None).unwrap();
    assert!(response.html.contains("Slow start story"));
    // The delay was real: the fetch waited the browser
    // out instead of failing at the 15 s caller budget.
    assert!(began.elapsed().as_secs() >= 19);
    assert_eq!(env.count("NAVIGATE "), 1);
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn fixture_proxy_change_relaunches_the_session() {
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let url = "http://127.0.0.1:9/proxy/story";
    let pages = serde_json::json!({ url: { "html": article_html("Proxy story") } });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("proxy", &cfg, pages);
    blindrss_core::browser::fetch_page_with_options(
        url,
        30.0,
        false,
        "http://127.0.0.1:9999",
        None,
    )
    .unwrap();
    assert_eq!(env.count("LAUNCH "), 1);
    assert!(
        env.log_text()
            .contains("--proxy-server=http://127.0.0.1:9999")
    );
    // Same proxy: warm reuse. Different proxy: relaunch.
    blindrss_core::browser::fetch_page_with_options(
        url,
        30.0,
        false,
        "http://127.0.0.1:9999",
        None,
    )
    .unwrap();
    assert_eq!(env.count("LAUNCH "), 1);
    blindrss_core::browser::fetch_page_with_options(url, 30.0, false, "", None).unwrap();
    assert_eq!(env.count("LAUNCH "), 2);
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn fixture_click_clears_the_challenge() {
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let url = "http://127.0.0.1:9/click/story";
    let pages = serde_json::json!({
        url: {
            "html": CHALLENGE_HTML,
            "clear_on_click": true,
            "after_html": article_html("Clicked through")
        }
    });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("click", &cfg, pages);
    let response =
        blindrss_core::browser::fetch_page_with_options(url, 30.0, false, "", None).unwrap();
    assert!(response.html.contains("Clicked through"));
    assert!(env.count("CLICK") >= 1);
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn fixture_solver_stays_off_when_unauthorized() {
    use std::io::Read;
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // Counting mock solver endpoint: any hit fails the
    // test's final assertion.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let solver_address = listener.local_addr().unwrap();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits2 = hits.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            hits2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
        }
    });
    // SAFETY: serialized on FIXTURE_SERIAL.
    unsafe {
        std::env::set_var(
            "BLINDRSS_CAPTCHA_ENDPOINT_BASE",
            format!("http://{solver_address}"),
        );
    }
    let url = "http://127.0.0.1:9/solver/story";
    let pages = serde_json::json!({
        url: {
            "html": CHALLENGE_HTML,
            "turnstile": {
                "sitekey": "0x4AAAAAAAfixture", "data": "cdata",
                "pagedata": "pdata", "action": "login",
                "userAgent": "FakeBrowserUA/1.0"
            }
        }
    });
    // Default config: the solver is disabled and has no
    // key, so the escalation may click but must never
    // submit anything to the solving service.
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("solveroff", &cfg, pages);
    let result = blindrss_core::browser::fetch_page_with_options(url, 15.0, false, "", None);
    assert!(result.is_err());
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(env.count("TOKEN"), 0);
    // SAFETY: serialized on FIXTURE_SERIAL.
    unsafe {
        std::env::remove_var("BLINDRSS_CAPTCHA_ENDPOINT_BASE");
    }
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn fixture_failure_cooldown_only_when_remembered() {
    use std::time::Instant;
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let url = "http://127.0.0.1:9/cooldown/story";
    let other = "http://127.0.0.1:9/cooldown/other";
    let pages = serde_json::json!({
        url: { "html": "<html><body>tiny</body></html>" },
        other: { "html": "<html><body>tiny</body></html>" }
    });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("cooldown", &cfg, pages);
    // A genuine failure with failure memory on: the full
    // settle window runs, then the cooldown is recorded.
    let began = Instant::now();
    assert!(blindrss_core::browser::fetch_page_with_options(url, 15.0, true, "", None).is_err());
    assert!(began.elapsed().as_secs() >= 10);
    // The remembered URL is now served from the cooldown:
    // no new navigation, an immediate error.
    let navigations = env.count("NAVIGATE ");
    let began = Instant::now();
    assert!(blindrss_core::browser::fetch_page_with_options(url, 15.0, true, "", None).is_err());
    assert!(began.elapsed().as_secs() < 5);
    assert_eq!(env.count("NAVIGATE "), navigations);
    // A cancelled fetch records nothing: afterwards the
    // other URL still reaches the browser.
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    assert!(
        blindrss_core::browser::fetch_page_with_options(other, 15.0, true, "", Some(&cancelled))
            .is_err()
    );
    let before = env.count(&format!("NAVIGATE {other}"));
    assert!(blindrss_core::browser::fetch_page_with_options(other, 15.0, true, "", None).is_err());
    assert_eq!(env.count(&format!("NAVIGATE {other}")), before + 1);
    teardown_fixture(&env);
}

/// A one-shot HTTP origin on loopback: the handler maps
/// each accepted request to a raw response.
#[cfg(unix)]
fn origin_server(handler: impl Fn(&str) -> String + Send + 'static) -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut raw = Vec::new();
            let mut buf = [0u8; 2048];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&raw).to_string();
            let _ = stream.write_all(handler(&request).as_bytes());
        }
    });
    address
}

#[cfg(unix)]
fn http_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
#[cfg(unix)]
fn fixture_feed_refresh_recovers_through_the_browser() {
    use blindrss_core::{Db, feeds};
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let origin = origin_server(|_| http_response(403, CHALLENGE_HTML));
    let url = format!("http://{origin}/feed");
    let rss = "<rss version='2.0'><channel><title>Recovered Feed</title><item><guid>g1</guid><title>Recovered Item</title></item></channel></rss>";
    let pages = serde_json::json!({ url.clone(): { "html": rss } });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("feed", &cfg, pages);
    let db_path = env.dir.join("rss.db");
    let db = Db::open_or_create(&db_path).unwrap();
    let recovered =
        feeds::fetch_feed_document_cancellable(&db, &url, false, None, None, "").unwrap();
    match recovered {
        feeds::FeedFetch::Fetched {
            parsed,
            body_digest,
        } => {
            assert_eq!(parsed.title, "Recovered Feed");
            assert_eq!(parsed.entries.len(), 1);
            // Python's replacement semantics: the digest
            // is never recorded for a browser body.
            assert_eq!(body_digest, "");
        }
        other => panic!("expected a browser-recovered fetch, got {other:?}"),
    }
    // Validators cleared in the HTTP cache.
    let cached = db.cached_feed(&url).unwrap().unwrap();
    assert_eq!(cached.0, None);
    assert_eq!(cached.1, None);
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn fixture_feed_refresh_honours_the_disabled_gate() {
    use blindrss_core::{Db, feeds};
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let origin = origin_server(|_| http_response(403, CHALLENGE_HTML));
    let url = format!("http://{origin}/feed");
    let pages = serde_json::json!({ url.clone(): { "html": "<rss version='2.0'><channel><title>X</title></channel></rss>" } });
    let mut cfg = blindrss_core::config::Config::default();
    cfg.set_path("browser_feed_fallback_enabled", serde_json::json!(false));
    let env = setup_fixture("feedoff", &cfg, pages);
    let db_path = env.dir.join("rss.db");
    let db = Db::open_or_create(&db_path).unwrap();
    let error =
        feeds::fetch_feed_document_cancellable(&db, &url, false, None, None, "").unwrap_err();
    assert!(error.contains("403"), "{error}");
    // The browser was never launched.
    assert_eq!(env.count("LAUNCH "), 0);
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn fixture_discovery_escalates_from_a_challenge_page() {
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let origin = origin_server(|_| http_response(200, CHALLENGE_HTML));
    let url = format!("http://{origin}/page");
    let page = format!(
        "<html><head><title>Site</title><link rel='alternate' type='application/rss+xml' href='/feed.xml'></head><body>{}</body></html>",
        "<p>Welcome to the site, plenty of words here.</p>".repeat(60)
    );
    let pages = serde_json::json!({ url.clone(): { "html": page } });
    let cfg = blindrss_core::config::Config::default();
    let env = setup_fixture("discovery", &cfg, pages);
    let found = blindrss_core::discovery::discover_feed_urls(&url).unwrap();
    assert_eq!(found, vec![format!("http://{origin}/feed.xml")]);
    teardown_fixture(&env);
}

#[test]
#[cfg(unix)]
fn extract_clearance_refresh_recovers_a_gated_article() {
    let _serial = FIXTURE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let marker = "Zebra lantern festival";
    let article = format!(
        "<html><head><title>{marker}</title></head><body><article><h1>{marker}</h1>{}</article></body></html>",
        format!("<p>The {marker} drew crowds again this year, with lanterns strung across the harbour and music from the pier stage all evening long.</p>").repeat(10)
    );
    let article2 = article.clone();
    let origin = origin_server(move |request| {
        if request.to_lowercase().contains("cf_clearance=fresh-token") {
            http_response(200, &article2)
        } else {
            http_response(403, CHALLENGE_HTML)
        }
    });
    let url = format!("http://{origin}/story");
    // Data directory + configuration for the cookie store.
    let dir = std::env::temp_dir().join(format!("blindrss-t24-clearance-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.json");
    let cfg = blindrss_core::config::Config::default();
    blindrss_core::browser::configure(&cfg, &config_path);
    blindrss_core::extract::configure_cookies(&config_path);
    // A Firefox profile under a fixture HOME holding the
    // fresh clearance cookie for the origin host. The
    // root is the platform's (Python `_firefox_roots`):
    // ~/.mozilla/firefox on Linux, ~/Library/Application
    // Support/Firefox on macOS — the macOS CI failure was
    // this test planting the profile where macOS roots
    // never look.
    let home = dir.join("home");
    let profile_dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Firefox/test.default")
    } else {
        home.join(".mozilla/firefox/test.default")
    };
    std::fs::create_dir_all(&profile_dir).unwrap();
    {
        let conn = rusqlite::Connection::open(profile_dir.join("cookies.sqlite")).unwrap();
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
        conn.execute(
            "INSERT INTO moz_cookies (name, value, host, path, expiry, isSecure, isHttpOnly)
             VALUES ('cf_clearance', 'fresh-token', '127.0.0.1', '/', 4102444800, 0, 0)",
            [],
        )
        .unwrap();
    }
    let old_home = std::env::var_os("HOME");
    // SAFETY: serialized on FIXTURE_SERIAL; restored below.
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let (outcome, _first_html) = blindrss_core::extract::fetch_full_text(&url, "", None, "");
    // SAFETY: serialized on FIXTURE_SERIAL, as above.
    unsafe {
        match &old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
    match outcome {
        blindrss_core::extract::ExtractOutcome::Article { text, .. } => {
            assert!(text.contains(marker), "{text}");
        }
        other => panic!("expected the clearance-recovered article, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
