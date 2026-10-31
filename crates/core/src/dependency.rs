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
