//! Single-instance activation (Python `core/single_instance.py`).
//!
//! The first copy writes a private state file
//! (~/.blindrss-instance.json, or %TEMP% on Windows) holding
//! an ephemeral loopback port, its pid, and a random token,
//! then listens. A second copy reads the file, sends
//! "<token> activate", waits for "ok", and exits — the first
//! copy raises its window. The token keeps other local
//! processes from poking the listener.
//!
//! The state file is created exclusively with mode 0600
//! (Python's `os.open(path, O_WRONLY | O_CREAT | O_EXCL,
//! 0o600)` after unlinking whatever was there): the unlink
//! first means a planted symlink at the state path is removed
//! rather than followed, and the exclusive create means a
//! racer's file is never overwritten. On Windows the second
//! copy first hands its foreground right to the running
//! process (`AllowSetForegroundWindow(pid)`), or Windows only
//! flashes the other window's taskbar button.

use std::io::{Read, Write};

fn state_path() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::temp_dir().join(".blindrss-instance.json")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| std::env::temp_dir())
            });
        base.join(".blindrss-instance.json")
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InstanceState {
    port: u16,
    pid: u32,
    token: String,
}

/// Write the state file the way Python does: unlink whatever
/// sits at the path (never write through a planted symlink),
/// then create exclusively with owner-only permissions.
/// Fails when a file appears between the unlink and the
/// create — the caller treats that as "listener not started"
/// rather than overwriting another process's state.
fn write_state_file(path: &std::path::Path, state: &InstanceState) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    create_state_file(path, state)
}

/// The exclusive create itself, split out so tests can drive
/// the O_EXCL guarantee directly.
fn create_state_file(path: &std::path::Path, state: &InstanceState) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(serde_json::to_string(state).unwrap_or_default().as_bytes())
}

/// Hand this process's foreground right to `pid` (Windows
/// only; Python `activate_existing`). The launching copy owns
/// the foreground because the user just pressed the hotkey;
/// without the transfer the running copy cannot raise itself.
#[cfg(target_os = "windows")]
fn allow_set_foreground_window(pid: u32) {
    unsafe extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    unsafe {
        let _ = AllowSetForegroundWindow(pid);
    }
}

/// If another live instance answers, send it the activation
/// request and return true (this copy should exit).
pub fn signal_existing() -> bool {
    signal_existing_at(&state_path())
}

fn signal_existing_at(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(state) = serde_json::from_str::<InstanceState>(&text) else {
        return false;
    };
    // Foreground transfer BEFORE the authenticated signal
    // (Python's order): by the time the running copy handles
    // "activate", it already holds the right to come forward.
    #[cfg(target_os = "windows")]
    allow_set_foreground_window(state.pid);
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", state.port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(2)));
    if stream
        .write_all(format!("{} activate", state.token).as_bytes())
        .is_err()
    {
        return false;
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut ack = Vec::new();
    if stream.read_to_end(&mut ack).is_err() {
        return false;
    }
    ack == b"ok"
}

/// Become the primary instance: write the state file and
/// listen for activations, calling `on_activate` (on the
/// listener thread) for each valid one. Returns false when
/// the listener could not be started (the app runs anyway,
/// just without activation).
pub fn serve<F>(on_activate: F) -> bool
where
    F: Fn() + Send + 'static,
{
    serve_at(&state_path(), on_activate)
}

fn serve_at<F>(path: &std::path::Path, on_activate: F) -> bool
where
    F: Fn() + Send + 'static,
{
    let token = uuid::Uuid::new_v4().to_string();
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(_) => return false,
    };
    let Ok(port) = listener.local_addr().map(|a| a.port()) else {
        return false;
    };
    let state = InstanceState {
        port,
        pid: std::process::id(),
        token: token.clone(),
    };
    if write_state_file(path, &state).is_err() {
        return false;
    }
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            let mut data = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                match conn.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        data.extend_from_slice(&buf[..n]);
                        if data.len() > 256 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let expected = format!("{token} activate");
            if String::from_utf8_lossy(&data).trim() == expected {
                let _ = conn.write_all(b"ok");
                let _ = conn.shutdown(std::net::Shutdown::Write);
                on_activate();
            }
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blindrss_si_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state(token: &str) -> InstanceState {
        InstanceState {
            port: 9,
            pid: 4242,
            token: token.to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("mode");
        let path = dir.join(".blindrss-instance.json");
        write_state_file(&path, &state("tok")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "state file must be owner-only");
        let parsed: InstanceState =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.token, "tok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn planted_symlink_is_removed_not_followed() {
        let dir = scratch_dir("symlink");
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        let path = dir.join(".blindrss-instance.json");
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        write_state_file(&path, &state("tok")).unwrap();
        // The symlink target was never written through, and
        // the state path is now a real private file.
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_file(),
            "state path must be a regular file after creation"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exclusive_create_refuses_an_existing_file() {
        // The race Python's O_EXCL guards: a file that
        // appears after the unlink (another process's state)
        // must fail the create, never be overwritten.
        let dir = scratch_dir("excl");
        let path = dir.join(".blindrss-instance.json");
        create_state_file(&path, &state("first")).unwrap();
        assert!(create_state_file(&path, &state("second")).is_err());
        let parsed: InstanceState =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.token, "first");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
