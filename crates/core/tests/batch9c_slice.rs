//! Batch 9c: F1 help-topic registry integrity (mirrors Python's
//! tests/test_help_system.py assertions).

use blindrss_core::help_topics::*;

#[test]
fn every_mapped_topic_is_a_real_section() {
    for (_, topic) in COMMAND_TOPIC_PAIRS {
        assert!(
            is_known_topic(topic),
            "command maps to unknown topic {topic}"
        );
    }
    for (_, topic) in DIALOG_TOPIC_PAIRS {
        assert!(
            is_known_topic(topic),
            "dialog maps to unknown topic {topic}"
        );
    }
    for (_, _, topic) in NOTEBOOK_PAGE_TOPICS {
        assert!(
            is_known_topic(topic),
            "notebook maps to unknown topic {topic}"
        );
    }
    for (_, topic) in MENU_TITLE_TOPIC_PAIRS {
        assert!(is_known_topic(topic), "menu maps to unknown topic {topic}");
    }
    for (_, topic) in CONTEXT_MENU_TOPIC_PAIRS {
        assert!(
            is_known_topic(topic),
            "context label maps to unknown topic {topic}"
        );
    }
}

#[test]
fn every_shortcut_command_has_a_topic() {
    let mut missing = Vec::new();
    for def in blindrss_core::shortcuts_data::COMMANDS {
        if !COMMAND_TOPIC_PAIRS.iter().any(|(k, _)| *k == def.0) {
            missing.push(def.0);
        }
    }
    assert!(missing.is_empty(), "commands without topics: {missing:?}");
}

#[test]
fn jaws_process_gate_matches_python() {
    use blindrss_core::announce::is_jaws_process_name;
    assert!(is_jaws_process_name("jfw.exe"));
    assert!(is_jaws_process_name("JAWS.EXE"));
    assert!(is_jaws_process_name("fusion.exe"));
    assert!(!is_jaws_process_name("nvda.exe"));
    assert!(!is_jaws_process_name("jfw.exe.mui"));
    assert!(!is_jaws_process_name(""));
}

#[test]
fn registry_basics() {
    // 79 since Batch 40 removed the accessible-browser topic
    // with the Accessible Browser itself (owner-directed).
    assert_eq!(TOPICS.len(), 79);
    assert_eq!(normalize("no-such-topic"), DEFAULT_TOPIC);
    assert_eq!(normalize("main-window"), "main-window");
    assert_eq!(topic_title("user-guide"), "BlindRSS User Guide");
    assert_eq!(topic_for_command("feeds.video_search"), "video-search");
    assert_eq!(topic_for_command("bogus.command"), DEFAULT_TOPIC);
}

