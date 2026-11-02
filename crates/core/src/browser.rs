//! Optional installed-Chromium retrieval, isolated from personal browser profiles.

use serde_json::{Value, json};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_SOURCE: usize = 30 * 1024 * 1024;
static ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn configure(_config: &crate::config::Config, config_path: &Path) {
    *ROOT.lock().unwrap_or_else(|p| p.into_inner()) = config_path.parent().map(Path::to_owned);
}

/// Sessions are scoped to each fetch and always closed before it returns.
pub fn shutdown() {}

pub struct BrowserPageResponse {
    pub html: String,
    pub url: String,
}

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

fn executable() -> Option<PathBuf> {
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
            paths.push(PathBuf::from(base).join("Microsoft/Edge/Application/msedge.exe"));
        }
    }
    paths.into_iter().find(|p| p.is_file())
}

struct Session {
    child: Child,
    profile: PathBuf,
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

struct Cdp {
    socket: tungstenite::WebSocket<TcpStream>,
    id: u64,
    deadline: Instant,
}
impl Cdp {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
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
            let remaining = self.deadline.saturating_duration_since(Instant::now());
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
}

fn save_cookies(root: &Path, cookies: &Value) -> Result<(), String> {
    let records: Vec<_> = cookies
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
        return Ok(());
    }
    let path = root.join("site_cookies.txt");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let merged = crate::chromium::merge_into_jar(&existing, &records, now);
    let temporary = root.join(format!(".browser-cookies-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, merged).map_err(|_| "Could not stage browser cookies")?;
    std::fs::rename(&temporary, &path).map_err(|_| "Could not save browser cookies".to_owned())
}

/// Blocking worker-only operation; never starts or reads a personal profile.
pub fn fetch_page(
    url: &str,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<BrowserPageResponse, String> {
    let target = url::Url::parse(url).map_err(|_| "Invalid browser URL")?;
    if !matches!(target.scheme(), "http" | "https") || target.host_str().is_none() {
        return Err("Browser URL must be HTTP or HTTPS".into());
    }
    if timeout.is_zero() || cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err("Browser retrieval timed out or cancelled".into());
    }
    let deadline = Instant::now() + timeout.min(Duration::from_secs(180));
    let root = ROOT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .ok_or("Browser is not configured")?;
    let executable = executable().ok_or("No installed Chromium browser found")?;
    let profile = root
        .join("feed_browser_runtime")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&profile).map_err(|_| "Could not create isolated browser profile")?;
    let mut command = Command::new(executable);
    command
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--remote-debugging-port=0",
            "--remote-debugging-address=127.0.0.1",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--user-agent={}", crate::http::user_agent()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command.spawn().map_err(|_| {
        let _ = std::fs::remove_dir_all(&profile);
        "Could not start Chromium"
    })?;
    let mut session = Session { child, profile };
    let active_port = session.profile.join("DevToolsActivePort");
    let port = loop {
        if Instant::now() >= deadline || cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err("Browser retrieval timed out or cancelled".into());
        }
        if let Ok(text) = std::fs::read_to_string(&active_port)
            && let Some(port) = text.lines().next().and_then(|s| s.parse::<u16>().ok())
        {
            break port;
        }
        if session
            .child
            .try_wait()
            .map_err(|_| "Browser process check failed")?
            .is_some()
        {
            return Err("Chromium exited before becoming ready".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(remaining))
        .build()
        .into();
    let body = agent
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .call()
        .map_err(|_| "Browser discovery failed")?
        .body_mut()
        .read_to_string()
        .map_err(|_| "Invalid browser discovery response")?;
    let tabs: Value =
        serde_json::from_str(&body).map_err(|_| "Invalid browser discovery response")?;
    let websocket = tabs
        .as_array()
        .and_then(|tabs| tabs.iter().find(|tab| tab["type"] == "page"))
        .and_then(|tab| tab["webSocketDebuggerUrl"].as_str())
        .ok_or("Browser page not available")?;
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let stream =
        TcpStream::connect_timeout(&address, remaining).map_err(|_| "Browser connection failed")?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| "Browser timeout setup failed")?;
    stream
        .set_write_timeout(Some(remaining))
        .map_err(|_| "Browser timeout setup failed")?;
    let (socket, _) =
        tungstenite::client(websocket, stream).map_err(|_| "Browser handshake failed")?;
    let mut cdp = Cdp {
        socket,
        id: 0,
        deadline,
    };
    cdp.call("Page.enable", json!({}))?;
    cdp.call("Page.navigate", json!({"url":url}))?;
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err("Browser retrieval cancelled".into());
        }
        let result = cdp.call("Runtime.evaluate", json!({"expression":"JSON.stringify({ready:document.readyState,html:document.documentElement.outerHTML,url:location.href})", "returnByValue":true}))?;
        let page: Value = serde_json::from_str(result["result"]["value"].as_str().unwrap_or(""))
            .map_err(|_| "Browser document unavailable")?;
        let html = page["html"].as_str().unwrap_or("");
        if html.len() > MAX_SOURCE {
            return Err("Browser document too large".into());
        }
        if page["ready"] == "complete"
            && page["url"].as_str().is_some_and(|v| v != "about:blank")
            && !is_challenge_page(html)
        {
            let final_url = page["url"].as_str().unwrap_or(url).to_owned();
            let cookies = cdp.call("Network.getCookies", json!({"urls":[final_url]}))?;
            save_cookies(&root, &cookies["cookies"])?;
            return Ok(BrowserPageResponse {
                html: html.to_owned(),
                url: final_url,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
