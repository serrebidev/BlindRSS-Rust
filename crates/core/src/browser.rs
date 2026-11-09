//! Browser-backed page and feed retrieval (Python
//! `core/browser_feed.py`): a local Chromium driven over
//! the DevTools protocol (CDP), used only as a challenge
//! recovery stage behind the plain HTTP paths.
//!
//! Lifecycle (Audit 8 B15, Python's shape): one warm
//! session is kept across fetches and reused while its
//! options (the proxy) still match; a reaper closes it
//! after 120 s idle; a warm session that fails on use is
//! closed and the fetch retried once from scratch. Failed
//! fetches on the remembering paths (feed refresh,
//! extraction) start a per-URL 6-hour negative-result
//! cooldown so a permanently gated page does not pay for
//! a launch on every cycle; a runtime failure (no browser,
//! unwritable profile directories) starts a 5-minute
//! runtime-unavailable cooldown instead. Fetches are
//! serialized on one lock, acquired in cancel-aware
//! slices, and cancellation never leaves an orphan
//! process: the session is process-owned and killed on
//! close, on replacement and on `shutdown`.
//!
//! Challenge interaction (Python tiers 3/4): while a
//! challenge page is on screen the settle loop retries a
//! Turnstile click on a slow cadence (first after 2 s,
//! then every 3 s). If the session settles on a challenge
//! it could not clear, the escalation runs within the
//! remaining budget: more clicking, and — only when the
//! user enabled a paid solver in Settings — the tier-4
//! flow: the `turnstile.render` hook is installed before
//! a reload, the intercepted parameters go to the solver
//! with their full challenge data (Audit 8 B02), and the
//! token is delivered through the widget's stored
//! callback. Python's escalation runs in a second,
//! WebDriver-free engine (pydoll); this port's CDP
//! session is the only engine, so the escalation reuses
//! it — the deviation is recorded in PARITY.md.
//!
//! After a success, the session's cookies and real
//! User-Agent are handed to `cookies::record_browser_
//! session`, which stores ONLY the clearance records for
//! the request host under Task 25's consent model.
//! Nothing here starts or reads a personal profile, and
//! no cookie, token or solver key is ever logged (URLs in
//! diagnostics go through the redacted form where any
//! are printed at all — this module logs nothing).

use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_SOURCE: usize = 30 * 1024 * 1024;

// Python's browser_feed constants.
const SESSION_IDLE_SECONDS: f64 = 120.0;
const SESSION_REAP_INTERVAL_SECONDS: f64 = 15.0;
const RUNTIME_FAILURE_COOLDOWN_SECONDS: f64 = 300.0;
const NEGATIVE_RESULT_COOLDOWN_SECONDS: f64 = 6.0 * 3600.0;
const NEGATIVE_RESULT_CACHE_LIMIT: usize = 512;
const SETTLE_POLL_SECONDS: f64 = 0.35;
const SETTLE_MAX_SECONDS: f64 = 25.0;
/// Browser startup is NOT charged against the caller's
/// fetch timeout: Python's `timeout_s` budgets the settle
/// poll (and the escalation), while SeleniumBase starts
/// the driver on its own terms — a cold Chromium on a
/// loaded machine may need many seconds before its
/// DevTools port answers, and that wait must not eat the
/// budget the page work needs. The cap exists only so a
/// wedged browser process cannot hang a fetch forever.
const LAUNCH_STARTUP_MAX_SECONDS: f64 = 60.0;
const MIN_USABLE_PAGE_CHARS: usize = 2048;
const SOLVE_FIRST_DELAY_SECONDS: f64 = 2.0;
const SOLVE_RETRY_SECONDS: f64 = 3.0;
const ESCALATION_BUDGET_MAX_SECONDS: f64 = 90.0;
const ESCALATION_BUDGET_MAX_WITH_SOLVER_SECONDS: f64 = 240.0;
const AUTOCLICK_MAX_WITH_SOLVER_SECONDS: f64 = 45.0;
const PARAMS_WAIT_MAX_SECONDS: f64 = 25.0;
const ESCALATION_POLL_SECONDS: f64 = 0.5;

