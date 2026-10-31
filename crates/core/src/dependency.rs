//! Media-tool lifecycle (Python `core/dependency_check.py`).
//!
//! Detect yt-dlp and ffmpeg, report their versions, and
//! self-update yt-dlp from its GitHub releases into
//! <data_dir>/bin (which find_ytdlp consults through the
//! BLINDRSS_YTDLP environment the app sets when a managed
//! copy exists).

/// One tool's detection result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolStatus {
    pub name: String,
    pub path: Option<String>,
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// User-configured tool paths (Python `core/dependency_check.py`'s
// _USER_TOOL_PATHS registry): explicit executable paths from the
// custom_ffmpeg_path / custom_ffprobe_path / custom_ytdlp_path
// config keys take the highest detection priority. A value may be
// the executable file itself or a directory containing it.
// ---------------------------------------------------------------------------

static USER_TOOL_PATHS: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

fn canon_tool_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .strip_suffix(".exe")
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.trim().to_lowercase())
}

/// Register user-chosen executable paths (Python
/// `set_user_tool_paths`). Keys are tool names ("ffmpeg",
/// "ffprobe", "yt-dlp"); empty values are ignored. Replaces any
/// previously registered overrides.
pub fn set_user_tool_paths(mapping: &std::collections::HashMap<String, String>) {
    let mut cleaned = std::collections::HashMap::new();
    for (key, value) in mapping {
        let tool = canon_tool_name(key);
        let path = value.trim().trim_matches('"').to_string();
        if !tool.is_empty() && !path.is_empty() {
            cleaned.insert(tool, path);
        }
    }
    let mut guard = USER_TOOL_PATHS.lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some(cleaned);
}

/// Populate the registry from the app config's custom_*_path keys.
pub fn init_tool_paths_from_config(cfg: &crate::config::Config) {
    let mut mapping = std::collections::HashMap::new();
    for (tool, key) in [
        ("ffmpeg", "custom_ffmpeg_path"),
        ("ffprobe", "custom_ffprobe_path"),
        ("yt-dlp", "custom_ytdlp_path"),
    ] {
        let value = cfg.get_str(key, "");
        if !value.trim().is_empty() {
            mapping.insert(tool.to_string(), value.to_string());
        }
    }
    set_user_tool_paths(&mapping);
}

/// Resolve the user-configured override for `tool` to a concrete
/// file, if any (Python `_user_tool_path`): the value itself when
/// it is a file, else the first candidate name found inside it
/// when it is a directory. A leading `~/` expands to the home dir.
pub fn user_tool_path(tool: &str, candidates: &[&str]) -> Option<std::path::PathBuf> {
    let raw = {
        let guard = USER_TOOL_PATHS.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .as_ref()
            .and_then(|m| m.get(&canon_tool_name(tool)).cloned())?
    };
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        std::env::home_dir()?.join(rest)
    } else {
        std::path::PathBuf::from(&raw)
    };
    if expanded.is_file() {
        return Some(expanded);
    }
    if expanded.is_dir() {
        for name in candidates {
            let full = expanded.join(name);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

/// Search PATH for the first of `names` that is a file.
pub fn find_on_path(names: &[&str]) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Locate ffprobe: user-configured path first, then PATH.
pub fn find_ffprobe() -> Option<std::path::PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["ffprobe.exe", "ffprobe"]
    } else {
        &["ffprobe"]
    };
    user_tool_path("ffprobe", names).or_else(|| find_on_path(names))
}

fn version_of(path: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

/// Detect the tools BlindRSS shells out to.
pub fn check_tools() -> Vec<ToolStatus> {
    let ytdlp = crate::media::find_ytdlp();
    let ffmpeg = crate::silence::find_ffmpeg();
    let ffprobe = find_ffprobe();
    vec![
        ToolStatus {
            name: "yt-dlp".to_string(),
            version: ytdlp.as_deref().and_then(version_of),
            path: ytdlp.map(|p| p.to_string_lossy().to_string()),
        },
        ToolStatus {
            name: "ffmpeg".to_string(),
            version: ffmpeg.as_deref().and_then(version_of),
            path: ffmpeg.map(|p| p.to_string_lossy().to_string()),
        },
        ToolStatus {
            name: "ffprobe".to_string(),
            version: ffprobe.as_deref().and_then(version_of),
            path: ffprobe.map(|p| p.to_string_lossy().to_string()),
        },
    ]
}

/// The managed yt-dlp location inside the data dir.
pub fn managed_ytdlp(data_dir: &std::path::Path) -> std::path::PathBuf {
    let name = if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };
    data_dir.join("bin").join(name)
}

/// Download the latest yt-dlp release binary for this
/// platform into <data_dir>/bin and make it executable.
/// Returns the installed path.
pub fn update_ytdlp(data_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let asset = if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else {
        "yt-dlp_linux"
    };
    let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}");
    let mut resp = crate::http::agent()
        .get(&url)
        .call()
        .map_err(|e| format!("yt-dlp download: {e}"))?;
    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| format!("yt-dlp download: {e}"))?;
    if bytes.len() < 100_000 {
        return Err("yt-dlp download looks truncated".to_string());
    }
    let dest = managed_ytdlp(data_dir);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("bin dir: {e}"))?;
    }
    let tmp = dest.with_extension("new");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, &dest).map_err(|e| format!("install: {e}"))?;
    Ok(dest)
}
