//! Audit 8 Task 1 (S02, S11, G20): config compatibility with the
//! Python app — atomic saves, two-location resolution/relocation,
//! and the shared `last_selected_feed` view-ID format.
//!
//! Python spec: `core/config.py` (save_config writes a sibling
//! `.tmp`, flushes, fsyncs, `os.replace`s; `_resolve_config_path`
//! picks the newer of the user-data and app-folder configs;
//! `change_data_location` moves the config and is restricted on
//! installed Windows builds) and `gui/mainframe.py` (view IDs are
//! `category:`, `favorites:all`, `deleted:all`, `smart:`, plain feed
//! IDs, wrapped in an `unread:`/`read:` filter prefix).

use blindrss_core::config::{
    Config, choose_config_path, copy_file_if_missing, decode_remembered_view,
    encode_remembered_view,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "blindrss_cfgcompat_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

// ---------- S02: atomic config replacement ----------

#[test]
fn save_preserves_unknown_fields_and_leaves_no_temp() {
    let dir = scratch("atomic_ok");
    let path = dir.join("config.json");
    std::fs::write(
        &path,
        "{\"volume\": 3, \"python_only_field\": {\"x\": [1, 2]}, \"data_location\": \"app_folder\"}",
    )
    .unwrap();
    let mut cfg = Config::load(&path).unwrap();
    cfg.set_path("volume", serde_json::json!(9));
    cfg.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["volume"], 9);
    assert_eq!(parsed["python_only_field"]["x"][1], 2);
    assert!(
        !dir.join("config.json.tmp").exists(),
        "the sibling temp file must not survive a successful save"
    );
}

#[test]
fn blocked_temp_write_keeps_old_config_and_errors() {
    let dir = scratch("atomic_temp");
    let path = dir.join("config.json");
    let old = "{\"volume\": 3}";
    std::fs::write(&path, old).unwrap();
    // Occupy the sibling temp path with a directory: the write to
    // the temp file must fail before anything touches the live file.
    std::fs::create_dir(dir.join("config.json.tmp")).unwrap();
    let cfg = Config::load(&path).unwrap();
    assert!(
        cfg.save(&path).is_err(),
        "a failed temp write must surface as an error, not a direct overwrite"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
}

#[test]
fn failed_replace_keeps_old_target() {
    let dir = scratch("atomic_replace");
    // The target itself is a directory: serialization and the temp
    // write succeed, the replace must fail and leave it untouched.
    let path = dir.join("config.json");
    std::fs::create_dir(&path).unwrap();
    let cfg = Config::load(&dir.join("absent.json")).unwrap();
    assert!(cfg.save(&path).is_err());
    assert!(
        path.is_dir(),
        "the old target must survive a failed replace"
    );
    assert!(
        !dir.join("config.json.tmp").exists(),
        "a failed replace must clean up its temp file"
    );
}

// ---------- S11: two-location resolution and relocation ----------

#[test]
fn choose_config_path_prefers_newer_of_both_locations() {
    let user = PathBuf::from("/u/config.json");
    let app = PathBuf::from("/a/config.json");
    // Both exist: newer wins, either order; a tie goes to user data
    // (Python uses >= for the user location).
    assert_eq!(
        choose_config_path(&user, &app, Some(t(200)), Some(t(100)), false, false),
        user
    );
    assert_eq!(
        choose_config_path(&user, &app, Some(t(100)), Some(t(200)), false, false),
        app
    );
    assert_eq!(
        choose_config_path(&user, &app, Some(t(100)), Some(t(100)), false, false),
        user
    );
    // Only one exists: that one.
    assert_eq!(
        choose_config_path(&user, &app, Some(t(1)), None, false, false),
        user
    );
    assert_eq!(
        choose_config_path(&user, &app, None, Some(t(1)), false, false),
        app
    );
    // Neither exists: the platform default location.
    assert_eq!(
        choose_config_path(&user, &app, None, None, false, false),
        app
    );
    assert_eq!(
        choose_config_path(&user, &app, None, None, false, true),
        user
    );
    // Installed Windows builds always resolve to user data.
    assert_eq!(
        choose_config_path(&user, &app, None, Some(t(999)), true, false),
        user
    );
    assert_eq!(
        choose_config_path(&user, &app, Some(t(1)), Some(t(999)), true, false),
        user
    );
}

#[test]
fn relocation_moves_config_preserving_fields() {
    let dir = scratch("relocate");
    let app = dir.join("app").join("config.json");
    let user = dir.join("user").join("config.json");
    std::fs::create_dir_all(app.parent().unwrap()).unwrap();
    std::fs::write(
        &app,
        "{\"volume\": 7, \"unknown_thing\": \"keep\", \"data_location\": \"app_folder\"}",
    )
    .unwrap();
    let mut cfg = Config::load(&app).unwrap();
    let new_path = cfg
        .change_data_location(&app, "user_data", &user, &app, false)
        .unwrap();
    assert_eq!(new_path, user);
    assert!(!app.exists(), "the old config file is removed after a move");
    let moved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&user).unwrap()).unwrap();
    assert_eq!(moved["volume"], 7);
    assert_eq!(moved["unknown_thing"], "keep");
    assert_eq!(moved["data_location"], "user_data");
    // Same-location move is a no-op success.
    let again = cfg
        .change_data_location(&user, "user_data", &user, &app, false)
        .unwrap();
    assert_eq!(again, user);
    assert!(user.exists());
}

