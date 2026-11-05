//! Fetch identity (Python `core/user_agents.py`).
//!
//! A stale User-Agent is a bot signal on its own, so Python's
//! "auto" mode claims a browser actually installed on the
//! machine at its real version. Detection is cached for the
//! process. Preset keys are persisted and must never change.

use std::sync::OnceLock;

#[derive(Clone, Debug)]
struct InstalledBrowser {
    key: String,
    label: String,
    engine: &'static str,
    major: u32,
}

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
    resolve_with_installed(mode, custom, installed())
}

fn resolve_with_installed(mode: &str, custom: &str, browsers: &[InstalledBrowser]) -> String {
    let mode = mode.trim();
    if mode.eq_ignore_ascii_case("custom") {
        let custom = custom.trim();
        if !custom.is_empty() {
            return custom.to_string();
        }
    }
    if let Some(key) = mode.strip_prefix("installed:")
        && let Some(browser) = browsers.iter().find(|b| b.key == key)
    {
        return build_ua(browser.engine, platform(), browser.major);
    }
    if let Some(ua) = preset_ua(mode) {
        return ua.to_string();
    }
    if let Some(browser) = browsers
        .iter()
        .min_by_key(|b| (b.engine == "firefox", std::cmp::Reverse(b.major)))
    {
        return build_ua(browser.engine, platform(), browser.major);
    }
    preset_ua(&auto_preset_key())
        .unwrap_or_else(|| preset_ua(FALLBACK_PRESET).unwrap())
        .to_string()
}

fn build_ua(engine: &str, platform: &str, major: u32) -> String {
    let system = match (engine, platform) {
        ("firefox", "macos") => "Macintosh; Intel Mac OS X 10.15",
        (_, "macos") => "Macintosh; Intel Mac OS X 10_15_7",
        (_, "linux") => "X11; Linux x86_64",
        _ => "Windows NT 10.0; Win64; x64",
    };
    if engine == "firefox" {
        return format!("Mozilla/5.0 ({system}; rv:{major}.0) Gecko/20100101 Firefox/{major}.0");
    }
    let mut ua = format!(
        "Mozilla/5.0 ({system}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36"
    );
    if engine == "edge" {
        ua.push_str(&format!(" Edg/{major}.0.0.0"));
    }
    ua
}

fn installed() -> &'static [InstalledBrowser] {
    static BROWSERS: OnceLock<Vec<InstalledBrowser>> = OnceLock::new();
    BROWSERS.get_or_init(detect_installed)
}

/// The User-Agent of an installed browser family ("chrome",
/// "edge", "brave") at its detected version, for pinning beside
/// cookies harvested from that family's profiles (Python's
/// `installed:<family>` resolution in `chromium_profile_user_agent`).
/// Brave has no preset of its own and resolves through Chrome's,
/// as in Python. Falls back to the platform preset when the
/// family is not detected.
pub fn installed_family_ua(family: &str) -> String {
    if let Some(browser) = installed().iter().find(|b| b.key == family) {
        return build_ua(browser.engine, platform(), browser.major);
    }
    if family == "edge"
        && let Some(ua) = preset_ua(&format!("edge_{}", platform()))
    {
        return ua.to_string();
    }
    preset_ua(&auto_preset_key())
        .unwrap_or_else(|| preset_ua(FALLBACK_PRESET).unwrap())
        .to_string()
}

/// Stable config keys and labels for detected browsers in Settings.
pub fn installed_choices() -> Vec<(String, String)> {
    installed()
        .iter()
        .map(|b| {
            (
                format!("installed:{}", b.key),
                format!("{} {} (installed)", b.label, b.major),
            )
        })
        .collect()
}

#[cfg(any(target_os = "windows", test))]
fn chromium_major(path: &std::path::Path) -> Option<u32> {
    std::fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name();
            let parts: Vec<_> = name.to_str()?.split('.').collect();
            if parts.len() != 4
                || parts
                    .iter()
                    .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            parts[0].parse::<u32>().ok().filter(|v| *v > 0)
        })
        .max()
}

#[cfg(any(target_os = "windows", test))]
fn firefox_major(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path.join("application.ini"))
        .ok()?
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            if !key.trim().eq_ignore_ascii_case("version") {
                return None;
            }
            value
                .trim()
                .split('.')
                .next()?
                .parse::<u32>()
                .ok()
                .filter(|v| *v > 0)
        })
}