/// A page retrieved through the browser. `text` is the
/// validated feed document for `fetch_feed` and the page
/// source for `fetch_page`; `html` is always the final
/// page source.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserPageResponse {
    pub html: String,
    pub url: String,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Refresh qualification deciders (Python
// `providers/local.py::_browser_fallback_error_qualifies`,
// `_browser_fallback_response_qualifies` and
// `_response_looks_cloudflare_challenge`). Pure functions:
// feeds.rs calls them on its fetch outcomes.
// ---------------------------------------------------------------------------

/// The HTTP status a failed feed fetch carried, if any
/// (Python `_http_status_from_error`, which reads the
/// status off the exception's attached response). In the
/// port the same fact survives as ureq's rendered
/// `http status: NNN` inside the fetch error string —
/// and ONLY that pattern is read: unrelated numbers in a
/// transport error (an OS error code, a port) are not a
/// status, exactly as Python never parses one out of the
/// message text.
pub fn fetch_error_status(error_text: &str) -> Option<u16> {
    let marker = "http status: ";
    let at = error_text.find(marker)?;
    let digits: String = error_text[at + marker.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

const REFRESH_RESET_MARKERS: &[&str] = &[
    "connection reset",
    "reset by peer",
    "connection aborted",
    "remotedisconnected",
    "curl: (56)",
    "curl: (35)",
    "unexpected eof",
    "handshake",
];

const NAME_RESOLUTION_MARKERS: &[&str] = &[
    // Python `_NAME_RESOLUTION_ERROR_MARKERS`.
    "failed to resolve",
    "name resolution",
    "temporary failure in name resolution",
    "nodename nor servname provided",
    "getaddrinfo failed",
    // ureq/rustls renderings of the same failure.
    "failed to lookup address information",
    "name or service not known",
    "no such host",
];

/// Whether a failed feed fetch qualifies for the browser
/// fallback (Python `_browser_fallback_error_qualifies`):
/// a carried status in {401, 403, 429, 503} or >= 520
/// qualifies outright; with no status, timeouts and
/// name-resolution failures never qualify, and
/// connection-reset-shaped failures do. Python keys the
/// timeout/DNS exclusions off exception types; the port
/// keys them off the rendered error text. (Python's
/// sub-case — a non-qualifying status whose attached
/// response itself looks like a challenge — has no
/// counterpart here: ureq does not hand the error body
/// to the caller.)
pub fn refresh_error_qualifies(error_text: &str) -> bool {
    if let Some(status) = fetch_error_status(error_text) {
        return matches!(status, 401 | 403 | 429 | 503) || status >= 520;
    }
    let text = error_text.to_lowercase();
    if text.contains("timeout") || text.contains("timed out") {
        return false;
    }
    if NAME_RESOLUTION_MARKERS.iter().any(|m| text.contains(m)) {
        return false;
    }
    REFRESH_RESET_MARKERS.iter().any(|m| text.contains(m))
}

/// Python `_response_looks_cloudflare_challenge`: the
/// `Cf-Mitigated: challenge` header, or the challenge host
/// plus "just a moment" in the first 4096 body chars.
pub fn response_looks_cloudflare_challenge(cf_mitigated: Option<&str>, body: &str) -> bool {
    if cf_mitigated.is_some_and(|v| v.trim().eq_ignore_ascii_case("challenge")) {
        return true;
    }
    let head: String = body.chars().take(4096).collect::<String>().to_lowercase();
    head.contains("challenges.cloudflare.com") && head.contains("just a moment")
}

/// Whether a fetched response qualifies for the browser
/// fallback (Python's response branch): a qualifying
/// status, a Cloudflare challenge shape, or a challenge
/// page body.
pub fn refresh_response_qualifies(status: u16, cf_mitigated: Option<&str>, body: &str) -> bool {
    matches!(status, 401 | 403 | 429 | 503)
        || status >= 520
        || response_looks_cloudflare_challenge(cf_mitigated, body)
        || is_challenge_page(body)
}

// ---------------------------------------------------------------------------
// Configuration context (set once by the app at startup
// and after Settings changes, like feeds::configure_cookies).
// ---------------------------------------------------------------------------

struct BrowserContext {
    /// The data directory (parent of the config file):
    /// home of the persistent browser profile, the runtime
    /// directory and the cookie sidecars.
    root: PathBuf,
    config_path: PathBuf,
    cfg: crate::config::Config,
}

static CONTEXT: Mutex<Option<BrowserContext>> = Mutex::new(None);

/// Configure from the loaded app config (`config_path` is
/// the config file; its parent is the data directory).
pub fn configure(cfg: &crate::config::Config, config_path: &std::path::Path) {
    let root = config_path
        .parent()
        .map(|p| p.to_owned())
        .unwrap_or_default();
    *CONTEXT.lock().unwrap_or_else(|p| p.into_inner()) = Some(BrowserContext {
        root,
        config_path: config_path.to_owned(),
        cfg: cfg.clone(),
    });
}

fn context() -> Option<(PathBuf, PathBuf, crate::config::Config)> {
    CONTEXT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|c| (c.root.clone(), c.config_path.clone(), c.cfg.clone()))
}

/// The config file the browser stage was configured
/// with, for callers (extraction) that need the cookie
/// store's location for the Task 25 recovery functions.
pub fn configured_config_path() -> Option<PathBuf> {
    CONTEXT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|c| c.config_path.clone())
}

/// The Settings gate for the automatic browser fallback
/// (Python's `browser_feed_fallback_enabled`, default
/// True). Refresh and discovery check this; extraction's
/// browser stage is not behind the flag in Python (it is
/// gated on the challenge evidence alone), so neither is
/// the port's.
pub fn fallback_enabled() -> bool {
    match CONTEXT.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        Some(c) => c.cfg.get_bool("browser_feed_fallback_enabled", true),
        None => true,
    }
}

/// The configured browser fallback timeout in seconds
/// (`browser_feed_fallback_timeout_seconds`, default 90);
/// callers pass it on and `fetch_document` clamps it to
/// Python's 15..=180 range.
pub fn fallback_timeout_seconds() -> f64 {
    match CONTEXT.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        Some(c) => c
            .cfg
            .get_i64("browser_feed_fallback_timeout_seconds", 90)
            .max(0) as f64,
        None => 90.0,
    }
}

// ---------------------------------------------------------------------------
// Feed / challenge recognition (kept from the original
// port; validated against Python's helpers).
// ---------------------------------------------------------------------------