#[test]
fn chromium_v10_round_trip_and_jar_merge() {
    use blindrss_core::chromium::*;
    use ring::aead::{Aad, BoundKey, SealingKey, UnboundKey};

    // Build a synthetic profile DB with one v10-GCM cookie.
    let dir = std::env::temp_dir().join(format!("blindrss-9c-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("Cookies");
    let key = [7u8; 32];
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES ('version', '24');
             CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT, path TEXT,
               expires_utc INTEGER, is_secure INTEGER, is_httponly INTEGER,
               encrypted_value BLOB);",
        )
        .unwrap();
        let mut plain = vec![0xABu8; 32]; // domain-hash prefix, stripped
        plain.extend_from_slice(b"secret-value");
        let nonce_bytes = [3u8; 12];
        struct FixedNonce([u8; 12]);
        impl ring::aead::NonceSequence for FixedNonce {
            fn advance(&mut self) -> Result<ring::aead::Nonce, ring::error::Unspecified> {
                Ok(ring::aead::Nonce::try_assume_unique_for_key(&self.0).unwrap())
            }
        }
        let mut sealed = plain.clone();
        let mut sealing = SealingKey::new(
            UnboundKey::new(&ring::aead::AES_256_GCM, &key).unwrap(),
            FixedNonce(nonce_bytes),
        );
        sealing
            .seal_in_place_append_tag(Aad::empty(), &mut sealed)
            .unwrap();
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&sealed);
        let expires_utc = (1_900_000_000i64 + 11_644_473_600) * 1_000_000;
        conn.execute(
            "INSERT INTO cookies VALUES ('.example.com', 'sid', '', '/', ?1, 1, 1, ?2)",
            rusqlite::params![expires_utc, blob],
        )
        .unwrap();
        // A v20 row must be skipped, not crash.
        conn.execute(
            "INSERT INTO cookies VALUES ('.example.com', 'v20row', '', '/', ?1, 0, 0, ?2)",
            rusqlite::params![expires_utc, b"v20garbagegarbagegarbage".to_vec()],
        )
        .unwrap();
    }
    let outcome = read_cookie_db(&db_path, Some(&V10Key::Gcm(key))).unwrap();
    assert_eq!(outcome.records.len(), 1);
    assert_eq!(outcome.skipped, 1);
    let rec = &outcome.records[0];
    assert_eq!(rec.value, "secret-value");
    assert_eq!(rec.expires, 1_900_000_000);
    assert!(rec.http_only && rec.secure && rec.include_subdomains);

    let merged = merge_into_jar(
        "#HttpOnly_.example.com\tTRUE\t/\tTRUE\t1500000000\tsid\told-value\n\
         .other.com\tTRUE\t/\tFALSE\t0\tkeep\tme\n",
        &outcome.records,
        1_800_000_000,
    );
    assert!(merged.contains("secret-value"), "merged jar: {merged}");
    assert!(!merged.contains("old-value"));
    assert!(merged.contains("keep\tme"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chromium_decrypt_rejects_garbage() {
    use blindrss_core::chromium::*;
    assert!(decrypt_cookie_value(b"v10short", Some(&V10Key::Gcm([0u8; 32])), true).is_none());
    assert!(decrypt_cookie_value(b"v20whatever-nonce-here", None, true).is_none());
    assert_eq!(decrypt_cookie_value(b"", None, true), Some(Vec::new()));
    assert_eq!(browser_key_for_label("Brave Nightly"), "Brave");
    assert_eq!(
        browser_key_for_label("Microsoft Edge Dev"),
        "Microsoft Edge"
    );
    assert_eq!(browser_key_for_label("Google Chrome"), "Chrome");
}

#[test]
fn global_hotkey_spec_rules() {
    use blindrss_core::shortcuts::global_hotkey_spec;
    assert_eq!(global_hotkey_spec("Ctrl+Alt+B"), Some((0x1 | 0x2, 0x42)));
    assert_eq!(global_hotkey_spec("ctrl+alt+b"), Some((0x3, 0x42)));
    assert_eq!(global_hotkey_spec("Win+F5"), Some((0x8, 0x74)));
    assert_eq!(global_hotkey_spec("Shift+B"), None); // shift-only swallows typing
    assert_eq!(global_hotkey_spec("B"), None);
    assert_eq!(global_hotkey_spec("Ctrl+F12"), None); // debugger-reserved
    assert_eq!(global_hotkey_spec("Ctrl+Alt+F24"), Some((0x3, 0x87)));
    assert_eq!(global_hotkey_spec(""), None);
    assert_eq!(global_hotkey_spec("Ctrl+Alt+Home"), None);
}

#[test]
fn help_topics_carry_anchor_ids() {
    let guide = "# BlindRSS\n\nintro words\n\n## The Main Window {#main-window}\n\nBody one.\n\n## Feeds and Folders List {#feed-tree}\n\nBody two.\n";
    let topics = blindrss_core::help::topics(guide);
    let main = topics.iter().find(|t| t.id == "main-window").unwrap();
    assert_eq!(main.title, "The Main Window");
    assert!(main.body.contains("Body one."));
    assert!(topics.iter().any(|t| t.id == "feed-tree"));
}

#[test]
fn every_registry_topic_exists_in_the_shipped_guide() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/help/en.md");
    let guide = std::fs::read_to_string(path).expect("shipped English guide");
    let topics = blindrss_core::help::topics(&guide);
    let ids: std::collections::HashSet<&str> = topics.iter().map(|t| t.id.as_str()).collect();
    for (id, _) in blindrss_core::help_topics::TOPICS {
        assert!(ids.contains(id), "guide is missing section {id}");
    }
}
