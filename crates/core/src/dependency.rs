//! Media-tool lifecycle (Python `core/dependency_check.py`).
//!
//! Detect yt-dlp and ffmpeg, report their versions, and
//! self-update yt-dlp from its GitHub releases into
//! <data_dir>/bin, registered at startup and checked by find_ytdlp
//! before PATH on every lookup.
//!
//! Audit 8 Task 23 (O06, O13, O14, O15, F20): one shared
//! resolution/validation policy lives here — `find_tool` (user
//! override, env override, managed copy, PATH, bundled and known
//! package locations), `validate_executable`/`probe_version`
//! (tool-correct version flag, finite timeout, capped output via
//! the owned-subprocess runner `run_tool`, which kills and reaps
//! on timeout), the nightly yt-dlp maintenance cycle
//! (`maintain_ytdlp`), Deno resolution (`find_js_runtime`) and
//! the packaged runtime smoke test (`runtime_smoke_test`).

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

static MANAGED_YTDLP: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

static DATA_DIR: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Register the runtime location; lookup checks it again after background installs.
pub fn set_managed_ytdlp_dir(data_dir: &std::path::Path) {
    *MANAGED_YTDLP.lock().unwrap_or_else(|p| p.into_inner()) = Some(managed_ytdlp(data_dir));
    *DATA_DIR.lock().unwrap_or_else(|p| p.into_inner()) = Some(data_dir.to_path_buf());
}

/// The data dir registered at startup, when any (used to locate
/// the managed ffmpeg/ffprobe install during tool resolution).
pub fn registered_data_dir() -> Option<std::path::PathBuf> {
    DATA_DIR.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub(crate) fn managed_ytdlp_path() -> Option<std::path::PathBuf> {
    MANAGED_YTDLP
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

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

/// Locate ffprobe through the shared resolution policy
/// (`find_tool`): user-configured path, env pointer, the managed
/// install, PATH, then known package locations.
pub fn find_ffprobe() -> Option<std::path::PathBuf> {
    find_tool("ffprobe")
}

// ---------------------------------------------------------------------------
// Owned subprocesses (F20) and tool validation (O13).
//
// Every helper the app shells out to is run through `run_tool`:
// stdin is closed, stdout/stderr are drained on reader threads
// and capped, and on timeout the child is KILLED AND REAPED
// (Python `core/discovery.py` terminates timed-out subprocesses
// the same way) — never detached to finish on its own.
// ---------------------------------------------------------------------------

/// Maximum bytes of stdout/stderr `run_tool` retains per stream.
/// A tool flooding its pipes cannot grow memory without bound;
/// the pipes are still drained past the cap so the child never
/// blocks on a full pipe.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 1 << 20;

/// One finished tool run: exit success plus capped output.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRun {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

fn read_capped<R: std::io::Read + Send + 'static>(
    mut reader: R,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if kept.len() < MAX_TOOL_OUTPUT_BYTES {
                        let take = (MAX_TOOL_OUTPUT_BYTES - kept.len()).min(n);
                        kept.extend_from_slice(&chunk[..take]);
                    }
                }
                Err(_) => break,
            }
        }
        kept
    })
}