/// Whether `html` carries a known challenge marker
/// (Python `_looks_like_challenge_page`: the first 200k
/// chars, lower-cased, against the marker set).
pub fn is_challenge_page(source: &str) -> bool {
    let text: String = source
        .chars()
        .take(200_000)
        .flat_map(char::to_lowercase)
        .collect();
    [
        "challenges.cloudflare.com",
        "just a moment...",
        "_cf_chl_opt",
        "cf-chl-",
        "performing security verification",
        "verify you are human",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn feed_root(name: &str) -> bool {
    ["rss", "rdf", "feed", "channel"]
        .iter()
        .any(|root| name.eq_ignore_ascii_case(root))
}

/// Strict structural feed validation (Python
/// `_looks_like_feed_text` via an XML parser): a JSON
/// Feed body, or well-formed XML whose single root is a
/// feed root element.
fn is_feed(text: &str) -> bool {
    let text = text.trim_start_matches(['\u{feff}', '\0', ' ', '\t', '\r', '\n']);
    if text.starts_with('{') {
        return serde_json::from_str::<Value>(text).ok().is_some_and(|v| {
            v.is_object()
                && v["version"]
                    .as_str()
                    .is_some_and(|v| v.starts_with("https://jsonfeed.org/version/"))
                && v.get("items").is_none_or(Value::is_array)
        });
    }
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().enable_all_checks(true);
    let mut depth = 0;
    let mut root = false;
    loop {
        use quick_xml::events::Event;
        match reader.read_event() {
            Ok(Event::Start(e)) if depth == 0 => {
                if root || !feed_root(e.local_name().as_ref()) || e.attributes().any(|a| a.is_err())
                {
                    return false;
                }
                root = true;
                depth += 1;
            }
            Ok(Event::Empty(e)) if depth == 0 => {
                if root || !feed_root(e.local_name().as_ref()) || e.attributes().any(|a| a.is_err())
                {
                    return false;
                }
                root = true;
            }
            Ok(Event::Start(e)) => {
                if e.attributes().any(|a| a.is_err()) {
                    return false;
                }
                depth += 1;
            }
            Ok(Event::Empty(e)) if e.attributes().any(|a| a.is_err()) => return false,
            Ok(Event::CData(_)) if depth == 0 => return false,
            Ok(Event::GeneralRef(e)) => {
                if depth == 0
                    || (!matches!(e.as_ref(), "amp" | "lt" | "gt" | "quot" | "apos")
                        && e.resolve_char_ref().ok().flatten().is_none())
                {
                    return false;
                }
            }
            Ok(Event::End(_)) => depth -= 1,
            Ok(Event::Text(e)) if depth == 0 => {
                if !e.as_ref().chars().all(char::is_whitespace) {
                    return false;
                }
            }
            Ok(Event::Eof) => return root && depth == 0,
            Err(_) => return false,
            _ => {}
        }
    }
}

/// Extract the feed document text from a rendered page
/// source: the raw body for XML served directly, the
/// `<pre>` text Chromium wraps it in, or a JSON Feed body.
pub fn feed_text_from_page_source(source: &str) -> Option<String> {
    if source.is_empty() || source.len() > MAX_SOURCE {
        return None;
    }
    if is_feed(source) {
        return Some(source.to_owned());
    }
    let pre = regex::Regex::new(r"(?is)<pre\b[^>]*>(.*?)</pre\s*>").ok()?;
    let candidate = crate::extract::html_to_text(pre.captures(source)?.get(1)?.as_str());
    is_feed(&candidate).then_some(candidate)
}

// ---------------------------------------------------------------------------
// Session state: the warm session, cooldowns, and the
// serializing fetch lock.
// ---------------------------------------------------------------------------

/// The options a warm session was launched with; a fetch
/// whose options differ closes and relaunches it
/// (Python's options-dict comparison). The profile
/// directory is constant, images are always blocked, so
/// the proxy is the only varying member.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchOptions {
    proxy: String,
}

struct WarmSession {
    child: Child,
    cdp: Cdp,
    options: FetchOptions,
    expires_at: Instant,
}

impl WarmSession {
    fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct BrowserState {
    negative: HashMap<String, Instant>,
    runtime_unavailable_until: Option<Instant>,
    reaper_started: bool,
}

static SESSION: Mutex<Option<WarmSession>> = Mutex::new(None);
static STATE: std::sync::LazyLock<Mutex<BrowserState>> =
    std::sync::LazyLock::new(|| Mutex::new(BrowserState::default()));
static FETCH_LOCK: Mutex<()> = Mutex::new(());

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Acquire the fetch lock in 0.25 s slices (Python
/// `_acquire_fetch_lock`): false on deadline/cancel.
fn acquire_fetch_lock(
    timeout_s: f64,
    cancel: Option<&AtomicBool>,
) -> Option<std::sync::MutexGuard<'static, ()>> {
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.max(1.0));
    loop {
        if cancelled(cancel) {
            return None;
        }
        match FETCH_LOCK.try_lock() {
            Ok(guard) => return Some(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::sync::TryLockError::Poisoned(p)) => return Some(p.into_inner()),
        }
    }
}

fn negative_active(url: &str) -> bool {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    match state.negative.get(url) {
        Some(at) if at.elapsed().as_secs_f64() < NEGATIVE_RESULT_COOLDOWN_SECONDS => true,
        Some(_) => {
            state.negative.remove(url);
            false
        }
        None => false,
    }
}

fn record_negative(url: &str) {
    let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    state
        .negative
        .retain(|_, at| now.duration_since(*at).as_secs_f64() < NEGATIVE_RESULT_COOLDOWN_SECONDS);
    if state.negative.len() >= NEGATIVE_RESULT_CACHE_LIMIT {
        state.negative.clear();
    }
    state.negative.insert(url.to_string(), now);
}

fn clear_negative(url: &str) {
    STATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .negative
        .remove(url);
}

fn runtime_unavailable() -> bool {
    STATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .runtime_unavailable_until
        .is_some_and(|until| Instant::now() < until)
}

fn mark_runtime_unavailable() {
    STATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .runtime_unavailable_until =
        Some(Instant::now() + Duration::from_secs_f64(RUNTIME_FAILURE_COOLDOWN_SECONDS));
}

fn start_reaper_once() {
    {
        let mut state = STATE.lock().unwrap_or_else(|p| p.into_inner());
        if state.reaper_started {
            return;
        }
        state.reaper_started = true;
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs_f64(SESSION_REAP_INTERVAL_SECONDS));
            // A fetch in flight refreshes the deadline anyway;
            // never block behind one.
            let Ok(_guard) = FETCH_LOCK.try_lock() else {
                continue;
            };
            let mut slot = SESSION.lock().unwrap_or_else(|p| p.into_inner());
            if slot
                .as_ref()
                .is_some_and(|s| Instant::now() >= s.expires_at)
                && let Some(session) = slot.take()
            {
                session.close();
            }
        }
    });
}

/// Close any warm browser session (called on application
/// exit; Python `browser_feed.shutdown`).
pub fn shutdown() {
    let deadline = Instant::now() + Duration::from_secs(10);
    let _guard = loop {
        match FETCH_LOCK.try_lock() {
            Ok(guard) => break Some(guard),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::sync::TryLockError::Poisoned(p)) => break Some(p.into_inner()),
        }
    };
    if let Some(session) = SESSION.lock().unwrap_or_else(|p| p.into_inner()).take() {
        session.close();
    }
}

// ---------------------------------------------------------------------------
// CDP client + session launch.
// ---------------------------------------------------------------------------

struct Cdp {
    socket: tungstenite::WebSocket<TcpStream>,
    id: u64,
}

impl Cdp {
    fn call(&mut self, method: &str, params: Value, deadline: Instant) -> Result<Value, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Browser retrieval timed out".into());
        }
        self.socket
            .get_mut()
            .set_write_timeout(Some(remaining))
            .map_err(|_| "Browser timeout setup failed")?;
        self.id += 1;
        self.socket
            .send(tungstenite::Message::text(
                json!({"id":self.id,"method":method,"params":params}).to_string(),
            ))
            .map_err(|_| "Browser command failed")?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Browser retrieval timed out".into());
            }
            self.socket
                .get_mut()
                .set_read_timeout(Some(remaining))
                .map_err(|_| "Browser timeout setup failed")?;
            let message = self
                .socket
                .read()
                .map_err(|_| "Browser response failed or timed out")?;
            if let tungstenite::Message::Text(text) = message {
                let value: Value =
                    serde_json::from_str(&text).map_err(|_| "Invalid browser response")?;
                if value["id"].as_u64() == Some(self.id) {
                    if value.get("error").is_some() {
                        return Err("Browser command rejected".into());
                    }
                    return Ok(value["result"].clone());
                }
            }
        }
    }

    /// Evaluate an expression and return its by-value
    /// result (Null when the page returned nothing).
    fn eval(&mut self, expression: &str, deadline: Instant) -> Result<Value, String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true}),
            deadline,
        )?;
        Ok(result["result"]["value"].clone())
    }

    /// (page source, current URL) in one round trip.
    fn page_state(&mut self, deadline: Instant) -> Result<(String, String), String> {
        let value = self.eval(
            "JSON.stringify({html:document.documentElement.outerHTML,url:location.href})",
            deadline,
        )?;
        let text = value.as_str().unwrap_or("");
        let page: Value = serde_json::from_str(text).map_err(|_| "Browser document unavailable")?;
        Ok((
            page["html"].as_str().unwrap_or("").to_string(),
            page["url"].as_str().unwrap_or("").to_string(),
        ))
    }
}

