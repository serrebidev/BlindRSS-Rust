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

pub fn show_shortcuts(frame: &Frame, config_path: &Path) {
    let cfg = Config::load(config_path).unwrap_or_default();
    let overrides = overrides_of(&cfg);
    let effective = blindrss_core::shortcuts::effective(&overrides);
    let commands = blindrss_core::shortcuts::commands();

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
    let close_btn = Button::builder(&dialog).with_label("Close").build();
    btn_row.add(&edit_btn, 0, SizerFlag::All, 4);
    btn_row.add(&close_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(640, 520));

    let config_buf = config_path.to_path_buf();
    let dialog_edit = dialog;
    edit_btn.on_click(move |_| {
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
        let norm = blindrss_core::shortcuts::normalize_accelerator(&value);
        let mut cfg = Config::load(&config_buf).unwrap_or_default();
        cfg.set_path(
            &format!("keyboard_shortcuts.{}", cmd.id),
            serde_json::json!(norm),
        );
        let _ = cfg.save(&config_buf);
        list.set_item_text_by_column(sel as i64, 2, &norm);
    });
    let dialog_close = dialog;
    close_btn.on_click(move |_| {
        dialog_close.end_modal(5100);
    });
    dialog.show_modal();
    dialog.destroy();
}
