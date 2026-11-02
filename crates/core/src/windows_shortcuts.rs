//! Windows shortcut creation (Python `core/windows_integration.py`'s
//! `create_shortcuts` family, Batch 42 / Audit 6 G2).
//!
//! Python creates `.lnk` files through PowerShell: it base64-encodes
//! a UTF-16LE script and runs `powershell -NoProfile -NonInteractive
//! -ExecutionPolicy Bypass -EncodedCommand <b64>`, driving
//! `WScript.Shell.CreateShortcut` (TargetPath / Arguments /
//! WorkingDirectory / IconLocation / Description "BlindRSS", then
//! Save — an existing file is overwritten silently). Taskbar pinning
//! is best-effort through the Shell.Application "Pin to taskbar"
//! verb; when the verb is unavailable Python falls back to writing
//! the `.lnk` into the `User Pinned\TaskBar` folder. This port
//! mirrors that behaviour exactly — no COM crate, no new dependency.
//!
//! The Rust build is a compiled executable, so Python's "frozen"
//! launch tuple applies: target = the exe itself, no arguments,
//! working directory = the exe's folder, icon = the exe itself.

use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "BlindRSS";

/// Whether this build runs on Windows (Python `is_windows`).
pub fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

/// Whether to offer the "Add Shortcuts..." File-menu item (Python
/// `mainframe.should_show_add_shortcuts`): hidden only on macOS,
/// where the dialog is dead weight; shown on Windows and Linux.
pub fn should_show_add_shortcuts() -> bool {
    !cfg!(target_os = "macos")
}

/// The three choices in Python's AddShortcutsDialog order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShortcutSelection {
    pub desktop: bool,
    pub start_menu: bool,
    pub taskbar: bool,
}

impl ShortcutSelection {
    pub fn any(&self) -> bool {
        self.desktop || self.start_menu || self.taskbar
    }
}

/// One requested shortcut's outcome: (ok, user-facing message),
/// mirroring one entry of Python's results dict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutOutcome {
    /// "desktop" | "start_menu" | "taskbar" (Python's dict keys).
    pub kind: &'static str,
    /// "Desktop" | "Start Menu" | "Taskbar" (the result labels).
    pub label: &'static str,
    pub ok: bool,
    pub message: String,
}