fn executable() -> Option<PathBuf> {
    // The house BLINDRSS_* pointer pattern (dependency.rs):
    // an explicit browser location wins when it exists.
    // The fixture tests drive the fake browser through it.
    if let Ok(path) = std::env::var("BLINDRSS_BROWSER") {
        let path = PathBuf::from(path.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(path) = crate::dependency::find_on_path(&[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome.exe",
        "msedge.exe",
    ]) {
        return Some(path);
    }
    let mut paths = vec![PathBuf::from(
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    )];
    for variable in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(variable) {
            paths.push(PathBuf::from(&base).join("Google/Chrome/Application/chrome.exe"));
            paths.push(PathBuf::from(&base).join("Microsoft/Edge/Application/msedge.exe"));
        }
    }
    paths.into_iter().find(|p| p.is_file())
}

enum LaunchError {
    /// No browser, or the profile directories cannot be
    /// prepared: the runtime-unavailable cooldown applies
    /// (Python's import/dir failure path).
    Unavailable(String),
    /// The browser started (or tried to) and failed: an
    /// ordinary attempt failure.
    Failed(String),
}

fn launch_session(
    root: &std::path::Path,
    options: &FetchOptions,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<WarmSession, LaunchError> {
    let executable = executable()
        .ok_or_else(|| LaunchError::Unavailable("No installed Chromium browser found".into()))?;
    let profile = root.join("feed_browser_profile");
    let runtime = root.join("feed_browser_runtime");
    std::fs::create_dir_all(&profile)
        .and_then(|_| std::fs::create_dir_all(&runtime))
        .map_err(|_| {
            LaunchError::Unavailable("Could not prepare browser profile directories".into())
        })?;
    // Pick the DevTools port ourselves so the websocket
    // address is known without racing the profile's
    // DevToolsActivePort file across warm reuses.
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|_| LaunchError::Failed("Could not allocate a DevTools port".into()))?;
        let port = listener
            .local_addr()
            .map_err(|_| LaunchError::Failed("Could not allocate a DevTools port".into()))?
            .port();
        drop(listener);
        port
    };
    let mut command = Command::new(&executable);
    command
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--remote-debugging-address=127.0.0.1",
            // Python's block_images option.
            "--blink-settings=imagesEnabled=false",
        ])
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--user-agent={}", crate::http::user_agent()));
    if !options.proxy.is_empty() {
        command.arg(format!("--proxy-server={}", options.proxy));
    }
    command
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|_| LaunchError::Failed("Could not start Chromium".into()))?;
    // Poll the discovery endpoint until the page target
    // answers; bail on cancel, deadline or early exit.
    // The endpoint is the loopback DevTools port of the
    // process just spawned: an environment proxy must
    // never sit between us and it (ureq's default config
    // reads one from the environment), so it is disabled
    // explicitly.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .proxy(None)
        .build()
        .into();
    let websocket = loop {
        if cancelled(cancel) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::Failed("Browser retrieval cancelled".into()));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LaunchError::Failed("Browser startup timed out".into()));
        }
        if child
            .try_wait()
            .map_err(|_| LaunchError::Failed("Browser process check failed".into()))?
            .is_some()
        {
            return Err(LaunchError::Failed(
                "Chromium exited before becoming ready".into(),
            ));
        }
        let found = agent
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .call()
            .ok()
            .and_then(|mut resp| resp.body_mut().read_to_string().ok())
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|tabs| {
                tabs.as_array().and_then(|tabs| {
                    tabs.iter()
                        .find(|tab| tab["type"] == "page")
                        .and_then(|tab| tab["webSocketDebuggerUrl"].as_str().map(str::to_owned))
                })
            });
        if let Some(ws) = found {
            break ws;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // The websocket must be the browser's own loopback
    // endpoint on the port we launched.
    let ws_url = url::Url::parse(&websocket)
        .map_err(|_| LaunchError::Failed("Browser page not available".into()))?;
    if ws_url.host_str() != Some("127.0.0.1") || ws_url.port() != Some(port) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(LaunchError::Failed(
            "Browser page endpoint is not the launched loopback browser".into(),
        ));
    }
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let remaining = deadline.saturating_duration_since(Instant::now());
    let stream = TcpStream::connect_timeout(&address, remaining)
        .map_err(|_| LaunchError::Failed("Browser connection failed".into()))?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| LaunchError::Failed("Browser timeout setup failed".into()))?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|_| LaunchError::Failed("Browser timeout setup failed".into()))?;
    let (socket, _) = tungstenite::client(websocket, stream)
        .map_err(|_| LaunchError::Failed("Browser handshake failed".into()))?;
    let mut session = WarmSession {
        child,
        cdp: Cdp { socket, id: 0 },
        options: options.clone(),
        expires_at: Instant::now() + Duration::from_secs_f64(SESSION_IDLE_SECONDS),
    };
    if let Err(e) = session.cdp.call("Page.enable", json!({}), deadline) {
        session.close();
        return Err(LaunchError::Failed(e));
    }
    Ok(session)
}

// ---------------------------------------------------------------------------
// Settle + challenge interaction + tier-4 escalation.
// ---------------------------------------------------------------------------

/// Python `_usable_document`: the usable text of a page
/// source, or None while the page is still settling.
fn usable_document(source: &str, feed_only: bool) -> Option<String> {
    if source.is_empty() || source.len() > MAX_SOURCE {
        return None;
    }
    if feed_only {
        return feed_text_from_page_source(source);
    }
    if source.chars().count() < MIN_USABLE_PAGE_CHARS || is_challenge_page(source) {
        return None;
    }
    Some(source.to_string())
}

struct Settled {
    text: String,
    final_url: String,
    html: String,
}

/// One Turnstile click attempt (the CDP counterpart of
/// SeleniumBase's `solve_captcha`): locate the challenge
/// widget and click its checkbox through trusted input
/// events. Best-effort; every failure just keeps polling.
fn try_solve_click(cdp: &mut Cdp, deadline: Instant) {
    let rect = cdp.eval(
        "(function(){var f=document.querySelector('iframe[src*=\"challenges.cloudflare.com\"],iframe[src*=\"turnstile\"],.cf-turnstile');if(!f)return null;var r=f.getBoundingClientRect();return JSON.stringify({x:r.left+30,y:r.top+r.height/2});})()",
        deadline,
    );
    let Ok(rect) = rect else { return };
    let Some(rect) = rect.as_str() else { return };
    let Ok(point) = serde_json::from_str::<Value>(rect) else {
        return;
    };
    let (Some(x), Some(y)) = (point["x"].as_f64(), point["y"].as_f64()) else {
        return;
    };
    for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
        let _ = cdp.call(
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1}),
            deadline,
        );
    }
}

