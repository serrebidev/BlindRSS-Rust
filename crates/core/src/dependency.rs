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

/// Locate ffprobe: user-configured path, then the managed
/// install's env pointer, then PATH.
pub fn find_ffprobe() -> Option<std::path::PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["ffprobe.exe", "ffprobe"]
    } else {
        &["ffprobe"]
    };
    if let Some(p) = user_tool_path("ffprobe", names) {
        return Some(p);
    }
    if let Ok(p) = std::env::var("BLINDRSS_FFPROBE")
        && !p.is_empty()
    {
        return Some(std::path::PathBuf::from(p));
    }
    find_on_path(names)
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

// ---------------------------------------------------------------------------
// Managed ffmpeg/ffprobe install (Python `install_media_tools` +
// `_install_ffmpeg_fallback`). The pair ships together in every
// distribution channel, so one install resolves both. Windows:
// winget (Python's first choice) with the gyan.dev essentials zip
// as fallback, extracted under <data_dir>/ffmpeg — the same
// layout Python uses under %LOCALAPPDATA%\BlindRSS. macOS:
// Homebrew, as Python. Linux: a static build (an extension —
// Python has no automated Linux path).
// ---------------------------------------------------------------------------

fn exe_name(tool: &str) -> String {
    if cfg!(windows) {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    }
}

fn dir_has_pair(dir: &std::path::Path) -> bool {
    dir.join(exe_name("ffmpeg")).is_file() && dir.join(exe_name("ffprobe")).is_file()
}

/// Like dir_has_pair but suffix-agnostic: the Windows zip's
/// .exe names also count. Used only when locating a fresh
/// extraction (the zip route is Windows-only in production);
/// managed-install detection stays platform-exact so a copied
/// Windows tree never shadows working tools elsewhere.
fn dir_has_pair_loose(dir: &std::path::Path) -> bool {
    ["ffmpeg", "ffmpeg.exe"]
        .iter()
        .any(|n| dir.join(n).is_file())
        && ["ffprobe", "ffprobe.exe"]
            .iter()
            .any(|n| dir.join(n).is_file())
}

/// A directory holding BOTH ffmpeg and ffprobe executables,
/// searching `root` and up to three levels below it (the gyan
/// zip nests them under <version>/bin, the static Linux build
/// keeps them at the top level).
fn find_bin_dir_matching(
    root: &std::path::Path,
    complete: &dyn Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    fn visit(
        dir: &std::path::Path,
        depth: usize,
        complete: &dyn Fn(&std::path::Path) -> bool,
    ) -> Option<std::path::PathBuf> {
        if complete(dir) {
            return Some(dir.to_path_buf());
        }
        if depth == 0 {
            return None;
        }
        let mut subs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subs.sort();
        for sub in subs {
            if let Some(found) = visit(&sub, depth - 1, complete) {
                return Some(found);
            }
        }
        None
    }
    visit(root, 3, complete)
}

fn find_bin_dir(root: &std::path::Path) -> Option<std::path::PathBuf> {
    find_bin_dir_matching(root, &dir_has_pair)
}

/// The bin directory of the managed ffmpeg install inside the
/// data dir, when a previous install landed there.
pub fn managed_ffmpeg_bin_dir(data_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    find_bin_dir(&data_dir.join("ffmpeg"))
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = crate::http::agent()
        .get(url)
        .call()
        .map_err(|e| format!("download: {e}"))?;
    resp.body_mut()
        .read_to_vec()
        .map_err(|e| format!("download: {e}"))
}

