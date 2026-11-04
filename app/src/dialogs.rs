//! Modal dialogs: the smart-folder / filter-rule condition
//! editor, the filter-rules manager, feed properties, About,
//! an article window, and persistent-search management.
//!
//! The condition editor mirrors Python's SmartFolderDialog:
//! a name, an All/Any match choice, and condition rows built
//! from a field choice, an operator choice and a value. The
//! eight boolean rows ("Is unread" etc.) are self-contained
//! field choices, exactly like Python's BOOL_ROW_KEYS. The
//! rule JSON it produces is the shape core::rules evaluates:
//! {"match": "all"|"any", "conditions": [leaf, ...]}.

use blindrss_core::Db;
use blindrss_core::config::Config;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use wxdragon::prelude::*;

/// (key, label) text fields, Python's TEXT_FIELDS order.
const TEXT_FIELDS: [(&str, &str); 7] = [
    ("title", "Title"),
    ("content", "Content"),
    ("description", "Description"),
    ("author", "Author"),
    ("feed", "Feed / Publication"),
    ("url", "Link (URL)"),
    ("tag", "Tag / Site category"),
];

/// (label, field, value) self-contained boolean rows,
/// Python's BOOL_ROW_KEYS in dialog order.
const BOOL_ROWS: [(&str, &str, bool); 8] = [
    ("Is unread", "read", false),
    ("Is read", "read", true),
    ("Is favorite", "favorite", true),
    ("Is not favorite", "favorite", false),
    ("Has been opened", "opened", true),
    ("Has not been opened", "opened", false),
    ("Was updated since first fetch", "updated", true),
    ("Was not updated", "updated", false),
];

/// (key, label) text operators, Python's TEXT_OPS order.
const OPS: [(&str, &str); 4] = [
    ("contains", "contains"),
    ("not_contains", "does not contain"),
    ("equals", "equals"),
    ("starts_with", "starts with"),
];

/// A readable one-line summary of one condition leaf.
fn cond_label(v: &Value) -> String {
    if v.get("conditions").is_some() {
        return "(condition group)".to_string();
    }
    let field = v.get("field").and_then(|x| x.as_str()).unwrap_or("");
    if let Some((label, _, _)) = BOOL_ROWS
        .iter()
        .find(|(_, f, val)| *f == field && v.get("value").and_then(|x| x.as_bool()) == Some(*val))
    {
        return label.to_string();
    }
    let flabel = TEXT_FIELDS
        .iter()
        .find(|(k, _)| *k == field)
        .map(|(_, l)| *l)
        .unwrap_or(field);
    let op = v.get("op").and_then(|x| x.as_str()).unwrap_or("contains");
    let olabel = OPS
        .iter()
        .find(|(k, _)| *k == op)
        .map(|(_, l)| *l)
        .unwrap_or(op);
    let value = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
    format!("{flabel} {olabel} '{value}'")
}

/// The condition-editing block shared by the smart-folder and
/// filter-rule editors: match choice, condition list, and the
/// field/operator/value add row.
struct CondBlock {
    conds: Rc<RefCell<Vec<Value>>>,
    list: ListBox,
    match_choice: Choice,
}

impl CondBlock {
    fn build(dialog: &Dialog, sizer: &BoxSizer, rule0: &Value) -> CondBlock {
        let match_row = BoxSizer::builder(Orientation::Horizontal).build();
        let match_label = StaticText::builder(dialog)
            .with_label(&crate::tr("Show articles that match"))
            .build();
        let match_choice = Choice::builder(dialog).build();
        match_choice.append("All conditions (AND)");
        match_choice.append("Any condition (OR)");
        let any = rule0
            .get("match")
            .and_then(|v| v.as_str())
            .map(|m| m.eq_ignore_ascii_case("any"))
            .unwrap_or(false);
        match_choice.set_selection(if any { 1 } else { 0 });
        match_row.add(&match_label, 0, SizerFlag::All, 4);
        match_row.add(&match_choice, 0, SizerFlag::All, 4);
        sizer.add_sizer(&match_row, 0, SizerFlag::Expand | SizerFlag::All, 2);

        let initial: Vec<Value> = rule0
            .get("conditions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let conds = Rc::new(RefCell::new(initial));
        let list = ListBox::builder(dialog).build();
        sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);

        // Add-condition row: field choice, operator choice, value.
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let field_choice = Choice::builder(dialog).build();
        for (_, label) in TEXT_FIELDS {
            field_choice.append(label);
        }
        for (label, _, _) in BOOL_ROWS {
            field_choice.append(label);
        }
        field_choice.set_selection(0);
        let op_choice = Choice::builder(dialog).build();
        for (_, label) in OPS {
            op_choice.append(label);
        }
        op_choice.set_selection(0);
        let value_ctrl = TextCtrl::builder(dialog).build();
        let add_btn = Button::builder(dialog)
            .with_label(&crate::tr("Add Condition"))
            .build();
        let remove_btn = Button::builder(dialog)
            .with_label(&crate::tr("Remove Condition"))
            .build();
        row.add(&field_choice, 0, SizerFlag::All, 4);
        row.add(&op_choice, 0, SizerFlag::All, 4);
        row.add(&value_ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
        row.add(&add_btn, 0, SizerFlag::All, 4);
        row.add(&remove_btn, 0, SizerFlag::All, 4);
        sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);

        let block = CondBlock {
            conds,
            list,
            match_choice,
        };
        block.refresh_list();

        {
            let conds = block.conds.clone();
            let list = block.list;
            add_btn.on_click(move |_| {
                let fi = field_choice.get_selection().unwrap_or(0) as usize;
                let leaf = if fi < TEXT_FIELDS.len() {
                    let op = OPS[op_choice.get_selection().unwrap_or(0) as usize].0;
                    json!({
                        "field": TEXT_FIELDS[fi].0,
                        "op": op,
                        "value": value_ctrl.get_value(),
                    })
                } else {
                    let (_, field, val) = BOOL_ROWS[fi - TEXT_FIELDS.len()];
                    json!({"field": field, "value": val})
                };
                conds.borrow_mut().push(leaf);
                refresh_cond_list(&list, &conds.borrow());
            });
        }
        {
            let conds = block.conds.clone();
            let list = block.list;
            remove_btn.on_click(move |_| {
                if let Some(sel) = list.get_selection() {
                    let mut c = conds.borrow_mut();
                    if (sel as usize) < c.len() {
                        c.remove(sel as usize);
                    }
                    refresh_cond_list(&list, &c);
                }
            });
        }
        block
    }

    fn refresh_list(&self) {
        refresh_cond_list(&self.list, &self.conds.borrow());
    }

    /// The rule JSON for the current block state.
    fn rule(&self) -> Value {
        let any = self.match_choice.get_selection() == Some(1);
        json!({
            "match": if any { "any" } else { "all" },
            "conditions": self.conds.borrow().clone(),
        })
    }
}

fn refresh_cond_list(list: &ListBox, conds: &[Value]) {
    list.clear();
    for c in conds {
        list.append(&cond_label(c));
    }
}

/// Shared name row for the editors; returns the text control.
fn name_row(dialog: &Dialog, sizer: &BoxSizer, label: &str, value: &str) -> TextCtrl {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let text = StaticText::builder(dialog).with_label(label).build();
    let ctrl = TextCtrl::builder(dialog).build();
    ctrl.set_value(value);
    row.add(&text, 0, SizerFlag::All, 4);
    row.add(&ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    ctrl
}

/// OK / Cancel buttons wired to end the dialog with wx's
/// standard modal codes (5100 = OK, like TextEntryDialog).
fn ok_cancel(dialog: &Dialog, sizer: &BoxSizer) {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let ok_btn = Button::builder(dialog).with_label(&crate::tr("OK")).build();
    let cancel_btn = Button::builder(dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    row.add(&ok_btn, 0, SizerFlag::All, 4);
    row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::All, 2);
    let d = *dialog;
    ok_btn.on_click(move |_| d.end_modal(5100));
    let d = *dialog;
    cancel_btn.on_click(move |_| d.end_modal(5101));
}

/// Edit a smart folder's name + rule. Returns the new
/// (name, rule) when the user confirms, None on cancel.
pub fn edit_smart_folder(
    frame: &Frame,
    title: &str,
    name0: &str,
    rule0: &Value,
) -> Option<(String, Value)> {
    let dialog = Dialog::builder(frame, title).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let name_ctrl = name_row(&dialog, &sizer, "Folder name:", name0);
    let block = CondBlock::build(&dialog, &sizer, rule0);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(640, 480));
    let result = if dialog.show_modal() == 5100 {
        let name = name_ctrl.get_value();
        if name.trim().is_empty() {
            None
        } else {
            Some((name.trim().to_string(), block.rule()))
        }
    } else {
        None
    };
    dialog.destroy();
    result
}

/// One filter rule as the editor returns it.
pub struct FilterRuleEdit {
    pub name: String,
    pub rule: Value,
    pub actions: Value,
    pub enabled: bool,
    pub stop: bool,
}

/// Edit one filter rule: name, conditions, and the action
/// checkboxes (Python's FilterRuleEditorDialog).
pub fn edit_filter_rule(
    frame: &Frame,
    existing: Option<&blindrss_core::rules::FilterRule>,
) -> Option<FilterRuleEdit> {
    let (name0, rule0, actions0, enabled0, stop0) = match existing {
        Some(r) => (
            r.name.clone(),
            r.rule.clone(),
            r.actions.clone(),
            r.enabled,
            r.stop,
        ),
        None => (
            String::new(),
            json!({"match": "all", "conditions": []}),
            json!({}),
            true,
            false,
        ),
    };
    let dialog = Dialog::builder(frame, "Filter Rule").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let name_ctrl = name_row(&dialog, &sizer, "Rule name:", &name0);
    let block = CondBlock::build(&dialog, &sizer, &rule0);

    let act_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("When an article matches:"))
        .build();
    sizer.add(&act_label, 0, SizerFlag::All, 4);
    let flag = |key: &str| actions0.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    let mk_check = |label: &str, on: bool| {
        let cb = CheckBox::builder(&dialog).with_label(label).build();
        cb.set_value(on);
        sizer.add(&cb, 0, SizerFlag::All, 2);
        cb
    };
    let cb_read = mk_check("Mark as read", flag("mark_read"));
    let cb_fav = mk_check("Mark as favorite", flag("mark_favorite"));
    let cb_delete = mk_check("Delete the article", flag("delete"));
    let cb_skip = mk_check("Skip its notification", flag("skip_notification"));
    let move0 = actions0
        .get("move")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let move_ctrl = name_row(&dialog, &sizer, "Move to category (blank = none):", &move0);
    let label0 = actions0
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let label_ctrl = name_row(&dialog, &sizer, "Add label (blank = none):", &label0);
    let cb_enabled = mk_check("Rule enabled", enabled0);
    let cb_stop = mk_check("Stop processing further rules", stop0);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(640, 640));
    let result = if dialog.show_modal() == 5100 {
        let name = name_ctrl.get_value();
        if name.trim().is_empty() {
            None
        } else {
            let mut actions = serde_json::Map::new();
            actions.insert("mark_read".into(), json!(cb_read.get_value()));
            actions.insert("mark_favorite".into(), json!(cb_fav.get_value()));
            actions.insert("delete".into(), json!(cb_delete.get_value()));
            actions.insert("skip_notification".into(), json!(cb_skip.get_value()));
            let mv = move_ctrl.get_value();
            if !mv.trim().is_empty() {
                actions.insert("move".into(), json!(mv.trim()));
            }
            let lb = label_ctrl.get_value();
            if !lb.trim().is_empty() {
                actions.insert("label".into(), json!(lb.trim()));
            }
            Some(FilterRuleEdit {
                name: name.trim().to_string(),
                rule: block.rule(),
                actions: Value::Object(actions),
                enabled: cb_enabled.get_value(),
                stop: cb_stop.get_value(),
            })
        }
    } else {
        None
    };
    dialog.destroy();
    result
}

/// The filter-rules manager (Python's FilterRulesDialog):
/// list, New / Edit / Delete, Move Up / Down (the pipeline
/// order), and Apply to Existing Articles. Works directly on
/// the database; modal and self-contained.
/// F1 inside a dialog opens the guide at the section documenting
/// that dialog (Python's DIALOG_TOPICS; the kind strings are the
/// Python class names, reused verbatim).
pub fn bind_f1(frame: &Frame, dialog: &Dialog, kind: &'static str) {
    let parent = *frame;
    dialog.bind_internal(EventType::CHAR_HOOK, move |ev| {
        if ev.get_key_code() == Some(340) {
            // Kind is a Python dialog class name when DIALOG_TOPICS
            // has it, else a guide topic id used directly.
            let topic = blindrss_core::help_topics::topic_for_dialog(kind).unwrap_or_else(|| {
                blindrss_core::help_topics::TOPICS
                    .iter()
                    .find(|(id, _)| *id == kind)
                    .map(|(id, _)| *id)
                    .unwrap_or(blindrss_core::help_topics::DEFAULT_TOPIC)
            });
            crate::help::show_help_topic(&parent, &crate::ui_language(), Some(topic));
            ev.skip(false);
        } else {
            ev.skip(true);
        }
    });
}