/// Poll the loading page until it is usable (Python
/// `_settle_page`): 0.35 s polls, Turnstile clicks on the
/// 2 s / 3 s cadence while a challenge is on screen.
/// Page-source read failures are tolerated as an empty
/// page (Python swallows them the same way); Ok(None)
/// means the page never became usable in the window.
fn settle_page(
    cdp: &mut Cdp,
    feed_only: bool,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    allow_clicks: bool,
) -> Result<Option<Settled>, String> {
    let mut next_solve_at = Instant::now() + Duration::from_secs_f64(SOLVE_FIRST_DELAY_SECONDS);
    loop {
        if cancelled(cancel) {
            return Ok(None);
        }
        // Python's `_settle_page` treats a failed
        // page-source read as an empty page and keeps
        // polling: between navigation and first paint the
        // document read can fail transiently, and one slow
        // read on a loaded machine must not abort the
        // whole fetch as a transport failure.
        let (html, url) = cdp
            .page_state(deadline)
            .unwrap_or_else(|_| (String::new(), String::new()));
        if let Some(text) = usable_document(&html, feed_only) {
            return Ok(Some(Settled {
                text,
                final_url: if url.is_empty() {
                    String::new()
                } else {
                    url.clone()
                },
                html,
            }));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        if allow_clicks
            && !html.is_empty()
            && Instant::now() >= next_solve_at
            && is_challenge_page(&html)
        {
            next_solve_at = Instant::now() + Duration::from_secs_f64(SOLVE_RETRY_SECONDS);
            try_solve_click(cdp, deadline);
        }
        std::thread::sleep(Duration::from_secs_f64(SETTLE_POLL_SECONDS));
    }
}

/// Python's `_TURNSTILE_HOOK_JS`, verbatim: replaces
/// `window.turnstile.render` to capture the challenge
/// parameters and stall the widget while the token is
/// solved externally.
const TURNSTILE_HOOK_JS: &str = r#"
(function () {
  const timer = setInterval(() => {
    if (!window.turnstile || !window.turnstile.render) return;
    clearInterval(timer);
    window.turnstile.render = (container, params) => {
      try {
        window.__blindrssTurnstile = {
          sitekey: params.sitekey,
          pageurl: location.href,
          data: params.cData,
          pagedata: params.chlPageData,
          action: params.action,
          userAgent: navigator.userAgent
        };
        window.__blindrssTurnstileCallback = params.callback;
      } catch (e) {}
      return;
    };
  }, 50);
})();
"#;

/// Tier-4: solve the challenge through the user's paid
/// solving service (Python `_pydoll_solve_turnstile`,
/// driven here on the warm CDP session). The hook is
/// installed before the reload; the token returns
/// through the stored widget callback; the hook is
/// removed again so the warm session is not left with a
/// stalled widget for the next fetch.
fn solve_with_service(
    cdp: &mut Cdp,
    cfg: &crate::config::Config,
    url: &str,
    feed_only: bool,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<Option<Settled>, String> {
    let installed = cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": TURNSTILE_HOOK_JS}),
        deadline,
    )?;
    let identifier = installed["identifier"].as_str().unwrap_or("").to_string();
    let outcome = solve_with_service_inner(cdp, cfg, url, feed_only, deadline, cancel);
    if !identifier.is_empty() {
        let _ = cdp.call(
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({"identifier": identifier}),
            Instant::now() + Duration::from_secs(5),
        );
    }
    outcome
}

fn solve_with_service_inner(
    cdp: &mut Cdp,
    cfg: &crate::config::Config,
    url: &str,
    feed_only: bool,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<Option<Settled>, String> {
    cdp.call("Page.navigate", json!({"url": url}), deadline)?;
    // Wait for the hook to capture the challenge
    // parameters (at most 25 s, and never past the
    // escalation deadline).
    let params_deadline =
        deadline.min(Instant::now() + Duration::from_secs_f64(PARAMS_WAIT_MAX_SECONDS));
    let mut captured: Option<Value> = None;
    while !cancelled(cancel) && Instant::now() < params_deadline {
        if let Ok(value) = cdp.eval("window.__blindrssTurnstile || null", params_deadline)
            && value.is_object()
            && !value["sitekey"].as_str().unwrap_or("").is_empty()
        {
            captured = Some(value);
            break;
        }
        std::thread::sleep(Duration::from_secs_f64(ESCALATION_POLL_SECONDS));
    }
    let Some(params) = captured else {
        return Ok(None);
    };
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .as_secs_f64();
    if remaining <= 5.0 || cancelled(cancel) {
        return Ok(None);
    }
    let text_param = |key: &str| -> Option<String> {
        params[key]
            .as_str()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    };
    let challenge = crate::captcha::ChallengeParams {
        sitekey: text_param("sitekey").unwrap_or_default(),
        pageurl: text_param("pageurl").unwrap_or_else(|| url.to_string()),
        data: text_param("data"),
        pagedata: text_param("pagedata"),
        action: text_param("action"),
        user_agent: text_param("userAgent"),
    };
    let Some(token) = crate::captcha::solve_turnstile(cfg, &challenge, remaining, cancel) else {
        return Ok(None);
    };
    if cancelled(cancel) {
        return Ok(None);
    }
    // Deliver the token through the stored callback. The
    // token is interpolated as a JSON string literal; it
    // is never logged.
    let delivery = format!(
        "(function(t){{ if (window.__blindrssTurnstileCallback) {{ window.__blindrssTurnstileCallback(t); }} }})({})",
        serde_json::to_string(&token).unwrap_or_else(|_| "\"\"".to_string())
    );
    cdp.eval(&delivery, deadline)?;
    settle_page(cdp, feed_only, deadline, cancel, false)
}

/// The escalation after the primary settle lands on a
/// challenge it could not clear (Python's tier 3/4):
/// budget min(max(timeout, 20), 90) s, or 240 s when a
/// solver is configured — with the auto-click phase
/// capped at 45 s then so the paid solve still fits.
fn escalate(
    cdp: &mut Cdp,
    cfg: &crate::config::Config,
    url: &str,
    feed_only: bool,
    timeout_s: f64,
    cancel: Option<&AtomicBool>,
) -> Result<Option<Settled>, String> {
    let solver = crate::captcha::solver_available(cfg);
    let cap = if solver {
        ESCALATION_BUDGET_MAX_WITH_SOLVER_SECONDS
    } else {
        ESCALATION_BUDGET_MAX_SECONDS
    };
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.max(20.0).min(cap));
    if !solver {
        return settle_page(cdp, feed_only, deadline, cancel, true);
    }
    let click_deadline =
        deadline.min(Instant::now() + Duration::from_secs_f64(AUTOCLICK_MAX_WITH_SOLVER_SECONDS));
    if let Some(settled) = settle_page(cdp, feed_only, click_deadline, cancel, true)? {
        return Ok(Some(settled));
    }
    if cancelled(cancel) || Instant::now() >= deadline {
        return Ok(None);
    }
    solve_with_service(cdp, cfg, url, feed_only, deadline, cancel)
}

/// Save the clearance this session just won, with the
/// browser's real User-Agent (Python `_harvest_clearance`).
/// Best-effort throughout: a failure here never turns a
/// successful fetch into a failed one.
fn harvest_clearance(cdp: &mut Cdp, config_path: &std::path::Path, url: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let Ok(cookies) = cdp.call("Network.getCookies", json!({"urls": [url]}), deadline) else {
        return;
    };
    let records: Vec<_> = cookies["cookies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|cookie| {
            let host = cookie["domain"].as_str()?.to_owned();
            Some(crate::chromium::JarRecord {
                include_subdomains: host.starts_with('.'),
                host,
                name: cookie["name"].as_str()?.to_owned(),
                value: cookie["value"].as_str()?.to_owned(),
                path: cookie["path"].as_str().unwrap_or("/").to_owned(),
                secure: cookie["secure"].as_bool().unwrap_or(false),
                http_only: cookie["httpOnly"].as_bool().unwrap_or(false),
                expires: cookie["expires"].as_f64().unwrap_or(0.0).max(0.0) as i64,
            })
        })
        .collect();
    if records.is_empty() {
        return;
    }
    let ua = cdp
        .eval(
            "navigator.userAgent",
            Instant::now() + Duration::from_secs(10),
        )
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(crate::http::user_agent);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    crate::cookies::record_browser_session(config_path, url, &records, &ua, now);
}

