//! Start-at-login registration (Python
//! `core/windows_integration.py` + `core/macos_integration.py`).
//!
//! Windows: the HKCU Run key, value name "BlindRSS", via raw
//! advapi32 FFI (same style as hotkey.rs). macOS: a LaunchAgent
//! plist in ~/Library/LaunchAgents plus launchctl load/unload.
//! Other platforms: unsupported (the Settings checkbox is
//! disabled there, like Python).

/// Whether start-at-login registration works on this platform.
pub fn supported() -> bool {
    cfg!(target_os = "windows") || cfg!(target_os = "macos")
}

/// The Settings checkbox label for this platform (Python
/// `startup_setting_label`).
pub fn setting_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "Start BlindRSS when Windows starts"
    } else if cfg!(target_os = "macos") {
        "Start BlindRSS when you log in"
    } else {
        "Start BlindRSS at login"
    }
}

/// The command that launches this build: the current executable,
/// quoted when it contains spaces (Python `build_startup_command`
/// / list2cmdline quoting).
#[cfg(target_os = "windows")]
fn launch_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_string_lossy().to_string();
    if exe.contains(' ') {
        Some(format!("\"{exe}\""))
    } else {
        Some(exe)
    }
}

/// Register or unregister start-at-login (Python
/// `set_startup_enabled`). Err carries the user-facing message.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        win::set_enabled(
            enabled,
            &launch_command().ok_or("Could not determine the program path.")?,
        )
    }
    #[cfg(target_os = "macos")]
    {
        mac::set_enabled(enabled)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = enabled;
        Err("Startup registration is not supported on this platform.".to_string())
    }
}

#[cfg(target_os = "windows")]
mod win {
    type Hkey = isize;
    type RegResult = i32;

    unsafe extern "system" {
        fn RegCreateKeyExW(
            hkey: Hkey,
            subkey: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            sam_desired: u32,
            security: *const core::ffi::c_void,
            result_key: *mut Hkey,
            disposition: *mut u32,
        ) -> RegResult;
        fn RegSetValueExW(
            hkey: Hkey,
            value_name: *const u16,
            reserved: u32,
            value_type: u32,
            data: *const u8,
            data_len: u32,
        ) -> RegResult;
        fn RegDeleteValueW(hkey: Hkey, value_name: *const u16) -> RegResult;
        fn RegCloseKey(hkey: Hkey) -> RegResult;
    }

    const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as i32 as Hkey;
    const KEY_SET_VALUE: u32 = 0x0002;
    const KEY_CREATE_SUB_KEY: u32 = 0x0004;
    const REG_SZ: u32 = 1;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "BlindRSS";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn set_enabled(enabled: bool, command: &str) -> Result<(), String> {
        let subkey = wide(RUN_KEY);
        let name = wide(VALUE_NAME);
        let mut hkey: Hkey = 0;
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE | KEY_CREATE_SUB_KEY,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(format!(
                "Could not open the startup registry key (error {rc})."
            ));
        }
        let result = if enabled {
            let data = wide(command);
            let rc = unsafe {
                RegSetValueExW(
                    hkey,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    (data.len() * 2) as u32,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("Could not set the startup entry (error {rc})."))
            }
        } else {
            let rc = unsafe { RegDeleteValueW(hkey, name.as_ptr()) };
            if rc == 0 || rc == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(format!("Could not remove the startup entry (error {rc})."))
            }
        };
        unsafe {
            RegCloseKey(hkey);
        }
        result
    }
}

#[cfg(target_os = "macos")]
mod mac {
    const LABEL: &str = "com.serrebidev.blindrss";

    fn plist_path() -> Option<std::path::PathBuf> {
        std::env::home_dir().map(|h| {
            h.join("Library")
                .join("LaunchAgents")
                .join(format!("{LABEL}.plist"))
        })
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn launchctl(args: &[&str]) -> (bool, String) {
        match std::process::Command::new("launchctl").args(args).output() {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).to_string();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                (out.status.success(), text)
            }
            Err(e) => (false, e.to_string()),
        }
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let target = plist_path().ok_or("Could not determine the home directory.")?;
        if !enabled {
            let _ = launchctl(&["unload", &target.to_string_lossy()]);
            if target.exists() {
                std::fs::remove_file(&target)
                    .map_err(|e| format!("Could not remove the login item: {e}"))?;
            }
            return Ok(());
        }
        let exe = std::env::current_exe()
            .map_err(|e| format!("Could not determine how to launch BlindRSS: {e}"))?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create the LaunchAgents folder: {e}"))?;
        }
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n<key>Label</key>\n<string>{LABEL}</string>\n<key>ProgramArguments</key>\n<array>\n<string>{}</string>\n</array>\n<key>RunAtLoad</key>\n<true/>\n</dict>\n</plist>\n",
            xml_escape(&exe.to_string_lossy())
        );
        std::fs::write(&target, plist)
            .map_err(|e| format!("Could not write the login item: {e}"))?;
        let path_str = target.to_string_lossy().to_string();
        let _ = launchctl(&["unload", &path_str]);
        let (ok, msg) = launchctl(&["load", "-w", &path_str]);
        // launchctl complains when the agent is already loaded;
        // the plist is written and that state is still "enabled".
        if !ok && !msg.to_lowercase().contains("already loaded") {
            let _ = std::fs::remove_file(&target);
            return Err(format!("launchctl load failed: {}", msg.trim()));
        }
        Ok(())
    }
}