/// Python `_ps_literal`: a PowerShell single-quoted literal.
pub fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Python `get_launch_parts` for a compiled executable (the
/// "frozen" branch): (target, arguments, working_dir, icon).
pub fn launch_parts_from_exe(exe: &Path) -> (String, String, String, String) {
    let target = exe.to_string_lossy().to_string();
    let working_dir = exe
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    (target.clone(), String::new(), working_dir, target)
}

/// The launch parts for this running build.
pub fn launch_parts() -> Option<(String, String, String, String)> {
    std::env::current_exe()
        .ok()
        .map(|exe| launch_parts_from_exe(&exe))
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs` (Python
/// `_start_menu_programs_dir`).
pub fn start_menu_programs_dir(appdata: &Path) -> PathBuf {
    appdata
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
}

/// Python `get_start_menu_shortcut_path`.
pub fn start_menu_shortcut_path(appdata: &Path, app_name: &str) -> PathBuf {
    let name = if app_name.trim().is_empty() {
        APP_NAME
    } else {
        app_name.trim()
    };
    start_menu_programs_dir(appdata).join(format!("{name}.lnk"))
}

/// Python `_taskbar_dir`.
pub fn taskbar_dir(appdata: &Path) -> PathBuf {
    appdata
        .join("Microsoft")
        .join("Internet Explorer")
        .join("Quick Launch")
        .join("User Pinned")
        .join("TaskBar")
}

/// Python `_desktop_dir`'s fallback chain: OneDrive Desktop,
/// OneDriveConsumer Desktop, then ~/Desktop — the first candidate
/// that is a directory, else the first candidate at all.
pub fn desktop_dir_fallback(
    one_drive: Option<&str>,
    one_drive_consumer: Option<&str>,
    home: &Path,
    is_dir: impl Fn(&Path) -> bool,
) -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for var in [one_drive, one_drive_consumer] {
        if let Some(v) = var.map(str::trim).filter(|v| !v.is_empty()) {
            candidates.push(Path::new(v).join("Desktop"));
        }
    }
    candidates.push(home.join("Desktop"));
    for path in &candidates {
        if is_dir(path) {
            return path.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| home.join("Desktop"))
}

/// The PowerShell script Python `_create_shortcut` runs.
pub fn create_shortcut_script(
    shortcut_path: &str,
    target_path: &str,
    arguments: &str,
    working_dir: &str,
    icon_path: &str,
) -> String {
    [
        "$ErrorActionPreference = 'Stop'".to_string(),
        "$ws = New-Object -ComObject WScript.Shell".to_string(),
        format!(
            "$shortcut = $ws.CreateShortcut({})",
            ps_literal(shortcut_path)
        ),
        format!("$shortcut.TargetPath = {}", ps_literal(target_path)),
        format!("$shortcut.Arguments = {}", ps_literal(arguments)),
        format!("$shortcut.WorkingDirectory = {}", ps_literal(working_dir)),
        format!("$shortcut.IconLocation = {}", ps_literal(icon_path)),
        "$shortcut.Description = 'BlindRSS'".to_string(),
        "$shortcut.Save()".to_string(),
    ]
    .join("\n")
}

/// The PowerShell script Python `_pin_shortcut_to_taskbar` runs.
pub fn pin_shortcut_script(shortcut_path: &str) -> String {
    [
        "$ErrorActionPreference = 'Stop'".to_string(),
        format!("$path = {}", ps_literal(shortcut_path)),
        "if (-not (Test-Path -LiteralPath $path)) { throw 'Shortcut not found.' }".to_string(),
        "$shell = New-Object -ComObject Shell.Application".to_string(),
        "$folder = Split-Path -Path $path".to_string(),
        "$file = Split-Path -Path $path -Leaf".to_string(),
        "$item = $shell.NameSpace($folder).ParseName($file)".to_string(),
        "if (-not $item) { throw 'Unable to load shortcut item.' }".to_string(),
        "$verbs = @($item.Verbs())".to_string(),
        "$normalized = $verbs | ForEach-Object { [PSCustomObject]@{ Raw=$_; Name=($_.Name -replace '&','').Trim().ToLowerInvariant() } }".to_string(),
        "$already = $normalized | Where-Object { $_.Name -like '*unpin from taskbar*' } | Select-Object -First 1".to_string(),
        "if ($already) { Write-Output 'already-pinned'; exit 0 }".to_string(),
        "$pin = $normalized | Where-Object { $_.Name -like '*pin to taskbar*' -or $_.Name -like '*taskbarpin*' } | Select-Object -First 1".to_string(),
        "if ($pin) { $pin.Raw.DoIt(); Write-Output 'pinned'; exit 0 }".to_string(),
        "exit 1".to_string(),
    ]
    .join("\n")
}

/// The PowerShell script Python `_desktop_dir` runs first on
/// Windows (the known Desktop folder, incl. OneDrive redirection).
pub fn desktop_dir_script() -> String {
    [
        "$ErrorActionPreference = 'Stop'",
        "$p = [Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory)",
        "if (-not $p) {",
        "  $ws = New-Object -ComObject WScript.Shell",
        "  $p = $ws.SpecialFolders.Item('Desktop')",
        "}",
        "if ($p) { Write-Output $p }",
    ]
    .join("\n")
}

/// Python `_run_powershell`'s encoding: base64 of the UTF-16LE
/// script bytes, for `-EncodedCommand`.
pub fn encode_powershell_command(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The result lines + failure flag of Python's `on_add_shortcuts`
/// message box: "Desktop: OK: <msg>" / "Start Menu: Failed: <msg>".
pub fn format_results(results: &[ShortcutOutcome]) -> (String, bool) {
    if results.is_empty() {
        return ("No shortcut actions were performed.".to_string(), false);
    }
    let mut lines = Vec::new();
    let mut failed = false;
    for r in results {
        if !r.ok {
            failed = true;
        }
        let prefix = if r.ok { "OK" } else { "Failed" };
        let detail = if r.message.is_empty() {
            String::new()
        } else {
            format!(": {}", r.message)
        };
        lines.push(format!("{}: {prefix}{detail}", r.label));
    }
    (lines.join("\n"), failed)
}

#[cfg(target_os = "windows")]
fn run_powershell(script: &str, timeout_s: u64) -> (bool, String) {
    use std::io::Read;
    let encoded = encode_powershell_command(script);
    let spawned = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encoded,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s.max(5));
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return (false, e.to_string()),
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    let (out, err) = (out.trim().to_string(), err.trim().to_string());
    match status {
        None => (false, "PowerShell timed out.".to_string()),
        Some(s) if s.success() => (true, out),
        Some(s) => (
            false,
            if !err.is_empty() {
                err
            } else if !out.is_empty() {
                out
            } else {
                format!("PowerShell exited with code {}.", s.code().unwrap_or(-1))
            },
        ),
    }
}

#[cfg(not(target_os = "windows"))]
fn run_powershell(_script: &str, _timeout_s: u64) -> (bool, String) {
    (
        false,
        "PowerShell integration is only available on Windows.".to_string(),
    )
}

/// Python `_create_shortcut`: ensure the parent folder, write the
/// `.lnk` via WScript.Shell (overwriting any existing file), and
/// verify it exists afterwards.
fn create_shortcut(
    shortcut_path: &Path,
    target: &str,
    args: &str,
    working_dir: &str,
    icon: &str,
) -> (bool, String) {
    if let Some(parent) = shortcut_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let script = create_shortcut_script(
        &shortcut_path.to_string_lossy(),
        target,
        args,
        working_dir,
        icon,
    );
    let (ok, msg) = run_powershell(&script, 20);
    if !ok {
        return (false, msg);
    }
    if !shortcut_path.exists() {
        return (false, "Shortcut was not created.".to_string());
    }
    (true, "OK".to_string())
}

/// Python `_pin_shortcut_to_taskbar` (best-effort pin verb).
fn pin_shortcut_to_taskbar(shortcut_path: &Path) -> (bool, String) {
    run_powershell(&pin_shortcut_script(&shortcut_path.to_string_lossy()), 20)
}

fn appdata_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join("AppData").join("Roaming")))
}

/// Python `_desktop_dir`.
fn desktop_dir() -> PathBuf {
    if is_windows() {
        let (ok, msg) = run_powershell(&desktop_dir_script(), 10);
        if ok && !msg.is_empty() {
            for line in msg.lines() {
                let path = line.trim();
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
        }
    }
    desktop_dir_fallback(
        std::env::var("OneDrive").ok().as_deref(),
        std::env::var("OneDriveConsumer").ok().as_deref(),
        &std::env::home_dir().unwrap_or_else(|| PathBuf::from(".")),
        |p| p.is_dir(),
    )
}

/// Python `create_shortcuts`: create exactly the requested
/// shortcuts, in Desktop / Start Menu / Taskbar order. Off Windows
/// every requested kind fails with Python's platform message.
pub fn create_shortcuts(selection: ShortcutSelection, app_name: &str) -> Vec<ShortcutOutcome> {
    let mut results = Vec::new();
    let mut push = |kind, label, result: (bool, String)| {
        results.push(ShortcutOutcome {
            kind,
            label,
            ok: result.0,
            message: result.1,
        });
    };
    if !is_windows() {
        let msg = "Shortcuts are only supported on Windows.".to_string();
        if selection.desktop {
            push("desktop", "Desktop", (false, msg.clone()));
        }
        if selection.start_menu {
            push("start_menu", "Start Menu", (false, msg.clone()));
        }
        if selection.taskbar {
            push("taskbar", "Taskbar", (false, msg));
        }
        return results;
    }
    let Some((target, args, working_dir, icon)) = launch_parts() else {
        let msg = "Could not determine the program path.".to_string();
        if selection.desktop {
            push("desktop", "Desktop", (false, msg.clone()));
        }
        if selection.start_menu {
            push("start_menu", "Start Menu", (false, msg.clone()));
        }
        if selection.taskbar {
            push("taskbar", "Taskbar", (false, msg));
        }
        return results;
    };
    let lnk_name = format!("{app_name}.lnk");
    if selection.desktop {
        let lnk = desktop_dir().join(&lnk_name);
        let r = create_shortcut(&lnk, &target, &args, &working_dir, &icon);
        push("desktop", "Desktop", r);
    }
    if selection.start_menu {
        match appdata_dir() {
            Some(appdata) => {
                let lnk = start_menu_shortcut_path(&appdata, app_name);
                let r = create_shortcut(&lnk, &target, &args, &working_dir, &icon);
                push("start_menu", "Start Menu", r);
            }
            None => push(
                "start_menu",
                "Start Menu",
                (false, "Could not determine the AppData folder.".to_string()),
            ),
        }
    }
    if selection.taskbar {
        push(
            "taskbar",
            "Taskbar",
            create_taskbar(&lnk_name, &target, &args, &working_dir, &icon),
        );
    }
    results
}

/// Python's taskbar branch: pin a temp shortcut via the shell verb;
/// if that fails, write the `.lnk` into the taskbar pinned folder.
fn create_taskbar(
    lnk_name: &str,
    target: &str,
    args: &str,
    working_dir: &str,
    icon: &str,
) -> (bool, String) {
    let temp_dir = std::env::temp_dir().join(format!(
        "blindrss_shortcut_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = (|| {
        let temp_lnk = temp_dir.join(lnk_name);
        let (made_temp, made_msg) = create_shortcut(&temp_lnk, target, args, working_dir, icon);
        if !made_temp {
            return (false, made_msg);
        }
        let (pinned, pin_msg) = pin_shortcut_to_taskbar(&temp_lnk);
        if pinned {
            return (true, "Pinned to taskbar.".to_string());
        }
        match appdata_dir() {
            Some(appdata) => {
                let taskbar_lnk = taskbar_dir(&appdata).join(lnk_name);
                let (made_tb, msg_tb) =
                    create_shortcut(&taskbar_lnk, target, args, working_dir, icon);
                if made_tb {
                    (
                        true,
                        "Created taskbar shortcut file (pin verb unavailable on this Windows build)."
                            .to_string(),
                    )
                } else {
                    (false, if pin_msg.is_empty() { msg_tb } else { pin_msg })
                }
            }
            None => (false, pin_msg),
        }
    })();
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}
