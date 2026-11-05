//! Audit 8 Task 30 (B10, B11): single-instance contract
//! through the public API. One sequential test, because the
//! state path derives from the process environment
//! (`XDG_RUNTIME_DIR`): serve writes an exclusive private
//! state file, a second copy's `signal_existing` activates
//! it with the recorded token, a wrong token is rejected,
//! and a stale state file is replaced by a fresh serve.

use blindrss_core::single_instance;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    }
    let path = dir.join(".blindrss-instance.json");

    // No state file: no instance to signal.
    assert!(!single_instance::signal_existing());

    // Plant a stale state file (dead port, dead token): the
    // primary's serve must replace it, not follow it.
    std::fs::write(
        &path,
        r#"{"port": 9, "pid": 999999, "token": "stale-token"}"#,
    )
    .unwrap();

    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    assert!(single_instance::serve(move || {
        counter.fetch_add(1, Ordering::SeqCst);
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
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Invalid token: rejected, no activation, no ack.
    let mut forged = recorded.clone();
    forged["token"] = serde_json::json!("wrong-token");
    std::fs::write(&path, forged.to_string()).unwrap();
    assert!(!single_instance::signal_existing());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Garbage state: treated as "no instance".
    std::fs::write(&path, "not json").unwrap();
    assert!(!single_instance::signal_existing());

    let _ = std::fs::remove_dir_all(&dir);
}
