//! Shortcuts dialog (Python's KeyboardShortcutsDialog): the
//! full command registry with effective accelerators, edited
//! through a press-to-capture sub-dialog, with conflict
//! reassignment and the fixed-keys reference rows appended
//! read-only (Audit 8 S05).
//!
//! Every change applies immediately: the whole overrides map
//! is written back as ONE flat `keyboard_shortcuts` object
//! through core's store (Audit 8 S01 — the old per-key dotted
//! `set_path` write nested the map and the main window's
//! dispatch rebuild silently ignored every edit).
//!
//! Accessibility pass (before this rewrite, per house rules):
//! native controls only, in Python's order — info text, the
//! list (accessible name "Keyboard shortcuts"), then Change /
//! Remove / Reset / Close with Python's labels and mnemonics.
//! Focus returns to the list after every change, as Python's
//! dialog restores it; the capture dialog names its readout
//! "Captured keyboard shortcut" so a screen reader announces
//! the candidate binding, and its OK stays disabled until a
//! valid combination has actually been captured.

use blindrss_core::config::Config;
use blindrss_core::shortcuts as sc;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Which list row a dialog row index is: a registry command
/// (index into commands()) or one of the read-only fixed rows
/// (identified by position, past the command rows).
enum Row {
    Command(usize),
    Fixed,
}

fn rows() -> Vec<Row> {
    let mut out: Vec<Row> = (0..sc::commands().len()).map(Row::Command).collect();
    out.extend((0..sc::fixed_key_rows().len()).map(|_| Row::Fixed));
    out
}

/// Persist the working overrides as the flat config map.
fn apply(config_path: &Path, working: &HashMap<String, String>) {
    let mut cfg = Config::load(config_path).unwrap_or_default();
    sc::store_overrides(&mut cfg, working);
    let _ = cfg.save(config_path);
}

/// Fill the list from the working overrides (resolved bindings;
/// unbound shows Python's "(none)").
fn refresh_list(list: &ListCtrl, working: &HashMap<String, String>) {
    let bindings = sc::resolve_bindings(working);
    let commands = sc::commands();
    for (i, cmd) in commands.iter().enumerate() {
        let accel = &bindings[&cmd.id];
        list.set_item_text_by_column(i as i64, 2, if accel.is_empty() { "(none)" } else { accel });
    }
    for (j, (keys, _desc)) in sc::fixed_key_rows().iter().enumerate() {
        list.set_item_text_by_column((commands.len() + j) as i64, 2, keys);
    }
}

/// Shortcut capture (Python's ShortcutCaptureDialog): the
/// candidate shows in a named readout as keys are pressed; OK
/// enables only once a valid combination (at least one
/// modifier, or a bare function key) is captured. Escape
/// cancels, Enter confirms. Deliberately NO F1 binding here:
/// the capture owns every key, F1 included (Python skips its
/// help filter while a capture dialog is up).
fn capture_shortcut(parent: &Dialog, command_label: &str) -> Option<String> {
    let dialog = Dialog::builder(parent, "Press Shortcut").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let prompt = StaticText::builder(&dialog)
        .with_label(&format!(
            "Press a key combination for {command_label}. Use at least one modifier \
             (Ctrl, Alt, Shift or Cmd), or a function key on its own. \
             Press Escape to cancel."
        ))
        .build();
    sizer.add(&prompt, 0, SizerFlag::All, 8);
    let readout = StaticText::builder(&dialog).with_label("").build();
    crate::msaa_names::set_name(&readout, "Captured keyboard shortcut");
    sizer.add(&readout, 0, SizerFlag::All, 8);
    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let ok_btn = Button::builder(&dialog).with_label("OK").build();
    ok_btn.enable(false);
    let cancel_btn = Button::builder(&dialog).with_label("Cancel").build();
    btn_row.add(&ok_btn, 0, SizerFlag::All, 4);
    btn_row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(460, 200));

    let captured: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    {
        let captured2 = captured.clone();
        let d = dialog;
        dialog.bind_internal(EventType::CHAR_HOOK, move |ev| {
            let code = ev.get_key_code().unwrap_or(0);
            if code == 27 {
                // Escape cancels.
                d.end_modal(5101);
                return;
            }
            if code == 13 && captured2.borrow().is_some() {
                // Enter confirms once something is captured.
                d.end_modal(5100);
                return;
            }
            let mods = sc::KeyModifiers {
                ctrl: ev.control_down(),
                alt: ev.alt_down(),
                shift: ev.shift_down(),
                cmd: ev.meta_down(),
            };
            if let Some(accel) = sc::event_accel(code, mods, true) {
                readout.set_label(&accel);
                *captured2.borrow_mut() = Some(accel);
                ok_btn.enable(true);
                ev.skip(false);
            } else {
                ev.skip(true);
            }
        });
    }
    let d_ok = dialog;
    ok_btn.on_click(move |_| d_ok.end_modal(5100));
    let d_cancel = dialog;
    cancel_btn.on_click(move |_| d_cancel.end_modal(5101));
    let ok = dialog.show_modal() == 5100;
    dialog.destroy();
    if ok { captured.borrow().clone() } else { None }
}

