//! Single-instance activation (Python `core/single_instance.py`).
//!
//! The first copy writes a private state file
//! (~/.blindrss-instance.json, or %TEMP% on Windows) holding
//! an ephemeral loopback port, its pid, and a random token,
//! then listens. A second copy reads the file, sends
//! "<token> activate", waits for "ok", and exits — the first
//! copy raises its window. The token keeps other local
//! processes from poking the listener.

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

/// If another live instance answers, send it the activation
/// request and return true (this copy should exit).
pub fn signal_existing() -> bool {
    let Ok(text) = std::fs::read_to_string(state_path()) else {
        return false;
    };
    let Ok(state) = serde_json::from_str::<InstanceState>(&text) else {
        return false;
    };
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
    let path = state_path();
    let _ = std::fs::remove_file(&path);
    if std::fs::write(&path, serde_json::to_string(&state).unwrap_or_default()).is_err() {
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