#[test]
fn relocation_failure_keeps_old_config() {
    let dir = scratch("relocate_fail");
    let app = dir.join("config.json");
    let old = "{\"volume\": 7, \"data_location\": \"app_folder\"}";
    std::fs::write(&app, old).unwrap();
    // The user location's parent is a regular file: nothing can be
    // created or written there.
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();
    let user = blocker.join("config.json");
    let mut cfg = Config::load(&app).unwrap();
    assert!(
        cfg.change_data_location(&app, "user_data", &user, &app, false)
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&app).unwrap(), old);
    assert_eq!(cfg.get_str("data_location", ""), "app_folder");
}

#[test]
fn relocation_restricted_on_installed_windows() {
    let dir = scratch("relocate_installed");
    let user = dir.join("user").join("config.json");
    let app = dir.join("app").join("config.json");
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::write(&user, "{\"volume\": 1}").unwrap();
    let mut cfg = Config::load(&user).unwrap();
    let err = cfg
        .change_data_location(&user, "app_folder", &user, &app, true)
        .unwrap_err();
    assert!(err.contains("User Data Folder"), "unexpected error: {err}");
    assert!(user.exists());
    assert!(!app.exists());
}

#[test]
fn copy_file_if_missing_never_overwrites() {
    let dir = scratch("seed");
    let src = dir.join("src.json");
    let dst = dir.join("dst.json");
    std::fs::write(&src, "{\"a\": 1}").unwrap();
    assert!(copy_file_if_missing(&src, &dst));
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "{\"a\": 1}");
    std::fs::write(&dst, "{\"a\": 2}").unwrap();
    assert!(copy_file_if_missing(&src, &dst));
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "{\"a\": 2}");
    assert!(!copy_file_if_missing(
        &dir.join("nope.json"),
        &dir.join("x.json")
    ));
}

// ---------- G20: shared remembered-view IDs ----------

#[test]
fn python_view_ids_decode_to_rust_views() {
    let v = decode_remembered_view("category:News / Tech");
    assert_eq!(v.read_filter, 0);
    assert_eq!(v.view_id.as_deref(), Some("cat:News / Tech"));

    let v = decode_remembered_view("unread:category:News");
    assert_eq!(v.read_filter, 1);
    assert_eq!(v.view_id.as_deref(), Some("cat:News"));

    let v = decode_remembered_view("read:feed-42");
    assert_eq!(v.read_filter, 2);
    assert_eq!(v.view_id.as_deref(), Some("feed-42"));

    let v = decode_remembered_view("favorites:all");
    assert_eq!(v.view_id.as_deref(), Some("favorites:all"));

    let v = decode_remembered_view("deleted:all");
    assert_eq!(v.view_id.as_deref(), Some("deleted:all"));

    let v = decode_remembered_view("smart:7");
    assert_eq!(v.view_id.as_deref(), Some("smart:7"));

    let v = decode_remembered_view("all");
    assert_eq!(v.view_id, None);
    let v = decode_remembered_view("");
    assert_eq!(v.view_id, None);
    assert_eq!(v.read_filter, 0);

    // Legacy Rust-written values stay readable.
    let v = decode_remembered_view("cat:News");
    assert_eq!(v.view_id.as_deref(), Some("cat:News"));
}

#[test]
fn rust_views_encode_to_python_ids_and_round_trip() {
    assert_eq!(
        encode_remembered_view(Some("cat:News / Tech"), 0),
        "category:News / Tech"
    );
    assert_eq!(
        encode_remembered_view(Some("cat:News"), 1),
        "unread:category:News"
    );
    assert_eq!(encode_remembered_view(Some("feed-42"), 2), "read:feed-42");
    assert_eq!(encode_remembered_view(None, 0), "all");
    assert_eq!(encode_remembered_view(None, 1), "unread:all");
    assert_eq!(
        encode_remembered_view(Some("favorites:all"), 1),
        "unread:favorites:all"
    );
    // Smart folders and the Deleted view carry their own semantics:
    // Python never wraps them in a filter prefix.
    assert_eq!(encode_remembered_view(Some("smart:7"), 1), "smart:7");
    assert_eq!(
        encode_remembered_view(Some("deleted:all"), 2),
        "deleted:all"
    );

    // Every Python-written value re-encodes to itself.
    for raw in [
        "all",
        "category:News / Tech",
        "unread:category:News",
        "read:feed-42",
        "favorites:all",
        "unread:favorites:all",
        "deleted:all",
        "smart:7",
        "feed-42",
        "unread:all",
    ] {
        let v = decode_remembered_view(raw);
        assert_eq!(
            encode_remembered_view(v.view_id.as_deref(), v.read_filter),
            raw,
            "round trip failed for {raw}"
        );
    }
}
