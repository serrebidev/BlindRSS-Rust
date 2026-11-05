//! Paid Turnstile captcha solving (Python
//! `core/captcha_solver.py`).
//!
//! Both supported providers implement the same HTTP API
//! shape: submit to in.php, poll res.php until the token is
//! ready. The solver is called from the browser stage's
//! tier-4 escalation (Audit 8 B02): the challenge parameters
//! a page's `turnstile.render` call carries — sitekey,
//! pageurl, cData, chlPageData, action and the page's own
//! User-Agent — travel with the submission, and the solved
//! token is delivered back through the widget's stored
//! callback by the browser stage.
//!
//! Authorization is structural: `solve_turnstile` returns
//! None before any request is built when the solver is
//! disabled, has no API key, or names an unknown provider.
//! The API key, the intercepted challenge parameters and
//! the solved token are never logged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// (submit URL, result URL) per provider key.
pub fn provider_endpoints(provider: &str) -> Option<(&'static str, &'static str)> {
    match provider.trim().to_lowercase().as_str() {
        "2captcha" => Some((
            "https://2captcha.com/in.php",
            "https://2captcha.com/res.php",
        )),
        "solvecaptcha" => Some((
            "https://api.solvecaptcha.com/in.php",
            "https://api.solvecaptcha.com/res.php",
        )),
        _ => None,
    }
}

/// Endpoint override for tests and self-hosted solver
/// proxies (`BLINDRSS_CAPTCHA_ENDPOINT_BASE`, the house
/// `BLINDRSS_*` seam pattern): `{base}/in.php` and
/// `{base}/res.php` replace the provider hosts.
fn endpoints_for(provider: &str) -> Option<(String, String)> {
    if let Ok(base) = std::env::var("BLINDRSS_CAPTCHA_ENDPOINT_BASE") {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() && provider_endpoints(provider).is_some() {
            return Some((format!("{base}/in.php"), format!("{base}/res.php")));
        }
    }
    provider_endpoints(provider).map(|(a, b)| (a.to_string(), b.to_string()))
}

pub const DEFAULT_PROVIDER: &str = "2captcha";
pub const DEFAULT_SOLVE_TIMEOUT_SECONDS: u64 = 120;

/// True when the settings make the solver usable.
pub fn solver_available(cfg: &crate::config::Config) -> bool {
    cfg.get_bool("captcha_solver_enabled", false)
        && !cfg.get_str("captcha_solver_api_key", "").trim().is_empty()
        && provider_endpoints(cfg.get_str("captcha_solver_provider", DEFAULT_PROVIDER)).is_some()
}

/// The challenge parameters intercepted from a page's
/// `turnstile.render` call (Python `solve_turnstile`'s
/// keyword arguments). The optional fields are submitted
/// only when non-empty, exactly like Python.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChallengeParams {
    pub sitekey: String,
    pub pageurl: String,
    pub data: Option<String>,
    pub pagedata: Option<String>,
    pub action: Option<String>,
    pub user_agent: Option<String>,
}

/// Sleep in 0.25 s slices so cancellation lands promptly
/// (Python `_sleep_or_cancel`).
fn sleep_or_cancel(seconds: f64, cancel: Option<&AtomicBool>) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

/// Poll cadence: Python's solver waits 5 s before the
/// first poll, then polls every 5 s. Production always
/// uses the Python cadence; the unit tests pass a short
/// one so the suite does not pay real sleeps.
pub struct PollCadence {
    pub first_delay_s: f64,
    pub interval_s: f64,
}

pub const PYTHON_CADENCE: PollCadence = PollCadence {
    first_delay_s: 5.0,
    interval_s: 5.0,
};

