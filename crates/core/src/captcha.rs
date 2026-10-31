//! Paid Turnstile captcha solving (Python
//! `core/captcha_solver.py`).
//!
//! Both supported providers implement the same HTTP API
//! shape: submit to in.php, poll res.php until the token is
//! ready. In Python the solver is called from the warm-
//! browser stage (it needs the challenge's intercepted
//! sitekey/data parameters); the Rust port ships the solver
//! client itself, and the browser-stage integration stays
//! with the documented browser non-port in PARITY §7.
//! The API key is never logged.

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

pub const DEFAULT_PROVIDER: &str = "2captcha";
pub const DEFAULT_SOLVE_TIMEOUT_SECONDS: u64 = 120;

/// True when the settings make the solver usable.
pub fn solver_available(cfg: &crate::config::Config) -> bool {
    cfg.get_bool("captcha_solver_enabled", false)
        && !cfg.get_str("captcha_solver_api_key", "").trim().is_empty()
        && provider_endpoints(cfg.get_str("captcha_solver_provider", DEFAULT_PROVIDER)).is_some()
}

/// Solve a Turnstile challenge; returns the token or None on
/// any failure/timeout (Python's contract). Polls every 5s
/// after a 5s first delay, up to `timeout_s`.
pub fn solve_turnstile(
    api_key: &str,
    provider: &str,
    sitekey: &str,
    pageurl: &str,
    timeout_s: u64,
) -> Option<String> {
    let (in_url, res_url) = provider_endpoints(provider)?;
    if api_key.trim().is_empty() || sitekey.trim().is_empty() || pageurl.trim().is_empty() {
        return None;
    }
    let mut resp = crate::http::agent()
        .post(in_url)
        .send_form([
            ("key", api_key),
            ("method", "turnstile"),
            ("sitekey", sitekey),
            ("pageurl", pageurl),
            ("json", "1"),
        ])
        .ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let submitted: serde_json::Value = serde_json::from_str(&text).ok()?;
    if submitted.get("status").and_then(|v| v.as_i64()) != Some(1) {
        return None;
    }
    let task_id = submitted.get("request")?.as_str()?.to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s.max(15));
    std::thread::sleep(std::time::Duration::from_secs(5));
    loop {
        if std::time::Instant::now() >= deadline {
            return None;
        }
        let mut resp = crate::http::agent()
            .get(&format!(
                "{res_url}?key={api_key}&action=get&id={task_id}&json=1"
            ))
            .call()
            .ok()?;
        let text = resp.body_mut().read_to_string().ok()?;
        let polled: serde_json::Value = serde_json::from_str(&text).ok()?;
        if polled.get("status").and_then(|v| v.as_i64()) == Some(1) {
            return polled
                .get("request")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
        let status = polled.get("request").and_then(|v| v.as_str()).unwrap_or("");
        if status != "CAPCHA_NOT_READY" {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}