// ---------------------------------------------------------------------------
// The fetch itself.
// ---------------------------------------------------------------------------

struct FetchRequest {
    timeout_s: f64,
    proxy: String,
    feed_only: bool,
    remember_failures: bool,
}

enum AttemptOutcome {
    Done(Result<BrowserPageResponse, String>),
    /// Transport failure: the caller decides about the
    /// one retry from scratch.
    Transport(String),
    /// The runtime is unavailable (no browser, profile
    /// directories unwritable): Python returns before any
    /// negative-result recording on this path, so the
    /// caller must not record one either.
    Unavailable(String),
}

/// One attempt on `session` (launched fresh when None).
/// On Done the session slot is settled by the caller via
/// the returned keep flag.
#[allow(clippy::too_many_arguments)]
fn attempt(
    session: Option<WarmSession>,
    root: &std::path::Path,
    config_path: &std::path::Path,
    cfg: &crate::config::Config,
    url: &str,
    request: &FetchRequest,
    timeout_s: f64,
    cancel: Option<&AtomicBool>,
) -> (Option<WarmSession>, AttemptOutcome) {
    let options = FetchOptions {
        proxy: request.proxy.clone(),
    };
    let mut session = session;
    if session.as_ref().is_some_and(|s| s.options != options)
        && let Some(old) = session.take()
    {
        old.close();
    }
    if session.is_none() {
        let deadline = Instant::now() + Duration::from_secs_f64(LAUNCH_STARTUP_MAX_SECONDS);
        match launch_session(root, &options, deadline, cancel) {
            Ok(launched) => session = Some(launched),
            Err(LaunchError::Unavailable(message)) => {
                mark_runtime_unavailable();
                return (None, AttemptOutcome::Unavailable(message));
            }
            Err(LaunchError::Failed(message)) => {
                return (None, AttemptOutcome::Transport(message));
            }
        }
    }
    let mut session = session.expect("session present");
    session.expires_at = Instant::now() + Duration::from_secs_f64(SESSION_IDLE_SECONDS);
    start_reaper_once();
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s);
    let result = run_on_session(
        &mut session,
        config_path,
        cfg,
        url,
        request,
        timeout_s,
        deadline,
        cancel,
    );
    match result {
        Ok(response) => (Some(session), AttemptOutcome::Done(Ok(response))),
        Err(AttemptFailure::Transport(message)) => {
            session.close();
            (None, AttemptOutcome::Transport(message))
        }
        Err(AttemptFailure::Unusable(message)) => {
            (Some(session), AttemptOutcome::Done(Err(message)))
        }
    }
}

enum AttemptFailure {
    Transport(String),
    Unusable(String),
}

