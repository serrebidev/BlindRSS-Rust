//! Fetch identity (Python `core/user_agents.py`).
//!
//! A stale User-Agent is a bot signal on its own, so Python's
//! "auto" mode claims a browser actually installed on the
//! machine at its real version. The Rust port resolves "auto"
//! (and "installed:<browser>") to the platform-matching
//! built-in preset; detecting the installed browser's true
//! version is not ported. Preset keys are persisted in config
//! and must never change.

/// (key, label, user agent) built-in presets, Python's
/// _PRESET_SPECS keys in order.
pub const PRESETS: [(&str, &str, &str); 8] = [
    (
        "chrome_windows",
        "Google Chrome (Windows)",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
    ),
    (
        "chrome_macos",
        "Google Chrome (macOS)",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
    ),
    (
        "chrome_linux",
        "Google Chrome (Linux)",
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
    ),
    (
        "edge_windows",
        "Microsoft Edge (Windows)",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
    ),
    (
        "edge_macos",
        "Microsoft Edge (macOS)",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
    ),
    (
        "firefox_windows",
        "Mozilla Firefox (Windows)",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0",
    ),
    (
        "firefox_macos",
        "Mozilla Firefox (macOS)",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0",
    ),
    (
        "firefox_linux",
        "Mozilla Firefox (Linux)",
        "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0",
    ),
];

/// Python's fallback preset when detection/choice fails.
pub const FALLBACK_PRESET: &str = "chrome_windows";

fn preset_ua(key: &str) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, _, ua)| *ua)
}

fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// The preset key "auto" resolves to on this platform.
pub fn auto_preset_key() -> String {
    format!("chrome_{}", platform())
}

/// Resolve a config `user_agent_mode` (+ `user_agent_custom`)
/// to the User-Agent string to send.
pub fn resolve(mode: &str, custom: &str) -> String {
    let mode = mode.trim();
    if mode.eq_ignore_ascii_case("custom") {
        let custom = custom.trim();
        if !custom.is_empty() {
            return custom.to_string();
        }
    }
    if let Some(engine) = mode.strip_prefix("installed:") {
        let engine = engine.trim().to_lowercase();
        let key = format!("{engine}_{}", platform());
        if let Some(ua) = preset_ua(&key) {
            return ua.to_string();
        }
    }
    if mode.is_empty() || mode.eq_ignore_ascii_case("auto") {
        return preset_ua(&auto_preset_key())
            .unwrap_or_else(|| preset_ua(FALLBACK_PRESET).unwrap())
            .to_string();
    }
    preset_ua(mode)
        .unwrap_or_else(|| preset_ua(FALLBACK_PRESET).unwrap())
        .to_string()
}

/// Resolve from the app config (user_agent_mode /
/// user_agent_custom keys).
pub fn resolve_from_config(cfg: &crate::config::Config) -> String {
    resolve(
        cfg.get_str("user_agent_mode", "auto"),
        cfg.get_str("user_agent_custom", ""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_and_modes() {
        assert!(resolve("chrome_linux", "").contains("Chrome/"));
        assert!(resolve("firefox_windows", "").contains("Firefox/"));
        assert_eq!(resolve("custom", "MyAgent/1.0"), "MyAgent/1.0");
        // Empty custom falls back instead of sending nothing.
        assert!(resolve("custom", "").contains("Chrome/"));
        assert!(resolve("auto", "").contains("Chrome/"));
        assert!(resolve("installed:firefox", "").contains("Firefox/"));
        assert!(resolve("nonsense", "").contains("Chrome/"));
    }
}