/// Extract an ffmpeg zip archive into `dest` (wiped first) and
/// return the directory holding ffmpeg + ffprobe.
pub fn extract_ffmpeg_zip(
    bytes: &[u8],
    dest: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| format!("clean extract dir: {e}"))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("extract dir: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("ffmpeg zip: {e}"))?;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("ffmpeg zip: {e}"))?;
        let Some(rel) = file.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("extract: {e}"))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("extract: {e}"))?;
        }
        let mut f = std::fs::File::create(&out).map_err(|e| format!("extract: {e}"))?;
        std::io::copy(&mut file, &mut f).map_err(|e| format!("extract: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let name = out.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "ffmpeg" || name == "ffprobe" {
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    find_bin_dir_matching(dest, &dir_has_pair_loose)
        .ok_or_else(|| "ffmpeg/ffprobe not found after extraction".to_string())
}

/// The gyan.dev essentials download (Python
/// `_install_ffmpeg_fallback`): fetch, extract under
/// <data_dir>/ffmpeg/extract, return the bin directory.
#[cfg(target_os = "windows")]
fn install_ffmpeg_from_zip(data_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
    let bytes = download_bytes(url)?;
    if bytes.len() < 1_000_000 {
        return Err("ffmpeg download looks truncated".to_string());
    }
    let base = data_dir.join("ffmpeg");
    std::fs::create_dir_all(&base).map_err(|e| format!("ffmpeg dir: {e}"))?;
    std::fs::write(base.join("ffmpeg.zip"), &bytes).map_err(|e| format!("write zip: {e}"))?;
    extract_ffmpeg_zip(&bytes, &base.join("extract"))
}

#[cfg(windows)]
fn winget_ffmpeg() -> Option<std::path::PathBuf> {
    // Python's exact invocation: user scope first, then default.
    let present = std::process::Command::new("winget")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !present {
        return None;
    }
    for scope in [Some("user"), None] {
        let mut cmd = std::process::Command::new("winget");
        cmd.args([
            "install",
            "-e",
            "--id",
            "Gyan.FFmpeg",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--no-upgrade",
            "--disable-interactivity",
        ]);
        if let Some(s) = scope {
            cmd.args(["--scope", s]);
        }
        if cmd.output().map(|o| o.status.success()).unwrap_or(false) {
            break;
        }
    }
    // The running process PATH does not learn about a fresh
    // winget install; probe the locations winget uses directly.
    let local = std::env::var_os("LOCALAPPDATA")?;
    let base = std::path::PathBuf::from(local)
        .join("Microsoft")
        .join("WinGet");
    if let Some(dir) = find_bin_dir(&base.join("Links")) {
        return Some(dir);
    }
    find_bin_dir(&base.join("Packages"))
}

#[cfg(target_os = "macos")]
fn brew_ffmpeg() -> Option<std::path::PathBuf> {
    let brew = find_on_path(&["brew"]).or_else(|| {
        ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
    })?;
    let ok = std::process::Command::new(&brew)
        .args(["install", "ffmpeg"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let d = std::path::PathBuf::from(dir);
        if d.join("ffmpeg").is_file() && d.join("ffprobe").is_file() {
            return Some(d);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn static_ffmpeg(data_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    let url =
        format!("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-{arch}-static.tar.xz");
    let bytes = download_bytes(&url)?;
    if bytes.len() < 1_000_000 {
        return Err("ffmpeg download looks truncated".to_string());
    }
    let base = data_dir.join("ffmpeg");
    let extract = base.join("extract");
    if extract.exists() {
        std::fs::remove_dir_all(&extract).map_err(|e| format!("clean extract dir: {e}"))?;
    }
    std::fs::create_dir_all(&extract).map_err(|e| format!("extract dir: {e}"))?;
    let tarball = base.join("ffmpeg.tar.xz");
    std::fs::write(&tarball, &bytes).map_err(|e| format!("write tarball: {e}"))?;
    let ok = std::process::Command::new("tar")
        .args(["-xJf"])
        .arg(&tarball)
        .arg("-C")
        .arg(&extract)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return Err("could not extract the ffmpeg static build (tar failed)".to_string());
    }
    find_bin_dir(&extract).ok_or_else(|| "ffmpeg/ffprobe not found after extraction".to_string())
}

/// Ensure ffmpeg and ffprobe are usable, installing them when
/// they are missing (the platform routes above). Returns the
/// directory holding both executables. Both must actually run
/// before success is reported (Python validates the same way).
pub fn install_ffmpeg(data_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if let (Some(f), Some(_p)) = (crate::silence::find_ffmpeg(), find_ffprobe())
        && let Some(dir) = f.parent()
    {
        return Ok(dir.to_path_buf());
    }
    if let Some(dir) = managed_ffmpeg_bin_dir(data_dir) {
        return Ok(dir);
    }
    let dir = {
        #[cfg(windows)]
        {
            winget_ffmpeg()
                .map(Ok)
                .or_else(|| Some(install_ffmpeg_from_zip(data_dir)))
                .unwrap()?
        }
        #[cfg(target_os = "macos")]
        {
            brew_ffmpeg().ok_or_else(|| {
                "Homebrew is not available; install ffmpeg manually or set a custom path in Settings".to_string()
            })?
        }
        #[cfg(target_os = "linux")]
        {
            static_ffmpeg(data_dir)?
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            return Err("no automated ffmpeg install on this platform".to_string());
        }
    };
    let ffmpeg = dir.join(exe_name("ffmpeg"));
    let ffprobe = dir.join(exe_name("ffprobe"));
    if version_of(&ffmpeg).is_none() || version_of(&ffprobe).is_none() {
        return Err(format!(
            "installed ffmpeg/ffprobe in {} but they do not run",
            dir.display()
        ));
    }
    Ok(dir)
}
