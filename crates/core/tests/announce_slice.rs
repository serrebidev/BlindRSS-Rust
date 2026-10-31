//! Announcements model tests (PARITY 7.4): mode resolution must
//! match Python's normalize_modes — defaults are "both", unknown
//! events dropped, invalid values fall back per event.

use blindrss_core::announce::{Announcements, EVENTS, Mode};
use serde_json::json;

#[test]
fn defaults_are_all_both() {
    let a = Announcements::from_value(None);
    for ev in EVENTS.iter() {
        assert_eq!(a.mode(ev.id), Mode::Both, "{}", ev.id);
    }
    assert_eq!(a.mode("no_such_event"), Mode::Both);
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
    assert_eq!(a.mode("stop_update"), Mode::Both, "missing stays default");
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
