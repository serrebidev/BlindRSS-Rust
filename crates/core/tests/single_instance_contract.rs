//! Audit 8 Task 30 (B10, B11): single-instance contract
//! through the public API. One sequential test, because the
//! state path derives from the process environment
//! (`XDG_RUNTIME_DIR`): serve writes an exclusive private
//! state file, a second copy's `signal_existing` activates
//! it with the recorded token, a wrong token is rejected,
//! and a stale state file is replaced by a fresh serve.

use blindrss_core::single_instance;
use std::sync::mpsc;
use std::time::Duration;

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blindrss_si_contract_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn serve_signal_round_trip_token_gate_and_stale_replacement() {
    let dir = scratch_dir("roundtrip");
    // SAFETY: this test binary's only test; the variable is
    // set before any state_path() call and never changed.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        // Windows ignores XDG_RUNTIME_DIR; isolate its state from the running app too.
        #[cfg(target_os = "windows")]
        {
            std::env::set_var("TEMP", &dir);
            std::env::set_var("TMP", &dir);
        }
    }
    // The expected path follows Python `_state_path`'s own
    // platform rule, which state_path() mirrors: Windows
    // uses %TEMP% unconditionally (per-user by construction);
    // POSIX uses XDG_RUNTIME_DIR. Assuming the XDG path on
    // Windows made serve() write %TEMP% while the test read
    // its scratch dir — the stale plant staring back is what
    // CI caught.
    let path = if cfg!(target_os = "windows") {
        std::env::temp_dir().join(".blindrss-instance.json")
    } else {
        dir.join(".blindrss-instance.json")
    };

    // No state file: no instance to signal.
    assert!(!single_instance::signal_existing());

    // Plant a stale state file (dead port, dead token): the
    // primary's serve must replace it, not follow it.
    std::fs::write(
        &path,
        r#"{"port": 9, "pid": 999999, "token": "stale-token"}"#,
    )
    .unwrap();

    // The listener acks before it runs the callback (Python's
    // order), so the test waits for the callback rather than
    // reading a counter right after the ack (v0.1.94 Linux leg).
    let (hit_tx, hits) = mpsc::channel::<()>();
    assert!(single_instance::serve(move || {
        let _ = hit_tx.send(());
    }));

    // The state file is now a real, private file (B11).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&path).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
    let recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(recorded["pid"].as_u64().unwrap(), std::process::id() as u64);
    assert_ne!(recorded["token"].as_str().unwrap(), "stale-token");

    // Valid token: activation lands (B10's signal path).
    assert!(single_instance::signal_existing());
    hits.recv_timeout(Duration::from_secs(10))
        .expect("activation callback ran");

    // Invalid token: rejected, no activation, no ack.
    let mut forged = recorded.clone();
    forged["token"] = serde_json::json!("wrong-token");
    std::fs::write(&path, forged.to_string()).unwrap();
    assert!(!single_instance::signal_existing());
    assert!(hits.recv_timeout(Duration::from_millis(300)).is_err());

    // Garbage state: treated as "no instance".
    std::fs::write(&path, "not json").unwrap();
    assert!(!single_instance::signal_existing());

    let _ = std::fs::remove_dir_all(&dir);
}