#[cfg(target_os = "windows")]
fn detect_installed() -> Vec<InstalledBrowser> {
    let roots: Vec<_> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .collect();
    let mut found = Vec::new();
    for (key, label, engine, channels) in [
        (
            "brave",
            "Brave",
            "chromium",
            [
                "BraveSoftware/Brave-Browser/Application",
                "BraveSoftware/Brave-Browser-Beta/Application",
                "BraveSoftware/Brave-Browser-Nightly/Application",
            ],
        ),
        (
            "chrome",
            "Google Chrome",
            "chromium",
            [
                "Google/Chrome/Application",
                "Google/Chrome Beta/Application",
                "Google/Chrome Dev/Application",
            ],
        ),
        (
            "edge",
            "Microsoft Edge",
            "edge",
            [
                "Microsoft/Edge/Application",
                "Microsoft/Edge Beta/Application",
                "Microsoft/Edge Dev/Application",
            ],
        ),
    ] {
        if let Some(major) = channels.iter().find_map(|channel| {
            roots
                .iter()
                .find_map(|root| chromium_major(&root.join(channel)))
        }) {
            found.push(InstalledBrowser {
                key: key.into(),
                label: label.into(),
                engine,
                major,
            });
        }
    }
    if let Some(major) = roots.iter().find_map(|root| {
        ["Mozilla Firefox", "Mozilla/Firefox"]
            .iter()
            .find_map(|rel| firefox_major(&root.join(rel)))
    }) {
        found.push(InstalledBrowser {
            key: "firefox".into(),
            label: "Mozilla Firefox".into(),
            engine: "firefox",
            major,
        });
    }
    found
}

