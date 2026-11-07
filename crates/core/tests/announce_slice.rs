//! Announcements model tests (PARITY 7.4): mode resolution must
//! match Python's normalize_modes — defaults are "both" (the
//! refresh family defaults to "none" per Brandon's 2026-11-07
//! opt-in order), unknown events dropped, invalid values fall
//! back per event.

use blindrss_core::announce::{Announcements, EVENTS, Mode};
use serde_json::json;

#[test]
fn defaults_are_both_except_the_refresh_family() {
    // Brandon's order (2026-11-07): spoken refresh alerts are
    // opt-in — start, stop and result default to none; every
    // other event keeps Python's "both" default.
    let a = Announcements::from_value(None);
    for ev in EVENTS.iter() {
        let want = match ev.id {
            "start_update" | "stop_update" | "refresh_complete" => Mode::None,
            _ => Mode::Both,
        };
        assert_eq!(a.mode(ev.id), want, "{}", ev.id);
    }
    assert_eq!(a.mode("no_such_event"), Mode::Both);
}

#[test]
fn refresh_results_event_exists_and_is_opt_in() {
    assert!(EVENTS.iter().any(|e| e.id == "refresh_complete"));
    let a = Announcements::from_value(None);
    assert_eq!(a.mode("refresh_complete"), Mode::None);
    assert!(!a.announce("refresh_complete", "Refresh complete: 3 new articles"));
    // A stored choice turns it back on (mode resolution only;
    // emission itself depends on the platform speech path).
    let raw = json!({ "refresh_complete": "speech", "start_update": "both" });
    let b = Announcements::from_value(Some(&raw));
    assert_eq!(b.mode("refresh_complete"), Mode::Speech);
    assert_eq!(b.mode("start_update"), Mode::Both);
}

#[test]
fn stored_map_merges_over_defaults() {
    let raw = json!({
        "start_update": "speech",
        "favorite_toggle": "none",
        "bogus_event": "braille",
        "general": "loud",
    });
    let a = Announcements::from_value(Some(&raw));
    assert_eq!(a.mode("start_update"), Mode::Speech);
    assert_eq!(a.mode("favorite_toggle"), Mode::None);
    assert_eq!(a.mode("general"), Mode::Both, "invalid value falls back");
    assert_eq!(a.mode("stop_update"), Mode::None, "missing stays default");
    // Round trip: only known events persist.
    let out = a.to_json();
    let obj = out.as_object().unwrap();
    assert_eq!(obj.len(), EVENTS.len());
    assert_eq!(obj["start_update"], json!("speech"));
    assert!(!obj.contains_key("bogus_event"));
}

#[test]
fn none_mode_and_empty_text_never_emit() {
    let raw = json!({ "general": "none" });
    let a = Announcements::from_value(Some(&raw));
    assert!(!a.announce("general", "hello"));
    assert!(!a.announce("general", "   "));
    // On platforms without a speech backend (this test runner),
    // even a "both" announcement reports false rather than
    // panicking; on Windows/macOS it reaches the screen reader.
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    assert!(!a.announce("start_update", "Refresh Feeds"));
}