/// True when `url`'s host is loopback (localhost,
/// 127.0.0.0/8, ::1). Used to keep local endpoints off
/// environment proxies (see solve_at).
fn is_loopback_url(url: &str) -> bool {
    let authority = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// The submit/poll core against explicit endpoints (the
/// unit tests drive it against a loopback mock; production
/// goes through `solve_turnstile`). Python's contract: a
/// poll TRANSPORT error is logged and polling continues
/// until the deadline; a non-ready status other than
/// CAPCHA_NOT_READY, an empty token, or cancellation ends
/// the solve with None.
#[allow(clippy::too_many_arguments)]
fn solve_at(
    in_url: &str,
    res_url: &str,
    api_key: &str,
    params: &ChallengeParams,
    timeout_s: f64,
    cancel: Option<&AtomicBool>,
    cadence: &PollCadence,
) -> Option<String> {
    if api_key.trim().is_empty()
        || params.sitekey.trim().is_empty()
        || params.pageurl.trim().is_empty()
    {
        return None;
    }
    // Python's solver calls carry their own 30 s HTTP
    // timeout and read error bodies as JSON, so the agent
    // does not turn statuses into transport errors.
    let mut agent_config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(crate::http::user_agent())
        .http_status_as_error(false);
    // Loopback endpoints are local services (a self-hosted
    // solver, or the test fixture): an environment proxy
    // must never sit between us and them — ureq's default
    // config reads one from the environment, and whether
    // NO_PROXY covers loopback depends on the machine (CI
    // runners differ), which made submits intermittently
    // take seconds-to-timeout through a proxy that has no
    // business relaying 127.0.0.1. Remote solver endpoints
    // keep the environment proxy, as Python's requests
    // calls do.
    if is_loopback_url(in_url) && is_loopback_url(res_url) {
        agent_config = agent_config.proxy(None);
    }
    let agent: ureq::Agent = agent_config.build().into();
    let mut form: Vec<(&str, &str)> = vec![
        ("key", api_key),
        ("method", "turnstile"),
        ("sitekey", params.sitekey.as_str()),
        ("pageurl", params.pageurl.as_str()),
        ("json", "1"),
    ];
    if let Some(data) = params.data.as_deref().filter(|s| !s.is_empty()) {
        form.push(("data", data));
    }
    if let Some(pagedata) = params.pagedata.as_deref().filter(|s| !s.is_empty()) {
        form.push(("pagedata", pagedata));
    }
    if let Some(action) = params.action.as_deref().filter(|s| !s.is_empty()) {
        form.push(("action", action));
    }
    if let Some(ua) = params.user_agent.as_deref().filter(|s| !s.is_empty()) {
        form.push(("userAgent", ua));
    }
    let mut resp = agent.post(in_url).send_form(form).ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let submitted: serde_json::Value = serde_json::from_str(&text).ok()?;
    if submitted.get("status").and_then(|v| v.as_i64()) != Some(1) {
        return None;
    }
    let task_id = submitted.get("request")?.as_str()?.to_string();
    // Python: deadline is max(15, timeout_s); the first
    // poll happens after the cadence's first delay, then
    // every interval.
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_s.max(15.0));
    if !sleep_or_cancel(cadence.first_delay_s, cancel) {
        return None;
    }
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) || Instant::now() >= deadline {
            return None;
        }
        let polled = (|| -> Option<serde_json::Value> {
            let mut resp = agent
                .get(&format!(
                    "{res_url}?key={api_key}&action=get&id={task_id}&json=1"
                ))
                .call()
                .ok()?;
            let text = resp.body_mut().read_to_string().ok()?;
            serde_json::from_str(&text).ok()
        })();
        // A transport failure while polling is not the
        // answer: Python logs it and keeps polling until the
        // deadline (the previous port returned None here).
        if let Some(polled) = polled {
            if polled.get("status").and_then(|v| v.as_i64()) == Some(1) {
                let token = polled
                    .get("request")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return if token.is_empty() { None } else { Some(token) };
            }
            let status = polled.get("request").and_then(|v| v.as_str()).unwrap_or("");
            if status != "CAPCHA_NOT_READY" {
                return None;
            }
        }
        if !sleep_or_cancel(cadence.interval_s, cancel) {
            return None;
        }
    }
}