/// Run `exe` with `args`, returning its capped output, or an
/// error when it cannot start or exceeds `timeout` (in which
/// case it is killed and reaped before this returns).
pub fn run_tool(
    exe: &std::path::Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> Result<ToolRun, String> {
    let mut child = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{} failed to start: {e}", exe.display()))?;
    let out_reader = child.stdout.take().map(read_capped);
    let err_reader = child.stderr.take().map(read_capped);
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(h) = out_reader {
                        let _ = h.join();
                    }
                    if let Some(h) = err_reader {
                        let _ = h.join();
                    }
                    return Err(format!(
                        "{} timed out after {:.1}s",
                        exe.display(),
                        timeout.as_secs_f64()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} wait failed: {e}", exe.display()));
            }
        }
    };
    let stdout = out_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = err_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(ToolRun {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// The harmless version flag for a tool (Python
/// `_version_arg_for_tool`): ffmpeg/ffprobe use single-dash
/// `-version`; yt-dlp, Deno and everything else use `--version`.
pub fn version_arg_for_tool(tool: &str) -> &'static str {
    match canon_tool_name(tool).as_str() {
        "ffmpeg" | "ffprobe" => "-version",
        _ => "--version",
    }
}

/// True when `path` is a file that runs its version command to
/// exit code 0 within `timeout` (Python `_validate_executable`).
pub fn validate_executable_with_timeout(
    path: &std::path::Path,
    tool: &str,
    timeout: std::time::Duration,
) -> bool {
    if !path.is_file() {
        return false;
    }
    run_tool(path, &[version_arg_for_tool(tool)], timeout)
        .map(|r| r.success)
        .unwrap_or(false)
}

/// `validate_executable_with_timeout` with Python's 6 s probe budget.
pub fn validate_executable(path: &std::path::Path, tool: &str) -> bool {
    validate_executable_with_timeout(path, tool, std::time::Duration::from_secs(6))
}

/// The first stdout line of the tool's version command when it
/// runs successfully, bounded at 10 s (Python `ytdlp_version`).
pub fn probe_version(path: &std::path::Path, tool: &str) -> Option<String> {
    // One retry when the RUNNER fails (spawn error or timeout
    // under load): a probe that never ran says nothing about the
    // tool, and treating it as "broken" would replace or report
    // missing a healthy executable. A tool that runs and fails is
    // NOT retried — that verdict is real.
    let run = run_tool(
        path,
        &[version_arg_for_tool(tool)],
        std::time::Duration::from_secs(10),
    )
    .or_else(|_| {
        run_tool(
            path,
            &[version_arg_for_tool(tool)],
            std::time::Duration::from_secs(10),
        )
    })
    .ok()?;
    if !run.success {
        return None;
    }
    run.stdout
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

fn version_of(path: &std::path::Path) -> Option<String> {
    let tool = path.file_stem()?.to_str()?;
    probe_version(path, tool)
}

// ---------------------------------------------------------------------------
// Shared resolution policy (O14): user override, env override,
// managed copy, PATH, bundled location, known package locations
// (Python `_find_executable_path` + `_collect_tool_candidates`).
// ---------------------------------------------------------------------------

fn tool_file_names(tool: &str) -> Vec<String> {
    let base = canon_tool_name(tool);
    if cfg!(windows) {
        vec![format!("{base}.exe"), base]
    } else {
        vec![base]
    }
}

fn env_override_path(tool: &str) -> Option<std::path::PathBuf> {
    let key = match canon_tool_name(tool).as_str() {
        "ffmpeg" => "BLINDRSS_FFMPEG",
        "ffprobe" => "BLINDRSS_FFPROBE",
        "yt-dlp" => "BLINDRSS_YTDLP",
        "deno" => "BLINDRSS_DENO",
        _ => return None,
    };
    std::env::var_os(key)
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
}

/// The first of `names` that is a file inside any of `dirs`.
pub fn find_in_dirs(names: &[&str], dirs: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    for dir in dirs {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Directories where package managers and portable layouts put
/// command-line tools, consulted when PATH is minimal or stripped
/// by a launcher (Python `_collect_tool_candidates`'s bounded
/// list — never a drive scan). The Windows set covers Scoop,
/// WinGet Links, Chocolatey, Gyan/FFmpeg and MSYS2 locations; the
/// POSIX set covers system, Homebrew, MacPorts, snap and per-user
/// locations.
pub fn known_package_dirs(tool: &str) -> Vec<std::path::PathBuf> {
    let tool = canon_tool_name(tool);
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(not(windows))]
    {
        let _ = &tool;
        dirs.extend(
            [
                "/usr/local/bin",
                "/usr/bin",
                "/bin",
                "/snap/bin",
                "/opt/homebrew/bin",
                "/opt/local/bin",
            ]
            .iter()
            .map(std::path::PathBuf::from),
        );
        if let Some(home) = std::env::home_dir() {
            dirs.push(home.join(".local").join("bin"));
            dirs.push(home.join("bin"));
        }
    }
    #[cfg(windows)]
    {
        let env_dir = |key: &str| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
        };
        let media = matches!(tool.as_str(), "ffmpeg" | "ffprobe");
        if let Some(user) = env_dir("USERPROFILE") {
            dirs.push(user.join("scoop").join("shims"));
            if media {
                for app in [
                    "ffmpeg",
                    "ffmpeg-essentials",
                    "ffmpeg-shared",
                    "ffmpeg-gyan-nightly",
                ] {
                    dirs.push(
                        user.join("scoop")
                            .join("apps")
                            .join(app)
                            .join("current")
                            .join("bin"),
                    );
                }
                dirs.push(user.join("bin"));
                dirs.push(user.join("ffmpeg").join("bin"));
            }
            if tool == "yt-dlp" {
                dirs.push(
                    user.join("scoop")
                        .join("apps")
                        .join("yt-dlp")
                        .join("current"),
                );
                dirs.push(user.join(".local").join("bin"));
                dirs.push(user.join("bin"));
            }
            if tool == "deno" {
                dirs.push(user.join("scoop").join("apps").join("deno").join("current"));
                dirs.push(user.join(".local").join("bin"));
            }
        }
        if let Some(local) = env_dir("LOCALAPPDATA") {
            dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
            if media {
                dirs.push(local.join("Programs").join("ffmpeg").join("bin"));
                dirs.push(local.join("Programs").join("FFmpeg").join("bin"));
                dirs.push(
                    local
                        .join("Programs")
                        .join("Gyan")
                        .join("FFmpeg")
                        .join("bin"),
                );
            }
            if tool == "yt-dlp" {
                dirs.push(local.join("Programs").join("yt-dlp"));
                dirs.push(local.join("Programs").join("yt-dlp").join("bin"));
                dirs.push(local.join("Microsoft").join("WindowsApps"));
            }
        }
        if media {
            for key in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
                if let Some(base) = env_dir(key) {
                    dirs.push(base.join("Gyan").join("FFmpeg").join("bin"));
                    dirs.push(base.join("FFmpeg").join("bin"));
                    dirs.push(base.join("ffmpeg").join("bin"));
                }
            }
            dirs.extend(
                [
                    r"C:\ffmpeg\bin",
                    r"C:\tools\ffmpeg\bin",
                    r"C:\msys64\ucrt64\bin",
                    r"C:\msys64\mingw64\bin",
                ]
                .iter()
                .map(std::path::PathBuf::from),
            );
        }
        if tool == "yt-dlp" {
            dirs.push(std::path::PathBuf::from(r"C:\msys64\usr\bin"));
        }
        let choco = env_dir("ChocolateyInstall")
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData\chocolatey"));
        dirs.push(choco.join("bin"));
    }
    dirs
}

/// Resolve a media tool through the one shared policy: the
/// user's configured path, the BLINDRSS_* env pointer, the
/// managed copy (yt-dlp's runtime download; the managed ffmpeg
/// install), PATH, the directory beside the executable (packaged
/// bundles ship their tools there or in its `bin/`), and finally
/// the known package locations. Mirrors Python's order except
/// that the managed yt-dlp deliberately beats PATH: PATH copies
/// are the stale ones (Python keeps the same preference for its
/// verified managed copy).
pub fn find_tool(tool: &str) -> Option<std::path::PathBuf> {
    let canon = canon_tool_name(tool);
    let names = tool_file_names(&canon);
    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    if let Some(p) = user_tool_path(&canon, &name_refs) {
        return Some(p);
    }
    if let Some(p) = env_override_path(&canon) {
        return Some(p);
    }
    if canon == "yt-dlp"
        && let Some(p) = managed_ytdlp_path().filter(|p| p.is_file())
    {
        return Some(p);
    }
    if (canon == "ffmpeg" || canon == "ffprobe")
        && let Some(data_dir) = registered_data_dir()
        && let Some(bin) = managed_ffmpeg_bin_dir(&data_dir)
    {
        let candidate = bin.join(&names[0]);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(p) = find_on_path(&name_refs) {
        return Some(p);
    }
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.to_path_buf());
        dirs.push(parent.join("bin"));
    }
    dirs.extend(known_package_dirs(&canon));
    find_in_dirs(&name_refs, &dirs)
}

