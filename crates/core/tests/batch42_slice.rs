//! Batch 42 (Audit 6 G2): Windows Add Shortcuts — regression
//! tests for the pure parts of `windows_shortcuts` (Python
//! `core/windows_integration.py`). The PowerShell execution itself
//! is Windows-only and exercised manually there; everything that
//! decides WHAT would run is pinned here on any platform.

use blindrss_core::windows_shortcuts::*;
use std::path::{Path, PathBuf};

#[test]
fn ps_literal_quotes_and_doubles_single_quotes() {
    assert_eq!(
        ps_literal(r"C:\Apps\BlindRSS.exe"),
        r"'C:\Apps\BlindRSS.exe'"
    );
    assert_eq!(ps_literal("it's"), "'it''s'");
    assert_eq!(ps_literal(""), "''");
}

#[test]
fn create_script_matches_python_wscript_shell_shape() {
    let script = create_shortcut_script(
        r"C:\Users\a\Desktop\BlindRSS.lnk",
        r"C:\Apps\BlindRSS.exe",
        "",
        r"C:\Apps",
        r"C:\Apps\BlindRSS.exe",
    );
    assert!(script.starts_with("$ErrorActionPreference = 'Stop'"));
    assert!(script.contains("$ws = New-Object -ComObject WScript.Shell"));
    assert!(script.contains(r"$shortcut = $ws.CreateShortcut('C:\Users\a\Desktop\BlindRSS.lnk')"));
    assert!(script.contains(r"$shortcut.TargetPath = 'C:\Apps\BlindRSS.exe'"));
    assert!(script.contains("$shortcut.Arguments = ''"));
    assert!(script.contains(r"$shortcut.WorkingDirectory = 'C:\Apps'"));
    assert!(script.contains(r"$shortcut.IconLocation = 'C:\Apps\BlindRSS.exe'"));
    assert!(script.contains("$shortcut.Description = 'BlindRSS'"));
    assert!(script.ends_with("$shortcut.Save()"));
}

#[test]
fn pin_script_uses_shell_verbs_with_already_pinned_check() {
    let script = pin_shortcut_script(r"C:\Temp\BlindRSS.lnk");
    assert!(script.contains("Shell.Application"));
    assert!(script.contains("*unpin from taskbar*"));
    assert!(script.contains("*pin to taskbar*"));
    assert!(script.contains("already-pinned"));
    assert!(script.contains("pinned"));
}

#[test]
fn encoded_command_is_base64_utf16le() {
    use base64::Engine;
    let encoded = encode_powershell_command("Write-Output 'x'");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let (pairs, _) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
    assert_eq!(String::from_utf16(&units).unwrap(), "Write-Output 'x'");
}

#[test]
fn start_menu_and_taskbar_paths_match_python_layout() {
    let appdata = Path::new(r"C:\Users\a\AppData\Roaming");
    assert_eq!(
        start_menu_shortcut_path(appdata, "BlindRSS"),
        PathBuf::from(r"C:\Users\a\AppData\Roaming")
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("BlindRSS.lnk")
    );
    // Blank app name falls back to BlindRSS, like Python.
    assert_eq!(
        start_menu_shortcut_path(appdata, "  "),
        start_menu_shortcut_path(appdata, "BlindRSS")
    );
    assert_eq!(
        taskbar_dir(appdata),
        PathBuf::from(r"C:\Users\a\AppData\Roaming")
            .join("Microsoft")
            .join("Internet Explorer")
            .join("Quick Launch")
            .join("User Pinned")
            .join("TaskBar")
    );
}

#[test]
fn desktop_fallback_prefers_existing_onedrive_then_home() {
    let home = Path::new("/home/a");
    // OneDrive Desktop exists -> wins.
    let picked = desktop_dir_fallback(Some(r"C:\Users\a\OneDrive"), None, home, |p| {
        p == Path::new(r"C:\Users\a\OneDrive").join("Desktop")
    });
    assert_eq!(picked, Path::new(r"C:\Users\a\OneDrive").join("Desktop"));
    // Nothing exists -> first candidate (OneDrive), like Python.
    let picked = desktop_dir_fallback(Some(r"C:\Users\a\OneDrive"), None, home, |_| false);
    assert_eq!(picked, Path::new(r"C:\Users\a\OneDrive").join("Desktop"));
    // No OneDrive vars -> ~/Desktop.
    let picked = desktop_dir_fallback(None, Some("  "), home, |_| false);
    assert_eq!(picked, home.join("Desktop"));
}

#[test]
fn launch_parts_are_the_frozen_tuple() {
    // Built with host separators so the parent-dir assertion is
    // meaningful on every platform (on Windows this is exactly
    // C:\Apps\BlindRSS\BlindRSS.exe).
    let exe = Path::new("Apps").join("BlindRSS").join("BlindRSS.exe");
    let (target, args, working_dir, icon) = launch_parts_from_exe(&exe);
    assert_eq!(target, exe.to_string_lossy());
    assert_eq!(args, "");
    assert_eq!(
        working_dir,
        Path::new("Apps").join("BlindRSS").to_string_lossy()
    );
    assert_eq!(icon, target);
}

#[test]
fn off_windows_every_requested_kind_fails_with_python_message() {
    if is_windows() {
        return; // The execution path is Windows-manual-tested.
    }
    let results = create_shortcuts(
        ShortcutSelection {
            desktop: true,
            start_menu: false,
            taskbar: true,
        },
        APP_NAME,
    );
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].kind, "desktop");
    assert_eq!(results[1].kind, "taskbar");
    for r in &results {
        assert!(!r.ok);
        assert_eq!(r.message, "Shortcuts are only supported on Windows.");
    }
    // Nothing requested -> no results, like Python's empty dict.
    assert!(create_shortcuts(ShortcutSelection::default(), APP_NAME).is_empty());
}

#[test]
fn format_results_matches_python_message_box_lines() {
    let outcomes = vec![
        ShortcutOutcome {
            kind: "desktop",
            label: "Desktop",
            ok: true,
            message: "OK".into(),
        },
        ShortcutOutcome {
            kind: "taskbar",
            label: "Taskbar",
            ok: false,
            message: "verb missing".into(),
        },
    ];
    let (text, failed) = format_results(&outcomes);
    assert_eq!(text, "Desktop: OK: OK\nTaskbar: Failed: verb missing");
    assert!(failed);
    let (text, failed) = format_results(&[]);
    assert_eq!(text, "No shortcut actions were performed.");
    assert!(!failed);
}
