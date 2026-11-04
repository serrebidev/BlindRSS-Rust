//! Shortcut registry (port of `core/shortcuts.py` semantics).
//!
//! The command table itself lives in `shortcuts_data.rs`
//! (extracted from Python). Effective accelerators = defaults
//! overlaid with the user's `keyboard_shortcuts` config map.
//! Normalisation is Python's `parse_accel`/`format_accel` pair:
//! strict — an unknown modifier or key makes the whole string
//! unbound rather than a made-up binding.
//!
//! This module also carries the pure event bridge (Python
//! `gui/shortcut_keys.py`): wx keycode + modifiers -> canonical
//! accelerator, shared by the main frame's dispatch, the player
//! window's delegation and the shortcuts dialog's capture, so
//! the three can never drift apart (Audit 8 S04). The media
//! arrow rule (`gui/hotkeys.py::resolve_media_action`), the
//! binding-aware F1 rule (`gui/help_context.py`) and the
//! text-guard set (`gui/mainframe.py::_SHORTCUT_TEXT_GUARDED`)
//! live here for the same reason: they are policy, testable
//! without a window.

use std::collections::HashMap;

use crate::config::Config;
use crate::shortcuts_data::COMMANDS;

pub struct Command {
    pub id: String,
    pub category: String,
    pub label: String,
    pub default_accel: String,
}

