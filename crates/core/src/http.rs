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
}

static STATE: Mutex<Option<HttpState>> = Mutex::new(None);

fn build(user_agent: String, timeout_secs: u64) -> HttpState {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs.max(1))))
        .user_agent(user_agent.clone())
        .build();
    HttpState {
        agent: ureq::Agent::new_with_config(config),
        user_agent,
        timeout_secs,
    }
}

fn state() -> std::sync::MutexGuard<'static, Option<HttpState>> {
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        let ua = crate::user_agents::resolve("auto", "");
        *guard = Some(build(ua, 15));
    }
    guard
}

/// (Re)configure the shared agent from the app config:
/// user_agent_mode / user_agent_custom / feed_timeout_seconds.
pub fn init_from_config(cfg: &crate::config::Config) {
    let ua = crate::user_agents::resolve_from_config(cfg);
    let timeout = cfg.get_i64("feed_timeout_seconds", 15).max(1) as u64;
    let mut guard = STATE.lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some(build(ua, timeout));
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
