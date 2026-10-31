//! Shortcuts dialog: the full command registry with defaults and
//! user overrides (config `keyboard_shortcuts`), editable per row.

use blindrss_core::config::Config;
use std::collections::HashMap;
use std::path::Path;
use wxdragon::prelude::*;

fn overrides_of(cfg: &Config) -> HashMap<String, String> {
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

/// Shortcut capture (Python's ShortcutCaptureDialog): press a
/// key combination; it is recorded in the same normalized
/// shape the main window's char hook dispatches on
/// ("Ctrl+Shift+X", "F5"). Escape cancels.
fn capture_shortcut(parent: &Dialog) -> Option<String> {
    let dialog = Dialog::builder(parent, "Capture Shortcut").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label("Press a key combination, or Escape to cancel.")
        .build();
    sizer.add(&label, 0, SizerFlag::All, 12);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(380, 140));
    let captured: std::rc::Rc<std::cell::RefCell<Option<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured2 = captured.clone();
    let d = dialog;
    dialog.bind_internal(EventType::CHAR_HOOK, move |ev| {
        let Some(code) = ev.get_key_code() else {
            ev.skip(true);
            return;
        };
        // Bare modifier presses are not a shortcut: wx codes
        // SHIFT 306, ALT 307, CONTROL 308.
        if matches!(code, 306..=308) {
            ev.skip(true);
            return;
        }
        if code == 27 {
            // Escape cancels.
            d.end_modal(5101);
            return;
        }
        let mut combo = String::new();
        if ev.control_down() {
            combo.push_str("Ctrl+");
        }
        if ev.alt_down() {
            combo.push_str("Alt+");
        }
        if ev.shift_down() {
            combo.push_str("Shift+");
        }
        if (340..=351).contains(&code) {
            combo.push_str(&format!("F{}", code - 339));
        } else if let Some(ch) = ev.get_unicode_key().and_then(|u| char::from_u32(u as u32)) {
            if ch.is_control() {
                ev.skip(true);
                return;
            }
            combo.push(ch.to_uppercase().next().unwrap_or(ch));
        } else if code > 0 {
            combo.push_str(&format!("{code}"));
        } else {
            ev.skip(true);
            return;
        }
        *captured2.borrow_mut() = Some(combo);
        d.end_modal(5100);
    });
    let ok = dialog.show_modal() == 5100;
    dialog.destroy();
    if ok { captured.borrow().clone() } else { None }
}

/// Save one command's override (shared by Edit and Capture).
fn save_override(config_path: &Path, command_id: &str, accel: &str) -> String {
    let norm = blindrss_core::shortcuts::normalize_accelerator(accel);
    let mut cfg = Config::load(config_path).unwrap_or_default();
    cfg.set_path(
        &format!("keyboard_shortcuts.{command_id}"),
        serde_json::json!(norm),
    );
    let _ = cfg.save(config_path);
    norm
}

pub fn show_shortcuts(frame: &Frame, config_path: &Path) {
    let cfg = Config::load(config_path).unwrap_or_default();
    let overrides = overrides_of(&cfg);
    let effective = blindrss_core::shortcuts::effective(&overrides);
    let commands = std::rc::Rc::new(blindrss_core::shortcuts::commands());

    let dialog = Dialog::builder(frame, "Keyboard Shortcuts").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListCtrl::builder(&dialog)
        .with_style(ListCtrlStyle::Report)
        .build();
    list.insert_column(0, "Command", ListColumnFormat::Left, 300);
    list.insert_column(1, "Category", ListColumnFormat::Left, 120);
    list.insert_column(2, "Shortcut", ListColumnFormat::Left, 160);
    for (i, cmd) in commands.iter().enumerate() {
        list.insert_item(i as i64, &cmd.label, None);
        list.set_item_text_by_column(i as i64, 1, &cmd.category);
        let current = effective.get(&cmd.id).cloned().unwrap_or_default();
        list.set_item_text_by_column(i as i64, 2, &current);
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let edit_btn = Button::builder(&dialog)
        .with_label("Edit Shortcut...")
        .build();
    let capture_btn = Button::builder(&dialog).with_label("Capture...").build();
    let close_btn = Button::builder(&dialog).with_label("Close").build();
    btn_row.add(&edit_btn, 0, SizerFlag::All, 4);
    btn_row.add(&capture_btn, 0, SizerFlag::All, 4);
    btn_row.add(&close_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(640, 520));

    let config_buf = config_path.to_path_buf();
    let dialog_edit = dialog;
    let commands_edit = commands.clone();
    edit_btn.on_click(move |_| {
        let commands = commands_edit.clone();
        let sel = list.get_first_selected_item();
        if sel < 0 {
            return;
        }
        let Some(cmd) = commands.get(sel as usize) else {
            return;
        };
        let current = effective.get(&cmd.id).cloned().unwrap_or_default();
        let entry = TextEntryDialog::builder(
            &dialog_edit,
            &format!("Shortcut for {} (blank = unbound):", cmd.label),
            "Edit Shortcut",
        )
        .with_default_value(&current)
        .build();
        if entry.show_modal() != 5100 {
            return;
        }
        let Some(value) = entry.get_value() else {
            return;
        };
        let norm = save_override(&config_buf, &cmd.id, &value);
        list.set_item_text_by_column(sel as i64, 2, &norm);
    });
    let config_buf2 = config_path.to_path_buf();
    let dialog_capture = dialog;
    let commands_capture = commands.clone();
    capture_btn.on_click(move |_| {
        let commands = commands_capture.clone();
        let sel = list.get_first_selected_item();
        if sel < 0 {
            return;
        }
        let Some(cmd) = commands.get(sel as usize) else {
            return;
        };
        if let Some(combo) = capture_shortcut(&dialog_capture) {
            let norm = save_override(&config_buf2, &cmd.id, &combo);
            list.set_item_text_by_column(sel as i64, 2, &norm);
        }
    });
    let dialog_close = dialog;
    close_btn.on_click(move |_| {
        dialog_close.end_modal(5100);
    });
    dialog.show_modal();
    dialog.destroy();
}
