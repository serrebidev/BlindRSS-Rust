//! Shortcut registry (port of `core/shortcuts.py` semantics).
//!
//! The command table itself lives in `shortcuts_data.rs`
//! (extracted from Python). Effective accelerators = defaults
//! overlaid with the user's `keyboard_shortcuts` config map.
//! Normalisation follows Python's alias rules so an override
//! typed as "control+shift+r" lands as "Ctrl+Shift+R".

use std::collections::HashMap;

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

fn normalize_key(key: &str) -> String {
    let lower = key.trim().to_lowercase();
    match lower.as_str() {
        "space" => "Space".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" | "pgup" => "PageUp".to_string(),
        "pagedown" | "pgdn" => "PageDown".to_string(),
        "insert" | "ins" => "Insert".to_string(),
        "delete" | "del" => "Delete".to_string(),
        "backspace" | "back" => "Backspace".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "comma" => ",".to_string(),
        "period" | "dot" => ".".to_string(),
        "slash" => "/".to_string(),
        "minus" | "dash" => "-".to_string(),
        "equals" | "equal" => "=".to_string(),
        "plus" => "+".to_string(),
        "semicolon" => ";".to_string(),
        "quote" => "'".to_string(),
        "leftbracket" => "[".to_string(),
        "rightbracket" => "]".to_string(),
        "backslash" => "\\".to_string(),
        "backtick" | "grave" => "`".to_string(),
        _ => {
            // F-keys and single characters keep their shape; other
            // words are capitalised first-letter (f5 -> F5).
            if lower.len() > 1
                && lower.starts_with('f')
                && lower[1..].chars().all(|c| c.is_ascii_digit())
            {
                format!("F{}", &lower[1..])
            } else if key.trim().chars().count() == 1 {
                key.trim().to_uppercase()
            } else {
                let t = key.trim();
                let mut cs = t.chars();
                match cs.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                    None => String::new(),
                }
            }
        }
    }
}

/// Normalise an accelerator string to canonical modifier order
/// (Ctrl, Alt, Shift, Cmd) + key. Empty stays empty (unbound).
pub fn normalize_accelerator(accel: &str) -> String {
    let mut mods: Vec<&str> = Vec::new();
    let mut key = String::new();
    for part in accel.split('+') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        match p.to_lowercase().as_str() {
            "ctrl" | "control" | "ctl" => mods.push("Ctrl"),
            "alt" | "option" | "opt" => mods.push("Alt"),
            "shift" => mods.push("Shift"),
            "cmd" | "command" | "win" | "meta" | "super" => mods.push("Cmd"),
            _ => key = normalize_key(p),
        }
    }
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
    out.push_str(&key);
    out
}

/// Effective accelerators: id -> canonical accelerator (only
/// bound commands included).
pub fn effective(overrides: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (id, _cat, _label, def) in COMMANDS {
        let raw = overrides.get(*id).map(|s| s.as_str()).unwrap_or(def);
        let norm = normalize_accelerator(raw);
        if !norm.is_empty() {
            out.insert(id.to_string(), norm);
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
