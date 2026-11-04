//! Audit 8 Task 17 (S01, S04, S05, S06, G15, M14): shared
//! keyboard normalization and editor semantics.
//!
//! The Python spec is `core/shortcuts.py` (pure parse/resolve),
//! `gui/shortcut_keys.py` (wx keycode -> canonical token bridge),
//! `gui/hotkeys.py::resolve_media_action`, and the F1 rule in
//! `gui/help_context.py::HelpKeyFilter._is_help_key`. These tests
//! pin the pure decisions the app windows route through: one
//! canonical event bridge for the main frame, the player and the
//! capture dialog; one flat `keyboard_shortcuts` map shape on
//! disk (S01); binding-aware F1 (S06); media arrows (M14).

use blindrss_core::config::Config;
use blindrss_core::shortcuts::{
    HelpKeyAction, KeyModifiers, accel_map, event_accel, fixed_key_rows, help_key_action,
    media_arrow_action, normalize_accelerator, overrides_from_config, resolve_bindings,
    store_overrides, text_guarded,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// wx keycodes (wxWidgets values, mirrored by wxDragon).
const KC_BACK: i32 = 8;
const KC_TAB: i32 = 9;
const KC_RETURN: i32 = 13;
const KC_ESCAPE: i32 = 27;
const KC_SPACE: i32 = 32;
const KC_SHIFT: i32 = 306;
const KC_CONTROL: i32 = 308;
const KC_END: i32 = 312;
const KC_HOME: i32 = 313;
const KC_LEFT: i32 = 314;
const KC_UP: i32 = 315;
const KC_RIGHT: i32 = 316;
const KC_DOWN: i32 = 317;
const KC_F1: i32 = 340;
const KC_F5: i32 = 344;
const KC_F13: i32 = 352;
const KC_F24: i32 = 363;
const KC_PAGEUP: i32 = 366;
const KC_NUMPAD1: i32 = 325;
const KC_NUMPAD_ENTER: i32 = 370;
const KC_WINDOWS_LEFT: i32 = 393;

const NONE: KeyModifiers = KeyModifiers {
    ctrl: false,
    alt: false,
    shift: false,
    cmd: false,
};
const CTRL: KeyModifiers = KeyModifiers { ctrl: true, ..NONE };
const ALT: KeyModifiers = KeyModifiers { alt: true, ..NONE };
const SHIFT: KeyModifiers = KeyModifiers {
    shift: true,
    ..NONE
};
const CMD: KeyModifiers = KeyModifiers { cmd: true, ..NONE };
const CTRL_SHIFT: KeyModifiers = KeyModifiers {
    ctrl: true,
    shift: true,
    ..NONE
};
const CTRL_ALT: KeyModifiers = KeyModifiers {
    ctrl: true,
    alt: true,
    ..NONE
};

fn overrides(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// S04 (parse side): normalization is Python's parse_accel, strict.
// ---------------------------------------------------------------------------

#[test]
fn normalize_matches_python_strictness() {
    // Canonical order and aliases (already worked).
    assert_eq!(normalize_accelerator("control+shift+r"), "Ctrl+Shift+R");
    assert_eq!(normalize_accelerator("shift+ctrl+p"), "Ctrl+Shift+P");
    assert_eq!(normalize_accelerator("f5"), "F5");
    assert_eq!(normalize_accelerator(""), "");
    // Function keys stop at F24 (Python _normalize_key).
    assert_eq!(normalize_accelerator("F24"), "F24");
    assert_eq!(normalize_accelerator("F25"), "");
    assert_eq!(normalize_accelerator("F0"), "");
    // Unknown modifiers and unknown multi-char keys are invalid,
    // not silently kept (Python parse_accel returns None).
    assert_eq!(normalize_accelerator("Hyper+X"), "");
    assert_eq!(normalize_accelerator("Ctrl+Foo"), "");
    assert_eq!(normalize_accelerator("Ctrl+Banana+F5"), "");
    // Duplicate modifiers collapse.
    assert_eq!(normalize_accelerator("Ctrl+Ctrl+P"), "Ctrl+P");
    // The literal '+' key: "Ctrl+Shift++" (Python special case).
    assert_eq!(normalize_accelerator("Ctrl+Shift++"), "Ctrl+Shift++");
    assert_eq!(normalize_accelerator("Ctrl++"), "Ctrl++");
    assert_eq!(normalize_accelerator("+"), "");
    // Named-key aliases.
    assert_eq!(normalize_accelerator("ctrl+pgup"), "Ctrl+PageUp");
    assert_eq!(normalize_accelerator("Ctrl+back"), "Ctrl+Backspace");
    assert_eq!(normalize_accelerator("Ctrl+Shift+comma"), "Ctrl+Shift+,");
    assert_eq!(normalize_accelerator("cmd+q"), "Cmd+Q");
    assert_eq!(normalize_accelerator("Control+Left"), "Ctrl+Left");
    // A modifier with no key is unbound.
    assert_eq!(normalize_accelerator("Ctrl"), "");
    assert_eq!(normalize_accelerator("Ctrl+Shift"), "");
}

#[test]
fn resolve_bindings_python_semantics() {
    // Defaults resolve for every command; unbound stays "".
    let b = resolve_bindings(&HashMap::new());
    assert_eq!(b["player.play_pause"], "Ctrl+P");
    assert_eq!(b["feeds.refresh_all"], "F5");
    assert_eq!(b["feeds.video_search"], "");
    assert_eq!(b["help.user_guide"], "F1");
    // Explicit unbind wins over the default (Python test).
    let b = resolve_bindings(&overrides(&[("player.stop", "")]));
    assert_eq!(b["player.stop"], "");
    assert_eq!(b["player.show_hide"], "Ctrl+Shift+P");
    // Unknown ids are ignored, not carried into the result.
    let b = resolve_bindings(&overrides(&[("bogus.id", "F9")]));
    assert!(!b.contains_key("bogus.id"));
    // An invalid override string resolves to unbound, never to a
    // made-up binding.
    let b = resolve_bindings(&overrides(&[("feeds.add", "Hyper+X")]));
    assert_eq!(b["feeds.add"], "");
}

#[test]
fn accel_map_inverts_in_command_order() {
    // Default map (Python invert_bindings checks).
    let m = accel_map(&HashMap::new());
    assert_eq!(m["Ctrl+S"], "player.stop");
    assert_eq!(m["Ctrl+P"], "player.play_pause");
    assert_eq!(m["F1"], "help.user_guide");
    assert!(!m.contains_key(""));
    // On conflict the LAST command in table order wins the map
    // (Python invert_bindings iterates the ordered bindings).
    let m = accel_map(&overrides(&[("queue.next", "Ctrl+P")]));
    assert_eq!(m["Ctrl+P"], "queue.next");
}

// ---------------------------------------------------------------------------
// S01: shortcut edits persist as one FLAT dotted-id map.
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "blindrss_task17_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn overrides_round_trip_flat() {
    let dir = scratch("flat");
    let path = dir.join("config.json");

    let mut cfg = Config::default();
    store_overrides(&mut cfg, &overrides(&[("feeds.refresh_all", "F6")]));
    cfg.save(&path).unwrap();

    // The on-disk shape is Python's: a flat object whose keys are
    // the dotted command ids — no nested {"feeds": {...}} object.
    let text = std::fs::read_to_string(&path).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&text).unwrap();
    let map = raw["keyboard_shortcuts"].as_object().unwrap();
    assert_eq!(map["feeds.refresh_all"], "F6");
    assert!(
        map.get("feeds").is_none_or(|v| !v.is_object()),
        "nested per-category object leaked into keyboard_shortcuts: {map:?}"
    );

    // Reload resolves the edit: F6 dispatches refresh, F5 no
    // longer does (what the main window rebuilds after the dialog
    // closes, and again at relaunch).
    let cfg = Config::load(&path).unwrap();
    let ov = overrides_from_config(&cfg);
    assert_eq!(ov["feeds.refresh_all"], "F6");
    let m = accel_map(&ov);
    assert_eq!(m["F6"], "feeds.refresh_all");
    assert_ne!(m.get("F5").map(|s| s.as_str()), Some("feeds.refresh_all"));

    // An explicit unbind survives the round trip as "".
    let mut cfg = Config::load(&path).unwrap();
    store_overrides(&mut cfg, &overrides(&[("feeds.refresh_all", "")]));
    cfg.save(&path).unwrap();
    let cfg = Config::load(&path).unwrap();
    let b = resolve_bindings(&overrides_from_config(&cfg));
    assert_eq!(b["feeds.refresh_all"], "");

    // Storing an empty override set restores defaults (Reset All).
    let mut cfg = Config::load(&path).unwrap();
    store_overrides(&mut cfg, &HashMap::new());
    cfg.save(&path).unwrap();
    let cfg = Config::load(&path).unwrap();
    let b = resolve_bindings(&overrides_from_config(&cfg));
    assert_eq!(b["feeds.refresh_all"], "F5");

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// S04 (event side): the shared bridge — Python gui/shortcut_keys.py.
// ---------------------------------------------------------------------------

#[test]
fn event_bridge_dispatch_requires_modifier_or_function_key() {
    let d = |code, mods| event_accel(code, mods, true);
    // Plain typing/navigation is never captured by dispatch.
    assert_eq!(d(b'A' as i32, NONE), None);
    assert_eq!(d(KC_LEFT, NONE), None);
    assert_eq!(d(KC_SPACE, NONE), None);
    assert_eq!(d(KC_ESCAPE, NONE), None);
    // Bare function keys are the exception (F2/F5-style bindings).
    assert_eq!(d(KC_F5, NONE).as_deref(), Some("F5"));
    assert_eq!(d(KC_F1, NONE).as_deref(), Some("F1"));
    assert_eq!(d(KC_F13, NONE).as_deref(), Some("F13"));
    assert_eq!(d(KC_F24, NONE).as_deref(), Some("F24"));
    // Modified letters/digits.
    assert_eq!(d(b'R' as i32, CTRL_SHIFT).as_deref(), Some("Ctrl+Shift+R"));
    assert_eq!(d(b'1' as i32, CTRL).as_deref(), Some("Ctrl+1"));
    // Shift alone still forms an accel (Python _event_mods); it is
    // simply unbound by default, so typing proceeds untouched.
    assert_eq!(d(b'A' as i32, SHIFT).as_deref(), Some("Shift+A"));
}

#[test]
fn event_bridge_key_families() {
    let d = |code, mods| event_accel(code, mods, true);
    // Named / navigation keys (S04: importable and dispatchable).
    assert_eq!(d(KC_LEFT, CTRL).as_deref(), Some("Ctrl+Left"));
    assert_eq!(d(KC_DOWN, CTRL).as_deref(), Some("Ctrl+Down"));
    assert_eq!(d(KC_HOME, ALT).as_deref(), Some("Alt+Home"));
    assert_eq!(d(KC_END, CTRL).as_deref(), Some("Ctrl+End"));
    assert_eq!(d(KC_PAGEUP, CMD).as_deref(), Some("Cmd+PageUp"));
    assert_eq!(d(KC_TAB, CTRL).as_deref(), Some("Ctrl+Tab"));
    assert_eq!(d(KC_BACK, CTRL).as_deref(), Some("Ctrl+Backspace"));
    // Numpad digit matches the digit row (Python maps both).
    assert_eq!(d(KC_NUMPAD1, CTRL).as_deref(), Some("Ctrl+1"));
    // Numpad Enter is Enter.
    assert_eq!(d(KC_NUMPAD_ENTER, CTRL).as_deref(), Some("Ctrl+Enter"));
    assert_eq!(d(KC_RETURN, CTRL).as_deref(), Some("Ctrl+Enter"));
    // Shifted punctuation reports the unshifted token, so bindings
    // match either report (Python punct table).
    assert_eq!(d(b'<' as i32, CTRL_SHIFT).as_deref(), Some("Ctrl+Shift+,"));
    assert_eq!(d(b',' as i32, CTRL_SHIFT).as_deref(), Some("Ctrl+Shift+,"));
    assert_eq!(d(b'_' as i32, CTRL).as_deref(), Some("Ctrl+-"));
    assert_eq!(d(b'+' as i32, CTRL).as_deref(), Some("Ctrl+="));
    // Cmd/Meta is a distinct modifier, never merged into Ctrl.
    assert_eq!(d(b'P' as i32, CMD).as_deref(), Some("Cmd+P"));
    assert_eq!(d(b'P' as i32, CTRL).as_deref(), Some("Ctrl+P"));
    // Alt is a real modifier now (the old hook skipped it whole).
    assert_eq!(d(KC_F5, ALT).as_deref(), Some("Alt+F5"));
    // Bare modifier presses and unknown codes yield nothing.
    assert_eq!(d(KC_SHIFT, CTRL), None);
    assert_eq!(d(KC_CONTROL, NONE), None);
    assert_eq!(d(KC_WINDOWS_LEFT, NONE), None);
    assert_eq!(d(9999, CTRL), None);
    assert_eq!(d(0, CTRL), None);
}

#[test]
fn event_bridge_capture_mode_accepts_single_keys() {
    // require_modifier=false (the bridge contract the capture
    // dialog is built on; Python's dialog itself passes true and
    // additionally demands a modifier by prompt).
    let c = |code, mods| event_accel(code, mods, false);
    assert_eq!(c(b'A' as i32, NONE).as_deref(), Some("A"));
    assert_eq!(c(KC_SPACE, NONE).as_deref(), Some("Space"));
    assert_eq!(c(KC_LEFT, NONE).as_deref(), Some("Left"));
    assert_eq!(c(KC_SHIFT, NONE), None);
}

// ---------------------------------------------------------------------------
// M14: media arrows — Python gui/hotkeys.py resolve_media_action.
// ---------------------------------------------------------------------------

#[test]
fn media_arrow_rules() {
    // Ctrl+Arrow everywhere.
    assert_eq!(media_arrow_action(false, CTRL, KC_LEFT), Some("seek_back"));
    assert_eq!(
        media_arrow_action(false, CTRL, KC_RIGHT),
        Some("seek_forward")
    );
    assert_eq!(media_arrow_action(false, CTRL, KC_UP), Some("volume_up"));
    assert_eq!(
        media_arrow_action(false, CTRL, KC_DOWN),
        Some("volume_down")
    );
    // macOS Option+Arrow too; elsewhere Alt stays navigation.
    assert_eq!(media_arrow_action(true, ALT, KC_LEFT), Some("seek_back"));
    assert_eq!(media_arrow_action(false, ALT, KC_LEFT), None);
    // Mixed/disqualifying modifiers.
    assert_eq!(media_arrow_action(false, CTRL_ALT, KC_LEFT), None);
    assert_eq!(media_arrow_action(true, CTRL_ALT, KC_LEFT), None);
    assert_eq!(media_arrow_action(false, CTRL_SHIFT, KC_LEFT), None);
    assert_eq!(
        media_arrow_action(
            false,
            KeyModifiers {
                ctrl: true,
                cmd: true,
                ..NONE
            },
            KC_LEFT
        ),
        None
    );
    // No modifier -> plain navigation, never a media action.
    assert_eq!(media_arrow_action(false, NONE, KC_LEFT), None);
    assert_eq!(media_arrow_action(false, CTRL, b'A' as i32), None);
}

// ---------------------------------------------------------------------------
// S06: binding-aware F1 — Python HelpKeyFilter._is_help_key.
// ---------------------------------------------------------------------------

#[test]
fn help_key_respects_bindings() {
    // Default map: F1 is help.user_guide's binding.
    let m = accel_map(&HashMap::new());
    assert_eq!(help_key_action("F1", &m), HelpKeyAction::ContextHelp);
    // help.user_guide unbound entirely: F1 still helps (platform
    // key) because nothing else owns it.
    let m = accel_map(&overrides(&[("help.user_guide", "")]));
    assert_eq!(help_key_action("F1", &m), HelpKeyAction::ContextHelp);
    // F1 given to another command (the dialog's conflict flow
    // unbinds the previous owner, so help.user_guide is "" here):
    // that command wins, no help.
    let m = accel_map(&overrides(&[
        ("feeds.refresh_all", "F1"),
        ("help.user_guide", ""),
    ]));
    assert_eq!(help_key_action("F1", &m), HelpKeyAction::Dispatch);
    // User Guide moved to F9: F9 opens context help.
    let m = accel_map(&overrides(&[("help.user_guide", "F9")]));
    assert_eq!(help_key_action("F9", &m), HelpKeyAction::ContextHelp);
    // An ordinary bound key dispatches; an unbound key is neither.
    let m = accel_map(&HashMap::new());
    assert_eq!(help_key_action("Ctrl+P", &m), HelpKeyAction::Dispatch);
    assert_eq!(help_key_action("Ctrl+Z", &m), HelpKeyAction::Neither);
}

// ---------------------------------------------------------------------------
// Text guard set (Python _SHORTCUT_TEXT_GUARDED) + fixed rows (S05).
// ---------------------------------------------------------------------------

#[test]
fn text_guarded_set_matches_python() {
    for id in [
        "player.play_pause",
        "player.stop",
        "queue.next",
        "queue.prev",
        "filter.read_all",
        "filter.read_unread",
        "filter.read_read",
        "filter.media_all",
        "filter.media_with",
        "filter.media_without",
    ] {
        assert!(text_guarded(id), "{id} must be text-guarded");
    }
    for id in [
        "feeds.refresh_all",
        "article.toggle_favorite",
        "view.focus_search",
    ] {
        assert!(!text_guarded(id), "{id} must not be text-guarded");
    }
}

#[test]
fn fixed_rows_match_python_dialog() {
    let rows = fixed_key_rows();
    assert_eq!(rows.len(), 12);
    assert!(rows.iter().any(|(k, _)| *k == "Ctrl+R"));
    assert!(rows.iter().any(|(_, d)| d.contains("Toggle read/unread")));
}