/// The JavaScript runtime yt-dlp uses for YouTube's signature
/// challenges (O15): Deno, resolved through the shared policy
/// exactly like the media tools (Python ships it in the app
/// bundle and finds it via `_find_executable_path("deno")`).
pub fn find_js_runtime() -> Option<std::path::PathBuf> {
    find_tool("deno")
}

/// Detect the tools BlindRSS shells out to. A candidate that
/// exists but fails validation counts as MISSING (O13): the
/// caller repairs whatever this reports path-less, which is how
/// a broken install gets fixed instead of silently trusted
/// (Python `detect_media_tool_paths(validate=True)` semantics).
pub fn check_tools() -> Vec<ToolStatus> {
    let status = |name: &str, found: Option<std::path::PathBuf>| {
        let (path, version) = match found {
            Some(p) if validate_executable(&p, name) => (
                Some(p.to_string_lossy().to_string()),
                probe_version(&p, name),
            ),
            _ => (None, None),
        };
        ToolStatus {
            name: name.to_string(),
            path,
            version,
        }
    };
    vec![
        status("yt-dlp", crate::media::find_ytdlp()),
        status("ffmpeg", crate::silence::find_ffmpeg()),
        status("ffprobe", find_ffprobe()),
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
    Ok(update_managed_ytdlp(data_dir, &ytdlp_nightly_asset_url(), YTDLP_NIGHTLY_API_URL)?.path)
}

// ---------------------------------------------------------------------------
// Nightly yt-dlp maintenance (O06; Python `_ensure_yt_dlp_cli` /
// `_update_managed_ytdlp`). yt-dlp runs on the NIGHTLY channel:
// stable releases trail YouTube's changes by weeks. The managed
// copy is checked against the newest nightly tag, updated first
// via `yt-dlp --update-to nightly` (whose exit code is NOT
// trusted — the version is re-read) and then via a full download
// that is verified to run BEFORE it replaces the working copy,
// so a failed update never leaves the app without a working
// yt-dlp. Checks are throttled to once per 24 h and the throttle
// marker is written only after a check actually succeeds, so a
// copy that fails to update retries on the next launch.
// ---------------------------------------------------------------------------

/// Newest-nightly lookup (GitHub API for yt-dlp-nightly-builds).
pub const YTDLP_NIGHTLY_API_URL: &str =
    "https://api.github.com/repos/yt-dlp/yt-dlp-nightly-builds/releases/latest";

/// Nightly download base (per-platform asset appended).
pub const YTDLP_NIGHTLY_ASSET_BASE: &str =
    "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download";

fn ytdlp_nightly_asset_url() -> String {
    let asset = if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else {
        "yt-dlp_linux"
    };
    format!("{YTDLP_NIGHTLY_ASSET_BASE}/{asset}")
}

/// The newest published nightly tag from `api_url`, or None when
/// it cannot be reached — "don't know", never "up to date"
/// (Python `latest_ytdlp_version`).
pub fn latest_ytdlp_version_from(api_url: &str) -> Option<String> {
    let mut resp = crate::http::agent().get(api_url).call().ok()?;
    let body = resp.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn download_to_file(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let mut resp = crate::http::agent()
        .get(url)
        .call()
        .map_err(|e| format!("yt-dlp download: {e}"))?;
    let bytes = resp
        .body_mut()
        .read_to_vec()
        .map_err(|e| format!("yt-dlp download: {e}"))?;
    std::fs::write(dest, &bytes).map_err(|e| format!("write: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// What one managed-yt-dlp maintenance pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtdlpUpdateAction {
    /// The managed copy already reports the newest nightly tag.
    AlreadyCurrent,
    /// `yt-dlp --update-to nightly` brought the copy current
    /// (verified by re-reading its version).
    SelfUpdated,
    /// The self-update did not get there; the full nightly binary
    /// was downloaded, verified and swapped in.
    Downloaded,
    /// No working copy existed; the nightly binary was
    /// downloaded, verified and installed.
    InstalledNew,
    /// Updating failed, but the previous working copy is intact.
    KeptExistingAfterFailure,
    /// The resolved copy belongs to the system (PATH/package
    /// install): `--update-to nightly` was offered, and the
    /// binary was never overwritten (Python's rule for copies it
    /// does not manage).
    SystemUpdateOffered,
}

/// Outcome of a managed-yt-dlp maintenance pass.
#[derive(Debug, Clone, PartialEq)]
pub struct YtdlpUpdateReport {
    pub path: std::path::PathBuf,
    pub action: YtdlpUpdateAction,
    pub version: Option<String>,
}

/// Bring the managed yt-dlp in `data_dir` to the newest nightly
/// served by `asset_url`/`api_url` (the injectable URLs are the
/// production nightly endpoints in `update_ytdlp`/
/// `maintain_ytdlp`; tests serve loopback fixtures). Failure-safe:
/// with a working copy present, any failure reports
/// `KeptExistingAfterFailure` with the copy untouched; with no
/// working copy, a failed download is an error.
pub fn update_managed_ytdlp(
    data_dir: &std::path::Path,
    asset_url: &str,
    api_url: &str,
) -> Result<YtdlpUpdateReport, String> {
    let dest = managed_ytdlp(data_dir);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("bin dir: {e}"))?;
    }
    let existing_version = if dest.is_file() {
        probe_version(&dest, "yt-dlp")
    } else {
        None
    };
    let works = existing_version.is_some();
    let latest = latest_ytdlp_version_from(api_url);
    if works && latest.is_some() && existing_version == latest {
        set_managed_ytdlp_dir(data_dir);
        return Ok(YtdlpUpdateReport {
            path: dest,
            action: YtdlpUpdateAction::AlreadyCurrent,
            version: existing_version,
        });
    }
    if works {
        // Python: `--update-to nightly` first (small patch), but
        // its exit code is not trusted — re-read the version.
        let _ = run_tool(
            &dest,
            &["--update-to", "nightly"],
            std::time::Duration::from_secs(300),
        );
        let after = probe_version(&dest, "yt-dlp");
        if let (Some(l), Some(a)) = (&latest, &after)
            && l == a
        {
            set_managed_ytdlp_dir(data_dir);
            return Ok(YtdlpUpdateReport {
                path: dest,
                action: YtdlpUpdateAction::SelfUpdated,
                version: after,
            });
        }
        // Still behind (or the newest tag is unknown): fall
        // through to the full verified download, as Python does.
    }
    let staging = dest.with_extension("new");
    let staged = download_to_file(asset_url, &staging).and_then(|()| {
        if probe_version(&staging, "yt-dlp").is_some() {
            Ok(())
        } else {
            Err("downloaded yt-dlp did not run".to_string())
        }
    });
    match staged {
        Ok(()) => {
            let version = probe_version(&staging, "yt-dlp");
            std::fs::rename(&staging, &dest).map_err(|e| format!("install: {e}"))?;
            set_managed_ytdlp_dir(data_dir);
            Ok(YtdlpUpdateReport {
                path: dest,
                action: if works {
                    YtdlpUpdateAction::Downloaded
                } else {
                    YtdlpUpdateAction::InstalledNew
                },
                version,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            if works {
                Ok(YtdlpUpdateReport {
                    path: dest,
                    action: YtdlpUpdateAction::KeptExistingAfterFailure,
                    version: existing_version,
                })
            } else {
                Err(e)
            }
        }
    }
}

/// The 24 h update-check throttle marker inside the data dir
/// (Python keeps the same marker under the temp dir; the data
/// dir is the durable equivalent for the port).
pub fn ytdlp_marker_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("last_ytdlp_update_check.txt")
}

/// True when a yt-dlp update check is due at `now`: no marker,
/// an unreadable marker, or the last successful check at least
/// 24 h ago (Python `_should_check_updates`).
pub fn ytdlp_check_due_at(marker: &std::path::Path, now: std::time::SystemTime) -> bool {
    let Ok(text) = std::fs::read_to_string(marker) else {
        return true;
    };
    let Ok(secs) = text.trim().parse::<u64>() else {
        return true;
    };
    let last = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    now.duration_since(last)
        .map(|d| d >= std::time::Duration::from_secs(86_400))
        .unwrap_or(false)
}

/// Record a successful update check at `now` (Python
/// `_mark_update_checked` — called only once an update has
/// actually landed or the copy proved current).
pub fn mark_ytdlp_checked_at(marker: &std::path::Path, now: std::time::SystemTime) {
    if let Ok(elapsed) = now.duration_since(std::time::SystemTime::UNIX_EPOCH) {
        let _ = std::fs::write(marker, elapsed.as_secs().to_string());
    }
}

/// True when a yt-dlp update check is due for `data_dir` now.
pub fn ytdlp_check_due(data_dir: &std::path::Path) -> bool {
    ytdlp_check_due_at(&ytdlp_marker_path(data_dir), std::time::SystemTime::now())
}

/// One throttled maintenance pass over the resolved yt-dlp
/// (Python `_ensure_yt_dlp_cli`'s update block): the managed copy
/// gets the full verified nightly maintenance; a system copy is
/// only offered `--update-to nightly`. Returns None when the
/// throttle window is still running or nothing working resolves
/// (the missing-tool install flow handles that case). The marker
/// is written only when the pass succeeded.
pub fn maintain_ytdlp(data_dir: &std::path::Path) -> Option<YtdlpUpdateReport> {
    if !ytdlp_check_due(data_dir) {
        return None;
    }
    let marker = ytdlp_marker_path(data_dir);
    if managed_ytdlp(data_dir).is_file() {
        let report =
            update_managed_ytdlp(data_dir, &ytdlp_nightly_asset_url(), YTDLP_NIGHTLY_API_URL)
                .ok()?;
        if report.action != YtdlpUpdateAction::KeptExistingAfterFailure {
            mark_ytdlp_checked_at(&marker, std::time::SystemTime::now());
        }
        return Some(report);
    }
    if let Some(exe) = crate::media::find_ytdlp()
        && validate_executable(&exe, "yt-dlp")
    {
        let ok = run_tool(
            &exe,
            &["--update-to", "nightly"],
            std::time::Duration::from_secs(300),
        )
        .map(|r| r.success)
        .unwrap_or(false);
        if ok {
            mark_ytdlp_checked_at(&marker, std::time::SystemTime::now());
        }
        let version = probe_version(&exe, "yt-dlp");
        return Some(YtdlpUpdateReport {
            path: exe,
            action: YtdlpUpdateAction::SystemUpdateOffered,
            version,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Packaged runtime smoke test (O15; Python
// `core/runtime_self_test.py`, invoked there as
// `--blindrss-runtime-self-test`). The Rust-appropriate subset:
// the bundled tools resolve through the shared policy and
// actually run (yt-dlp, Deno, ffmpeg, ffprobe — Python's
// _REQUIRED_TOOLS plus ffprobe), and the extraction cascade
// produces complete, stable text for a known article. VLC and
// updater-helper checks are the app/player layer's own smoke
// paths and stay there.
// ---------------------------------------------------------------------------

/// The extraction half of the smoke test: a fixed sample article
/// must extract completely (>= 500 chars, Python's threshold)
/// and identically across repeated passes. Returns the failure
/// message, or None when extraction is healthy.
pub fn smoke_extraction_error() -> Option<String> {
    let paragraphs: String = (0..10)
        .map(|i| {
            format!(
                "<p>Paragraph {i} contains enough distinct article prose to verify \
                 that the packaged full-text extractor keeps the complete story when \
                 repeated prefetch and on-demand extraction passes run.</p>"
            )
        })
        .collect();
    let html = format!(
        "<html><head><title>Runtime extraction check</title></head><body>\
         <main><article><h1>Runtime extraction check</h1>{paragraphs}</article></main></body></html>"
    );
    let extract = |html: &str| match crate::extract::extract_article(
        html,
        "https://example.com/runtime-extraction-check",
        None,
    ) {
        crate::extract::ExtractOutcome::Article { text, .. } => Some(text),
        _ => None,
    };
    match extract(&html) {
        None => Some("full-text extraction returned no article".to_string()),
        Some(text) if text.chars().count() < 500 => {
            Some("full-text extraction returned incomplete article text".to_string())
        }
        Some(text) => {
            if extract(&html).as_deref() != Some(text.as_str()) {
                Some("full-text extraction changed across repeated passes".to_string())
            } else {
                None
            }
        }
    }
}

/// Run the packaged-runtime smoke checks and return the list of
/// failures (empty = healthy). The app exposes this as
/// `--blindrss-runtime-self-test` for packaging verification.
pub fn runtime_smoke_test() -> Vec<String> {
    let mut errors = Vec::new();
    for tool in ["yt-dlp", "deno", "ffmpeg", "ffprobe"] {
        match find_tool(tool) {
            None => errors.push(format!("bundled tool not found: {tool}")),
            Some(p) => {
                if !validate_executable(&p, tool) {
                    errors.push(format!("bundled tool does not run: {tool}"));
                }
            }
        }
    }
    if let Some(e) = smoke_extraction_error() {
        errors.push(e);
    }
    errors
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