/// Solve a Turnstile challenge through the configured paid
/// provider; returns the token or None on any failure,
/// timeout or cancellation (Python's contract). No request
/// is made at all when the solver is not authorized
/// (disabled, no API key, unknown provider).
pub fn solve_turnstile(
    cfg: &crate::config::Config,
    params: &ChallengeParams,
    timeout_s: f64,
    cancel: Option<&AtomicBool>,
) -> Option<String> {
    if !solver_available(cfg) {
        return None;
    }
    let provider = cfg.get_str("captcha_solver_provider", DEFAULT_PROVIDER);
    let (in_url, res_url) = endpoints_for(provider)?;
    let api_key = cfg.get_str("captcha_solver_api_key", "");
    solve_at(
        &in_url,
        &res_url,
        api_key,
        params,
        timeout_s,
        cancel,
        &PYTHON_CADENCE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    type Shared<T> = std::sync::Arc<T>;

    /// Loopback mock of a solver provider: records every
    /// request, answers from a script.
    fn mock_solver(
        script: Vec<&'static str>,
    ) -> (String, Shared<AtomicUsize>, Shared<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Shared::new(AtomicUsize::new(0));
        let bodies = Shared::new(Mutex::new(Vec::new()));
        let hits2 = hits.clone();
        let bodies2 = bodies.clone();
        let script: Vec<String> = script.iter().map(|s| s.to_string()).collect();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let index = hits2.fetch_add(1, Ordering::SeqCst);
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                // Read headers, then the content-length body.
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
                bodies2
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&raw).to_string());
                // One scripted response can be a deliberate
                // connection drop (transport-error retry).
                let body = script.get(index).cloned().unwrap_or_default();
                if body == "DROP" {
                    continue;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{address}"), hits, bodies)
    }

    const FAST: PollCadence = PollCadence {
        first_delay_s: 0.05,
        interval_s: 0.05,
    };

    fn params() -> ChallengeParams {
        ChallengeParams {
            sitekey: "0x4AAAAAAAtest".to_string(),
            pageurl: "https://example.test/story".to_string(),
            data: Some("cdata-value".to_string()),
            pagedata: Some("pagedata-value".to_string()),
            action: Some("login".to_string()),
            user_agent: Some("FixtureUA/1.0".to_string()),
        }
    }

    #[test]
    fn submission_carries_the_full_challenge_parameters() {
        let (base, hits, bodies) = mock_solver(vec![
            r#"{"status":1,"request":"task-42"}"#,
            r#"{"status":0,"request":"CAPCHA_NOT_READY"}"#,
            r#"{"status":1,"request":"token-abc"}"#,
        ]);
        let token = solve_at(
            &format!("{base}/in.php"),
            &format!("{base}/res.php"),
            "secret-key",
            &params(),
            30.0,
            None,
            &FAST,
        );
        assert_eq!(token.as_deref(), Some("token-abc"));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        let bodies = bodies.lock().unwrap();
        let submit = &bodies[0];
        for field in [
            "key=secret-key",
            "method=turnstile",
            "sitekey=0x4AAAAAAAtest",
            "pageurl=https%3A%2F%2Fexample.test%2Fstory",
            "data=cdata-value",
            "pagedata=pagedata-value",
            "action=login",
            "userAgent=FixtureUA%2F1.0",
            "json=1",
        ] {
            assert!(submit.contains(field), "missing {field} in {submit}");
        }
        assert!(bodies[1].contains("action=get&id=task-42&json=1"));
    }

    #[test]
    fn empty_optional_parameters_are_not_submitted() {
        let (base, _hits, bodies) = mock_solver(vec![
            r#"{"status":1,"request":"task-1"}"#,
            r#"{"status":1,"request":"tok"}"#,
        ]);
        let bare = ChallengeParams {
            sitekey: "k".to_string(),
            pageurl: "https://example.test/".to_string(),
            ..Default::default()
        };
        let token = solve_at(
            &format!("{base}/in.php"),
            &format!("{base}/res.php"),
            "key",
            &bare,
            30.0,
            None,
            &FAST,
        );
        assert_eq!(token.as_deref(), Some("tok"));
        let submit = bodies.lock().unwrap()[0].clone();
        assert!(!submit.contains("pagedata="));
        assert!(!submit.contains("userAgent="));
        assert!(!submit.contains("&data="));
        assert!(!submit.contains("action=") || submit.contains("action=get"));
    }

    #[test]
    fn poll_transport_errors_are_retried_until_the_deadline() {
        let (base, hits, _bodies) = mock_solver(vec![
            r#"{"status":1,"request":"task-7"}"#,
            "DROP",
            r#"{"status":1,"request":"tok-after-drop"}"#,
        ]);
        let token = solve_at(
            &format!("{base}/in.php"),
            &format!("{base}/res.php"),
            "key",
            &params(),
            40.0,
            None,
            &FAST,
        );
        assert_eq!(token.as_deref(), Some("tok-after-drop"));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn solver_errors_and_empty_tokens_fail_closed() {
        let (base, _h, _b) = mock_solver(vec![r#"{"status":0,"request":"ERROR_WRONG_USER_KEY"}"#]);
        assert!(
            solve_at(
                &format!("{base}/in.php"),
                &format!("{base}/res.php"),
                "key",
                &params(),
                20.0,
                None,
                &FAST
            )
            .is_none()
        );
        let (base, _h, _b) = mock_solver(vec![
            r#"{"status":1,"request":"task-9"}"#,
            r#"{"status":0,"request":"ERROR_CAPTCHA_UNSOLVABLE"}"#,
        ]);
        assert!(
            solve_at(
                &format!("{base}/in.php"),
                &format!("{base}/res.php"),
                "key",
                &params(),
                20.0,
                None,
                &FAST
            )
            .is_none()
        );
        let (base, _h, _b) = mock_solver(vec![
            r#"{"status":1,"request":"task-10"}"#,
            r#"{"status":1,"request":""}"#,
        ]);
        assert!(
            solve_at(
                &format!("{base}/in.php"),
                &format!("{base}/res.php"),
                "key",
                &params(),
                20.0,
                None,
                &FAST
            )
            .is_none()
        );
    }

    #[test]
    fn loopback_endpoints_are_classified_for_proxy_bypass() {
        assert!(is_loopback_url("http://127.0.0.1:8080/in.php"));
        assert!(is_loopback_url("http://localhost:9000/res.php"));
        assert!(is_loopback_url("http://[::1]:8080/in.php"));
        assert!(is_loopback_url("http://127.5.0.9/in.php"));
        assert!(!is_loopback_url("https://2captcha.com/in.php"));
        assert!(!is_loopback_url("http://192.168.1.20:8080/in.php"));
        assert!(!is_loopback_url("http://user@127.0.0.1.evil.test/in.php"));
    }

    #[test]
    fn cancellation_stops_the_solve() {
        // Pre-set cancel: the submission still happens
        // (Python checks cancellation in its sleeps), then
        // the first poll delay aborts and no poll request
        // is ever sent. The proof is structural — exactly
        // one hit (the submit) and no token. A wall-clock
        // bound used to stand here; it failed three times
        // on loaded CI runners (suite times 16-30 s vs
        // ~6 s locally) while the structural facts held,
        // because a socket round-trip's latency under load
        // is not the property under test. If cancellation
        // were ignored, the first poll would fire after
        // the 0.05 s FAST delay and hits would be 2.
        let (base, hits, _b) = mock_solver(vec![
            r#"{"status":1,"request":"task-11"}"#,
            r#"{"status":1,"request":"tok"}"#,
        ]);
        let cancel = AtomicBool::new(true);
        let token = solve_at(
            &format!("{base}/in.php"),
            &format!("{base}/res.php"),
            "key",
            &params(),
            60.0,
            Some(&cancel),
            &FAST,
        );
        assert!(token.is_none());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unauthorized_configs_never_reach_the_solver() {
        // The gate is structural: solve_turnstile checks
        // solver_available before endpoints are resolved or
        // any request is built, so these return None without
        // network access of any kind.
        let mut cfg = crate::config::Config::default();
        assert!(solve_turnstile(&cfg, &params(), 5.0, None).is_none());
        cfg.set_path("captcha_solver_api_key", serde_json::json!("some-key"));
        // Key present but the solver is disabled.
        assert!(solve_turnstile(&cfg, &params(), 5.0, None).is_none());
        cfg.set_path("captcha_solver_enabled", serde_json::json!(true));
        cfg.set_path(
            "captcha_solver_provider",
            serde_json::json!("no-such-provider"),
        );
        assert!(solve_turnstile(&cfg, &params(), 5.0, None).is_none());
        assert!(!solver_available(&cfg));
    }
}