#[allow(clippy::too_many_arguments)]
fn run_on_session(
    session: &mut WarmSession,
    config_path: &std::path::Path,
    cfg: &crate::config::Config,
    url: &str,
    request: &FetchRequest,
    timeout_s: f64,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserPageResponse, AttemptFailure> {
    let cdp = &mut session.cdp;
    cdp.call("Page.navigate", json!({"url": url}), deadline)
        .map_err(AttemptFailure::Transport)?;
    let settle_deadline =
        Instant::now() + Duration::from_secs_f64(timeout_s.clamp(5.0, SETTLE_MAX_SECONDS));
    let settled = settle_page(cdp, request.feed_only, settle_deadline, cancel, true)
        .map_err(AttemptFailure::Transport)?;
    let settled = match settled {
        Some(settled) => Some(settled),
        None => {
            // Escalation only when the session settled on
            // a challenge it could not clear: one extra
            // page-source read decides.
            let challenge = cdp
                .page_state(deadline)
                .map(|(html, _)| is_challenge_page(&html))
                .unwrap_or(false);
            if challenge && !cancelled(cancel) {
                escalate(cdp, cfg, url, request.feed_only, timeout_s, cancel)
                    .map_err(AttemptFailure::Transport)?
            } else {
                None
            }
        }
    };
    match settled {
        Some(settled) => {
            let final_url = if settled.final_url.is_empty() {
                url.to_string()
            } else {
                settled.final_url.clone()
            };
            harvest_clearance(cdp, config_path, &final_url);
            Ok(BrowserPageResponse {
                html: settled.html,
                url: final_url,
                text: settled.text,
            })
        }
        None => Err(AttemptFailure::Unusable(
            "Browser retrieval produced no usable document".into(),
        )),
    }
}

fn fetch_document(
    url: &str,
    request: &FetchRequest,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserPageResponse, String> {
    let target = url::Url::parse(url.trim()).map_err(|_| "Invalid browser URL")?;
    if !matches!(target.scheme(), "http" | "https") || target.host_str().is_none() {
        return Err("Browser URL must be HTTP or HTTPS".into());
    }
    let url = url.trim();
    if cancelled(cancel) {
        return Err("Browser retrieval cancelled".into());
    }
    if runtime_unavailable() {
        return Err("Browser runtime is in its failure cooldown".into());
    }
    if request.remember_failures && negative_active(url) {
        return Err("Browser fetch is in its failure cooldown for this URL".into());
    }
    let timeout_s = if request.timeout_s <= 0.0 {
        90.0
    } else {
        request.timeout_s
    }
    .clamp(15.0, 180.0);
    let Some(_guard) = acquire_fetch_lock(timeout_s, cancel) else {
        return Err("Browser retrieval timed out or cancelled".into());
    };
    if cancelled(cancel) {
        return Err("Browser retrieval cancelled".into());
    }
    let Some((root, config_path, cfg)) = context() else {
        return Err("Browser is not configured".into());
    };
    // A warm session can have been killed off-process;
    // that only shows up on use, so a transport failure on
    // a REUSED session earns one retry from scratch.
    let had_session = SESSION.lock().unwrap_or_else(|p| p.into_inner()).is_some();
    let mut session = SESSION.lock().unwrap_or_else(|p| p.into_inner()).take();
    let (kept, outcome) = attempt(
        session.take(),
        &root,
        &config_path,
        &cfg,
        url,
        request,
        timeout_s,
        cancel,
    );
    let (kept, outcome) = match outcome {
        AttemptOutcome::Transport(_) if had_session => attempt(
            None,
            &root,
            &config_path,
            &cfg,
            url,
            request,
            timeout_s,
            cancel,
        ),
        outcome => (kept, outcome),
    };
    if let Some(session) = kept {
        *SESSION.lock().unwrap_or_else(|p| p.into_inner()) = Some(session);
    }
    match outcome {
        AttemptOutcome::Done(Ok(response)) => {
            clear_negative(url);
            Ok(response)
        }
        AttemptOutcome::Done(Err(message)) => {
            if request.remember_failures && !cancelled(cancel) {
                record_negative(url);
            }
            Err(message)
        }
        AttemptOutcome::Transport(message) => {
            if request.remember_failures && !cancelled(cancel) {
                record_negative(url);
            }
            Err(message)
        }
        AttemptOutcome::Unavailable(message) => Err(message),
    }
}

/// Fetch a webpage through the browser (Python
/// `browser_feed.fetch_page`). `remember_failures`
/// starts the per-URL cooldown on a genuine failure —
/// repeating callers (extraction) pass true; one-shot
/// user-initiated detection passes false so it always
/// retries.
pub fn fetch_page_with_options(
    url: &str,
    timeout_s: f64,
    remember_failures: bool,
    proxy: &str,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserPageResponse, String> {
    fetch_document(
        url,
        &FetchRequest {
            timeout_s,
            proxy: proxy.to_string(),
            feed_only: false,
            remember_failures,
        },
        cancel,
    )
}

/// Fetch a feed document through the browser (Python
/// `browser_feed.fetch_feed`): the response is returned
/// only after structural feed validation, and genuine
/// failures start the per-URL cooldown.
pub fn fetch_feed(
    url: &str,
    timeout_s: f64,
    proxy: &str,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserPageResponse, String> {
    fetch_document(
        url,
        &FetchRequest {
            timeout_s,
            proxy: proxy.to_string(),
            feed_only: true,
            remember_failures: true,
        },
        cancel,
    )
}

/// Blocking worker-only operation; never starts or reads
/// a personal profile. (Kept signature: page fetch with
/// the default options — no failure memory, no proxy.)
pub fn fetch_page(
    url: &str,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserPageResponse, String> {
    if timeout.is_zero() {
        return Err("Browser retrieval timed out or cancelled".into());
    }
    fetch_page_with_options(url, timeout.as_secs_f64(), false, "", cancel)
}

/// Anonymous YouTube identity minted by the hidden browser
/// (Python `youtube_browser_session._page_identity` plus the
/// session cookies): visitor data, playability status, title,
/// the browser's real UA, and raw CDP cookie records.
pub struct YouTubeIdentity {
    pub visitor_data: String,
    pub status: String,
    pub title: String,
    pub user_agent: String,
    pub cookies: Vec<Value>,
}

const YOUTUBE_IDENTITY_JS: &str = "JSON.stringify({visitor:(window.ytcfg?.get('VISITOR_DATA')||''),status:(window.ytInitialPlayerResponse?.playabilityStatus?.status||''),title:(window.ytInitialPlayerResponse?.videoDetails?.title||''),ua:navigator.userAgent})";

/// Read the identity script's value: (visitor, status, title, UA).
fn identity_values(value: &Value) -> (String, String, String, String) {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    (
        field("visitor"),
        field("status"),
        field("title"),
        field("ua"),
    )
}

fn identity_attempt(
    session: Option<WarmSession>,
    root: &std::path::Path,
    url: &str,
    timeout_s: f64,
) -> (Option<WarmSession>, Result<Option<YouTubeIdentity>, String>) {
    let options = FetchOptions {
        proxy: String::new(),
    };
    let mut session = session;
    if session.as_ref().is_some_and(|s| s.options != options)
        && let Some(old) = session.take()
    {
        old.close();
    }
    if session.is_none() {
        let deadline = Instant::now() + Duration::from_secs_f64(LAUNCH_STARTUP_MAX_SECONDS);
        match launch_session(root, &options, deadline, None) {
            Ok(launched) => session = Some(launched),
            Err(LaunchError::Unavailable(message)) => {
                mark_runtime_unavailable();
                return (None, Err(message));
            }
            Err(LaunchError::Failed(message)) => return (None, Err(message)),
        }
    }
    let mut session = session.expect("session present");
    session.expires_at = Instant::now() + Duration::from_secs_f64(SESSION_IDLE_SECONDS);
    start_reaper_once();
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s);
    if let Err(message) = session
        .cdp
        .call("Page.navigate", json!({"url": url}), deadline)
    {
        session.close();
        return (None, Err(message));
    }
    let mut found: Option<(String, String, String, String)> = None;
    while Instant::now() < deadline {
        let value = session
            .cdp
            .eval(YOUTUBE_IDENTITY_JS, deadline)
            .unwrap_or(Value::Null);
        let parsed = value
            .as_str()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let current = identity_values(parsed.as_ref().unwrap_or(&value));
        if !current.0.is_empty() && !current.1.is_empty() {
            found = Some(current);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let Some((visitor_data, status, title, user_agent)) = found else {
        return (Some(session), Ok(None));
    };
    let cookies = session
        .cdp
        .call("Network.getCookies", json!({"urls": [url]}), deadline)
        .ok()
        .and_then(|v| v.get("cookies").and_then(|c| c.as_array()).cloned())
        .unwrap_or_default();
    (
        Some(session),
        Ok(Some(YouTubeIdentity {
            visitor_data,
            status,
            title,
            user_agent,
            cookies,
        })),
    )
}

/// Mint an anonymous YouTube identity through the warm browser
/// (Python `bootstrap_youtube_session`'s browser half): shares
/// the fetch lock and the warm session with the feed fallback,
/// retries a dead reused session once from scratch, and keeps
/// the session warm for the next caller. None on any failure —
/// no runtime, no playable identity, no cookies needed.
pub fn youtube_identity(url: &str, timeout_s: f64) -> Option<YouTubeIdentity> {
    let target = url::Url::parse(url.trim()).ok()?;
    if !matches!(target.scheme(), "http" | "https") || target.host_str().is_none() {
        return None;
    }
    if runtime_unavailable() {
        return None;
    }
    let timeout_s = if timeout_s.is_finite() {
        timeout_s.clamp(15.0, 90.0)
    } else {
        45.0
    };
    let _guard = acquire_fetch_lock(timeout_s, None)?;
    let (root, _, _) = context()?;
    // A warm session can have been killed off-process; that
    // only shows up on use, so a transport failure on a REUSED
    // session earns one retry from scratch.
    let had_session = SESSION.lock().unwrap_or_else(|p| p.into_inner()).is_some();
    let session = SESSION.lock().unwrap_or_else(|p| p.into_inner()).take();
    let (kept, outcome) = identity_attempt(session, &root, url.trim(), timeout_s);
    let (kept, outcome) = match outcome {
        Err(_) if had_session => identity_attempt(None, &root, url.trim(), timeout_s),
        outcome => (kept, outcome),
    };
    if let Some(session) = kept {
        *SESSION.lock().unwrap_or_else(|p| p.into_inner()) = Some(session);
    }
    outcome.unwrap_or(None)
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    /// Loopback mock of the paid solver: submit returns a
    /// task, the first poll answers the token. Records
    /// the submission bodies for the parameter assertions.
    fn mock_solver() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let bodies2 = bodies.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw).to_string();
                        let content_length = head
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        let have = raw
                            .windows(4)
                            .position(|w| w == b"\r\n\r\n")
                            .map(|p| raw.len() - (p + 4))
                            .unwrap_or(0);
                        if have >= content_length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&raw).to_string();
                let body = if request.contains("/in.php") {
                    bodies2.lock().unwrap().push(request);
                    r#"{"status":1,"request":"task-1"}"#
                } else {
                    r#"{"status":1,"request":"solved-token-xyz"}"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{address}"), bodies)
    }

    #[test]
    fn tier4_solve_delivers_the_token_through_the_callback() {
        let url = "http://127.0.0.1:9/tier4/story";
        let dir = std::env::temp_dir().join(format!("blindrss-t24-tier4-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("browser.log");
        let article = format!(
            "<html><body><article>{}</article></body></html>",
            "<p>Solved page content, readable at last.</p>".repeat(60)
        );
        let behavior = serde_json::json!({
            "log": log.to_string_lossy(),
            "user_agent": "FakeBrowserUA/1.0",
            "pages": {
                url: {
                    "html": "<html><head><title>Just a moment...</title></head><body><p>challenges.cloudflare.com</p><div class='cf-turnstile'></div></body></html>",
                    "turnstile": {
                        "sitekey": "0x4AAAAAAAfixture", "data": "cdata-1",
                        "pagedata": "pdata-1", "action": "login",
                        "userAgent": "FakeBrowserUA/1.0"
                    },
                    "solved_html": article,
                }
            },
        });
        let behavior_path = dir.join("behavior.json");
        std::fs::write(&behavior_path, behavior.to_string()).unwrap();
        let (solver_base, bodies) = mock_solver();
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fake_browser.py");
        // SAFETY: this test owns the process-wide browser
        // stage for its duration; the variables are the
        // documented seams and are removed afterwards.
        unsafe {
            std::env::set_var("BLINDRSS_BROWSER", &script);
            std::env::set_var("BLINDRSS_FAKE_BROWSER_BEHAVIOR", &behavior_path);
            std::env::set_var("BLINDRSS_CAPTCHA_ENDPOINT_BASE", &solver_base);
        }
        let mut cfg = crate::config::Config::default();
        cfg.set_path("captcha_solver_enabled", serde_json::json!(true));
        cfg.set_path("captcha_solver_api_key", serde_json::json!("fixture-key"));
        cfg.set_path("captcha_solver_provider", serde_json::json!("2captcha"));
        configure(&cfg, &dir.join("config.json"));
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut session = launch_session(
            &dir,
            &FetchOptions {
                proxy: String::new(),
            },
            deadline,
            None,
        )
        .map_err(|e| match e {
            LaunchError::Unavailable(m) => m,
            LaunchError::Failed(m) => m,
        })
        .unwrap();
        session
            .cdp
            .call("Page.navigate", json!({"url": url}), deadline)
            .unwrap();
        let settled = solve_with_service(
            &mut session.cdp,
            &cfg,
            url,
            false,
            Instant::now() + Duration::from_secs(60),
            None,
        )
        .unwrap()
        .expect("the solved page settles");
        assert!(settled.text.contains("Solved page content"));
        session.close();
        // The submission carried the FULL challenge data.
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        for field in [
            "data=cdata-1",
            "pagedata=pdata-1",
            "action=login",
            "userAgent=FakeBrowserUA%2F1.0",
            "sitekey=0x4AAAAAAAfixture",
        ] {
            assert!(bodies[0].contains(field), "missing {field}");
        }
        drop(bodies);
        // The token went back through the page callback.
        let log_text = std::fs::read_to_string(&log).unwrap();
        assert!(log_text.contains("TOKEN solved-token-xyz"), "{log_text}");
        // SAFETY: as above.
        unsafe {
            std::env::remove_var("BLINDRSS_BROWSER");
            std::env::remove_var("BLINDRSS_FAKE_BROWSER_BEHAVIOR");
            std::env::remove_var("BLINDRSS_CAPTCHA_ENDPOINT_BASE");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