#[cfg(not(target_os = "windows"))]
fn command_version(program: &str, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status
                    .success()
                    .then(|| child.wait_with_output().ok())
                    .flatten()
                    .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
            }
            Ok(None) if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn detect_installed() -> Vec<InstalledBrowser> {
    [
        ("brave", "Brave", "chromium", "Brave Browser.app"),
        ("chrome", "Google Chrome", "chromium", "Google Chrome.app"),
        ("edge", "Microsoft Edge", "edge", "Microsoft Edge.app"),
        ("firefox", "Mozilla Firefox", "firefox", "Firefox.app"),
    ]
    .into_iter()
    .filter_map(|(key, label, engine, app)| {
        let plist = format!("/Applications/{app}/Contents/Info.plist");
        let version = command_version(
            "/usr/bin/plutil",
            &[
                "-extract",
                "CFBundleShortVersionString",
                "raw",
                "-o",
                "-",
                &plist,
            ],
        )?;
        let major = version
            .trim()
            .split('.')
            .next()?
            .parse::<u32>()
            .ok()
            .filter(|v| *v > 0)?;
        Some(InstalledBrowser {
            key: key.into(),
            label: label.into(),
            engine,
            major,
        })
    })
    .collect()
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn detect_installed() -> Vec<InstalledBrowser> {
    let version = regex::Regex::new(r"(\d+)\.\d+").unwrap();
    [
        (
            "brave",
            "Brave",
            "chromium",
            &["brave-browser", "brave"][..],
        ),
        (
            "chrome",
            "Google Chrome",
            "chromium",
            &["google-chrome", "google-chrome-stable"][..],
        ),
        (
            "edge",
            "Microsoft Edge",
            "edge",
            &["microsoft-edge", "microsoft-edge-stable"][..],
        ),
        ("firefox", "Mozilla Firefox", "firefox", &["firefox"][..]),
    ]
    .into_iter()
    .filter_map(|(key, label, engine, commands)| {
        let major = commands.iter().find_map(|program| {
            let output = command_version(program, &["--version"])?;
            version
                .captures(&output)?
                .get(1)?
                .as_str()
                .parse::<u32>()
                .ok()
                .filter(|v| *v > 0)
        })?;
        Some(InstalledBrowser {
            key: key.into(),
            label: label.into(),
            engine,
            major,
        })
    })
    .collect()
}

/// Chromium client hints that agree with a resolved UA; Gecko sends none.
pub fn client_hints(ua: &str) -> Vec<(&'static str, String)> {
    if ua.contains("Firefox/") {
        return Vec::new();
    }
    let engine = if ua.contains("Edg/") {
        "Edg/"
    } else {
        "Chrome/"
    };
    let Some(major) = ua
        .split_once(engine)
        .and_then(|(_, version)| version.split('.').next())
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    else {
        return Vec::new();
    };
    let brand = if engine == "Edg/" {
        "Microsoft Edge"
    } else {
        "Google Chrome"
    };
    let platform = if ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "Windows"
    };
    vec![
        (
            "sec-ch-ua",
            format!(
                "\"Chromium\";v=\"{major}\", \"{brand}\";v=\"{major}\", \"Not-A.Brand\";v=\"99\""
            ),
        ),
        ("sec-ch-ua-mobile", "?0".into()),
        ("sec-ch-ua-platform", format!("\"{platform}\"")),
    ]
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
    fn actual_versions_and_auto_ranking() {
        let browsers = vec![
            InstalledBrowser {
                key: "firefox".into(),
                label: "Firefox".into(),
                engine: "firefox",
                major: 165,
            },
            InstalledBrowser {
                key: "chrome".into(),
                label: "Chrome".into(),
                engine: "chromium",
                major: 157,
            },
            InstalledBrowser {
                key: "edge".into(),
                label: "Edge".into(),
                engine: "edge",
                major: 158,
            },
        ];
        assert!(resolve_with_installed("auto", "", &browsers).contains("Edg/158.0.0.0"));
        assert!(
            resolve_with_installed("installed:firefox", "", &browsers).contains("Firefox/165.0")
        );
        assert!(
            resolve_with_installed("installed:removed", "", &browsers).contains("Edg/158.0.0.0")
        );
        assert!(resolve_with_installed("custom", "", &browsers).contains("Edg/158.0.0.0"));
        assert!(build_ua("edge", "linux", 157).contains("X11; Linux x86_64"));
    }

    #[test]
    fn installed_version_files_ignore_nonversions_and_pick_highest_major() {
        let dir = std::env::temp_dir().join(format!("blindrss-browser-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("157.0.1.0")).unwrap();
        std::fs::create_dir_all(dir.join("158.0.1.0")).unwrap();
        std::fs::create_dir_all(dir.join("999.0")).unwrap();
        std::fs::write(dir.join("999.0.1.0"), "not a directory").unwrap();
        assert_eq!(chromium_major(&dir), Some(158));
        std::fs::write(dir.join("application.ini"), "[App]\nVersion=165.0.1\n").unwrap();
        assert_eq!(firefox_major(&dir), Some(165));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn client_hints_match_engine_version_and_platform() {
        let chrome = client_hints(&build_ua("chromium", "macos", 157));
        assert_eq!(chrome[0].0, "sec-ch-ua");
        assert!(chrome[0].1.contains("Google Chrome\";v=\"157"));
        assert!(
            chrome
                .iter()
                .any(|(key, value)| *key == "sec-ch-ua-platform" && value == "\"macOS\"")
        );
        assert!(
            client_hints(&build_ua("edge", "windows", 158))[0]
                .1
                .contains("Microsoft Edge")
        );
        assert!(client_hints(&build_ua("firefox", "linux", 165)).is_empty());
        assert!(client_hints("MyCustomAgent").is_empty());
    }

    #[test]
    fn presets_and_modes() {
        assert!(resolve("chrome_linux", "").contains("Chrome/"));
        assert!(resolve("firefox_windows", "").contains("Firefox/"));
        assert_eq!(resolve("custom", "MyAgent/1.0"), "MyAgent/1.0");
        // Empty custom falls back instead of sending nothing.
        assert!(resolve("custom", "").contains("Chrome/"));
        assert!(resolve("auto", "").contains("Chrome/"));
        assert!(resolve("nonsense", "").contains("Chrome/"));
        // installed:<browser> resolves against the DETECTED set
        // (Python: detect_installed match, else preset, else
        // automatic). Driven with synthetic sets so the test does
        // not depend on which browsers the test machine has.
        let firefox = InstalledBrowser {
            key: "firefox".to_string(),
            label: "Firefox".to_string(),
            engine: "firefox",
            major: 140,
        };
        assert!(
            resolve_with_installed("installed:firefox", "", &[firefox]).contains("Firefox/140")
        );
        let fallback = resolve_with_installed("installed:firefox", "", &[]);
        assert!(fallback.contains("Mozilla/5.0") && !fallback.is_empty());
    }
}