pub fn show_filter_rules(frame: &Frame, db: &Db) {
    let dialog = Dialog::builder(frame, "Filter Rules").build();
    bind_f1(frame, &dialog, "FilterRulesDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListCtrl::builder(&dialog)
        .with_style(ListCtrlStyle::Report | ListCtrlStyle::SingleSel)
        .build();
    list.insert_column(0, "Rule", ListColumnFormat::Left, 260);
    list.insert_column(1, "Enabled", ListColumnFormat::Left, 80);
    list.insert_column(2, "Stop", ListColumnFormat::Left, 60);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let status = StaticText::builder(&dialog).with_label("").build();
    sizer.add(&status, 0, SizerFlag::All, 4);

    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let mk = |label: &str| {
        let b = Button::builder(&dialog).with_label(label).build();
        row.add(&b, 0, SizerFlag::All, 4);
        b
    };
    let new_btn = mk("New...");
    let edit_btn = mk("Edit...");
    let del_btn = mk("Delete");
    let up_btn = mk("Move Up");
    let down_btn = mk("Move Down");
    let apply_btn = mk("Apply to Existing Articles");
    let close_btn = mk("Close");
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    // The dialog outlives the Db borrow problem by re-opening
    // the database per action through the shared path helper.
    let db_path = crate::db_path();
    let reload = {
        move || {
            list.delete_all_items();
            if let Ok(db) = Db::open(&db_path)
                && let Ok(rules) = db.filter_rules()
            {
                for (i, r) in rules.iter().enumerate() {
                    list.insert_item(i as i64, &r.name, None);
                    list.set_item_text_by_column(i as i64, 1, if r.enabled { "Yes" } else { "No" });
                    list.set_item_text_by_column(i as i64, 2, if r.stop { "Yes" } else { "No" });
                }
            }
        }
    };
    let reload = Rc::new(reload);
    reload();

    let selected_id = {
        move || -> Option<String> {
            let sel = list.get_first_selected_item();
            if sel < 0 {
                return None;
            }
            let db = Db::open(&crate::db_path()).ok()?;
            let rules = db.filter_rules().ok()?;
            rules.get(sel as usize).map(|r| r.id.clone())
        }
    };
    let selected_id = Rc::new(selected_id);

    {
        let reload = reload.clone();
        let frame2 = *frame;
        new_btn.on_click(move |_| {
            if let Some(edit) = edit_filter_rule(&frame2, None) {
                if let Ok(db) = Db::open(&crate::db_path()) {
                    let _ = db.create_filter_rule(
                        &edit.name,
                        &edit.rule.to_string(),
                        &edit.actions.to_string(),
                        edit.enabled,
                        edit.stop,
                    );
                }
                reload();
            }
        });
    }
    {
        let reload = reload.clone();
        let selected_id = selected_id.clone();
        let frame2 = *frame;
        edit_btn.on_click(move |_| {
            let Some(id) = selected_id() else { return };
            let Ok(db) = Db::open(&crate::db_path()) else {
                return;
            };
            let Ok(rules) = db.filter_rules() else { return };
            let Some(rule) = rules.iter().find(|r| r.id == id) else {
                return;
            };
            if let Some(edit) = edit_filter_rule(&frame2, Some(rule)) {
                let _ = db.update_filter_rule(
                    &id,
                    &edit.name,
                    &edit.rule.to_string(),
                    &edit.actions.to_string(),
                    edit.enabled,
                    edit.stop,
                );
                reload();
            }
        });
    }
    {
        let reload = reload.clone();
        let selected_id = selected_id.clone();
        del_btn.on_click(move |_| {
            if let Some(id) = selected_id() {
                if let Ok(db) = Db::open(&crate::db_path()) {
                    let _ = db.delete_filter_rule(&id);
                }
                reload();
            }
        });
    }
    let move_rule = {
        let reload = reload.clone();
        move |delta: i32| {
            let sel = list.get_first_selected_item();
            if sel < 0 {
                return;
            }
            let target = sel + delta;
            if target < 0 {
                return;
            }
            let Ok(db) = Db::open(&crate::db_path()) else {
                return;
            };
            let Ok(rules) = db.filter_rules() else { return };
            if target as usize >= rules.len() {
                return;
            }
            let mut ids: Vec<String> = rules.iter().map(|r| r.id.clone()).collect();
            ids.swap(sel as usize, target as usize);
            let _ = db.reorder_filter_rules(&ids);
            reload();
            list.set_item_state(
                target as i64,
                ListItemState::Selected,
                ListItemState::Selected,
            );
        }
    };
    let move_rule = Rc::new(move_rule);
    {
        let m = move_rule.clone();
        up_btn.on_click(move |_| m(-1));
    }
    {
        let m = move_rule.clone();
        down_btn.on_click(move |_| m(1));
    }
    {
        apply_btn.on_click(move |_| match Db::open(&crate::db_path()) {
            Ok(db) => match blindrss_core::refresh::apply_rules_to_existing(
                &db,
                blindrss_core::config::Config::load(&crate::config_path())
                    .map(|c| c.get_str("delete_behavior", "deleted").to_string())
                    .unwrap_or_else(|_| "deleted".to_string())
                    .as_str(),
            ) {
                Ok(n) => status.set_label(&format!("Rules applied: {n} article(s) changed")),
                Err(e) => status.set_label(&format!("Apply failed: {e}")),
            },
            Err(e) => status.set_label(&format!("Database error: {e}")),
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(720, 520));
    let _ = db; // the manager works through fresh connections
    dialog.show_modal();
    dialog.destroy();
}

/// Feed properties (Python's FeedPropertiesDialog): custom
/// title, feed URL, and category. Returns true when changed.
/// Category picker (Python ChooseCategoryDialog): pick an
/// existing category from a list, or create a new one inline
/// without leaving the dialog. The list starts with
/// "Uncategorized". Returns Some(path) for a category, Some("")
/// for Uncategorized, None when cancelled.
pub fn show_category_picker(
    frame: &Frame,
    db: &Db,
    current: &str,
    title: &str,
    prompt: &str,
) -> Option<String> {
    let dialog = Dialog::builder(frame, title).build();
    bind_f1(frame, &dialog, "ChooseCategoryDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog).with_label(prompt).build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = ListBox::builder(&dialog).build();
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    // ids[0] is "" (Uncategorized); the rest are category paths.
    let ids: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new({
        let mut v = vec![String::new()];
        if let Ok(cats) = db.categories() {
            v.extend(cats.into_iter().map(|c| c.title));
        }
        v
    }));
    let reload = {
        let ids = ids.clone();
        move || {
            list.clear();
            for (i, id) in ids.borrow().iter().enumerate() {
                list.append(if i == 0 { "Uncategorized" } else { id });
            }
        }
    };
    let reload = Rc::new(reload);
    reload();
    let initial = ids
        .borrow()
        .iter()
        .position(|id| id == current)
        .unwrap_or(0);
    list.set_selection(initial as u32, true);
    let new_btn = Button::builder(&dialog)
        .with_label(&crate::tr("New Category..."))
        .build();
    sizer.add(&new_btn, 0, SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(480, 420));
    {
        let ids = ids.clone();
        let reload = reload.clone();
        let d = dialog;
        new_btn.on_click(move |_| {
            let entry = TextEntryDialog::builder(&d, "Category name:", "New Category").build();
            if entry.show_modal() != 5100 {
                return;
            }
            let name = entry.get_value().unwrap_or_default();
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            // add_category stores the full path; mirror remote
            // providers the way the main window's add does is
            // the caller's job — this dialog only writes the Db.
            if let Ok(db) = Db::open(&crate::db_path()) {
                let _ = db.add_category(&name);
            }
            let mut ids_mut = ids.borrow_mut();
            if !ids_mut.contains(&name) {
                ids_mut.push(name.clone());
            }
            let pos = ids_mut.iter().position(|id| id == &name).unwrap_or(0);
            drop(ids_mut);
            reload();
            list.set_selection(pos as u32, true);
            list.set_focus();
        });
    }
    let result = if dialog.show_modal() == 5100 {
        list.get_selection()
            .and_then(|sel| ids.borrow().get(sel as usize).cloned())
    } else {
        None
    };
    dialog.destroy();
    result
}

/// Categories that may become the parent of `path` (Python's
/// `_eligible_parent_categories`): every stored category except
/// the category itself, its own descendants (which cannot
/// become their own ancestors) and the Uncategorized sentinel.
fn category_parent_choices(path: &str, all: &[String]) -> Vec<String> {
    let descendant_prefix = format!("{path}{}", blindrss_core::CATEGORY_PATH_SEP);
    all.iter()
        .filter(|c| {
            c.as_str() != path
                && !c.starts_with(&descendant_prefix)
                && !c.trim().eq_ignore_ascii_case("uncategorized")
        })
        .cloned()
        .collect()
}

/// The full path for a category after a Category Properties
/// edit: the (sanitized) leaf under the chosen parent, or the
/// leaf alone at top level. Path segments are joined with
/// CATEGORY_PATH_SEP (" / "), Python's storage separator.
fn category_edited_path(leaf: &str, parent: Option<&str>) -> String {
    blindrss_core::make_category_path(parent, &blindrss_core::sanitize_category_leaf(leaf))
}

/// Split a stored category path into (leaf, parent path).
fn category_leaf_parent(path: &str) -> (String, Option<String>) {
    (
        blindrss_core::category_display_leaf(path.trim()),
        blindrss_core::category_parent_path(path.trim()),
    )
}

/// Category Properties (Python's CategoryPropertiesDialog,
/// Batch 41a / Audit 6 G1.6): rename the leaf plus a
/// parent-category picker whose first entry is
/// "(None - Top Level)". Returns the new full path on OK
/// (which may equal the old path), None on cancel. Applying
/// the rename/move is the caller's job (Db::rename_category
/// moves the whole subtree, which covers both edits).
pub fn show_category_properties(frame: &Frame, db: &Db, category_path: &str) -> Option<String> {
    let dialog = Dialog::builder(frame, "Category Properties").build();
    bind_f1(frame, &dialog, "CategoryPropertiesDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let (leaf, current_parent) = category_leaf_parent(category_path);
    let name_ctrl = name_row(&dialog, &sizer, "Name:", &leaf);
    let parents: Vec<String> = db
        .categories()
        .map(|cats| {
            category_parent_choices(
                category_path,
                &cats.into_iter().map(|c| c.title).collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    let parent_label = StaticText::builder(&dialog).with_label("Category:").build();
    sizer.add(&parent_label, 0, SizerFlag::All, 4);
    let parent_choice = Choice::builder(&dialog).build();
    parent_choice.append("(None - Top Level)");
    for p in &parents {
        parent_choice.append(p);
    }
    let initial = current_parent
        .as_ref()
        .and_then(|cur| parents.iter().position(|p| p == cur))
        .map(|pos| pos + 1)
        .unwrap_or(0);
    parent_choice.set_selection(initial as u32);
    sizer.add(&parent_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(460, 220));
    if dialog.show_modal() != 5100 {
        dialog.destroy();
        return None;
    }
    let name = name_ctrl.get_value();
    let leaf = if name.trim().is_empty() {
        leaf
    } else {
        name.trim().to_string()
    };
    let sel = parent_choice.get_selection().unwrap_or(0) as usize;
    let parent = if sel > 0 {
        parents.get(sel - 1).map(|s| s.as_str())
    } else {
        None
    };
    let new_path = category_edited_path(&leaf, parent);
    dialog.destroy();
    Some(new_path)
}

pub fn show_feed_properties(frame: &Frame, db: &Db, feed: &blindrss_core::Feed) -> bool {
    let dialog = Dialog::builder(frame, "Feed Properties").build();
    bind_f1(frame, &dialog, "FeedPropertiesDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let title_ctrl = name_row(&dialog, &sizer, "Title:", &feed.title);
    let url_ctrl = name_row(&dialog, &sizer, "Feed URL:", &feed.url);
    let cat0 = feed.category.clone().unwrap_or_default();
    let cat_ctrl = name_row(&dialog, &sizer, "Category (blank = none):", &cat0);
    // Choose... opens the category picker (existing categories
    // plus inline creation) and fills the field.
    let choose_cat_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Choose Category..."))
        .build();
    sizer.add(&choose_cat_btn, 0, SizerFlag::All, 4);
    {
        let frame_cp = *frame;
        let cat_ctrl2 = cat_ctrl;
        choose_cat_btn.on_click(move |_| {
            if let Ok(db2) = Db::open(&crate::db_path()) {
                let current = cat_ctrl2.get_value();
                if let Some(picked) = show_category_picker(
                    &frame_cp,
                    &db2,
                    current.trim(),
                    "Choose Category",
                    "Category:",
                ) {
                    cat_ctrl2.set_value(&picked);
                }
            }
        });
    }
    // Per-feed encoding override (feeds.feed_settings.encoding,
    // issue #75): wins over header/meta charset detection when
    // decoding this feed's article pages.
    let encoding0 = Db::open(&crate::db_path())
        .ok()
        .and_then(|db| db.feed_settings(&feed.id).ok())
        .and_then(|s| {
            s.get("encoding")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    let enc_ctrl = name_row(
        &dialog,
        &sizer,
        "Encoding override (blank = auto-detect):",
        &encoding0,
    );
    let reset_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Reset Title to Feed's Own"))
        .build();
    sizer.add(&reset_btn, 0, SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(560, 380));
    let feed_id = feed.id.clone();
    let reset = Rc::new(RefCell::new(false));
    {
        let reset = reset.clone();
        let title_ctrl2 = title_ctrl;
        let fid = feed_id.clone();
        reset_btn.on_click(move |_| {
            if let Ok(db) = Db::open(&crate::db_path()) {
                // The feed's own title comes from a fresh fetch in
                // Python; here the stored pre-custom title is not
                // kept, so reset re-fetches the feed document.
                if let Ok(feeds) = db.feeds()
                    && let Some(f) = feeds.iter().find(|f| f.id == fid)
                    && let Ok(parsed) = blindrss_core::feeds::fetch_and_parse(&f.url)
                {
                    let _ = db.reset_feed_title(&fid, &parsed.title);
                    title_ctrl2.set_value(&parsed.title);
                    *reset.borrow_mut() = true;
                }
            }
        });
    }
    let mut changed = false;
    if dialog.show_modal() == 5100 {
        let title = title_ctrl.get_value();
        let url = url_ctrl.get_value();
        let _ = db.update_feed(
            &feed_id,
            if title.trim().is_empty() {
                None
            } else {
                Some(title.trim())
            },
            if url.trim().is_empty() {
                None
            } else {
                Some(url.trim())
            },
        );
        let cat = cat_ctrl.get_value();
        let _ = db.set_feed_category(
            &feed_id,
            if cat.trim().is_empty() {
                None
            } else {
                Some(cat.trim())
            },
        );
        let enc = enc_ctrl.get_value();
        let _ = db.set_feed_settings(&feed_id, &serde_json::json!({"encoding": enc.trim()}));
        changed = true;
    }
    dialog.destroy();
    changed || *reset.borrow()
}

/// About BlindRSS (Python's AboutDialog content).
pub fn show_about(frame: &Frame) {
    let dialog = Dialog::builder(frame, "About BlindRSS").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let lines = [
        format!("BlindRSS {}", env!("CARGO_PKG_VERSION")),
        "Copyright (c) 2024-2026 serrebidev and contributors".to_string(),
        "An accessible RSS reader for blind users.".to_string(),
        "https://github.com/serrebidev/BlindRSS-Rust".to_string(),
    ];
    for line in &lines {
        let t = StaticText::builder(&dialog).with_label(line).build();
        sizer.add(&t, 0, SizerFlag::All, 4);
    }
    let ok_btn = Button::builder(&dialog)
        .with_label(&crate::tr("OK"))
        .build();
    sizer.add(&ok_btn, 0, SizerFlag::All, 4);
    let d = dialog;
    ok_btn.on_click(move |_| d.end_modal(5100));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(480, 260));
    dialog.show_modal();
    dialog.destroy();
}

/// A standalone article window (Python's ArticleWindow): the
/// article text in its own frame, non-modal.
pub fn show_article_window(title: &str, body: &str) {
    crate::article_window::open(title, body, None);
}

/// Article change history (Python's on_view_article_history):
/// every recorded version of the article, newest first, with
/// the selected version's text beside the list.
pub fn show_article_history(frame: &Frame, db: &Db, article: &blindrss_core::Article) {
    let versions = db.article_versions(&article.id).unwrap_or_default();
    if versions.is_empty() {
        let md = MessageDialog::builder(
            frame,
            "No change history for this article.",
            "Article History",
        )
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
        .build();
        md.show_modal();
        md.destroy();
        return;
    }
    let total = versions.len();
    let dialog = Dialog::builder(frame, "Article History").build();
    bind_f1(frame, &dialog, "ArticleHistoryDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let heading = StaticText::builder(&dialog)
        .with_label(&format!("{total} versions of: {}", article.title))
        .build();
    sizer.add(&heading, 0, SizerFlag::All, 4);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let list = ListBox::builder(&dialog).build();
    for (i, v) in versions.iter().enumerate() {
        let num = total - i;
        let suffix = if i == 0 {
            " (current)"
        } else if num == 1 {
            " (original)"
        } else {
            ""
        };
        list.append(&format!(
            "Version {num} - {}{suffix}",
            crate::fmt_ts(Some(v.captured_at))
        ));
    }
    row.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let text = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    row.add(&text, 2, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&row, 1, SizerFlag::Expand | SizerFlag::All, 2);
    let close_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Close"))
        .build();
    sizer.add(&close_btn, 0, SizerFlag::All, 4);
    let versions = Rc::new(versions);
    let show_version: Rc<dyn Fn(usize)> = {
        let versions = versions.clone();
        let keep_alt = {
            let cfg =
                blindrss_core::config::Config::load(&crate::config_path()).unwrap_or_default();
            db.feed_show_images(&article.feed_id)
                .ok()
                .flatten()
                .unwrap_or_else(|| cfg.get_bool("show_image_alt", false))
        };
        Rc::new(move |idx: usize| {
            if let Some(v) = versions.get(idx) {
                let body = blindrss_core::extract::html_to_text_opts(&v.content, keep_alt);
                text.set_value(&format!("{}\n\n{}", v.title, body));
            }
        })
    };
    show_version(0);
    list.set_selection(0, true);
    {
        let show_version = show_version.clone();
        list.on_selection_changed(move |_| {
            if let Some(sel) = list.get_selection() {
                show_version(sel as usize);
            }
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5100));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(720, 520));
    list.set_focus();
    dialog.show_modal();
    dialog.destroy();
}

/// Article column layout editor (Python's ArticleColumnsPanel
/// in Settings): list order is column order, checked is shown,
/// Move Up / Move Down reorder, Title stays first and visible.
/// Writes config "article_columns" on OK.
pub fn show_article_columns(frame: &Frame, config_path: &Path) {
    use blindrss_core::article_columns::{ColumnLayout, layout_from_config, resolve_layout};
    let cfg = Config::load(config_path).unwrap_or_default();
    let state: Rc<RefCell<Vec<ColumnLayout>>> = Rc::new(RefCell::new(layout_from_config(&cfg)));
    let dialog = Dialog::builder(frame, "Article Columns").build();
    bind_f1(frame, &dialog, "list-headers");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let help = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Columns (checked = shown). Use Move Up / Move Down to reorder:",
        ))
        .build();
    sizer.add(&help, 0, SizerFlag::All, 4);
    let list = CheckListBox::builder(&dialog).build();
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let note = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Title is always the first column and cannot be hidden.",
        ))
        .build();
    sizer.add(&note, 0, SizerFlag::All, 4);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let up_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Move Up"))
        .build();
    let down_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Move Down"))
        .build();
    let reset_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Reset All to Defaults"))
        .build();
    let ok_btn = Button::builder(&dialog)
        .with_label(&crate::tr("OK"))
        .build();
    let cancel_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    row.add(&up_btn, 0, SizerFlag::All, 4);
    row.add(&down_btn, 0, SizerFlag::All, 4);
    row.add(&reset_btn, 0, SizerFlag::All, 4);
    row.add(&ok_btn, 0, SizerFlag::All, 4);
    row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::All, 2);

    let rebuild: Rc<dyn Fn(Option<usize>)> = {
        let state = state.clone();
        Rc::new(move |select: Option<usize>| {
            let st = state.borrow();
            list.clear();
            for (i, c) in st.iter().enumerate() {
                if c.key == blindrss_core::article_columns::PINNED_KEY {
                    list.append(&format!("{} (always first)", c.label));
                } else {
                    list.append(&c.label);
                }
                list.check(i as u32, c.visible);
            }
            if let Some(sel) = select.or(Some(0))
                && sel < st.len()
            {
                list.set_selection(sel as u32, true);
            }
        })
    };
    // Pull the widget's check states back into the layout
    // (Title's check is forced on, like Python).
    let sync_checks: Rc<dyn Fn()> = {
        let state = state.clone();
        Rc::new(move || {
            let mut st = state.borrow_mut();
            for (i, c) in st.iter_mut().enumerate() {
                c.visible = if c.key == blindrss_core::article_columns::PINNED_KEY {
                    true
                } else {
                    list.is_checked(i as u32)
                };
            }
        })
    };
    rebuild(Some(0));
    {
        let state = state.clone();
        let rebuild = rebuild.clone();
        let sync_checks = sync_checks.clone();
        up_btn.on_click(move |_| {
            sync_checks();
            let Some(sel) = list.get_selection().map(|s| s as usize) else {
                return;
            };
            // Title (row 0) never moves; nothing moves above it.
            if sel < 2 {
                return;
            }
            state.borrow_mut().swap(sel, sel - 1);
            rebuild(Some(sel - 1));
            list.set_focus();
        });
    }
    {
        let state = state.clone();
        let rebuild = rebuild.clone();
        let sync_checks = sync_checks.clone();
        down_btn.on_click(move |_| {
            sync_checks();
            let Some(sel) = list.get_selection().map(|s| s as usize) else {
                return;
            };
            let len = state.borrow().len();
            if sel == 0 || sel + 1 >= len {
                return;
            }
            state.borrow_mut().swap(sel, sel + 1);
            rebuild(Some(sel + 1));
            list.set_focus();
        });
    }
    {
        let state = state.clone();
        let rebuild = rebuild.clone();
        reset_btn.on_click(move |_| {
            *state.borrow_mut() = resolve_layout(None);
            rebuild(Some(0));
            list.set_focus();
        });
    }
    let saved: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    {
        let saved = saved.clone();
        let sync_checks = sync_checks.clone();
        let d = dialog;
        ok_btn.on_click(move |_| {
            sync_checks();
            *saved.borrow_mut() = true;
            d.end_modal(5100);
        });
    }
    let d = dialog;
    cancel_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(480, 460));
    list.set_focus();
    if dialog.show_modal() == 5100 && *saved.borrow() {
        let items: Vec<Value> = state
            .borrow()
            .iter()
            .map(|c| json!({"key": c.key, "visible": c.visible, "width": c.width}))
            .collect();
        let mut cfg = Config::load(config_path).unwrap_or_default();
        cfg.set_path("article_columns", Value::Array(items));
        let _ = cfg.save(config_path);
    }
    dialog.destroy();
}

/// Video Search (Python's YtdlpGlobalSearchDialog): search
/// videos across yt-dlp sites, then Play / Subscribe / Copy
/// URL a result. Returns ("play" | "subscribe", result) for
/// the caller to act on; Copy URL is handled in place.
/// Per-site searches run on their own threads (Python caps
/// concurrency at 8; the site list itself is the cap here)
/// and stream into the list through a timer drain, deduped
/// by canonical key. Load More widens the per-site limit by
/// 80 and re-runs, matching Python's step.
pub fn show_video_search(
    frame: &Frame,
    config_path: &Path,
) -> Option<(&'static str, blindrss_core::videosearch::VideoResult)> {
    use blindrss_core::videosearch::{VideoResult, canonical_key, search_site, searchable_sites};
    let cfg = Config::load(config_path).unwrap_or_default();
    let sites = searchable_sites(cfg.get_bool("enable_adult_search", false));
    let dialog = Dialog::builder(frame, "Video Search").build();
    bind_f1(frame, &dialog, "YtdlpGlobalSearchDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let query_ctrl = name_row(&dialog, &sizer, "Search:", "");
    let opt_row = BoxSizer::builder(Orientation::Horizontal).build();
    let scope_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Search Sites:"))
        .build();
    opt_row.add(&scope_label, 0, SizerFlag::All, 4);
    let mut scope_items: Vec<&str> = vec!["All sites"];
    scope_items.extend(sites.iter().map(|s| s.label));
    let scope_choice = Choice::builder(&dialog).build();
    for item in &scope_items {
        scope_choice.append(item);
    }
    scope_choice.set_selection(0);
    opt_row.add(&scope_choice, 1, SizerFlag::All, 4);
    let sort_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Sort by:"))
        .build();
    opt_row.add(&sort_label, 0, SizerFlag::All, 4);
    let sort_choice = Choice::builder(&dialog).build();
    for item in ["Relevance", "Title", "Site", "Plays"] {
        sort_choice.append(item);
    }
    sort_choice.set_selection(0);
    opt_row.add(&sort_choice, 0, SizerFlag::All, 4);
    sizer.add_sizer(&opt_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let list = ListCtrl::builder(&dialog)
        .with_style(ListCtrlStyle::Report | ListCtrlStyle::SingleSel)
        .build();
    list.insert_column(0, "Title", ListColumnFormat::Left, 340);
    list.insert_column(1, "Site", ListColumnFormat::Left, 120);
    list.insert_column(2, "Plays", ListColumnFormat::Left, 90);
    list.insert_column(3, "Duration", ListColumnFormat::Left, 80);
    list.insert_column(4, "Uploader", ListColumnFormat::Left, 160);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let status = StaticText::builder(&dialog)
        .with_label(&crate::tr("Ready."))
        .build();
    sizer.add(&status, 0, SizerFlag::All, 4);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let mk = |label: &str| {
        let b = Button::builder(&dialog).with_label(label).build();
        row.add(&b, 0, SizerFlag::All, 4);
        b
    };
    let search_btn = mk("Search");
    let more_btn = mk("Load More Results (+80/site)");
    let play_btn = mk("Play");
    let sub_btn = mk("Subscribe");
    let copy_btn = mk("Copy URL");
    let close_btn = mk("Close");
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let results: Rc<RefCell<Vec<VideoResult>>> = Rc::new(RefCell::new(Vec::new()));
    let per_site_limit = Rc::new(std::cell::Cell::new(80usize));
    let expected = Rc::new(std::cell::Cell::new(0usize));
    let completed = Rc::new(std::cell::Cell::new(0usize));
    let last_search: Rc<RefCell<(String, Vec<usize>)>> =
        Rc::new(RefCell::new((String::new(), Vec::new())));
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<Vec<VideoResult>, String>)>();
    let rx = Rc::new(rx);
    let sites = Rc::new(sites);

    // Refill the list from results, honouring the sort choice.
    let refill: Rc<dyn Fn()> = {
        let results = results.clone();
        Rc::new(move || {
            let mut items = results.borrow().clone();
            match sort_choice.get_selection() {
                Some(1) => items.sort_by_key(|a| a.title.to_lowercase()),
                Some(2) => items.sort_by(|a, b| a.site.cmp(&b.site)),
                Some(3) => items.sort_by_key(|b| std::cmp::Reverse(b.play_count.unwrap_or(-1))),
                _ => {}
            }
            list.delete_all_items();
            for (i, r) in items.iter().enumerate() {
                list.insert_item(i as i64, &r.title, None);
                list.set_item_text_by_column(i as i64, 1, &r.site);
                list.set_item_text_by_column(
                    i as i64,
                    2,
                    &r.play_count.map(|n| n.to_string()).unwrap_or_default(),
                );
                list.set_item_text_by_column(
                    i as i64,
                    3,
                    &r.duration
                        .map(|d| {
                            let s = d as i64;
                            format!("{}:{:02}", s / 60, s % 60)
                        })
                        .unwrap_or_default(),
                );
                list.set_item_text_by_column(i as i64, 4, r.uploader.as_deref().unwrap_or(""));
            }
            // Keep the visible order as the selection order.
            *results.borrow_mut() = items;
        })
    };

    let run_search: Rc<dyn Fn(bool)> = {
        let results = results.clone();
        let sites = sites.clone();
        let per_site_limit = per_site_limit.clone();
        let last_search = last_search.clone();
        let expected = expected.clone();
        let completed = completed.clone();
        Rc::new(move |more: bool| {
            let term = query_ctrl.get_value().trim().to_string();
            if term.is_empty() {
                return;
            }
            let scope = scope_choice.get_selection().unwrap_or(0) as usize;
            let chosen: Vec<usize> = if scope == 0 {
                (0..sites.len()).collect()
            } else {
                vec![scope - 1]
            };
            expected.set(chosen.len());
            completed.set(0);
            if more {
                per_site_limit.set(per_site_limit.get() + 80);
            } else {
                per_site_limit.set(80);
                results.borrow_mut().clear();
            }
            *last_search.borrow_mut() = (term.clone(), chosen.clone());
            status.set_label(&format!("Searching {} site(s)...", chosen.len()));
            for idx in chosen {
                let site = sites[idx].clone();
                let tx = tx.clone();
                let term = term.clone();
                let limit = per_site_limit.get();
                std::thread::spawn(move || {
                    let _ = tx.send((idx, search_site(&term, &site, limit)));
                });
            }
        })
    };
    {
        let run_search = run_search.clone();
        search_btn.on_click(move |_| run_search(false));
    }
    {
        let run_search = run_search.clone();
        let last_search = last_search.clone();
        more_btn.on_click(move |_| {
            if !last_search.borrow().0.is_empty() {
                run_search(true);
            }
        });
    }
    {
        let refill = refill.clone();
        sort_choice.on_selection_changed(move |_| refill());
    }

    let chosen: Rc<RefCell<Option<(&'static str, VideoResult)>>> = Rc::new(RefCell::new(None));
    let pick = {
        let chosen = chosen.clone();
        let results = results.clone();
        let d = dialog;
        move |action: &'static str| {
            let sel = list.get_first_selected_item();
            if sel >= 0
                && let Some(r) = results.borrow().get(sel as usize).cloned()
            {
                *chosen.borrow_mut() = Some((action, r));
                d.end_modal(5100);
            }
        }
    };
    {
        let pick = pick.clone();
        play_btn.on_click(move |_| pick("play"));
    }
    {
        let pick = pick.clone();
        sub_btn.on_click(move |_| pick("subscribe"));
    }
    {
        let pick = pick.clone();
        list.on_item_activated(move |_| pick("play"));
    }
    {
        let results = results.clone();
        copy_btn.on_click(move |_| {
            let sel = list.get_first_selected_item();
            if sel >= 0
                && let Some(r) = results.borrow().get(sel as usize)
            {
                let _ = Clipboard::get().set_text(&r.url);
                status.set_label("URL copied");
            }
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));

    // Drain per-site results while the dialog is open.
    let timer = Box::leak(Box::new(Timer::new(&dialog)));
    {
        let rx = rx.clone();
        let results = results.clone();
        let refill = refill.clone();
        let expected = expected.clone();
        let completed = completed.clone();
        let last_search = last_search.clone();
        timer.on_tick(move |_| {
            let mut changed = false;
            while let Ok((_idx, result)) = rx.try_recv() {
                changed = true;
                completed.set(completed.get() + 1);
                if let Ok(found) = result {
                    let mut have: std::collections::HashSet<String> = results
                        .borrow()
                        .iter()
                        .map(|r| canonical_key(&r.url))
                        .collect();
                    let mut all = results.borrow_mut();
                    for r in found {
                        let key = canonical_key(&r.url);
                        if !key.is_empty() && have.insert(key) {
                            all.push(r);
                        }
                    }
                }
            }
            if changed {
                refill();
                let (term, chosen_sites) = last_search.borrow().clone();
                let total = results.borrow().len();
                if completed.get() >= expected.get() && !term.is_empty() {
                    status.set_label(&format!(
                        "{} result(s) from {} site(s)",
                        total,
                        chosen_sites.len()
                    ));
                } else {
                    status.set_label(&format!("{total} result(s) so far..."));
                }
            }
        });
    }
    timer.start(200, false);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(900, 620));
    query_ctrl.set_focus();
    let picked = if dialog.show_modal() == 5100 {
        chosen.borrow().clone()
    } else {
        None
    };
    dialog.destroy();
    picked
}

/// Manage persistent searches (Python's
/// PersistentSearchDialog): the saved query list lives in
/// config "persistent_searches". Returns a query to run when
/// the user picks Run on one, None otherwise.
pub fn show_persistent_searches(frame: &Frame, config_path: &Path) -> Option<String> {
    let cfg = Config::load(config_path).unwrap_or_default();
    let searches: Vec<String> = cfg
        .raw()
        .get("persistent_searches")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let searches = Rc::new(RefCell::new(searches));
    let dialog = Dialog::builder(frame, "Persistent Search").build();
    bind_f1(frame, &dialog, "PersistentSearchDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListBox::builder(&dialog).build();
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let reload = {
        let searches = searches.clone();
        move || {
            list.clear();
            for q in searches.borrow().iter() {
                list.append(q);
            }
        }
    };
    let reload = Rc::new(reload);
    reload();
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let mk = |label: &str| {
        let b = Button::builder(&dialog).with_label(label).build();
        row.add(&b, 0, SizerFlag::All, 4);
        b
    };
    let run_btn = mk("Run Search");
    let add_btn = mk("Add...");
    let remove_btn = mk("Remove");
    let close_btn = mk("Close");
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let chosen: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let save = {
        let searches = searches.clone();
        let path = config_path.to_path_buf();
        move || {
            let mut cfg = Config::load(&path).unwrap_or_default();
            let arr: Vec<Value> = searches.borrow().iter().map(|q| json!(q)).collect();
            cfg.set_path("persistent_searches", Value::Array(arr));
            let _ = cfg.save(&path);
        }
    };
    let save = Rc::new(save);
    {
        let d = dialog;
        let chosen = chosen.clone();
        let list2 = list;
        let searches = searches.clone();
        run_btn.on_click(move |_| {
            if let Some(sel) = list2.get_selection() {
                let q = searches.borrow().get(sel as usize).cloned();
                *chosen.borrow_mut() = q;
            }
            d.end_modal(5100);
        });
    }
    {
        let reload = reload.clone();
        let save = save.clone();
        let searches = searches.clone();
        let d = dialog;
        add_btn.on_click(move |_| {
            let entry = TextEntryDialog::builder(&d, "Search query:", "Add Search").build();
            if entry.show_modal() == 5100
                && let Some(q) = entry.get_value()
            {
                let q = q.trim().to_string();
                if !q.is_empty() && !searches.borrow().contains(&q) {
                    searches.borrow_mut().push(q);
                    save();
                    reload();
                }
            }
            entry.destroy();
        });
    }
    {
        let reload = reload.clone();
        let save = save.clone();
        let searches = searches.clone();
        let list2 = list;
        remove_btn.on_click(move |_| {
            if let Some(sel) = list2.get_selection() {
                let mut s = searches.borrow_mut();
                if (sel as usize) < s.len() {
                    s.remove(sel as usize);
                }
                drop(s);
                save();
                reload();
            }
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 420));
    dialog.show_modal();
    dialog.destroy();

    chosen.borrow().clone()
}

/// Messages from the archive dialog's worker threads (scan,
/// batch downloads, find-and-add), drained by its timer.
enum PodcastArchiveMsg {
    ScanProgress(String),
    /// Ok(Some(summary)) finished, Ok(None) was canceled,
    /// Err(message) failed.
    ScanDone(Result<Option<String>, String>),
    DownloadProgress(String),
    DownloadDone {
        downloaded: usize,
        skipped: usize,
        failed: usize,
        canceled: bool,
    },
    /// Ok(new feed id when known) reloads the feed list.
    FeedAdded(Result<Option<String>, String>),
}

/// Podcast archive manager (Python's PodcastArchiveDialog): a
/// feed picker over every subscribed feed, the feed's playable
/// episodes (live articles merged with the hosted sidecar when a
/// hosted provider is active) with a text filter and a
/// Downloaded / Available online column, the persistent archive
/// state as the status line, Play (opens the episode window;
/// in-player playback needs a main-window callback and stays an
/// integration follow-up), batch downloads with progress and
/// cancel, and an in-dialog archive rescan that reports scan
/// progress and can be canceled. Recovery state lives in
/// core::archive_store under the active provider's scope, in the
/// active store's own database file. The dialog does its own
/// work on workers and always returns None; the main window's
/// separate Rescan Archive action keeps the old one-shot flow.
pub fn show_podcast_archive(frame: &Frame, db: &Db) -> Option<blindrss_core::Feed> {
    use blindrss_core::archive_store::ArchiveSettings;
    use std::sync::atomic::{AtomicBool, Ordering};
    let db_path = crate::db_path();
    let config_path = crate::config_path();
    let cfg = Config::load(&config_path).unwrap_or_default();
    let scope = blindrss_core::archive_store::scope_for_config(&cfg);
    let settings = std::sync::Arc::new(ArchiveSettings::from_config(&cfg));
    let feeds: Rc<RefCell<Vec<blindrss_core::Feed>>> =
        Rc::new(RefCell::new(db.feeds().unwrap_or_default()));
    if feeds.borrow().is_empty() {
        let md =
            MessageDialog::builder(frame, "No subscribed feeds were found.", "Podcast Archive")
                .with_style(MessageDialogStyle::OK)
                .build();
        md.show_modal();
        md.destroy();
        return None;
    }

    let dialog = Dialog::builder(frame, "Podcast Archive").build();
    bind_f1(frame, &dialog, "PodcastArchiveDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let feed_row = BoxSizer::builder(Orientation::Horizontal).build();
    let feed_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Podcast:"))
        .build();
    feed_row.add(&feed_label, 0, SizerFlag::All, 4);
    let feed_choice = Choice::builder(&dialog).build();
    feed_choice.set_name("Podcast feed");
    feed_row.add(&feed_choice, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let find_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Find or add podcast..."))
        .build();
    feed_row.add(&find_btn, 0, SizerFlag::All, 4);
    let rescan_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Rescan archive"))
        .build();
    feed_row.add(&rescan_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&feed_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let filter_row = BoxSizer::builder(Orientation::Horizontal).build();
    let filter_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Filter episodes:"))
        .build();
    filter_row.add(&filter_label, 0, SizerFlag::All, 4);
    let filter_ctrl = TextCtrl::builder(&dialog).build();
    filter_ctrl.set_name("Filter podcast episodes");
    filter_row.add(&filter_ctrl, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&filter_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let status = StaticText::builder(&dialog)
        .with_label(&crate::tr("Choose a subscribed podcast."))
        .build();
    status.set_name("Podcast archive status");
    sizer.add(&status, 0, SizerFlag::Expand | SizerFlag::All, 4);
    let episodes = ListCtrl::builder(&dialog)
        .with_style(ListCtrlStyle::Report)
        .build();
    episodes.set_name("Podcast episodes");
    episodes.insert_column(0, "Title", ListColumnFormat::Left, 500);
    episodes.insert_column(1, "Date", ListColumnFormat::Left, 170);
    episodes.insert_column(2, "Media", ListColumnFormat::Left, 180);
    sizer.add(&episodes, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let mk = |label: &str| {
        let b = Button::builder(&dialog).with_label(label).build();
        btn_row.add(&b, 0, SizerFlag::All, 4);
        b
    };
    let play_btn = mk(&crate::tr("Play"));
    let dl_selected_btn = mk(&crate::tr("Download selected"));
    let dl_all_btn = mk(&crate::tr("Download all"));
    let cancel_btn = mk(&crate::tr("Cancel downloads"));
    let close_btn = mk(&crate::tr("Close"));
    sizer.add_sizer(&btn_row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let all_articles: Rc<RefCell<Vec<blindrss_core::Article>>> = Rc::new(RefCell::new(Vec::new()));
    let visible: Rc<RefCell<Vec<blindrss_core::Article>>> = Rc::new(RefCell::new(Vec::new()));
    let scan_cancel = std::sync::Arc::new(AtomicBool::new(false));
    let download_cancel = std::sync::Arc::new(AtomicBool::new(false));
    let scan_running = Rc::new(std::cell::Cell::new(false));
    let download_running = Rc::new(std::cell::Cell::new(false));
    let close_when_idle = Rc::new(std::cell::Cell::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<PodcastArchiveMsg>();
    let tx_download = tx.clone();
    let tx_scan = tx.clone();
    let tx_find = tx;
    let rx = Rc::new(rx);

    let selected_feed: Rc<dyn Fn() -> Option<blindrss_core::Feed>> = {
        let feeds = feeds.clone();
        Rc::new(move || {
            let index = feed_choice.get_selection().unwrap_or(0) as usize;
            feeds.borrow().get(index).cloned()
        })
    };
    let update_action_state: Rc<dyn Fn()> = {
        let visible = visible.clone();
        let all_articles = all_articles.clone();
        let scan_running = scan_running.clone();
        let download_running = download_running.clone();
        let selected_feed = selected_feed.clone();
        Rc::new(move || {
            let busy = scan_running.get() || download_running.get();
            let has_rows = !visible.borrow().is_empty();
            play_btn.enable(has_rows && !busy);
            dl_selected_btn.enable(has_rows && !busy);
            dl_all_btn.enable(!all_articles.borrow().is_empty() && !busy);
            cancel_btn.enable(scan_running.get() || download_running.get());
            rescan_btn.enable(selected_feed().is_some() && !busy);
            feed_choice.enable(!busy);
        })
    };
    let apply_filter: Rc<dyn Fn()> = {
        let visible = visible.clone();
        let all_articles = all_articles.clone();
        let update_action_state = update_action_state.clone();
        let config_path = config_path.clone();
        Rc::new(move || {
            let needle = filter_ctrl.get_value().trim().to_lowercase();
            let filtered: Vec<blindrss_core::Article> = all_articles
                .borrow()
                .iter()
                .filter(|article| {
                    needle.is_empty()
                        || article.title.to_lowercase().contains(&needle)
                        || article
                            .author
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&needle)
                        || article
                            .date
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&needle)
                })
                .cloned()
                .collect();
            let cfg = Config::load(&config_path).unwrap_or_default();
            episodes.delete_all_items();
            for (index, article) in filtered.iter().enumerate() {
                let title = if article.title.is_empty() {
                    "Untitled episode"
                } else {
                    &article.title
                };
                episodes.insert_item(index as i64, title, None);
                episodes.set_item_text_by_column(
                    index as i64,
                    1,
                    article.date.as_deref().unwrap_or(""),
                );
                let downloaded =
                    blindrss_core::download::downloaded_media_path(&cfg, article).is_some();
                episodes.set_item_text_by_column(
                    index as i64,
                    2,
                    if downloaded {
                        "Downloaded"
                    } else {
                        "Available online"
                    },
                );
            }
            *visible.borrow_mut() = filtered;
            update_action_state();
        })
    };
    let show_state: Rc<dyn Fn()> = {
        let all_articles = all_articles.clone();
        let selected_feed = selected_feed.clone();
        let db_path = db_path.clone();
        let scope = scope.clone();
        Rc::new(move || {
            let Some(feed) = selected_feed() else {
                status.set_label("Choose a subscribed podcast.");
                return;
            };
            let count = all_articles.borrow().len();
            let noun = if count == 1 { "episode" } else { "episodes" };
            let state = blindrss_core::archive_store::state(&db_path, &scope, &feed.id).unwrap_or(
                blindrss_core::archive_store::ArchiveState {
                    feed_id: feed.id.clone(),
                    feed_url: String::new(),
                    status: "never".into(),
                    last_attempt: None,
                    last_success: None,
                    snapshot_count: 0,
                    episode_count: 0,
                    error: String::new(),
                },
            );
            let text = match state.status.as_str() {
                "complete" => format!(
                    "{count} playable {noun}; archive scanned snapshots: {}.",
                    state.snapshot_count
                ),
                "scanning" => format!(
                    "Archive scan is running. {count} playable {noun} is currently available."
                ),
                "error" => format!(
                    "Archive scan needs retrying: {}. {count} playable {noun} is available.",
                    if state.error.is_empty() {
                        "unknown error"
                    } else {
                        &state.error
                    }
                ),
                _ => format!(
                    "Archive scan is pending. {count} playable {noun} is currently available."
                ),
            };
            status.set_label(&text);
        })
    };
    let load_feed: Rc<dyn Fn()> = {
        let all_articles = all_articles.clone();
        let apply_filter = apply_filter.clone();
        let show_state = show_state.clone();
        let selected_feed = selected_feed.clone();
        let db_path = db_path.clone();
        let scope = scope.clone();
        Rc::new(move || {
            let mut articles = Vec::new();
            if let Some(feed) = selected_feed()
                && let Ok(fresh) = Db::open(&db_path)
                && let Ok(mut rows) = fresh.articles(Some(&feed.id), 100_000)
            {
                rows.retain(|a| a.media_url.as_deref().is_some_and(|m| !m.is_empty()));
                articles = rows;
                if !scope.is_empty() {
                    // Hosted view: merge the account's sidecar
                    // episodes into the feed's article list.
                    if let Ok(merged) = blindrss_core::archive_store::merge_articles(
                        &db_path,
                        &scope,
                        &feed.id,
                        articles.clone(),
                    ) {
                        articles = merged;
                    }
                }
            }
            *all_articles.borrow_mut() = articles;
            apply_filter();
            show_state();
        })
    };
    let fill_feeds = {
        let feeds = feeds.clone();
        let load_feed = load_feed.clone();
        Rc::new(move |select_id: Option<&str>| {
            feed_choice.clear();
            for feed in feeds.borrow().iter() {
                feed_choice.append(if feed.title.is_empty() {
                    &feed.url
                } else {
                    &feed.title
                });
            }
            let index = select_id
                .and_then(|id| feeds.borrow().iter().position(|f| f.id == id))
                .unwrap_or(0);
            if !feeds.borrow().is_empty() {
                feed_choice.set_selection(index as u32);
            }
            load_feed();
        })
    };
    let selected_articles: Rc<dyn Fn() -> Vec<blindrss_core::Article>> = {
        let visible = visible.clone();
        Rc::new(move || {
            let mut picked = Vec::new();
            let mut index = episodes.get_first_selected_item();
            while index >= 0 {
                if let Some(article) = visible.borrow().get(index as usize) {
                    picked.push(article.clone());
                }
                index = episodes.get_next_item(
                    index as i64,
                    ListNextItemFlag::All,
                    ListItemState::Selected,
                );
            }
            picked
        })
    };

    // Batch download (Python's _start_batch_download and worker):
    // confirm, then download on a worker with per-episode
    // progress and a cancel flag.
    let start_download: Rc<dyn Fn(Vec<blindrss_core::Article>)> = {
        let download_running = download_running.clone();
        let download_cancel = download_cancel.clone();
        let update_action_state = update_action_state.clone();
        let selected_feed = selected_feed.clone();
        let config_path = config_path.clone();
        let frame2 = *frame;
        Rc::new(move |articles: Vec<blindrss_core::Article>| {
            if articles.is_empty() || download_running.get() {
                return;
            }
            let cfg = Config::load(&config_path).unwrap_or_default();
            if !cfg.get_bool("downloads_enabled", false) {
                let md = MessageDialog::builder(
                    &frame2,
                    "Downloads are disabled. Enable them in Settings > Downloads.",
                    "Downloads disabled",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                md.show_modal();
                md.destroy();
                return;
            }
            let count = articles.len();
            let md = MessageDialog::builder(
                &frame2,
                &format!(
                    "Download {count} episode{}?",
                    if count == 1 { "" } else { "s" }
                ),
                "Podcast Archive",
            )
            .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
            .build();
            let answer = md.show_modal();
            md.destroy();
            if answer != ID_YES {
                return;
            }
            download_cancel.store(false, Ordering::Relaxed);
            download_running.set(true);
            update_action_state();
            let feed_title = selected_feed().map(|f| f.title).unwrap_or_default();
            let tx = tx_download.clone();
            let cancel = download_cancel.clone();
            let config_path = config_path.clone();
            std::thread::spawn(move || {
                let cfg = Config::load(&config_path).unwrap_or_default();
                let base = {
                    let dir = cfg.get_str("download_path", "");
                    if dir.is_empty() {
                        crate::home_dir().join("Downloads").join("BlindRSS")
                    } else {
                        std::path::PathBuf::from(dir)
                    }
                };
                let folder = base.join(blindrss_core::download::sanitize_filename(
                    if feed_title.is_empty() {
                        "Podcast Archive"
                    } else {
                        &feed_title
                    },
                ));
                let _ = std::fs::create_dir_all(&folder);
                let format = cfg.get_str("download_format", "video_best").to_string();
                let total = articles.len();
                let (mut downloaded, mut skipped, mut failed) = (0usize, 0usize, 0usize);
                for (index, article) in articles.iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = tx.send(PodcastArchiveMsg::DownloadProgress(format!(
                        "Downloading episode {} of {total}: {}",
                        index + 1,
                        if article.title.is_empty() {
                            "Untitled episode"
                        } else {
                            &article.title
                        }
                    )));
                    if blindrss_core::download::downloaded_media_path(&cfg, article).is_some() {
                        skipped += 1;
                        continue;
                    }
                    let result = podcast_archive_download_one(article, &folder, &format);
                    match result {
                        Ok(path) => {
                            let mut cfg = Config::load(&config_path).unwrap_or_default();
                            if blindrss_core::download::record_article_download(
                                &mut cfg, article, &path,
                            )
                            .is_ok()
                            {
                                let _ = cfg.save(&config_path);
                            }
                            downloaded += 1;
                        }
                        Err(_) => failed += 1,
                    }
                }
                let _ = tx.send(PodcastArchiveMsg::DownloadDone {
                    downloaded,
                    skipped,
                    failed,
                    canceled: cancel.load(Ordering::Relaxed),
                });
            });
        })
    };

    {
        let load_feed = load_feed.clone();
        feed_choice.on_selection_changed(move |_| load_feed());
    }
    {
        let apply_filter = apply_filter.clone();
        filter_ctrl.on_text_updated(move |_| apply_filter());
    }
    let play_selected: Rc<dyn Fn()> = {
        let selected_articles = selected_articles.clone();
        let visible = visible.clone();
        Rc::new(move || {
            let mut picked = selected_articles();
            if picked.is_empty()
                && let Some(first) = visible.borrow().first().cloned()
            {
                picked.push(first);
            }
            if let Some(article) = picked.into_iter().next() {
                let body = article
                    .content
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .or(article.description.as_deref())
                    .unwrap_or("");
                let body = blindrss_core::extract::html_to_text_opts(body, false);
                let body = match article.media_url.as_deref() {
                    Some(media) if !media.is_empty() => format!("{body}\n\nMedia: {media}"),
                    _ => body,
                };
                crate::article_window::open(&article.title, &body, Some(&article.url));
            }
        })
    };
    {
        let play_selected = play_selected.clone();
        play_btn.on_click(move |_| play_selected());
    }
    {
        let play_selected = play_selected.clone();
        episodes.on_item_activated(move |_| play_selected());
    }
    {
        let start_download = start_download.clone();
        let selected_articles = selected_articles.clone();
        let frame2 = *frame;
        dl_selected_btn.on_click(move |_| {
            let picked = selected_articles();
            if picked.is_empty() {
                let md = MessageDialog::builder(
                    &frame2,
                    "Select one or more episodes first.",
                    "Podcast Archive",
                )
                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                .build();
                md.show_modal();
                md.destroy();
                return;
            }
            start_download(picked);
        });
    }
    {
        let start_download = start_download.clone();
        let all_articles = all_articles.clone();
        dl_all_btn.on_click(move |_| start_download(all_articles.borrow().clone()));
    }
    {
        let scan_cancel = scan_cancel.clone();
        let download_cancel = download_cancel.clone();
        cancel_btn.on_click(move |_| {
            scan_cancel.store(true, Ordering::Relaxed);
            download_cancel.store(true, Ordering::Relaxed);
        });
    }
    {
        let scan_running = scan_running.clone();
        let scan_cancel = scan_cancel.clone();
        let update_action_state = update_action_state.clone();
        let selected_feed = selected_feed.clone();
        let db_path = db_path.clone();
        let scope = scope.clone();
        let settings = settings.clone();
        rescan_btn.on_click(move |_| {
            let Some(feed) = selected_feed() else { return };
            if scan_running.get() {
                return;
            }
            scan_running.set(true);
            scan_cancel.store(false, Ordering::Relaxed);
            status.set_label("Starting archive rescan...");
            update_action_state();
            let tx = tx_scan.clone();
            let cancel = scan_cancel.clone();
            let db_path = db_path.clone();
            let scope = scope.clone();
            let settings = settings.clone();
            std::thread::spawn(move || {
                let progress = {
                    let tx = tx.clone();
                    move |p: blindrss_core::archive::ScanProgress| {
                        let label = match p.stage {
                            "current" => format!(
                                "Reading the current feed... {} episodes found",
                                p.episodes_found
                            ),
                            "index" => "Asking the Wayback Machine for snapshots...".to_string(),
                            "snapshot" => format!(
                                "Loading snapshots: {} of {} loaded, {} episodes found",
                                p.snapshots_loaded, p.snapshots_found, p.episodes_found
                            ),
                            _ => format!("{} episodes found", p.episodes_found),
                        };
                        let _ = tx.send(PodcastArchiveMsg::ScanProgress(label));
                    }
                };
                let result: Result<Option<String>, String> = if scope.is_empty() {
                    match blindrss_core::archive::perform_local_scan(
                        &db_path,
                        &feed,
                        &settings,
                        blindrss_core::archive::Claim::Force,
                        &cancel,
                        progress,
                    ) {
                        Ok(outcome) => match outcome.kind {
                            blindrss_core::archive::LocalScanKind::Stored => Ok(Some(format!(
                                "Archive rescan complete: {} recovered episode(s) added.",
                                outcome.recovered
                            ))),
                            blindrss_core::archive::LocalScanKind::NotPodcast => Ok(Some(
                                "That feed has no podcast episodes to archive.".to_string(),
                            )),
                            blindrss_core::archive::LocalScanKind::NotDue => {
                                Ok(Some("Archive scan is not due yet.".to_string()))
                            }
                            blindrss_core::archive::LocalScanKind::Canceled => Ok(None),
                        },
                        Err(error) => Err(error),
                    }
                } else {
                    use blindrss_core::archive_store as store;
                    let claimed = store::reset(&db_path, &scope, &feed.id).is_ok()
                        && matches!(
                            store::claim(
                                &db_path,
                                &scope,
                                &feed.id,
                                &feed.url,
                                settings.hosted_rescan_seconds(),
                                true,
                                store::now_seconds(),
                            ),
                            Ok(true)
                        );
                    if !claimed {
                        Err("Could not claim the archive scan.".to_string())
                    } else {
                        let outcome = blindrss_core::archive::run_hosted_scan(
                            &db_path, &scope, &feed.id, &feed.url, &settings, &cancel, progress,
                        );
                        match outcome.kind {
                            blindrss_core::archive::HostedScanKind::Stored => Ok(Some(format!(
                                "Archive rescan complete: {} recovered episode(s) in the archive.",
                                outcome.recovered
                            ))),
                            blindrss_core::archive::HostedScanKind::Cleared => Ok(Some(
                                "That feed has no podcast episodes to archive.".to_string(),
                            )),
                            blindrss_core::archive::HostedScanKind::Canceled => Ok(None),
                            blindrss_core::archive::HostedScanKind::Failed => Err(outcome.error),
                        }
                    }
                };
                let _ = tx.send(PodcastArchiveMsg::ScanDone(result));
            });
        });
    }
    {
        let frame2 = *frame;
        let db_path = db_path.clone();
        find_btn.on_click(move |_| {
            let Some(url) = show_feed_search(&frame2) else {
                return;
            };
            status.set_label(&format!("Adding {url}..."));
            let tx = tx_find.clone();
            let db_path = db_path.clone();
            std::thread::spawn(move || {
                let cfg = Config::load(&crate::config_path()).unwrap_or_default();
                let result = if blindrss_core::provider::active_name(&cfg) != "local" {
                    let provider = blindrss_core::provider::get_provider(&cfg, crate::db_path());
                    provider.add_feed(&url).map(|_| None)
                } else {
                    (|| -> Result<Option<String>, String> {
                        let (feed_url, _parsed) =
                            blindrss_core::discovery::resolve_subscribe_url(&url)?;
                        let db = Db::open(&db_path).map_err(|e| e.to_string())?;
                        if db.feed_url_exists(&feed_url).unwrap_or(false) {
                            return Ok(None);
                        }
                        // The shared worker subscribe path (Audit 8
                        // F01/D22): row + first store through this
                        // thread's own connection, listing sources
                        // through the unified refresh.
                        let feed = blindrss_core::refresh::subscribe_url(
                            &db,
                            &url,
                            None,
                            cfg.get_str("delete_behavior", "deleted"),
                            crate::article_retention_cutoff().as_deref(),
                        )?;
                        Ok(Some(feed.id))
                    })()
                };
                let _ = tx.send(PodcastArchiveMsg::FeedAdded(result));
            });
        });
    }
    {
        let scan_running = scan_running.clone();
        let download_running = download_running.clone();
        let close_when_idle = close_when_idle.clone();
        let scan_cancel = scan_cancel.clone();
        let download_cancel = download_cancel.clone();
        let frame2 = *frame;
        let d = dialog;
        close_btn.on_click(move |_| {
            if scan_running.get() || download_running.get() {
                let md = MessageDialog::builder(
                    &frame2,
                    "Stop the active work and close this window when it is safe?",
                    "Podcast Archive",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                .build();
                let answer = md.show_modal();
                md.destroy();
                if answer != ID_YES {
                    return;
                }
                scan_cancel.store(true, Ordering::Relaxed);
                download_cancel.store(true, Ordering::Relaxed);
                close_when_idle.set(true);
                status.set_label("Stopping; this window will close when the active work finishes.");
                return;
            }
            d.end_modal(5101);
        });
    }

    fill_feeds(None);

    // Drain worker results while the dialog is open.
    let timer = Timer::new(&dialog);
    {
        let rx = rx.clone();
        let feeds = feeds.clone();
        let fill_feeds = fill_feeds.clone();
        let load_feed = load_feed.clone();
        let apply_filter = apply_filter.clone();
        let scan_running = scan_running.clone();
        let download_running = download_running.clone();
        let close_when_idle = close_when_idle.clone();
        let update_action_state = update_action_state.clone();
        let frame2 = *frame;
        let d = dialog;
        timer.on_tick(move |_| {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    PodcastArchiveMsg::ScanProgress(label) => status.set_label(&label),
                    PodcastArchiveMsg::ScanDone(result) => {
                        scan_running.set(false);
                        load_feed();
                        update_action_state();
                        if close_when_idle.get() {
                            d.end_modal(5101);
                            continue;
                        }
                        match result {
                            Ok(Some(summary)) => status.set_label(&summary),
                            Ok(None) => status.set_label("Archive scan canceled."),
                            Err(error) => {
                                let md = MessageDialog::builder(
                                    &frame2,
                                    &format!("Archive rescan failed: {error}"),
                                    "Podcast Archive",
                                )
                                .with_style(
                                    MessageDialogStyle::OK | MessageDialogStyle::IconError,
                                )
                                .build();
                                md.show_modal();
                                md.destroy();
                            }
                        }
                    }
                    PodcastArchiveMsg::DownloadProgress(label) => status.set_label(&label),
                    PodcastArchiveMsg::DownloadDone {
                        downloaded,
                        skipped,
                        failed,
                        canceled,
                    } => {
                        download_running.set(false);
                        apply_filter();
                        update_action_state();
                        let summary = format!(
                            "{}Downloaded: {downloaded}; already present: {skipped}; failed: {failed}.",
                            if canceled { "Canceled. " } else { "" }
                        );
                        if close_when_idle.get() {
                            d.end_modal(5101);
                            continue;
                        }
                        status.set_label(&summary);
                        let md = MessageDialog::builder(&frame2, &summary, "Podcast Archive")
                            .with_style(
                                MessageDialogStyle::OK | MessageDialogStyle::IconInformation,
                            )
                            .build();
                        md.show_modal();
                        md.destroy();
                    }
                    PodcastArchiveMsg::FeedAdded(result) => match result {
                        Ok(new_id) => {
                            if let Ok(fresh) = Db::open(&crate::db_path())
                                && let Ok(list) = fresh.feeds()
                            {
                                *feeds.borrow_mut() = list;
                            }
                            fill_feeds(new_id.as_deref());
                        }
                        Err(error) => {
                            status.set_label(&format!("Could not add podcast: {error}"))
                        }
                    },
                }
            }
        });
    }
    timer.start(150, false);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(980, 680));
    dialog.show_modal();
    timer.stop();
    dialog.destroy();
    None
}

/// One episode download for the archive manager's batch worker:
/// direct media when it is a plain playable file, otherwise the
/// same resolve-then-download chain as the main window's
/// Download action.
fn podcast_archive_download_one(
    article: &blindrss_core::Article,
    dir: &Path,
    format: &str,
) -> Result<std::path::PathBuf, String> {
    if let Some(media) = article.media_url.as_deref().filter(|url| {
        !url.is_empty()
            && blindrss_core::media::is_playable_media(url, article.media_type.as_deref())
            && !blindrss_core::media::is_ytdlp_supported(url)
    }) {
        return blindrss_core::download::download_direct(media, dir, &article.title);
    }
    if article.url.is_empty() {
        return Err("Article has nothing to download".to_string());
    }
    match blindrss_core::media::resolve_for_play(
        article.media_url.as_deref(),
        article.media_type.as_deref(),
        &article.url,
        article.content.as_deref(),
    ) {
        Ok(resolved) if resolved.via_ytdlp => {
            blindrss_core::download::download_with_ytdlp(&article.url, dir, format)
        }
        Ok(resolved) => {
            blindrss_core::download::download_direct(&resolved.url, dir, &article.title)
        }
        Err(error) => Err(error),
    }
}

/// Load the persisted play queue (config "play_queue": entry
/// objects with article_id / media_url / use_ytdlp / title /
/// media_type / feed_id / feed_title).
pub fn load_queue(config_path: &Path) -> Vec<Value> {
    let cfg = Config::load(config_path).unwrap_or_default();
    cfg.raw()
        .get("play_queue")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter(|e| {
                    e.get("media_url").and_then(|v| v.as_str()).is_some()
                        || e.get("article_id").and_then(|v| v.as_str()).is_some()
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Persist the play queue.
pub fn save_queue(config_path: &Path, entries: &[Value]) {
    let mut cfg = Config::load(config_path).unwrap_or_default();
    cfg.set_path("play_queue", Value::Array(entries.to_vec()));
    let _ = cfg.save(config_path);
}

/// The Play Queue dialog (Python's QueueDialog): a single list
/// with labeled action buttons. Edits save immediately; Play
/// returns the chosen entry index and closes.
pub fn show_queue(frame: &Frame, config_path: &Path) -> Option<usize> {
    let entries = Rc::new(RefCell::new(load_queue(config_path)));
    let dialog = Dialog::builder(frame, "Play Queue").build();
    bind_f1(frame, &dialog, "play-queue");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListBox::builder(&dialog).build();
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let reload = {
        let entries = entries.clone();
        move |select: Option<usize>| {
            list.clear();
            for e in entries.borrow().iter() {
                let title = e.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let feed = e.get("feed_title").and_then(|v| v.as_str()).unwrap_or("");
                list.append(&if feed.is_empty() {
                    title.to_string()
                } else {
                    format!("{title} - {feed}")
                });
            }
            if let Some(i) = select {
                list.set_selection(i as u32, true);
            }
        }
    };
    let reload = Rc::new(reload);
    reload(Some(0));
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let mk = |label: &str| {
        let b = Button::builder(&dialog).with_label(label).build();
        row.add(&b, 0, SizerFlag::All, 4);
        b
    };
    let play_btn = mk("Play");
    let up_btn = mk("Move Up");
    let down_btn = mk("Move Down");
    let remove_btn = mk("Remove");
    let clear_btn = mk("Clear All");
    let close_btn = mk("Close");
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let path = config_path.to_path_buf();
    let persist = {
        let entries = entries.clone();
        let path = path.clone();
        move || save_queue(&path, &entries.borrow())
    };
    let persist = Rc::new(persist);
    let chosen: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
    {
        let chosen = chosen.clone();
        let d = dialog;
        play_btn.on_click(move |_| {
            *chosen.borrow_mut() = list.get_selection().map(|s| s as usize);
            d.end_modal(5100);
        });
    }
    let move_entry = {
        let entries = entries.clone();
        let reload = reload.clone();
        let persist = persist.clone();
        move |delta: i32| {
            let Some(sel) = list.get_selection().map(|s| s as i32) else {
                return;
            };
            let target = sel + delta;
            let mut e = entries.borrow_mut();
            if target < 0 || target as usize >= e.len() {
                return;
            }
            e.swap(sel as usize, target as usize);
            drop(e);
            persist();
            reload(Some(target as usize));
        }
    };
    let move_entry = Rc::new(move_entry);
    {
        let m = move_entry.clone();
        up_btn.on_click(move |_| m(-1));
    }
    {
        let m = move_entry.clone();
        down_btn.on_click(move |_| m(1));
    }
    {
        let entries = entries.clone();
        let reload = reload.clone();
        let persist = persist.clone();
        remove_btn.on_click(move |_| {
            if let Some(sel) = list.get_selection() {
                let mut e = entries.borrow_mut();
                if (sel as usize) < e.len() {
                    e.remove(sel as usize);
                }
                drop(e);
                persist();
                reload(None);
            }
        });
    }
    {
        let entries = entries.clone();
        let reload = reload.clone();
        let persist = persist.clone();
        clear_btn.on_click(move |_| {
            entries.borrow_mut().clear();
            persist();
            reload(None);
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(640, 440));
    let played = if dialog.show_modal() == 5100 {
        *chosen.borrow()
    } else {
        None
    };
    dialog.destroy();
    played.filter(|i| *i < entries.borrow().len())
}

/// Takeout import selection (Python's
/// TakeoutImportSelectionDialog): a check list of the feeds
/// found in the Takeout ZIP; everything starts checked and
/// only the checked feeds import.
pub fn select_takeout_feeds(
    frame: &Frame,
    feeds: &[blindrss_core::takeout::TakeoutFeed],
) -> Option<Vec<blindrss_core::takeout::TakeoutFeed>> {
    let dialog = Dialog::builder(frame, "Import YouTube Takeout").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Choose the subscriptions to import:"))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = CheckListBox::builder(&dialog).build();
    for (i, f) in feeds.iter().enumerate() {
        list.append(&format!("{} ({})", f.title, f.source));
        list.check(i as u32, true);
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(620, 480));
    let result = if dialog.show_modal() == 5100 {
        Some(
            feeds
                .iter()
                .enumerate()
                .filter(|(i, _)| list.is_checked(*i as u32))
                .map(|(_, f)| f.clone())
                .collect(),
        )
    } else {
        None
    };
    dialog.destroy();
    result
}

/// Feed search / discovery (Python's FeedSearchDialog): give
/// a site or feed URL; the dialog fetches it on a worker,
/// lists the feeds found on the page (or the feed itself),
/// and subscribes the chosen one.
pub fn show_feed_search(frame: &Frame) -> Option<String> {
    let dialog = Dialog::builder(frame, "Find Feeds").build();
    bind_f1(frame, &dialog, "FeedSearchDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let url_ctrl = name_row(&dialog, &sizer, "Search terms or site/feed URL:", "");
    let label = StaticText::builder(&dialog)
        .with_label("Search provider:")
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let provider = Choice::builder(&dialog).build();
    provider.append("All sources");
    provider.append("All podcast sources");
    provider.append("All RSS feed sources");
    provider.set_name("Search source");
    for (_, label) in blindrss_core::feedsearch::SEARCH_PROVIDERS {
        provider.append(label);
    }
    provider.set_selection(0);
    sizer.add(&provider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    let list = ListBox::builder(&dialog).build();
    list.set_name("Feed search results");
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let status = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::ReadOnly)
        .build();
    status.set_name("Search status");
    sizer.add(&status, 0, SizerFlag::All, 4);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let mk = |label: &str| {
        let b = Button::builder(&dialog).with_label(label).build();
        row.add(&b, 0, SizerFlag::All, 4);
        b
    };
    let search_btn = mk("Search");
    let sub_btn = mk("Subscribe");
    let close_btn = mk("Close");
    sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    let candidates: Rc<RefCell<Vec<blindrss_core::feedsearch::FeedResult>>> =
        Rc::new(RefCell::new(Vec::new()));
    let (tx, rx) =
        std::sync::mpsc::channel::<Result<blindrss_core::feedsearch::SearchReport, String>>();
    let rx = Rc::new(rx);
    {
        let tx = tx.clone();
        let status2 = status;
        search_btn.on_click(move |_| {
            let url = url_ctrl.get_value();
            let url = url.trim().to_string();
            if url.is_empty() {
                return;
            }
            status2.set_value("Searching...");
            search_btn.enable(false);
            sub_btn.enable(false);
            list.clear();
            url_ctrl.enable(false);
            provider.enable(false);
            let source = provider.get_selection().unwrap_or(0) as usize;
            let tx = tx.clone();
            std::thread::spawn(move || {
                use blindrss_core::feedsearch::{
                    SEARCH_PROVIDERS, SearchGroup, search_group, search_group_providers,
                };
                let config = Config::load(&crate::config_path()).unwrap_or_default();
                let result = match source {
                    0 => search_group(SearchGroup::All, &url, 100, &config),
                    1 => search_group(SearchGroup::Podcasts, &url, 100, &config),
                    2 => search_group(SearchGroup::Rss, &url, 100, &config),
                    _ => search_group_providers(
                        &[SEARCH_PROVIDERS[source - 3].0],
                        &url,
                        100,
                        &config,
                    ),
                };
                let _ = tx.send(result);
            });
        });
    }
    let chosen: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    {
        let chosen = chosen.clone();
        let candidates = candidates.clone();
        let d = dialog;
        sub_btn.on_click(move |_| {
            if let Some(sel) = list.get_selection() {
                *chosen.borrow_mut() = candidates.borrow().get(sel as usize).map(|r| r.url.clone());
            }
            if chosen.borrow().is_some() {
                d.end_modal(5100);
            }
        });
    }
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));

    // Drain worker results while the dialog is open.
    let timer = Timer::new(&dialog);
    {
        let rx = rx.clone();
        let candidates = candidates.clone();
        let list2 = list;
        let status3 = status;
        timer.on_tick(move |_| {
            while let Ok(result) = rx.try_recv() {
                search_btn.enable(true);
                url_ctrl.enable(true);
                provider.enable(true);
                match result {
                    Ok(report) => {
                        let urls = report.results;
                        sub_btn.enable(!urls.is_empty());
                        list2.clear();
                        for result in &urls {
                            list2.append(&format!("{} — {}", result.title, result.url));
                        }
                        if !urls.is_empty() {
                            list2.set_selection(0, true);
                        }
                        let mut message = format!("{} feed(s) found", urls.len());
                        if !report.errors.is_empty() {
                            message.push_str(&format!(
                                ". Some sources failed: {}",
                                report.errors.join("; ")
                            ));
                        }
                        status3.set_value(&message);
                        crate::announce("feed_search", &message);
                        *candidates.borrow_mut() = urls;
                    }
                    Err(e) => {
                        let message = format!("Search failed: {e}");
                        status3.set_value(&message);
                        crate::announce("feed_search", &message);
                    }
                }
            }
        });
    }
    timer.start(150, false);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(620, 460));
    let picked = if dialog.show_modal() == 5100 {
        chosen.borrow().clone()
    } else {
        None
    };
    timer.stop();
    dialog.destroy();
    picked
}

/// Exclude-notification-feeds picker (Python's
/// ExcludeNotificationFeedsDialog): every feed with a
/// checkbox; checked feeds are stored by ID in
/// windows_notifications_excluded_feeds and get no refresh
/// notifications.
pub fn show_exclude_feeds(frame: &impl WxWidget, config_path: &Path) {
    let Ok(db) = Db::open(&crate::db_path()) else {
        return;
    };
    let feeds = db.feeds().unwrap_or_default();
    let cfg = Config::load(config_path).unwrap_or_default();
    let excluded: Vec<String> = cfg
        .raw()
        .get("windows_notifications_excluded_feeds")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let dialog = Dialog::builder(frame, "Exclude Feeds from Notifications").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Checked feeds will not send notifications:"))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let list = CheckListBox::builder(&dialog).build();
    for (i, f) in feeds.iter().enumerate() {
        list.append(&f.title);
        if excluded.contains(&f.id) {
            list.check(i as u32, true);
        }
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 480));
    if dialog.show_modal() == 5100 {
        let ids: Vec<Value> = feeds
            .iter()
            .enumerate()
            .filter(|(i, _)| list.is_checked(*i as u32))
            .map(|(_, f)| json!(f.id))
            .collect();
        let mut cfg = Config::load(config_path).unwrap_or_default();
        cfg.set_path("windows_notifications_excluded_feeds", Value::Array(ids));
        let _ = cfg.save(config_path);
    }
    dialog.destroy();
}

/// YouTube account device sign-in (Python's device-code
/// flow): shows the user code + verification URL, polls on a
/// worker until Google approves, then stores the refresh
/// token in config youtube_account_refresh_token.
pub fn show_youtube_signin(frame: &impl WxWidget, config_path: &Path) {
    let cfg = Config::load(config_path).unwrap_or_default();
    let client_id = {
        let v = cfg.get_str("youtube_account_client_id", "");
        if v.is_empty() {
            blindrss_core::youtube_account::CLIENT_ID.to_string()
        } else {
            v.to_string()
        }
    };
    let secret = cfg.get_str("youtube_account_client_secret", "").to_string();
    let dialog = Dialog::builder(frame, "Connect YouTube Account").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let status = StaticText::builder(&dialog)
        .with_label(&crate::tr("Contacting Google..."))
        .build();
    sizer.add(&status, 0, SizerFlag::All, 8);
    let code_label = StaticText::builder(&dialog).with_label("").build();
    sizer.add(&code_label, 0, SizerFlag::All, 8);
    let close_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    sizer.add(&close_btn, 0, SizerFlag::All, 4);
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let rx = Rc::new(rx);
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let signin = match blindrss_core::youtube_account::start_sign_in(&client_id) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            // Instructions travel through the channel; only
            // the dialog's timer (UI thread) touches widgets.
            let _ = tx.send(Ok(format!(
                "PROGRESS:{}:{}",
                signin.user_code, signin.verification_url
            )));
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(signin.expires_in.max(60));
            loop {
                if std::time::Instant::now() >= deadline {
                    let _ = tx.send(Err("sign-in code expired".to_string()));
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(signin.interval.max(2)));
                match blindrss_core::youtube_account::poll_sign_in(
                    &client_id,
                    &secret,
                    &signin.device_code,
                ) {
                    Ok(Some(token)) => {
                        let _ = tx.send(Ok(format!("TOKEN:{token}")));
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
        });
    }
    let done: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let timer = Box::leak(Box::new(Timer::new(&dialog)));
    {
        let rx = rx.clone();
        let status3 = status;
        let code_label3 = code_label;
        let done = done.clone();
        let d = dialog;
        let path = config_path.to_path_buf();
        timer.on_tick(move |_| {
            while let Ok(result) = rx.try_recv() {
                match result {
                    Ok(msg) if msg.starts_with("PROGRESS:") => {
                        let rest = msg.trim_start_matches("PROGRESS:");
                        let (code, url) = rest
                            .split_once(':')
                            .map(|(c, u)| (c.to_string(), u.to_string()))
                            .unwrap_or_default();
                        status3.set_label("Open the verification page and enter the code:");
                        code_label3.set_label(&format!("Code: {code}\n{url}"));
                        crate::open_in_browser(&url);
                    }
                    Ok(msg) if msg.starts_with("TOKEN:") => {
                        let token = msg.trim_start_matches("TOKEN:").to_string();
                        let mut cfg = Config::load(&path).unwrap_or_default();
                        cfg.set_path("youtube_account_refresh_token", serde_json::json!(token));
                        let _ = cfg.save(&path);
                        status3.set_label("YouTube account connected.");
                        *done.borrow_mut() = true;
                        d.end_modal(5100);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        status3.set_label(&format!("Sign-in failed: {e}"));
                        *done.borrow_mut() = true;
                        d.end_modal(5101);
                    }
                }
            }
        });
    }
    timer.start(200, false);
    let d = dialog;
    close_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(520, 260));
    dialog.show_modal();
    dialog.destroy();
}

/// Cast device picker: every discovered device (all
/// protocols) with its kind. Returns the chosen device.
pub fn show_cast_picker(
    frame: &Frame,
    devices: &[blindrss_core::cast::CastDevice],
) -> Option<blindrss_core::cast::CastDevice> {
    let dialog = Dialog::builder(frame, "Cast to Device").build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let list = ListBox::builder(&dialog).build();
    for d in devices {
        list.append(&format!("{} ({})", d.name, d.kind));
    }
    if !devices.is_empty() {
        list.set_selection(0, true);
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(480, 360));
    let picked = if dialog.show_modal() == 5100 {
        list.get_selection()
            .and_then(|i| devices.get(i as usize).cloned())
    } else {
        None
    };
    dialog.destroy();
    picked
}

/// Import Site Cookies (Python's ImportSiteCookiesDialog, issue
/// #79): pick Chromium-family profiles; their decrypted v10
/// cookies merge into site_cookies.txt beside the config, so
/// challenge-protected sites can be read. v20 app-bound rows are
/// counted as skipped (no elevation machinery in the Rust app).
pub fn show_import_cookies(frame: &Frame, config_path: &Path) {
    use blindrss_core::chromium;
    let dialog = Dialog::builder(frame, "Import Site Cookies").build();
    bind_f1(frame, &dialog, "site-cookies");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let label = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Choose the browser profiles to import cookies from:",
        ))
        .build();
    sizer.add(&label, 0, SizerFlag::All, 4);
    let profiles = chromium::list_profiles();
    let list = CheckListBox::builder(&dialog).build();
    for (i, p) in profiles.iter().enumerate() {
        list.append(&format!(
            "{} — {} ({})",
            p.browser,
            p.profile,
            crate::fmt_ts(Some(p.mtime))
        ));
        list.check(i as u32, true);
    }
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    let status = StaticText::builder(&dialog)
        .with_label(if profiles.is_empty() {
            "No Chromium-family browser profiles found."
        } else {
            ""
        })
        .build();
    sizer.add(&status, 0, SizerFlag::All, 4);
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let import_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Import"))
        .build();
    row.add(&import_btn, 0, SizerFlag::All, 4);
    let close_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Close"))
        .build();
    row.add(&close_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::All, 2);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(620, 440));

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let profiles = Rc::new(profiles);
    {
        let profiles = profiles.clone();
        let cfg_path = config_path.to_path_buf();
        import_btn.on_click(move |_| {
            let chosen: Vec<chromium::ChromiumProfile> = profiles
                .iter()
                .enumerate()
                .filter(|(i, _)| list.is_checked(*i as u32))
                .map(|(_, p)| p.clone())
                .collect();
            if chosen.is_empty() {
                status.set_label("No profiles selected.");
                return;
            }
            status.set_label("Importing...");
            let tx = tx.clone();
            let jar_path = cfg_path
                .parent()
                .map(|d| d.join("site_cookies.txt"))
                .unwrap_or_default();
            std::thread::spawn(move || {
                let mut records = Vec::new();
                let mut skipped = 0usize;
                for p in &chosen {
                    let outcome = chromium::read_profile_cookies(p);
                    skipped += outcome.skipped;
                    records.extend(outcome.records);
                }
                let existing = std::fs::read_to_string(&jar_path).unwrap_or_default();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let merged = chromium::merge_into_jar(&existing, &records, now);
                let imported = records.len();
                let msg = match std::fs::write(&jar_path, merged) {
                    Ok(()) => format!(
                        "Imported {imported} cookies from {} profiles; {skipped} skipped (could not be decrypted).",
                        chosen.len()
                    ),
                    Err(e) => format!("Could not write the cookie jar: {e}"),
                };
                let _ = tx.send(msg);
            });
        });
    }
    {
        let d = dialog;
        close_btn.on_click(move |_| {
            d.end_modal(5100);
        });
    }
    let timer = Box::leak(Box::new(Timer::new(&dialog)));
    timer.start(200, false);
    timer.on_tick(move |_| {
        if let Ok(msg) = rx.try_recv() {
            status.set_label(&msg);
        }
    });
    dialog.show_modal();
    dialog.destroy();
}

/// The Equalizer dialog (Python's EqualizerDialog): enable
/// checkbox, preset choice (libVLC's built-ins plus the user's
/// own), preamp + ten band fields in stored dB units, and
/// save/delete for user presets. Writes the "equalizer" and
/// "equalizer_user_presets" config keys on OK.
pub fn show_equalizer(frame: &Frame, config_path: &Path) {
    use blindrss_core::equalizer::*;
    let mut cfg = Config::load(config_path).unwrap_or_default();
    let current = EqConfig::from_config(&cfg);
    let user_presets: Rc<RefCell<Vec<UserPreset>>> = Rc::new(RefCell::new(normalize_user_presets(
        cfg.raw().get("equalizer_user_presets"),
    )));
    let dialog = Dialog::builder(frame, "Equalizer").build();
    bind_f1(frame, &dialog, "EqualizerDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let enabled_cb = CheckBox::builder(&dialog)
        .with_label(&crate::tr("Enable equalizer"))
        .build();
    enabled_cb.set_value(current.enabled);
    sizer.add(&enabled_cb, 0, SizerFlag::All, 4);

    let preset_row = BoxSizer::builder(Orientation::Horizontal).build();
    let preset_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Preset:"))
        .build();
    let preset_choice = Choice::builder(&dialog).build();
    preset_row.add(&preset_label, 0, SizerFlag::All, 4);
    preset_row.add(&preset_choice, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&preset_row, 0, SizerFlag::Expand | SizerFlag::All, 2);

    // Spin fields: preamp + one per band, labelled by frequency.
    let make_spin = |parent: &Dialog| -> SpinCtrlDouble {
        let s = SpinCtrlDouble::builder(parent)
            .with_range(AMP_MIN, AMP_MAX)
            .build();
        s.set_digits(1);
        s.set_increment(0.5);
        s
    };
    let preamp_row = BoxSizer::builder(Orientation::Horizontal).build();
    let preamp_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Preamp (dB):"))
        .build();
    let preamp_spin = make_spin(&dialog);
    preamp_spin.set_value(current.preamp);
    preamp_row.add(&preamp_label, 0, SizerFlag::All, 4);
    preamp_row.add(&preamp_spin, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&preamp_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    let mut band_spins: Vec<SpinCtrlDouble> = Vec::new();
    for (i, freq) in BAND_FREQUENCIES.iter().enumerate() {
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let label = StaticText::builder(&dialog)
            .with_label(&format!("{} (dB):", format_band_label(*freq)))
            .build();
        let spin = make_spin(&dialog);
        spin.set_value(current.bands[i]);
        row.add(&label, 0, SizerFlag::All, 4);
        row.add(&spin, 1, SizerFlag::Expand | SizerFlag::All, 4);
        sizer.add_sizer(&row, 0, SizerFlag::Expand | SizerFlag::All, 1);
        band_spins.push(spin);
    }

    // Preset list: index 0 = Custom, then built-ins, then the
    // user's presets. Refilled after save/delete.
    let fill_presets: Rc<dyn Fn(Option<u32>)> = {
        let user_presets = user_presets.clone();
        Rc::new(move |select: Option<u32>| {
            preset_choice.clear();
            preset_choice.append("Custom");
            for (name, _, _) in PRESETS.iter() {
                preset_choice.append(name);
            }
            for p in user_presets.borrow().iter() {
                preset_choice.append(&p.name);
            }
            preset_choice.set_selection(select.unwrap_or(0));
        })
    };
    // Initial selection: the stored preset name if it matches.
    let initial_sel = current
        .preset
        .as_ref()
        .and_then(|name| {
            if let Some(i) = PRESETS.iter().position(|p| p.0 == name) {
                Some((i + 1) as u32)
            } else {
                user_presets
                    .borrow()
                    .iter()
                    .position(|p| &p.name == name)
                    .map(|i| (i + 1 + PRESETS.len()) as u32)
            }
        })
        .unwrap_or(0);
    fill_presets(Some(initial_sel));

    // Choosing a preset loads its values into the fields.
    {
        let user_presets = user_presets.clone();
        let band_spins = band_spins.clone();
        preset_choice.on_selection_changed(move |_| {
            let Some(sel) = preset_choice.get_selection().map(|s| s as usize) else {
                return;
            };
            if sel == 0 {
                return;
            }
            let (preamp, bands) = if sel <= PRESETS.len() {
                let (_, pre, bands) = &PRESETS[sel - 1];
                // Built-ins are stored as effective dB; the
                // config units carry the +12 convention.
                (pre + 12.0, *bands)
            } else {
                let users = user_presets.borrow();
                let Some(p) = users.get(sel - 1 - PRESETS.len()) else {
                    return;
                };
                (p.preamp, p.bands)
            };
            preamp_spin.set_value(preamp);
            for (i, spin) in band_spins.iter().enumerate() {
                spin.set_value(bands[i]);
            }
            enabled_cb.set_value(true);
        });
    }

    let name_row = BoxSizer::builder(Orientation::Horizontal).build();
    let name_label = StaticText::builder(&dialog)
        .with_label(&crate::tr("Preset name:"))
        .build();
    let name_field = TextCtrl::builder(&dialog).build();
    let save_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Save Preset"))
        .build();
    let delete_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Delete Preset"))
        .build();
    name_row.add(&name_label, 0, SizerFlag::All, 4);
    name_row.add(&name_field, 1, SizerFlag::Expand | SizerFlag::All, 4);
    name_row.add(&save_btn, 0, SizerFlag::All, 4);
    name_row.add(&delete_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&name_row, 0, SizerFlag::Expand | SizerFlag::All, 2);
    {
        let user_presets = user_presets.clone();
        let fill = fill_presets.clone();
        let band_spins = band_spins.clone();
        save_btn.on_click(move |_| {
            let name = name_field.get_value();
            if name.trim().is_empty() {
                return;
            }
            let bands: [f64; BAND_COUNT] = std::array::from_fn(|i| band_spins[i].get_value());
            let mut list = user_presets.borrow_mut();
            list.retain(|p| !p.name.eq_ignore_ascii_case(name.trim()));
            list.push(UserPreset {
                name: name.trim().to_string(),
                preamp: clamp_amp(preamp_spin.get_value()),
                bands: bands.map(clamp_amp),
            });
            let sel = (list.len() + PRESETS.len()) as u32;
            drop(list);
            fill(Some(sel));
        });
    }
    {
        let user_presets = user_presets.clone();
        let fill = fill_presets.clone();
        delete_btn.on_click(move |_| {
            let Some(sel) = preset_choice.get_selection().map(|s| s as usize) else {
                return;
            };
            if sel <= PRESETS.len() {
                return;
            }
            let idx = sel - 1 - PRESETS.len();
            let mut list = user_presets.borrow_mut();
            if idx < list.len() {
                list.remove(idx);
            }
            drop(list);
            fill(Some(0));
        });
    }

    let btn_row = BoxSizer::builder(Orientation::Horizontal).build();
    let ok_btn = Button::builder(&dialog)
        .with_label(&crate::tr("OK"))
        .build();
    let cancel_btn = Button::builder(&dialog)
        .with_label(&crate::tr("Cancel"))
        .build();
    btn_row.add(&ok_btn, 0, SizerFlag::All, 4);
    btn_row.add(&cancel_btn, 0, SizerFlag::All, 4);
    sizer.add_sizer(&btn_row, 0, SizerFlag::All, 2);
    let saved: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    {
        let saved = saved.clone();
        let d = dialog;
        ok_btn.on_click(move |_| {
            *saved.borrow_mut() = true;
            d.end_modal(5100);
        });
    }
    let d = dialog;
    cancel_btn.on_click(move |_| d.end_modal(5101));
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(430, 560));
    enabled_cb.set_focus();
    let rc = dialog.show_modal();
    if rc == 5100 && *saved.borrow() {
        let bands: [f64; BAND_COUNT] = std::array::from_fn(|i| band_spins[i].get_value());
        let sel = preset_choice.get_selection().unwrap_or(0) as usize;
        // The preset name is recorded only when the fields
        // still match the chosen preset exactly.
        let preset_name = if sel == 0 {
            None
        } else if sel <= PRESETS.len() {
            let (name, pre, pbands) = &PRESETS[sel - 1];
            if (preamp_spin.get_value() - (pre + 12.0)).abs() < 0.001 && bands == *pbands {
                Some(name.to_string())
            } else {
                None
            }
        } else {
            user_presets
                .borrow()
                .get(sel - 1 - PRESETS.len())
                .filter(|p| (preamp_spin.get_value() - p.preamp).abs() < 0.001 && bands == p.bands)
                .map(|p| p.name.clone())
        };
        let eq = EqConfig {
            enabled: enabled_cb.get_value(),
            preamp: clamp_amp(preamp_spin.get_value()),
            bands: bands.map(clamp_amp),
            preset: preset_name,
        };
        cfg.set_path("equalizer", eq.to_value());
        let users: Vec<Value> = user_presets
            .borrow()
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "preamp": p.preamp,
                    "bands": p.bands.to_vec(),
                })
            })
            .collect();
        cfg.set_path("equalizer_user_presets", Value::Array(users));
        let _ = cfg.save(config_path);
    }
    dialog.destroy();
}

/// Add Shortcuts (Python's AddShortcutsDialog +
/// `mainframe.on_add_shortcuts`, Batch 42 / Audit 6 G2): choose
/// Desktop / Start Menu / Taskbar, create the shortcuts through
/// core's PowerShell/WScript.Shell port, then report each result
/// in one message box (warning icon when anything failed).
/// Reachable via the File menu item the parent wires to this
/// function; Python shows that item on every platform except
/// macOS and disables the checkboxes off Windows.
pub fn show_add_shortcuts(frame: &Frame) {
    use blindrss_core::windows_shortcuts::{self, ShortcutSelection};
    let dialog = Dialog::builder(frame, &crate::tr("Add BlindRSS Shortcuts")).build();
    bind_f1(frame, &dialog, "AddShortcutsDialog");
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let intro = StaticText::builder(&dialog)
        .with_label(&crate::tr(
            "Choose where to add BlindRSS shortcuts.\nTaskbar pinning may be limited by your Windows version/policies.",
        ))
        .build();
    sizer.add(&intro, 0, SizerFlag::All, 10);
    let mk_check = |label: &str, on: bool| {
        let cb = CheckBox::builder(&dialog)
            .with_label(&crate::tr(label))
            .build();
        cb.set_value(on);
        if !windows_shortcuts::is_windows() {
            cb.enable(false);
        }
        sizer.add(&cb, 0, SizerFlag::All, 4);
        cb
    };
    let desktop_cb = mk_check("Desktop", true);
    let start_menu_cb = mk_check("Start Menu", true);
    let taskbar_cb = mk_check("Taskbar", false);
    ok_cancel(&dialog, &sizer);
    dialog.set_sizer(sizer, true);
    dialog.set_size(Size::new(460, 280));
    if dialog.show_modal() != 5100 {
        dialog.destroy();
        return;
    }
    let selection = ShortcutSelection {
        desktop: desktop_cb.get_value(),
        start_menu: start_menu_cb.get_value(),
        taskbar: taskbar_cb.get_value(),
    };
    dialog.destroy();
    if !selection.any() {
        return;
    }
    let results = windows_shortcuts::create_shortcuts(selection, windows_shortcuts::APP_NAME);
    let (text, failed) = windows_shortcuts::format_results(&results);
    let style = if failed {
        MessageDialogStyle::OK | MessageDialogStyle::IconWarning
    } else {
        MessageDialogStyle::OK | MessageDialogStyle::IconInformation
    };
    let md = MessageDialog::builder(frame, &text, &crate::tr("Shortcuts"))
        .with_style(style)
        .build();
    md.show_modal();
    md.destroy();
}

#[cfg(test)]
mod category_properties_tests {
    use super::{category_edited_path, category_leaf_parent, category_parent_choices};

    #[test]
    fn parent_choices_exclude_self_and_descendants() {
        let all: Vec<String> = ["News", "News / Tech", "News / Tech / AI", "Sports"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            category_parent_choices("News / Tech", &all),
            vec!["News".to_string(), "Sports".to_string()]
        );
        // A top-level category can move under anything but itself.
        assert_eq!(category_parent_choices("Sports", &all).len(), 3);
    }

    #[test]
    fn edited_path_joins_leaf_under_parent() {
        assert_eq!(category_edited_path("Tech", None), "Tech");
        assert_eq!(category_edited_path("Tech", Some("News")), "News / Tech");
        assert_eq!(
            category_edited_path("  Tech  ", Some(" News ")),
            "News / Tech"
        );
        // The spaced separator typed into the leaf cannot corrupt
        // the path (Python sanitize_category_leaf); a bare "/"
        // is an ordinary leaf character.
        assert_eq!(category_edited_path("A / B", None), "A - B");
        assert_eq!(category_edited_path("AC/DC", None), "AC/DC");
    }

    #[test]
    fn leaf_parent_split_matches_storage_paths() {
        assert_eq!(
            category_leaf_parent("News / Tech"),
            ("Tech".to_string(), Some("News".to_string()))
        );
        assert_eq!(category_leaf_parent("News"), ("News".to_string(), None));
    }
}