pub fn show_shortcuts(frame: &Frame, config_path: &Path) {
    let cfg = Config::load(config_path).unwrap_or_default();
    let working: Rc<RefCell<HashMap<String, String>>> =
        Rc::new(RefCell::new(sc::overrides_from_config(&cfg)));
    let commands: Rc<Vec<sc::Command>> = Rc::new(sc::commands());
    let rows: Rc<Vec<Row>> = Rc::new(rows());

    let dialog = Dialog::builder(frame, "Keyboard Shortcuts").build();
    crate::dialogs::bind_f1(frame, &dialog, "KeyboardShortcutsDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let info = StaticText::builder(&dialog)
        .with_label("Select a command and choose Change to set its shortcut.")
        .build();
    sizer.add(&info, 0, SizerFlag::All, 4);
    let list = ListCtrl::builder(&dialog)
        .with_style(ListCtrlStyle::Report | ListCtrlStyle::SingleSel)
        .build();
    crate::msaa_names::set_name(&list, "Keyboard shortcuts");
    list.insert_column(0, "Category", ListColumnFormat::Left, 130);
    list.insert_column(1, "Command", ListColumnFormat::Left, 230);
    list.insert_column(2, "Shortcut", ListColumnFormat::Left, 150);
    for (i, cmd) in commands.iter().enumerate() {
        list.insert_item(i as i64, &cmd.category, None);
        list.set_item_text_by_column(i as i64, 1, &cmd.label);
    }
    for (j, (keys, desc)) in sc::fixed_key_rows().iter().enumerate() {
        let i = (commands.len() + j) as i64;
        list.insert_item(i, "Fixed keys", None);
        list.set_item_text_by_column(i, 1, desc);
        list.set_item_text_by_column(i, 2, keys);
    }
    refresh_list(&list, &working.borrow());
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let change_btn = Button::builder(&dialog)
        .with_label("&Change Shortcut...")
        .build();
    let remove_btn = Button::builder(&dialog)
        .with_label("&Remove Shortcut")
        .build();
    let reset_btn = Button::builder(&dialog)
        .with_label("Reset All to &Defaults")
        .build();
    let close_btn = Button::builder(&dialog).with_label("Close").build();
    btn_row.add(&change_btn, 0, SizerFlag::All, 4);
    btn_row.add(&remove_btn, 0, SizerFlag::All, 4);
    btn_row.add(&reset_btn, 0, SizerFlag::All, 4);
    btn_row.add(&close_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 420));
    list.set_item_state(
        0,
        ListItemState::Selected | ListItemState::Focused,
        ListItemState::Selected | ListItemState::Focused,
    );

    // Button enablement follows the selection (Python
    // _sync_buttons): Change/Remove only on command rows, and
    // Remove only while the row has a binding.
    let sync_buttons: Rc<dyn Fn()> = {
        let working = working.clone();
        let rows = rows.clone();
        let commands = commands.clone();
        Rc::new(move || {
            let sel = list.get_first_selected_item();
            let editable = sel >= 0 && matches!(rows.get(sel as usize), Some(Row::Command(_)));
            change_btn.enable(editable);
            let bound = editable && {
                let Some(Row::Command(ci)) = rows.get(sel as usize) else {
                    unreachable!()
                };
                !sc::resolve_bindings(&working.borrow())[&commands[*ci].id].is_empty()
            };
            remove_btn.enable(bound);
        })
    };
    sync_buttons();
    {
        let sync = sync_buttons.clone();
        list.on_item_selected(move |_| sync());
    }

    // Change: capture, resolve conflicts by asking (Python's
    // YES/NO reassign), apply immediately, back to the list.
    let do_change: Rc<dyn Fn()> = {
        let working = working.clone();
        let config_buf = config_path.to_path_buf();
        let sync = sync_buttons.clone();
        let rows = rows.clone();
        let commands = commands.clone();
        Rc::new(move || {
            let sel = list.get_first_selected_item();
            if sel < 0 {
                return;
            }
            let Some(Row::Command(ci)) = rows.get(sel as usize) else {
                return;
            };
            let cmd = &commands[*ci];
            let Some(accel) = capture_shortcut(&dialog, &cmd.label) else {
                return;
            };
            let bindings = sc::resolve_bindings(&working.borrow());
            let conflict = commands.iter().find(|other| {
                other.id != cmd.id && bindings[&other.id] == accel && !accel.is_empty()
            });
            if let Some(other) = conflict {
                let md = MessageDialog::builder(
                    &dialog,
                    &format!(
                        "{} is already assigned to {}. Reassign it to {}?",
                        accel, other.label, cmd.label
                    ),
                    "Shortcut Conflict",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                .build();
                let answer = md.show_modal();
                md.destroy();
                if answer != ID_YES {
                    return;
                }
                working.borrow_mut().insert(other.id.clone(), String::new());
            }
            working.borrow_mut().insert(cmd.id.clone(), accel.clone());
            apply(&config_buf, &working.borrow());
            refresh_list(&list, &working.borrow());
            list.set_focus();
            sync();
        })
    };
    {
        let ch = do_change.clone();
        change_btn.on_click(move |_| ch());
    }
    {
        let ch = do_change.clone();
        list.on_item_activated(move |_| ch());
    }
    // Remove: explicit unbind ("" override), applied at once.
    {
        let working = working.clone();
        let config_buf = config_path.to_path_buf();
        let sync = sync_buttons.clone();
        let rows = rows.clone();
        let commands = commands.clone();
        remove_btn.on_click(move |_| {
            let sel = list.get_first_selected_item();
            if sel < 0 {
                return;
            }
            let Some(Row::Command(ci)) = rows.get(sel as usize) else {
                return;
            };
            working
                .borrow_mut()
                .insert(commands[*ci].id.clone(), String::new());
            apply(&config_buf, &working.borrow());
            refresh_list(&list, &working.borrow());
            list.set_focus();
            sync();
        });
    }
    // Reset All: confirmed, then every override goes.
    {
        let working = working.clone();
        let config_buf = config_path.to_path_buf();
        let sync = sync_buttons.clone();
        reset_btn.on_click(move |_| {
            let md = MessageDialog::builder(
                &dialog,
                "Reset all shortcuts to their defaults?",
                "Reset Shortcuts",
            )
            .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
            .build();
            let answer = md.show_modal();
            md.destroy();
            if answer != ID_YES {
                return;
            }
            working.borrow_mut().clear();
            apply(&config_buf, &working.borrow());
            refresh_list(&list, &working.borrow());
            list.set_focus();
            sync();
        });
    }
    let dialog_close = dialog;
    close_btn.on_click(move |_| {
        dialog_close.end_modal(5100);
    });
    dialog.show_modal();
    dialog.destroy();
}