pub fn commands() -> Vec<Command> {
    COMMANDS
        .iter()
        .map(|(id, cat, label, def)| Command {
            id: id.to_string(),
            category: cat.to_string(),
            label: label.to_string(),
            default_accel: def.to_string(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Accelerator parsing / normalisation (Python parse_accel/format_accel)
// ---------------------------------------------------------------------------

fn normalize_key(token: &str) -> Option<String> {
    let raw = token.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    let named = match lower.as_str() {
        "space" => Some("Space"),
        "left" => Some("Left"),
        "right" => Some("Right"),
        "up" => Some("Up"),
        "down" => Some("Down"),
        "home" => Some("Home"),
        "end" => Some("End"),
        "pageup" | "pgup" => Some("PageUp"),
        "pagedown" | "pgdn" => Some("PageDown"),
        "insert" | "ins" => Some("Insert"),
        "delete" | "del" => Some("Delete"),
        "backspace" | "back" => Some("Backspace"),
        "enter" | "return" => Some("Enter"),
        "tab" => Some("Tab"),
        "escape" | "esc" => Some("Escape"),
        "comma" => Some(","),
        "period" | "dot" => Some("."),
        "slash" => Some("/"),
        "minus" | "dash" => Some("-"),
        "equals" | "equal" => Some("="),
        "plus" => Some("+"),
        "semicolon" => Some(";"),
        "quote" => Some("'"),
        "leftbracket" => Some("["),
        "rightbracket" => Some("]"),
        "backslash" => Some("\\"),
        "backtick" | "grave" => Some("`"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_string());
    }
    // Function keys F1..F24 only (Python _normalize_key).
    if lower.len() > 1 && lower.starts_with('f') && lower[1..].chars().all(|c| c.is_ascii_digit()) {
        let n: u32 = lower[1..].parse().ok()?;
        return if (1..=24).contains(&n) {
            Some(format!("F{n}"))
        } else {
            None
        };
    }
    if raw.chars().count() == 1 {
        let ch = raw.chars().next().unwrap();
        if ch.is_alphabetic() {
            return Some(ch.to_uppercase().collect());
        }
        // Digit or punctuation kept literal.
        return Some(ch.to_string());
    }
    None
}

/// Parse an accelerator string into (canonical-order modifiers,
/// key token) — Python `parse_accel`. None for blank (unbound)
/// or malformed values (unknown modifier or key).
pub fn parse_accel(accel: &str) -> Option<(Vec<&'static str>, String)> {
    let raw = accel.trim();
    if raw.is_empty() {
        return None;
    }
    // Special-case a literal '+' key so "Ctrl+Shift++" parses
    // correctly (Python does exactly this).
    let (mods_raw, key_raw): (Vec<&str>, &str) = if raw.ends_with('+') && raw.len() > 1 {
        let mod_part = raw[..raw.len() - 1].trim_end_matches('+');
        (
            mod_part
                .split('+')
                .filter(|m| !m.trim().is_empty())
                .collect(),
            "+",
        )
    } else {
        let parts: Vec<&str> = raw.split('+').collect();
        let (key, mods) = parts.split_last()?;
        (mods.to_vec(), key)
    };
    let mut mods: Vec<&'static str> = Vec::new();
    for m in mods_raw {
        let canon = match m.trim().to_lowercase().as_str() {
            "ctrl" | "control" | "ctl" => "Ctrl",
            "alt" | "option" | "opt" => "Alt",
            "shift" => "Shift",
            "cmd" | "command" | "win" | "meta" | "super" => "Cmd",
            _ => return None,
        };
        if !mods.contains(&canon) {
            mods.push(canon);
        }
    }
    let key = normalize_key(key_raw)?;
    let ordered: Vec<&'static str> = ["Ctrl", "Alt", "Shift", "Cmd"]
        .into_iter()
        .filter(|m| mods.contains(m))
        .collect();
    Some((ordered, key))
}

/// Build a canonical accelerator string from modifiers + a key
/// token (Python `format_accel`).
pub fn format_accel(mods: &[&str], key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for m in ["Ctrl", "Alt", "Shift", "Cmd"] {
        if mods.contains(&m) {
            out.push_str(m);
            out.push('+');
        }
    }
    out.push_str(key);
    out
}

/// Normalise an accelerator string to canonical modifier order
/// (Ctrl, Alt, Shift, Cmd) + key. Empty stays empty (unbound);
/// invalid input also yields empty, as Python's
/// `normalize_accel` does.
pub fn normalize_accelerator(accel: &str) -> String {
    match parse_accel(accel) {
        Some((mods, key)) => format_accel(&mods, &key),
        None => String::new(),
    }
}

/// Win32 RegisterHotKey spec for a system-wide hotkey
/// (Python's global_hotkey_spec): (modifier flags, virtual key).
/// Only ASCII letters/digits and F1-F24 (not F12, reserved for
/// debuggers); a letter/digit needs Ctrl, Alt or Win — a bare or
/// Shift-only key would swallow ordinary typing everywhere else.
pub fn global_hotkey_spec(accel: &str) -> Option<(u32, u32)> {
    let normalized = normalize_accelerator(accel);
    if normalized.is_empty() {
        return None;
    }
    let parts: Vec<&str> = normalized.split('+').collect();
    let (mods, key) = parts.split_at(parts.len() - 1);
    let mut flags: u32 = 0;
    for m in mods {
        flags |= match *m {
            "Alt" => 0x1,
            "Ctrl" => 0x2,
            "Shift" => 0x4,
            "Cmd" => 0x8,
            _ => return None,
        };
    }
    let key = key[0];
    let bytes = key.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
        if flags & !0x4 == 0 {
            return None;
        }
        return Some((flags, bytes[0].to_ascii_uppercase() as u32));
    }
    if let Some(num) = key.strip_prefix('F')
        && let Ok(n) = num.parse::<u32>()
        && (1..=24).contains(&n)
        && n != 12
    {
        return Some((flags, 0x6F + n));
    }
    None
}

// ---------------------------------------------------------------------------
// Binding resolution (Python resolve_bindings / invert_bindings)
// ---------------------------------------------------------------------------

/// Resolved bindings for EVERY command: id -> canonical
/// accelerator, "" meaning unbound. Overrides may be strings,
/// "" or absent (unbound / default); unknown ids are ignored.
pub fn resolve_bindings(overrides: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (id, _cat, _label, def) in COMMANDS {
        let accel = match overrides.get(*id) {
            Some(v) => normalize_accelerator(v),
            None => normalize_accelerator(def),
        };
        out.insert(id.to_string(), accel);
    }
    out
}

/// Effective accelerators: id -> canonical accelerator (only
/// bound commands included).
pub fn effective(overrides: &HashMap<String, String>) -> HashMap<String, String> {
    resolve_bindings(overrides)
        .into_iter()
        .filter(|(_, accel)| !accel.is_empty())
        .collect()
}

/// The dispatch map: canonical accelerator -> command id,
/// built by iterating the command table in order so that on a
/// conflict the LAST command wins (Python `invert_bindings`).
pub fn accel_map(overrides: &HashMap<String, String>) -> HashMap<String, String> {
    let bindings = resolve_bindings(overrides);
    let mut out = HashMap::new();
    for (id, _cat, _label, _def) in COMMANDS {
        let accel = &bindings[*id];
        if !accel.is_empty() {
            out.insert(accel.clone(), id.to_string());
        }
    }
    out
}

/// Conflicts: accelerator -> ids bound to it, for groups of 2+.
pub fn conflicts(effective: &HashMap<String, String>) -> HashMap<String, Vec<String>> {
    let mut by_accel: HashMap<String, Vec<String>> = HashMap::new();
    for (id, accel) in effective {
        by_accel.entry(accel.clone()).or_default().push(id.clone());
    }
    by_accel
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .collect()
}

// ---------------------------------------------------------------------------
// Config persistence: one FLAT dotted-id map (Audit 8 S01)
// ---------------------------------------------------------------------------

/// The user's shortcut overrides exactly as stored: the flat
/// `keyboard_shortcuts` object's string values, keyed by the
/// dotted command ids Python uses (`gui/mainframe.py:3609`).
/// Non-string values (e.g. nested objects an older build wrote)
/// are ignored, as Python's resolve step ignores them.
pub fn overrides_from_config(cfg: &Config) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(obj) = cfg
        .raw()
        .get("keyboard_shortcuts")
        .and_then(|v| v.as_object())
    {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

/// Replace the stored `keyboard_shortcuts` object with
/// `overrides` as one flat map (command-table order first, then
/// any unknown keys sorted) — never a dotted `set_path`, which
/// would nest `{"feeds": {"refresh_all": ...}}` where Python
/// keeps `{"feeds.refresh_all": ...}` and the dispatch rebuild
/// would silently ignore the edit.
pub fn store_overrides(cfg: &mut Config, overrides: &HashMap<String, String>) {
    let mut map = serde_json::Map::new();
    for (id, _cat, _label, _def) in COMMANDS {
        if let Some(v) = overrides.get(*id) {
            map.insert(id.to_string(), serde_json::Value::String(v.clone()));
        }
    }
    let mut extra: Vec<&String> = overrides
        .keys()
        .filter(|k| !COMMANDS.iter().any(|(id, _, _, _)| id == *k))
        .collect();
    extra.sort();
    for k in extra {
        map.insert(k.clone(), serde_json::Value::String(overrides[k].clone()));
    }
    cfg.raw_mut().as_object_mut().unwrap().insert(
        "keyboard_shortcuts".to_string(),
        serde_json::Value::Object(map),
    );
}

// ---------------------------------------------------------------------------
// The event bridge (Python gui/shortcut_keys.py)
// ---------------------------------------------------------------------------

/// The four modifier states of a key event, kept distinct —
/// Cmd/Meta is never merged into Ctrl (Audit 8 S04).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

impl KeyModifiers {
    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.cmd
    }

    fn list(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.ctrl {
            out.push("Ctrl");
        }
        if self.alt {
            out.push("Alt");
        }
        if self.shift {
            out.push("Shift");
        }
        if self.cmd {
            out.push("Cmd");
        }
        out
    }
}

/// Bare modifier keycodes are never a "key" (Python
/// _MODIFIER_KEYCODES): Shift, Alt, Control, Windows keys,
/// raw Control.
pub fn is_modifier_keycode(code: i32) -> bool {
    matches!(code, 306 | 307 | 308 | 393 | 394 | 396)
}

/// wx keycode -> canonical key token (Python KEYCODE_TO_TOKEN).
/// Matching is permissive in the same places Python is: numpad
/// digits report as the digit row, and shifted punctuation
/// reports as its unshifted token so a binding matches however
/// the platform reports the chord.
pub fn keycode_token(code: i32) -> Option<String> {
    let named = match code {
        32 => "Space",
        314 => "Left",
        316 => "Right",
        315 => "Up",
        317 => "Down",
        313 => "Home",
        312 => "End",
        366 => "PageUp",
        367 => "PageDown",
        322 => "Insert",
        127 => "Delete",
        8 => "Backspace",
        13 | 370 => "Enter",
        9 => "Tab",
        27 => "Escape",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    if (340..=363).contains(&code) {
        return Some(format!("F{}", code - 339));
    }
    if (65..=90).contains(&code) || (97..=122).contains(&code) {
        return char::from_u32(code as u32).map(|c| c.to_uppercase().collect::<String>());
    }
    if (48..=57).contains(&code) {
        return char::from_u32(code as u32).map(|c| c.to_string());
    }
    if (324..=333).contains(&code) {
        return Some((code - 324).to_string());
    }
    let punct = match code {
        44 | 60 => ",",
        46 | 62 => ".",
        47 | 63 => "/",
        45 | 95 => "-",
        61 | 43 => "=",
        59 | 58 => ";",
        39 | 34 => "'",
        91 | 123 => "[",
        93 | 125 => "]",
        92 | 124 => "\\",
        96 | 126 => "`",
        _ => "",
    };
    if !punct.is_empty() {
        return Some(punct.to_string());
    }
    None
}

/// Canonical accelerator for a key event, or None (Python
/// `event_to_accel`). With `require_modifier` (dispatch, and
/// Python's capture dialog), events with no modifier yield None
/// so plain typing/navigation is never captured — bare function
/// keys are the exception, since F2/F5-style bindings never
/// collide with typing.
pub fn event_accel(code: i32, mods: KeyModifiers, require_modifier: bool) -> Option<String> {
    if is_modifier_keycode(code) {
        return None;
    }
    let token = keycode_token(code)?;
    let is_function_key = token.len() >= 2
        && token.starts_with('F')
        && token[1..].chars().all(|c| c.is_ascii_digit());
    if require_modifier && !mods.any() && !is_function_key {
        return None;
    }
    Some(format_accel(&mods.list(), &token))
}

// ---------------------------------------------------------------------------
// Media arrows (Python gui/hotkeys.py::resolve_media_action)
// ---------------------------------------------------------------------------

/// The media action for an arrow key, or None. Ctrl+Arrow maps
/// on every platform; on macOS Alt(Option)+Arrow maps too,
/// because Ctrl+Left/Right are claimed by Mission Control.
/// Ctrl+Alt mixed, Shift, or Cmd disqualify the chord
/// (Ctrl+Shift+Arrow is the chapter shortcut; Cmd combos
/// belong to the system), and no modifier means plain
/// navigation, never a media action.
pub fn media_arrow_action(macos: bool, mods: KeyModifiers, code: i32) -> Option<&'static str> {
    if mods.shift || mods.cmd || (mods.ctrl && mods.alt) {
        return None;
    }
    let mut accel = mods.ctrl;
    if !accel && mods.alt && macos {
        accel = true;
    }
    if !accel {
        return None;
    }
    match code {
        314 => Some("seek_back"),
        316 => Some("seek_forward"),
        315 => Some("volume_up"),
        317 => Some("volume_down"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Binding-aware F1 (Python gui/help_context.py::HelpKeyFilter)
// ---------------------------------------------------------------------------

/// What an F1-or-help-key press should do, given the dispatch
/// map. Python's rule: the key bound to `help.user_guide` opens
/// context help; F1 itself also opens context help when no
/// other command owns it (F1 is the platform help key) — but
/// when the user has given F1 to another command, that command
/// wins and help does not intercept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpKeyAction {
    ContextHelp,
    Dispatch,
    Neither,
}

pub fn help_key_action(accel: &str, accel_map: &HashMap<String, String>) -> HelpKeyAction {
    match accel_map.get(accel).map(|s| s.as_str()) {
        Some("help.user_guide") => HelpKeyAction::ContextHelp,
        Some(_) => HelpKeyAction::Dispatch,
        None if accel == "F1" => HelpKeyAction::ContextHelp,
        None => HelpKeyAction::Neither,
    }
}

/// Commands that must not fire while an editable text field has
/// focus (Python `_SHORTCUT_TEXT_GUARDED`): the transport keys
/// and the Ctrl+digit filter shortcuts, which have always
/// deferred to a text field being edited.
pub fn text_guarded(command_id: &str) -> bool {
    matches!(
        command_id,
        "player.play_pause"
            | "player.stop"
            | "queue.next"
            | "queue.prev"
            | "filter.read_all"
            | "filter.read_unread"
            | "filter.read_read"
            | "filter.media_all"
            | "filter.media_with"
            | "filter.media_without"
    )
}

/// The shortcuts dialog's fixed rows (Python
/// KeyboardShortcutsDialog._fixed_rows): keys owned by a
/// control's own behavior or a native accelerator, listed
/// read-only so the dialog is a complete keyboard reference.
/// (keys, description) pairs in Python's order.
pub fn fixed_key_rows() -> &'static [(&'static str, &'static str)] {
    &[
        ("Enter", "Open or play the selected article"),
        ("Del", "Delete the selected article"),
        (
            "Shift+Del",
            "Delete the selected article without confirmation",
        ),
        ("Backspace", "Toggle read/unread in the article list"),
        ("Ctrl+R", "Refresh feeds (fixed alias)"),
        ("Ctrl+Left / Ctrl+Right", "Rewind / fast forward playback"),
        ("Ctrl+Up / Ctrl+Down", "Volume up / volume down"),
        (
            "Ctrl+Shift+Left / Ctrl+Shift+Right",
            "Previous / next chapter",
        ),
        ("Ctrl+F", "Find in the article reader"),
        (
            "F3 / Shift+F3",
            "Next / previous match in the article reader",
        ),
        ("F6 / Shift+F6", "Move between panes in the rich view"),
        (
            "Ctrl+X / Ctrl+C / Ctrl+V / Ctrl+A",
            "Cut / Copy / Paste / Select All in text fields",
        ),
    ]
}
