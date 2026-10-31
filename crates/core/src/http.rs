//! Shared HTTP client state: one ureq agent carrying the
//! configured User-Agent (core/user_agents.rs) and the feed
//! timeout (`feed_timeout_seconds`, Python default 15).
//!
//! The app initializes this at startup and after Settings
//! changes; before that, the defaults apply (auto UA, 15s).
//! Per-host connection caps (`per_host_max_connections`) are
//! enforced by the refresh scheduler's worker pool shape, not
//! here — ureq has no per-host pool knob.

use std::sync::Mutex;
use std::time::Duration;

struct HttpState {
    agent: ureq::Agent,
    user_agent: String,
    timeout_secs: u64,
    ignore_feed_ssl_errors: bool,
}

static STATE: Mutex<Option<HttpState>> = Mutex::new(None);

fn build(user_agent: String, timeout_secs: u64, ignore_feed_ssl_errors: bool) -> HttpState {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs.max(1))))
        .user_agent(user_agent.clone())
        .build();
    HttpState {
        agent: ureq::Agent::new_with_config(config),
        user_agent,
        timeout_secs,
        ignore_feed_ssl_errors,
    }
}

fn state() -> std::sync::MutexGuard<'static, Option<HttpState>> {
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        let ua = crate::user_agents::resolve("auto", "");
        *guard = Some(build(ua, 15, true));
    }
    guard
}

/// (Re)configure the shared agent from the app config:
/// user_agent_mode / user_agent_custom / feed_timeout_seconds /
/// ignore_feed_ssl_errors.
pub fn init_from_config(cfg: &crate::config::Config) {
    let ua = crate::user_agents::resolve_from_config(cfg);
    let timeout = cfg.get_i64("feed_timeout_seconds", 15).max(1) as u64;
    // Python default is True (issue #42): broken-cert feeds still
    // load via a one-time unverified retry.
    let ignore_ssl = cfg.get_bool("ignore_feed_ssl_errors", true);
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some(build(ua, timeout, ignore_ssl));
}

/// The shared agent (carries UA + timeout on every request).
pub fn agent() -> ureq::Agent {
    state().as_ref().unwrap().agent.clone()
}

/// The User-Agent the shared agent sends, for call sites that
/// build their own requests (yt-dlp --user-agent, raw headers).
pub fn user_agent() -> String {
    state().as_ref().unwrap().user_agent.clone()
}

/// The configured global timeout in seconds (for call sites
/// that must build a sibling agent, e.g. status-aware calls).
pub fn timeout_secs() -> u64 {
    state().as_ref().unwrap().timeout_secs
}

/// An agent that does NOT turn HTTP error statuses into
/// transport errors, for callers that need the status and
/// headers themselves (rate-limit backoff reads Retry-After).
pub fn status_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs().max(1))))
        .user_agent(user_agent())
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

/// Whether the feed SSL fallback is enabled (config
/// `ignore_feed_ssl_errors`, Python default true).
pub fn ssl_fallback_allowed() -> bool {
    state().as_ref().unwrap().ignore_feed_ssl_errors
}

/// An agent with certificate verification disabled, for the
/// feed-fetch retry only (Python issue #42 semantics: feeds
/// behind broken certificates still load; every other network
/// operation keeps full verification).
pub fn insecure_agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .disable_verification(true)
        .build();
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs().max(1))))
        .user_agent(user_agent())
        .tls_config(tls)
        .build();
    ureq::Agent::new_with_config(config)
}

/// True for certificate-validation failures (Python
/// `_looks_like_ssl_certificate_error`): Python's text markers
/// plus the rustls/ureq phrasings for the same failures.
pub fn looks_like_ssl_cert_error(message: &str) -> bool {
    let text = message.to_lowercase();
    const MARKERS: &[&str] = &[
        // Python's marker list (requests / curl_cffi wording).
        "ssl certificate problem",
        "certificate verify failed",
        "certificate_verify_failed",
        "unable to get local issuer certificate",
        "self-signed certificate",
        "self signed certificate",
        "certificate has expired",
        "curl: (60)",
        "curl: (77)",
        // rustls / ureq wording for the same failures.
        "unknownissuer",
        "certexpired",
        "invalid peer certificate",
        "notvalidforname",
        "certificate name mismatch",
    ];
    MARKERS.iter().any(|m| text.contains(m))
}

/// Run a request with one rate-limit retry: when the response
/// is 429, wait Retry-After (capped at 30s) and try once more.
/// The closure performs the request on the status agent and
/// returns the response for inspection.
pub fn with_rate_limit_backoff<T>(
    mut attempt: impl FnMut() -> Result<(u16, Option<String>, T), String>,
) -> Result<T, String> {
    let (status, retry_after, value) = attempt()?;
    if status != 429 {
        return Ok(value);
    }
    let wait = retry_after
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(5)
        .min(30);
    std::thread::sleep(Duration::from_secs(wait));
    let (status2, _, value2) = attempt()?;
    if status2 == 429 {
        return Err("rate limited by the provider (429)".to_string());
    }
    Ok(value2)
}
